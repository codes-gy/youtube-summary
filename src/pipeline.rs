use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;

use crate::cache::cached;
use crate::claude::{BackendChoice, Llm};
use crate::domain::{FrameSet, Summary, Transcript, VideoMeta, assign_frames};
use crate::lang::LangCode;
use crate::source::Source;
use crate::{frames, media, render, summarize, transcribe, translate};

#[derive(Debug, Clone)]
pub struct Options {
    pub cache_dir: PathBuf,
    pub whisper_model: String,
    pub max_images: usize,
    pub summarizer: BackendChoice,
    /// None keeps the spoken language.
    pub translate: Option<LangCode>,
    /// None writes `<video id or file name>.pdf` in the current directory.
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Download,
    Audio,
    Transcribe,
    Frames,
    Translate,
    Summarize,
    Pdf,
}

impl Stage {
    pub const ALL: [Stage; 7] = [
        Stage::Download,
        Stage::Audio,
        Stage::Transcribe,
        Stage::Frames,
        Stage::Translate,
        Stage::Summarize,
        Stage::Pdf,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Stage::Download => "영상 준비",
            Stage::Audio => "소리 꺼내기",
            Stage::Transcribe => "받아쓰기",
            Stage::Frames => "장면 뽑기",
            Stage::Translate => "번역",
            Stage::Summarize => "요약",
            Stage::Pdf => "PDF 만들기",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Cached,
    Skipped,
}

pub enum Event {
    Started(Stage),
    /// Sub-step progress, e.g. a download percentage.
    Detail(Stage, String),
    Finished(Stage, Duration, Outcome),
}

/// Receives pipeline events; the CLI prints them, the web server stores them for polling.
pub trait Progress: Sync {
    fn event(&self, event: Event);
}

pub struct Finished {
    pub pdf: PathBuf,
    pub title: String,
}

/// Runs `step`, timing it and reporting whether it was cached.
fn stage<T>(progress: &dyn Progress, stage: Stage, step: impl FnOnce() -> Result<(T, Outcome)>) -> Result<T> {
    progress.event(Event::Started(stage));
    let began = Instant::now();
    let (value, outcome) = step()?;
    progress.event(Event::Finished(stage, began.elapsed(), outcome));
    Ok(value)
}

fn outcome(was_cached: bool) -> Outcome {
    if was_cached { Outcome::Cached } else { Outcome::Done }
}

/// The whole job, shared by the CLI and the web server. Every stage is cached in
/// `<cache_dir>/<key>/`, so a rerun resumes where the last one stopped.
pub fn run_pipeline(source: &Source, opts: &Options, progress: &dyn Progress) -> Result<Finished> {
    let detail = |s: Stage| move |msg: String| progress.event(Event::Detail(s, msg));

    let (work, meta) = stage(progress, Stage::Download, || {
        let (work, meta, was_cached) = match source {
            Source::YouTube { id } => {
                let work = opts.cache_dir.join(id);
                let (meta, c) = cached(&work.join("meta.json"), || {
                    media::download_youtube(id, &work, &detail(Stage::Download))
                })?;
                (work, meta, c)
            }
            Source::LocalFile { path } => {
                progress.event(Event::Detail(Stage::Download, "파일 확인 중".into()));
                let work = opts.cache_dir.join(media::file_key(path)?);
                let (meta, c) = cached(&work.join("meta.json"), || media::ingest_local(path, &work))?;
                (work, meta, c)
            }
        };
        Ok(((work, meta), outcome(was_cached)))
    })?;
    let meta: VideoMeta = meta;
    let video = media::video_path(&work)?;

    let wav = work.join("audio.wav");
    stage(progress, Stage::Audio, || {
        if wav.is_file() {
            return Ok(((), Outcome::Cached));
        }
        media::extract_audio(&video, &wav)?;
        Ok(((), Outcome::Done))
    })?;

    let model_tag = Path::new(&opts.whisper_model)
        .file_stem()
        .map_or_else(|| opts.whisper_model.clone(), |s| s.to_string_lossy().into_owned());
    // Whisper and ffmpeg each need only the video, so they run side by side.
    let (transcript, frame_set) = std::thread::scope(|scope| {
        let frames_job = scope.spawn(|| {
            stage(progress, Stage::Frames, || {
                if !meta.has_video {
                    return Ok((FrameSet { frames: Vec::new() }, Outcome::Skipped));
                }
                let (f, c) = cached(&work.join("frames.json"), || Ok(FrameSet { frames: frames::extract(&video, &work)? }))?;
                Ok((f, outcome(c)))
            })
        });
        let transcript: Result<Transcript> = stage(progress, Stage::Transcribe, || {
            let (t, c) = cached(&work.join(format!("transcript.{model_tag}.json")), || {
                let on = detail(Stage::Transcribe);
                let model = transcribe::model_file(&opts.whisper_model, &on)?;
                transcribe::transcribe(&media::read_wav(&wav)?, &model, &on)
            })?;
            Ok((t, outcome(c)))
        });
        let frame_set: Result<FrameSet> = frames_job.join().expect("frames thread panicked");
        anyhow::Ok((transcript?, frame_set?))
    })?;
    anyhow::ensure!(!transcript.segments.is_empty(), "영상에서 말소리를 찾지 못해 요약할 내용이 없습니다.");

    let spoken: Option<LangCode> = transcript.language.parse().ok();
    let target = opts.translate.clone().filter(|t| Some(t) != spoken.as_ref());
    let backend = std::sync::OnceLock::new();
    let llm = || -> Result<&dyn Llm> {
        if let Some(b) = backend.get() {
            return Ok(b as &dyn Llm);
        }
        let b = opts.summarizer.resolve()?;
        Ok(backend.get_or_init(|| b) as &dyn Llm)
    };

    // The summary reads the original transcript, so it need not wait for the translation.
    let out_lang: LangCode = target.clone().or(spoken.clone()).unwrap_or_else(|| "en".parse().unwrap());
    let (translation, summary) = std::thread::scope(|scope| {
        let translate_job = scope.spawn(|| {
            stage(progress, Stage::Translate, || match &target {
                None => Ok((None, Outcome::Skipped)),
                Some(lang) => {
                    let (t, c) = cached(&work.join(format!("translation.{lang}.{model_tag}.json")), || {
                        translate::translate(llm()?, &transcript, lang)
                    })?;
                    Ok((Some(t), outcome(c)))
                }
            })
        });
        let summary: Result<Summary> = stage(progress, Stage::Summarize, || {
            let (s, c) = cached(&work.join(format!("summary.{out_lang}.{model_tag}.json")), || {
                summarize::summarize(llm()?, &meta, &transcript, &out_lang)
            })?;
            Ok((s, outcome(c)))
        });
        let translation: Result<Option<Transcript>> = translate_job.join().expect("translate thread panicked");
        anyhow::Ok((translation?, summary?))
    })?;

    let pdf = opts.output.clone().unwrap_or_else(|| PathBuf::from(default_pdf_name(&meta)));
    stage(progress, Stage::Pdf, || {
        let sections = assign_frames(&summary.chapters, &frame_set.frames, opts.max_images);
        let doc = render::Document {
            meta: &meta,
            summary: &summary,
            sections: &sections,
            transcript: translation.as_ref().unwrap_or(&transcript),
            lang: &out_lang,
            translated_from: target.as_ref().and(spoken.as_ref()),
        };
        // Frame paths are relative to the work dir.
        render::render_pdf(&doc, &work, &pdf)?;
        Ok(((), Outcome::Done))
    })?;
    Ok(Finished { pdf, title: summary.title })
}

fn default_pdf_name(meta: &VideoMeta) -> String {
    match &meta.origin {
        crate::domain::Origin::YouTube { id } => format!("{id}.pdf"),
        crate::domain::Origin::LocalFile { file_name } => {
            let stem = Path::new(file_name).file_stem().map_or("video".into(), |s| s.to_string_lossy());
            format!("{stem}.pdf")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use crate::{frames, media, transcribe};

    /// Runs the outside tools every OS must have: ffmpeg/ffprobe and whisper.cpp.
    /// Downloads the small `tiny` Whisper model, so CI runs it with `--include-ignored`.
    #[test]
    #[ignore = "needs ffmpeg and network"]
    fn local_tools_work_on_this_os() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("입력 영상.mp4");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=4:size=320x240:rate=10"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4", "-shortest", "-pix_fmt", "yuv420p"])
            .arg(&input)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "ffmpeg made the test video");

        let work = dir.path().join("work");
        let meta = media::ingest_local(&input, &work).unwrap();
        assert!(meta.has_video && meta.duration > 3.0, "ffprobe read the video: {meta:?}");

        let video = media::video_path(&work).unwrap();
        let wav = work.join("audio.wav");
        media::extract_audio(&video, &wav).unwrap();
        let samples = media::read_wav(&wav).unwrap();
        assert!(samples.len() > 3 * media::SAMPLE_RATE as usize, "audio is about 4 seconds long");

        let picked = frames::extract(&video, &work).unwrap();
        assert!(!picked.is_empty(), "ffmpeg picked at least one frame");
        assert!(work.join(&picked[0].path).is_file(), "frame file exists");

        let model = transcribe::model_file("tiny", &|_| {}).unwrap();
        transcribe::transcribe(&samples, &model, &|_| {}).unwrap();
    }
}
