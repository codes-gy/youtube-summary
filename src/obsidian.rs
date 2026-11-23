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
    let config =
        config_dirs().into_iter().map(|d| d.join("obsidian.json")).find(|f| f.is_file()).context(
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
    let folder = vault.join(FOLDER);
    let note = note_path(&folder, &doc.summary.title, &key);
    let olds = older_notes(&folder, &key, &note);
    let mut carried = Carried::default();
    for path in std::iter::once(&note).filter(|p| is_note_for(p, &key)).chain(&olds) {
        carried.absorb(&std::fs::read_to_string(path).with_context(|| format!("{} 읽기 실패", path.display()))?);
    }
    crate::cache::write_atomic(&note, markdown(doc, &key, &images, &carried).as_bytes())?;
    for old in olds {
        std::fs::remove_file(&old).with_context(|| format!("{} 정리 실패", old.display()))?;
    }
    Ok(note)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `<title>.md`, unless a note by that name is about another video or isn't ours.
fn note_path(folder: &Path, title: &str, key: &str) -> PathBuf {
    let title = safe_name(title);
    let plain = folder.join(format!("{title}.md"));
    if !plain.exists() || is_note_for(&plain, key) { plain } else { folder.join(format!("{title} ({key}).md")) }
}

/// True when `path` is a note ytdoc wrote for `key`: the key is in its properties.
fn is_note_for(path: &Path, key: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some(props) = text.strip_prefix("---\n").and_then(|rest| rest.split("\n---").next()) else {
        return false;
    };
    props.lines().any(|line| line == key_line(key))
}

/// Earlier notes for the same video under an old title, so a rerun leaves one note.
fn older_notes(folder: &Path, key: &str, keep: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return vec![];
    };
    entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p != keep && p.extension().is_some_and(|x| x == "md") && is_note_for(p, key))
        .collect()
}

fn key_line(key: &str) -> String {
    format!("ytdoc: {}", Value::from(key))
}

/// Characters Obsidian refuses in note names or that break `[[links]]`.
fn safe_name(text: &str) -> String {
    let clean: String = text.chars().map(|c| if c.is_control() || "/\\:*?\"<>|#^[]".contains(c) { ' ' } else { c }).collect();
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

/// Properties ytdoc writes itself; every other key in an old note belongs to the user.
const GENERATED: [&str; 11] =
    ["title", "ytdoc", "source", "file", "original_title", "channel", "duration", "uploaded", "created", "language", "tags"];

/// What a rerun keeps from the notes it replaces.
#[derive(Default)]
struct Carried {
    created: Option<String>,
    /// Tags as written, minus `youtube-summary`.
    tags: Vec<String>,
    /// Whole YAML blocks (key line plus continuation lines) of the user's own properties.
    extra: Vec<(String, String)>,
}

impl Carried {
    /// Take the user's properties from an old note; what an earlier note already gave wins.
    fn absorb(&mut self, note: &str) {
        let Some(props) = note.strip_prefix("---\n").and_then(|rest| rest.split("\n---").next()) else {
            return;
        };
        for (key, block) in yaml_blocks(props) {
            let first = block.lines().next().unwrap_or_default();
            let value = first.split_once(':').map_or("", |(_, v)| v.trim());
            match key.as_str() {
                "created" if self.created.is_none() && !value.is_empty() => self.created = Some(value.to_string()),
                "tags" => {
                    let inline = value.trim_start_matches('[').trim_end_matches(']').split(',');
                    let listed = block.lines().skip(1).filter_map(|l| l.trim_start().strip_prefix("- "));
                    for tag in inline.chain(listed).map(str::trim).filter(|t| !t.is_empty()) {
                        let bare = |t: &str| t.trim_matches(['"', '\'']).to_string();
                        if bare(tag) != "youtube-summary" && !self.tags.iter().any(|t| bare(t) == bare(tag)) {
                            self.tags.push(tag.to_string());
                        }
                    }
                }
                k if !GENERATED.contains(&k) && !self.extra.iter().any(|(e, _)| e == k) => self.extra.push((key.clone(), block)),
                _ => {}
            }
        }
    }
}

/// Top-level `key:` blocks of a YAML mapping: each key with the lines that belong to it.
fn yaml_blocks(props: &str) -> Vec<(String, String)> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    for line in props.lines() {
        let top = !line.is_empty() && !line.starts_with([' ', '\t', '#']) && !line.starts_with("- ");
        match (top, line.split_once(':'), blocks.last_mut()) {
            (true, Some((key, _)), _) => blocks.push((key.trim().trim_matches(['"', '\'']).to_string(), format!("{line}\n"))),
            (_, _, Some((_, block))) => {
                block.push_str(line);
                block.push('\n');
            }
            _ => {}
        }
    }
    blocks
}

