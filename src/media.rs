//! Getting the video and its audio onto disk: yt-dlp, local files, ffprobe, ffmpeg.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::domain::{Origin, VideoMeta};

const FORMAT: &str = "bv*[height<=720][ext=mp4]+ba[ext=m4a]/b[height<=720][ext=mp4]/b[height<=720]/b";

/// Run an external tool to completion, turning "not installed" into an install hint.
pub fn run(cmd: &mut Command) -> Result<Output> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    match cmd.stdin(Stdio::null()).output() {
        Ok(out) => Ok(out),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("{program}이(가) 필요합니다. macOS: brew install {program}"),
        Err(e) => Err(e).with_context(|| format!("{program} 실행 실패")),
    }
}

pub fn stderr_tail(out: &Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// The working copy of the video inside the cache dir.
pub fn video_path(work: &Path) -> Result<PathBuf> {
    fs::read_dir(work)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.file_stem().is_some_and(|s| s == "video"))
        .context("캐시에 영상 파일이 없습니다. 캐시 폴더를 지우고 다시 실행하세요.")
}

#[derive(Deserialize)]
struct YtInfo {
    id: String,
    title: Option<String>,
    channel: Option<String>,
    uploader: Option<String>,
    duration: Option<f64>,
    upload_date: Option<String>,
}

/// Download (720p or lower, with audio) into `work/video.mp4`, reporting progress percentages.
pub fn download_youtube(id: &str, work: &Path, on_progress: &dyn Fn(String)) -> Result<VideoMeta> {
    fs::create_dir_all(work)?;
    let url = crate::source::Source::youtube_url(id);
    let mut child = Command::new("yt-dlp")
        .args(["-f", FORMAT, "--merge-output-format", "mp4", "--remux-video", "mp4"])
        .args(["--no-playlist", "--retries", "10", "--fragment-retries", "10", "--no-warnings"])
        .args(["--newline", "--progress-template", "download:%(progress._percent_str)s"])
        .args(["--write-info-json", "-o"])
        .arg(work.join("video.%(ext)s"))
        .arg("-o")
        .arg(format!("infojson:{}", work.join("info").display()))
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => anyhow::anyhow!("yt-dlp가 필요합니다. macOS: brew install yt-dlp deno"),
            _ => anyhow::Error::new(e).context("yt-dlp 실행 실패"),
        })?;
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    for line in BufReader::new(child.stdout.take().expect("piped stdout")).lines() {
        let line = line?;
        let pct = line.trim();
        if pct.ends_with('%') {
            on_progress(format!("내려받는 중 {pct}"));
        }
    }
    let status = child.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        let reason = stderr
            .lines()
            .rev()
            .find(|l| l.starts_with("ERROR:"))
            .map_or_else(|| stderr.trim().to_string(), |l| l.trim_start_matches("ERROR:").trim().to_string());
        bail!("영상을 내려받지 못했습니다: {reason}");
    }
    let info: YtInfo = serde_json::from_slice(&fs::read(work.join("info.info.json"))?)
        .context("yt-dlp 정보 파일을 읽지 못했습니다")?;
    Ok(VideoMeta {
        title: info.title.unwrap_or_else(|| info.id.clone()),
        channel: info.channel.or(info.uploader).filter(|c| !c.is_empty()),
        duration: info.duration.unwrap_or(0.0),
        upload_date: info.upload_date,
        origin: Origin::YouTube { id: info.id },
        has_video: true,
    })
}

/// Cache key for a local file: a hash of its bytes, so a renamed or re-uploaded file reuses the cache.
pub fn file_key(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("{}을(를) 열 수 없습니다", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(format!("file-{}", digest[..8].iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

#[derive(Deserialize)]
struct Probe {
    format: ProbeFormat,
    #[serde(default)]
    streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    #[serde(default)]
    disposition: std::collections::HashMap<String, i64>,
}

/// Link (or copy) a local video into the work dir and read its metadata with ffprobe.
pub fn ingest_local(path: &Path, work: &Path) -> Result<VideoMeta> {
    let out = run(Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path))?;
    if !out.status.success() {
        bail!("영상 파일로 읽을 수 없습니다: {}", stderr_tail(&out));
    }
    let probe: Probe = serde_json::from_slice(&out.stdout).context("ffprobe 출력을 읽지 못했습니다")?;
    let has = |kind: &str| {
        probe.streams.iter().any(|s| s.codec_type.as_deref() == Some(kind) && s.disposition.get("attached_pic") != Some(&1))
    };
    if !has("audio") {
        bail!("이 파일에는 소리가 없어서 받아쓸 수 없습니다.");
    }
    fs::create_dir_all(work)?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_ascii_lowercase();
    let dest = work.join(format!("video.{ext}"));
    if !dest.exists() {
        let tmp = work.join(format!("video.{ext}.part"));
        let _ = fs::remove_file(&tmp);
        if fs::hard_link(path, &tmp).is_err() {
            fs::copy(path, &tmp).context("영상 파일을 캐시에 복사하지 못했습니다")?;
        }
        fs::rename(&tmp, &dest)?;
    }
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(VideoMeta {
        title: probe.format.tags.get("title").filter(|t| !t.trim().is_empty()).cloned().unwrap_or(stem),
        channel: probe.format.tags.get("artist").cloned(),
        duration: probe.format.duration.and_then(|d| d.parse().ok()).unwrap_or(0.0),
        upload_date: None,
        origin: Origin::LocalFile { file_name },
        has_video: has("video"),
    })
}

pub const SAMPLE_RATE: u32 = 16000;

/// Decode the audio track to 16 kHz mono 16-bit WAV, the input Whisper expects.
pub fn extract_audio(video: &Path, wav: &Path) -> Result<()> {
    let tmp = wav.with_extension("tmp.wav");
    let out = run(Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(video)
        .args(["-vn", "-ac", "1", "-ar", &SAMPLE_RATE.to_string(), "-c:a", "pcm_s16le"])
        .arg(&tmp))?;
    if !out.status.success() {
        bail!("ffmpeg로 소리를 꺼내지 못했습니다: {}", stderr_tail(&out));
    }
    fs::rename(&tmp, wav)?;
    Ok(())
}

pub fn read_wav(wav: &Path) -> Result<Vec<f32>> {
    let reader = hound::WavReader::open(wav).with_context(|| format!("{} 읽기 실패", wav.display()))?;
    reader
        .into_samples::<i16>()
        .map(|s| Ok(f32::from(s?) / 32768.0))
        .collect()
}
