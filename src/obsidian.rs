//! The same document as the PDF, as a Markdown note in an Obsidian vault.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::domain::{Origin, clock};
use crate::render::Document;

/// Notes go in this folder of the vault, scene images in its `첨부` subfolder.
const FOLDER: &str = "YouTube 요약";

/// The vault Obsidian has open, or else the one it opened last.
pub fn find_vault() -> Result<PathBuf> {
    let config = config_dirs().into_iter().map(|d| d.join("obsidian.json")).find(|f| f.is_file()).context(
        "Obsidian 설정을 찾지 못했습니다. Obsidian에서 보관함을 한 번 연 뒤 다시 하거나 --obsidian-vault로 폴더를 지정하세요.",
    )?;
    let text = std::fs::read_to_string(&config)?;
    let vault = latest_vault(&text).with_context(|| format!("{}에 보관함이 없습니다", config.display()))?;
    check_vault(&vault)
}

pub fn check_vault(path: &Path) -> Result<PathBuf> {
    if !path.join(".obsidian").is_dir() {
        bail!("{}은(는) Obsidian 보관함이 아닙니다 (.obsidian 폴더가 없음)", path.display());
    }
    Ok(std::path::absolute(path)?)
}

/// Where each OS keeps Obsidian's `obsidian.json`.
fn config_dirs() -> Vec<PathBuf> {
    let home = crate::platform::home_dir().unwrap_or_default();
    let mut dirs = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.push(home.join("Library/Application Support/obsidian"));
    } else if cfg!(windows) {
        dirs.extend(std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("obsidian")));
    } else {
        let config = std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        dirs.push(config.join("obsidian"));
        dirs.push(home.join(".var/app/md.obsidian.Obsidian/config/obsidian"));
    }
    dirs
}

/// `obsidian.json` lists vaults as `{"vaults": {id: {"path", "ts", "open"}}}`.
fn latest_vault(config: &str) -> Option<PathBuf> {
    let json: Value = serde_json::from_str(config).ok()?;
    json["vaults"]
        .as_object()?
        .values()
        .filter_map(|v| Some((v["open"].as_bool() == Some(true), v["ts"].as_u64().unwrap_or(0), v["path"].as_str()?)))
        .max()
        .map(|(_, _, path)| PathBuf::from(path))
}