/// A title as heading text. Obsidian cannot link to a heading holding `#|^:%[]` or a
/// backslash, so those become spaces and runs of spaces collapse.
fn heading_text(title: &str) -> String {
    let clean: String = title.chars().map(|c| if c.is_control() || "#|^:%[]\\".contains(c) { ' ' } else { c }).collect();
    clean.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The one place a section heading is built; the contents list links to this exact text.
fn section_heading(index: usize, title: &str) -> String {
    format!("{}. {}", index + 1, heading_text(title)).trim_end().to_string()
}

/// A colored Obsidian box with a bullet list; `items` are already escaped Markdown.
fn callout(md: &mut String, kind: &str, title: &str, items: &[String], folded: bool) {
    let _ = writeln!(md, "> [!{kind}]{} {title}", if folded { "-" } else { "" });
    for item in items {
        let _ = writeln!(md, "> - {item}");
    }
}

/// `text` with every line put inside a callout.
fn quoted(text: &str) -> String {
    text.lines().map(|l| if l.is_empty() { ">".to_string() } else { format!("> {l}") }).collect::<Vec<_>>().join("\n")
}

/// A Mermaid flowchart drawn from the relations. Labels are quoted, so any text is safe.
fn concept_map(summary: &crate::domain::Summary) -> String {
    let label = |s: &str| s.replace('"', "#quot;").replace(['\n', '\r'], " ");
    let mut names: Vec<&str> = Vec::new();
    for r in &summary.relations {
        for name in [r.from.as_str(), r.to.as_str()] {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    let id = |name: &str| format!("n{}", names.iter().position(|n| *n == name).unwrap_or(0));
    let edges: Vec<String> =
        summary.relations.iter().map(|r| format!("  {} -->|\"{}\"| {}", id(&r.from), label(&r.label), id(&r.to))).collect();
    let mut out = String::from("```mermaid\nflowchart LR\n");
    for (i, name) in names.iter().enumerate() {
        let _ = writeln!(out, "  n{i}[\"{}\"]", label(name));
    }
    for edge in edges {
        let _ = writeln!(out, "{edge}");
    }
    out.push_str("```");
    out
}

fn markdown(doc: &Document, key: &str, images: &str, carried: &Carried) -> String {
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
    let created = carried.created.clone().unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
    let _ = writeln!(md, "created: {created}");
    let _ = writeln!(md, "language: {}", doc.lang.as_str());
    md.push_str("tags:\n  - youtube-summary\n");
    for tag in &carried.tags {
        let _ = writeln!(md, "  - {tag}");
    }
    for (_, block) in &carried.extra {
        md.push_str(block);
    }
    md.push_str("---\n\n");

    // The summary is what a reader sees; everything longer is folded until asked for.
    let _ = writeln!(md, "> [!summary]\n> {}\n", text(&doc.summary.one_line));
    let _ = writeln!(md, "## {}\n", t.overview);
    for para in doc.summary.overview.lines().map(str::trim).filter(|p| !p.is_empty()) {
        let _ = writeln!(md, "{}\n", text(para));
    }
    let points: Vec<String> = doc.summary.key_points.iter().map(|p| text(p)).collect();
    callout(&mut md, "tip", t.key_points, &points, false);

    if !doc.summary.relations.is_empty() {
        let _ = writeln!(md, "\n> [!example]- {}", t.concept_map);
        let _ = writeln!(md, "{}", quoted(&concept_map(doc.summary)));
    }
    for c in &doc.summary.comparisons {
        let _ = writeln!(md, "\n> [!example]- {}: {}", t.comparison, text(&c.title));
        let cells = |items: Vec<&str>| format!("> | {} |", items.into_iter().map(text).collect::<Vec<_>>().join(" | "));
        let _ = writeln!(md, "{}", cells(std::iter::once("").chain(c.subjects.iter().map(String::as_str)).collect()));
        let _ = writeln!(md, "> |{}", " --- |".repeat(c.subjects.len() + 1));
        for row in &c.rows {
            let _ = writeln!(md, "{}", cells(std::iter::once(row.aspect.as_str()).chain(row.values.iter().map(String::as_str)).collect()));
        }
    }
    if !doc.summary.concepts.is_empty() {
        let terms: Vec<String> = doc.summary.concepts.iter().map(|c| format!("**{}**: {}", text(&c.name), text(&c.meaning))).collect();
        md.push('\n');
        callout(&mut md, "info", t.glossary, &terms, true);
    }

    let _ = writeln!(md, "\n## {}", t.contents);
    for (i, s) in doc.sections.iter().enumerate() {
        let c = &s.chapter;
        let _ = writeln!(md, "\n### {}\n", section_heading(i, &c.title));
        let _ = writeln!(md, "{}\n", at(c.start, format!("▶ {}", span(c.start, c.end))));
        let mut body = vec![text(&c.summary), String::new(), format!("**{}**", t.key_points)];
        body.extend(c.key_points.iter().map(|p| format!("- {}", text(p))));
        let images: Vec<String> = s.frames.iter().map(|f| format!("![[{images}/{}|320]]", file_name(&f.path))).collect();
        if !images.is_empty() {
            body.push(String::new());
            body.push(images.join(" "));
        }
        let _ = writeln!(md, "> [!note]- {}\n{}", t.details, quoted(&body.join("\n")));
    }

    // Folded, so the summary stays the first thing a reader sees.
    let _ = writeln!(md, "\n> [!quote]- {}", t.transcript);
    for seg in &doc.transcript.segments {
        let _ = writeln!(md, "> - {} {}", at(seg.start, clock(seg.start)), text(&seg.text));
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Chapter, Comparison, ComparisonRow, Concept, Frame, Relation, Section, Segment, Summary, Transcript, VideoMeta};
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
        let chapter = Chapter {
            start: 0.0,
            end: 107.0,
            title: "브라우저 호환성".into(),
            summary: "요약 문장".into(),
            key_points: vec!["핵심".into()],
        };
        let summary = Summary {
            title: title.into(),
            one_line: "한 줄 요약".into(),
            overview: "첫 문단\n\n둘째 문단".into(),
            key_points: vec!["전체 핵심".into()],
            chapters: vec![chapter.clone()],
            concepts: vec![
                Concept { name: "프로세스".into(), meaning: "실행 중인 \"프로그램\"".into() },
                Concept { name: "스레드".into(), meaning: "프로세스 안의 실행 흐름".into() },
            ],
            relations: vec![Relation { from: "프로세스".into(), to: "스레드".into(), label: "여러 개 품음".into() }],
            comparisons: vec![Comparison {
                title: "프로세스 vs 스레드".into(),
                subjects: vec!["프로세스".into(), "스레드".into()],
                rows: vec![ComparisonRow { aspect: "메모리".into(), values: vec!["따로".into(), "공유 | 일부".into()] }],
            }],
        };
        let frames = vec![Frame { time: 30.0, path: "frames/0001.jpg".into(), hash: 0 }];
        let transcript =
            Transcript { language: "ko".into(), segments: vec![Segment { start: 65.0, end: 67.0, text: "말한 내용".into() }] };
        (meta, summary, vec![Section { chapter, frames }], transcript)
    }

    fn write(vault: &Path, title: &str) -> PathBuf {
        let (meta, summary, sections, transcript) = sample(title);
        let lang: LangCode = "ko".parse().unwrap();
        let doc =
            Document { meta: &meta, summary: &summary, sections: &sections, transcript: &transcript, lang: &lang, translated_from: None };
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
        assert!(md.starts_with(
            "---\ntitle: \"caniuse 사용법\"\nytdoc: \"JdgjWfve1DY\"\nsource: \"https://www.youtube.com/watch?v=JdgjWfve1DY\""
        ));
        assert!(md.contains("uploaded: 2015-12-13\n"));
        let top = "> [!summary]\n> 한 줄 요약\n\n## 전체 요약\n\n첫 문단\n\n둘째 문단\n\n> [!tip] 핵심 포인트\n> - 전체 핵심\n";
        assert!(md.contains(top), "the summary is shown first: {md}");
        let chapter = "\n### 1. 브라우저 호환성\n\n[▶ 0:00–1:47](https://www.youtube.com/watch?v=JdgjWfve1DY&t=0s)\n\n> [!note]- 자세히\n> 요약 문장\n>\n> **핵심 포인트**\n> - 핵심\n>\n> ![[YouTube 요약/첨부/JdgjWfve1DY/0001.jpg|320]]\n";
        assert!(md.contains(chapter), "chapter details are folded: {md}");
        assert!(md.contains("\n> [!example]- 개념 관계도\n> ```mermaid\n> flowchart LR\n>   n0[\"프로세스\"]\n>   n1[\"스레드\"]\n>   n0 -->|\"여러 개 품음\"| n1\n> ```"));
        assert!(md.contains("\n> [!example]- 비교: 프로세스 vs 스레드\n> |  | 프로세스 | 스레드 |\n> | --- | --- | --- |\n> | 메모리 | 따로 | 공유 \\| 일부 |"));
        assert!(md.contains("> [!info]- 용어 정리\n> - **프로세스**: 실행 중인 \"프로그램\""));
        assert!(!md.contains("[[#") && !md.contains("## 부록"));
        assert!(md.contains("\n> [!quote]- 부록: 전체 받아쓰기\n> - [1:05](https://www.youtube.com/watch?v=JdgjWfve1DY&t=65s) 말한 내용"));
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
    fn a_new_title_replaces_the_old_note_of_the_same_video() {
        let vault = tempfile::tempdir().unwrap();
        let old = write(vault.path(), "옛 제목");
        let mine = vault.path().join("YouTube 요약/내 메모.md");
        std::fs::write(&mine, "본문에 적은 ytdoc: \"JdgjWfve1DY\"").unwrap();
        let new = write(vault.path(), "새 제목");
        assert!(new.is_file() && !old.exists(), "the old note is gone");
        assert!(mine.is_file(), "a note that only mentions the id is not ours");
    }

    #[test]
    fn images_sit_on_one_line_inside_the_folded_details() {
        let (meta, summary, mut sections, transcript) = sample("t");
        sections[0].frames.push(Frame { time: 60.0, path: "frames/0002.jpg".into(), hash: 1 });
        let lang: LangCode = "ko".parse().unwrap();
        let doc =
            Document { meta: &meta, summary: &summary, sections: &sections, transcript: &transcript, lang: &lang, translated_from: None };
        let md = markdown(&doc, "JdgjWfve1DY", "img", &Carried::default());
        assert!(md.contains("> - 핵심\n>\n> ![[img/0001.jpg|320]] ![[img/0002.jpg|320]]\n"), "{md}");
    }

    #[test]
    fn chapter_headings_stay_linkable() {
        let (meta, summary, mut sections, transcript) = sample("t");
        for title in ["C# | 배열: [기초]", "100% ^승^ \\역슬래시", "###", "평범한 제목"] {
            let mut s = sections[0].clone();
            s.chapter.title = title.into();
            sections.push(s);
        }
        let lang: LangCode = "ko".parse().unwrap();
        let doc =
            Document { meta: &meta, summary: &summary, sections: &sections, transcript: &transcript, lang: &lang, translated_from: None };
        let md = markdown(&doc, "JdgjWfve1DY", "img", &Carried::default());
        let headings: Vec<&str> = md.lines().filter_map(|l| l.strip_prefix("### ")).collect();
        assert_eq!(headings.len(), sections.len());
        for h in headings {
            assert!(!h.contains(['#', '|', '^', ':', '%', '[', ']', '\\']), "{h:?} breaks [[#heading]] links");
        }
        assert!(md.contains("### 2. C 배열 기초\n"));
    }

    #[test]
    fn chapter_time_is_plain_without_a_url() {
        let (mut meta, summary, sections, transcript) = sample("t");
        meta.origin = Origin::LocalFile { file_name: "a.mp4".into() };
        let lang: LangCode = "ko".parse().unwrap();
        let doc =
            Document { meta: &meta, summary: &summary, sections: &sections, transcript: &transcript, lang: &lang, translated_from: None };
        let md = markdown(&doc, "a.mp4", "img", &Carried::default());
        assert!(md.contains("### 1. 브라우저 호환성\n\n▶ 0:00–1:47\n"));
    }

    const EDITED: &str = "---
title: \"옛 제목\"
ytdoc: \"JdgjWfve1DY\"
created: 2020-01-02
language: ko
tags:
  - youtube-summary
  - 네트워크
  - \"복습 필요\"
status: 복습
rating: 4
related:
  - \"[[TCP]]\"
  - \"[[UDP]]\"
memo: |
  첫 줄
  둘째 줄
---

내용
";

    fn assert_user_properties_kept(note: &Path) {
        let md = std::fs::read_to_string(note).unwrap();
        let props = md.strip_prefix("---\n").unwrap().split("\n---").next().unwrap();
        assert!(props.contains("\ncreated: 2020-01-02\n"), "{props}");
        assert!(props.contains("\ntags:\n  - youtube-summary\n  - 네트워크\n  - \"복습 필요\"\n"), "{props}");
        assert_eq!(props.matches("youtube-summary").count(), 1, "{props}");
        assert!(
            props.contains("\nstatus: 복습\nrating: 4\nrelated:\n  - \"[[TCP]]\"\n  - \"[[UDP]]\"\nmemo: |\n  첫 줄\n  둘째 줄"),
            "{props}"
        );
        assert!(props.contains("\nlanguage: ko\n") && !props.contains("옛 제목"), "generated keys are rewritten");
    }

    #[test]
    fn rerun_keeps_the_users_properties() {
        let vault = tempfile::tempdir().unwrap();
        let note = write(vault.path(), "제목");
        std::fs::write(&note, EDITED).unwrap();
        assert_eq!(write(vault.path(), "제목"), note);
        assert_user_properties_kept(&note);
        assert_eq!(write(vault.path(), "제목"), note);
        assert_user_properties_kept(&note);
    }

    #[test]
    fn a_new_title_carries_the_users_properties_to_the_new_note() {
        let vault = tempfile::tempdir().unwrap();
        let old = write(vault.path(), "옛 제목");
        std::fs::write(&old, EDITED).unwrap();
        let new = write(vault.path(), "새 제목");
        assert!(!old.exists());
        assert_user_properties_kept(&new);
    }

    #[test]
    fn inline_tags_are_merged_and_deduped() {
        let mut carried = Carried::default();
        carried.absorb("---\nytdoc: \"x\"\ntags: [네트워크, youtube-summary, \"a\"]\n---\n");
        carried.absorb("---\ntags:\n- a\n- b\n---\n");
        assert_eq!(carried.tags, ["네트워크", "\"a\"", "b"]);
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
