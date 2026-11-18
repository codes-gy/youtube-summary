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
    let transcript: Transcript = stage(progress, Stage::Transcribe, || {
        let (t, c) = cached(&work.join(format!("transcript.{model_tag}.json")), || {
            let on = detail(Stage::Transcribe);
            let model = transcribe::model_file(&opts.whisper_model, &on)?;
            transcribe::transcribe(&media::read_wav(&wav)?, &model, &on)
        })?;
        Ok((t, outcome(c)))
    })?;
    anyhow::ensure!(!transcript.segments.is_empty(), "영상에서 말소리를 찾지 못해 요약할 내용이 없습니다.");

    let frame_set: FrameSet = stage(progress, Stage::Frames, || {
        if !meta.has_video {
            return Ok((FrameSet { frames: Vec::new() }, Outcome::Skipped));
        }
        let (f, c) = cached(&work.join("frames.json"), || Ok(FrameSet { frames: frames::extract(&video, &work)? }))?;
        Ok((f, outcome(c)))
    })?;

    let spoken: Option<LangCode> = transcript.language.parse().ok();
    let target = opts.translate.clone().filter(|t| Some(t) != spoken.as_ref());
    let backend = std::cell::OnceCell::new();
    let llm = || -> Result<&dyn Llm> {
        if let Some(b) = backend.get() {
            return Ok(b as &dyn Llm);
        }
        let b = opts.summarizer.resolve()?;
        Ok(backend.get_or_init(|| b) as &dyn Llm)
    };

    let translation: Option<Transcript> = stage(progress, Stage::Translate, || match &target {
        None => Ok((None, Outcome::Skipped)),
        Some(lang) => {
            let (t, c) = cached(&work.join(format!("translation.{lang}.{model_tag}.json")), || {
                translate::translate(llm()?, &transcript, lang)
            })?;
            Ok((Some(t), outcome(c)))
        }
    })?;

    let out_lang: LangCode = target.clone().or(spoken.clone()).unwrap_or_else(|| "en".parse().unwrap());
    let summary: Summary = stage(progress, Stage::Summarize, || {
        let (s, c) = cached(&work.join(format!("summary.{out_lang}.{model_tag}.json")), || {
            summarize::summarize(llm()?, &meta, &transcript, &out_lang)
        })?;
        Ok((s, outcome(c)))
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
        // The HTML lives next to frames/ so its relative image paths resolve.
        let html = work.join(format!("document.{out_lang}.html"));
        crate::cache::write_atomic(&html, render::render_html(&doc)?.as_bytes())?;
        render::html_to_pdf(&html, &pdf)?;
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