/// Write the note and copy its scene images; returns the note's path.
pub fn write_note(doc: &Document, work: &Path, vault: &Path) -> Result<PathBuf> {
    let key = match &doc.meta.origin {
        Origin::YouTube { id } => id.clone(),
        Origin::LocalFile { file_name } => safe_name(file_name),
    };
    let images = format!("{FOLDER}/첨부/{key}");
    for frame in doc.sections.iter().flat_map(|s| &s.frames) {
        let dest = vault.join(&images).join(file_name(&frame.path));
        std::fs::create_dir_all(dest.parent().unwrap())?;
        std::fs::copy(work.join(&frame.path), &dest).with_context(|| format!("{} 복사 실패", dest.display()))?;
    }
    let note = note_path(&vault.join(FOLDER), &doc.summary.title, &key);
    crate::cache::write_atomic(&note, markdown(doc, &key, &images).as_bytes())?;
    Ok(note)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `<title>.md`, unless a note by that name is about another video or isn't ours.
fn note_path(folder: &Path, title: &str, key: &str) -> PathBuf {
    let title = safe_name(title);
    let ours = |p: &Path| !p.exists() || std::fs::read_to_string(p).is_ok_and(|t| t.contains(&key_line(key)));
    let plain = folder.join(format!("{title}.md"));
    if ours(&plain) { plain } else { folder.join(format!("{title} ({key}).md")) }
}

fn key_line(key: &str) -> String {
    format!("ytdoc: {}", Value::from(key))
}

/// Characters Obsidian refuses in note names or that break `[[links]]`.
fn safe_name(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() || "/\\:*?\"<>|#^[]".contains(c) { ' ' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean: String = clean.chars().take(80).collect();
    let clean = clean.trim_start_matches('.').trim().to_string();
    if clean.is_empty() { "영상 요약".into() } else { clean }
}

/// Text from the video shown as text: Markdown and Obsidian syntax is escaped.
fn text(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        // `%%`, `==` and `~~` only mean something doubled; a lone `91.52%` stays as it is.
        let doubled = chars.get(i + 1) == Some(&c) || (i > 0 && chars[i - 1] == c);
        if "\\`*_[]<>#|$^".contains(c) || ("%=~".contains(c) && doubled) {
            out.push('\\');
        }
        out.push(c);
    }
    out.replace('\n', " ")
}

fn markdown(doc: &Document, key: &str, images: &str) -> String {
    let t = doc.lang.labels();
    let url = doc.meta.watch_url();
    let at = |seconds: f64, label: String| match &url {
        Some(url) => format!("[{label}]({url}&t={}s)", seconds.max(0.0) as u64),
        None => label,
    };
    let span = |start: f64, end: f64| format!("{}–{}", clock(start), clock(end));
    let mut md = String::new();

    // Properties: JSON strings are valid YAML, so every value is quoted safely.
    let q = |s: &str| Value::from(s).to_string();
    md.push_str("---\n");
    let _ = writeln!(md, "title: {}", q(&doc.summary.title));
    let _ = writeln!(md, "{}", key_line(key));
    match (&url, &doc.meta.origin) {
        (Some(url), _) => _ = writeln!(md, "source: {}", q(url)),
        (None, Origin::LocalFile { file_name }) => _ = writeln!(md, "file: {}", q(file_name)),
        (None, Origin::YouTube { .. }) => {}
    }
    let _ = writeln!(md, "original_title: {}", q(&doc.meta.title));
    if let Some(channel) = &doc.meta.channel {
        let _ = writeln!(md, "channel: {}", q(channel));
    }
    let _ = writeln!(md, "duration: {}", q(&clock(doc.meta.duration)));
    if let Some(d) = doc.meta.upload_date.as_deref().filter(|d| d.len() == 8) {
        let _ = writeln!(md, "uploaded: {}-{}-{}", &d[..4], &d[4..6], &d[6..]);
    }
    let _ = writeln!(md, "created: {}", chrono::Local::now().format("%Y-%m-%d"));
    let _ = writeln!(md, "language: {}", doc.lang.as_str());
    md.push_str("tags:\n  - youtube-summary\n---\n\n");

    let _ = writeln!(md, "> {}\n", text(&doc.summary.one_line));

    let _ = writeln!(md, "## {}\n", t.overview);
    for para in doc.summary.overview.lines().map(str::trim).filter(|p| !p.is_empty()) {
        let _ = writeln!(md, "{}\n", text(para));
    }
    for point in &doc.summary.key_points {
        let _ = writeln!(md, "- {}", text(point));
    }

    let _ = writeln!(md, "\n## {}\n", t.contents);
    for (i, s) in doc.sections.iter().enumerate() {
        let c = &s.chapter;
        let _ = writeln!(md, "{}. {} {}", i + 1, at(c.start, span(c.start, c.end)), text(&c.title));
    }

    for (i, s) in doc.sections.iter().enumerate() {
        let c = &s.chapter;
        let _ = writeln!(md, "\n## {}. {}\n", i + 1, text(&c.title));
        let _ = writeln!(md, "{}\n", at(c.start, format!("▶ {}", span(c.start, c.end))));
        for f in &s.frames {
            let _ = writeln!(md, "![[{images}/{}|480]]", file_name(&f.path));
            let _ = writeln!(md, "{}\n", at(f.time, clock(f.time)));
        }
        let _ = writeln!(md, "{}\n", text(&c.summary));
        let _ = writeln!(md, "**{}**\n", t.key_points);
        for point in &c.key_points {
            let _ = writeln!(md, "- {}", text(point));
        }
    }

    // Folded, so the summary stays the first thing a reader sees.
    let _ = writeln!(md, "\n## {}\n\n> [!quote]- {}", t.transcript, t.transcript);
    for seg in &doc.transcript.segments {
        let _ = writeln!(md, "> - {} {}", at(seg.start, clock(seg.start)), text(&seg.text));
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Chapter, Frame, Section, Segment, Summary, Transcript, VideoMeta};
    use crate::lang::LangCode;

    fn sample(title: &str) -> (VideoMeta, Summary, Vec<Section>, Transcript) {
        let meta = VideoMeta {
            title: "원래 제목".into(),
            channel: Some("생활코딩".into()),
            duration: 370.0,
            upload_date: Some("20151213".into()),
            origin: Origin::YouTube { id: "JdgjWfve1DY".into() },
            has_video: true,
        };
        let chapter =
            Chapter { start: 0.0, end: 107.0, title: "브라우저 호환성".into(), summary: "요약 문장".into(), key_points: vec!["핵심".into()] };
        let summary = Summary {
            title: title.into(),
            one_line: "한 줄 요약".into(),
            overview: "첫 문단\n\n둘째 문단".into(),
            key_points: vec!["전체 핵심".into()],
            chapters: vec![chapter.clone()],
        };
        let frames = vec![Frame { time: 30.0, path: "frames/0001.jpg".into(), hash: 0 }];
        let transcript = Transcript { language: "ko".into(), segments: vec![Segment { start: 65.0, end: 67.0, text: "말한 내용".into() }] };
        (meta, summary, vec![Section { chapter, frames }], transcript)
    }

    fn write(vault: &Path, title: &str) -> PathBuf {
        let (meta, summary, sections, transcript) = sample(title);
        let lang: LangCode = "ko".parse().unwrap();
        let doc = Document { meta: &meta, summary: &summary, sections: &sections, transcript: &transcript, lang: &lang, translated_from: None };
        let work = tempfile::tempdir().unwrap();
        std::fs::create_dir(work.path().join("frames")).unwrap();
        std::fs::write(work.path().join("frames/0001.jpg"), b"jpg").unwrap();
        write_note(&doc, work.path(), vault).unwrap()
    }

    #[test]
    fn note_has_properties_links_images_and_folded_transcript() {
        let vault = tempfile::tempdir().unwrap();
        let note = write(vault.path(), "caniuse 사용법");
        assert_eq!(note, vault.path().join("YouTube 요약/caniuse 사용법.md"));
        let md = std::fs::read_to_string(&note).unwrap();
        assert!(md.starts_with("---\ntitle: \"caniuse 사용법\"\nytdoc: \"JdgjWfve1DY\"\nsource: \"https://www.youtube.com/watch?v=JdgjWfve1DY\""));
        assert!(md.contains("uploaded: 2015-12-13\n"));
        assert!(md.contains("1. [0:00–1:47](https://www.youtube.com/watch?v=JdgjWfve1DY&t=0s) 브라우저 호환성"));
        assert!(md.contains("![[YouTube 요약/첨부/JdgjWfve1DY/0001.jpg|480]]"));
        assert!(md.contains("> [!quote]- 부록: 전체 받아쓰기\n> - [1:05](https://www.youtube.com/watch?v=JdgjWfve1DY&t=65s) 말한 내용"));
        assert_eq!(std::fs::read(vault.path().join("YouTube 요약/첨부/JdgjWfve1DY/0001.jpg")).unwrap(), b"jpg");
    }

    #[test]
    fn rerun_replaces_its_own_note_but_never_someone_elses() {
        let vault = tempfile::tempdir().unwrap();
        let first = write(vault.path(), "같은 제목");
        assert_eq!(write(vault.path(), "같은 제목"), first, "the same video updates its note");
        std::fs::write(&first, "내가 직접 쓴 노트").unwrap();
        let second = write(vault.path(), "같은 제목");
        assert_eq!(second, vault.path().join("YouTube 요약/같은 제목 (JdgjWfve1DY).md"));
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "내가 직접 쓴 노트");
    }

    #[test]
    fn video_text_cannot_become_obsidian_syntax() {
        assert_eq!(text("[[비밀]] #태그 <b>x</b> ==강조== %%숨김%%"), r"\[\[비밀\]\] \#태그 \<b\>x\</b\> \=\=강조\=\= \%\%숨김\%\%");
        assert_eq!(text("지원율 91.52% = 높음"), "지원율 91.52% = 높음");
        assert_eq!(safe_name("a/b: [[c]]#d"), "a b c d");
    }

    #[test]
    fn picks_the_open_vault_then_the_latest() {
        let config = r#"{"vaults":{"a":{"path":"/old","ts":1},"b":{"path":"/open","ts":0,"open":true},"c":{"path":"/new","ts":9}}}"#;
        assert_eq!(latest_vault(config), Some(PathBuf::from("/open")));
        let config = r#"{"vaults":{"a":{"path":"/old","ts":1},"c":{"path":"/new","ts":9}}}"#;
        assert_eq!(latest_vault(config), Some(PathBuf::from("/new")));
        assert_eq!(latest_vault("{}"), None);
    }
}
