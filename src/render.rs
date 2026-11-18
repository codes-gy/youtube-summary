use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use minijinja::Environment;
use minijinja::value::Serde;
use serde_json::json;

use crate::domain::{Section, Summary, Transcript, VideoMeta, clock};
use crate::lang::LangCode;
use crate::platform;

const TEMPLATE: &str = include_str!("../templates/document.html");

pub struct Document<'a> {
    pub meta: &'a VideoMeta,
    pub summary: &'a Summary,
    pub sections: &'a [Section],
    /// In the output language: the translation when translating, else the original.
    pub transcript: &'a Transcript,
    pub lang: &'a LangCode,
    /// Set when the transcript was translated: the spoken language it came from.
    pub translated_from: Option<&'a LangCode>,
}

pub fn render_html(doc: &Document) -> Result<String> {
    let mut env = Environment::new();
    env.add_filter("clock", clock);
    env.add_template("document.html", TEMPLATE)?;
    let upload_date = doc
        .meta
        .upload_date
        .as_deref()
        .filter(|d| d.len() == 8)
        .map(|d| format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..]));
    let html = env.get_template("document.html")?.render(Serde(json!({
        "lang": doc.lang.as_str(),
        "t": doc.lang.labels(),
        "meta": doc.meta,
        "video_url": doc.meta.watch_url(),
        "file_name": match &doc.meta.origin {
            crate::domain::Origin::LocalFile { file_name } => Some(file_name.as_str()),
            crate::domain::Origin::YouTube { .. } => None,
        },
        "summary": doc.summary,
        "overview": doc.summary.overview.lines().map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>(),
        "sections": doc.sections,
        "segments": &doc.transcript.segments,
        "translated_from": doc.translated_from.map(|l| l.english_name()),
        "upload_date": upload_date,
        "generated": chrono::Local::now().format("%Y-%m-%d").to_string(),
    })))?;
    Ok(html)
}

/// Print the HTML file to an A4 PDF with headless Chrome.
pub fn html_to_pdf(html: &Path, pdf: &Path) -> Result<()> {
    let chrome = platform::find_chrome().context("PDF를 만들려면 Google Chrome, Chromium, Microsoft Edge 중 하나가 필요합니다.")?;
    let profile = tempfile::tempdir()?;
    if let Some(dir) = pdf.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let pdf_abs = std::path::absolute(pdf)?;
    let tmp = pdf_abs.with_extension("pdf.tmp");
    let _ = std::fs::remove_file(&tmp);
    let html_abs = html.canonicalize()?;
    let mut child = Command::new(chrome)
        .args(["--headless", "--disable-gpu", "--no-pdf-header-footer", "--allow-file-access-from-files"])
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg(format!("--print-to-pdf={}", tmp.display()))
        .arg(platform::file_url(&html_abs))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(profile.path().join("stderr.log"))?)
        .spawn()
        .context("Chrome을 실행하지 못했습니다")?;
    let began = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if began.elapsed() > Duration::from_secs(180) {
            let _ = child.kill();
            bail!("Chrome이 3분 안에 PDF를 만들지 못했습니다.");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if !status.success() || !tmp.is_file() {
        let stderr = std::fs::read_to_string(profile.path().join("stderr.log")).unwrap_or_default();
        bail!("Chrome이 PDF를 만들지 못했습니다: {}", stderr.lines().last().unwrap_or_default());
    }
    std::fs::rename(&tmp, pdf)?;
    Ok(())
}
