//! The PDF, typeset with Typst inside this program: no browser needed.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst_layout::PagedDocument;
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::fonts::FontStore;

use crate::domain::{Section, Summary, Transcript, VideoMeta, clock};
use crate::lang::LangCode;

const TEMPLATE: &str = include_str!("../templates/document.typ");

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

/// Typeset `doc` to an A4 PDF. Scene images are read from `assets`, the folder their paths are relative to.
pub fn render_pdf(doc: &Document, assets: &Path, pdf: &Path) -> Result<()> {
    let bytes = typeset(doc, assets)?;
    if let Some(dir) = pdf.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    crate::cache::write_atomic(pdf, &bytes).with_context(|| format!("{}에 PDF를 쓰지 못했습니다", pdf.display()))
}

fn typeset(doc: &Document, assets: &Path) -> Result<Vec<u8>> {
    let page = Page {
        library: LazyHash::new(Library::default()),
        main: Source::new(file_id("/document.typ"), TEMPLATE.to_string()),
        data: Bytes::new(serde_json::to_vec(&view(doc))?),
        assets: assets.to_path_buf(),
    };
    let document: PagedDocument = typst::compile(&page).output.map_err(|errors| {
        anyhow!("PDF 조판 실패: {}", errors.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("; "))
    })?;
    typst_pdf::pdf(&document, &typst_pdf::PdfOptions::default())
        .map_err(|errors| anyhow!("PDF 저장 실패: {}", errors.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("; ")))
}

/// Everything the template shows, with times already formatted.
fn view(doc: &Document) -> Value {
    let time = |seconds: f64| json!({ "s": seconds.max(0.0) as u64, "label": clock(seconds) });
    let upload_date = doc
        .meta
        .upload_date
        .as_deref()
        .filter(|d| d.len() == 8)
        .map(|d| format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..]));
    let sections: Vec<Value> = doc
        .sections
        .iter()
        .map(|s| {
            let c = &s.chapter;
            json!({
                "start": c.start.max(0.0) as u64,
                "span": format!("{}–{}", clock(c.start), clock(c.end)),
                "title": c.title,
                "summary": c.summary,
                "key_points": c.key_points,
                "frames": s.frames.iter().map(|f| json!({ "path": format!("/{}", f.path), "time": time(f.time) })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "lang": doc.lang.as_str(),
        "fonts": fonts_for(doc.lang.as_str()),
        "t": doc.lang.labels(),
        "title": doc.summary.title,
        "one_line": doc.summary.one_line,
        "overview": doc.summary.overview.lines().map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>(),
        "key_points": doc.summary.key_points,
        "video_url": doc.meta.watch_url(),
        "details": details(doc, upload_date),
        "sections": sections,
        "segments": doc.transcript.segments.iter().map(|s| json!({ "time": time(s.start), "text": s.text })).collect::<Vec<_>>(),
    })
}

/// The label/value rows on the cover.
fn details(doc: &Document, upload_date: Option<String>) -> Vec<[String; 2]> {
    let t = doc.lang.labels();
    let file_name = match &doc.meta.origin {
        crate::domain::Origin::LocalFile { file_name } => Some(file_name.clone()),
        crate::domain::Origin::YouTube { .. } => None,
    };
    [
        Some((t.original, Some(doc.meta.title.clone()))),
        Some((t.channel, doc.meta.channel.clone())),
        Some((t.duration, Some(clock(doc.meta.duration)))),
        Some((t.uploaded, upload_date)),
        Some((t.url, doc.meta.watch_url())),
        Some((t.file, file_name)),
        Some((t.translated_from, doc.translated_from.map(|l| l.english_name()))),
        Some((t.generated, Some(chrono::Local::now().format("%Y-%m-%d").to_string()))),
    ]
    .into_iter()
    .flatten()
    .filter_map(|(label, value)| value.map(|v| [label.to_string(), v]))
    .collect()
}

/// Font families to try in order; Typst falls back to any installed font for other scripts.
fn fonts_for(lang: &str) -> Vec<&'static str> {
    const KOREAN: [&str; 4] = ["Apple SD Gothic Neo", "Malgun Gothic", "Noto Sans KR", "Noto Sans CJK KR"];
    const JAPANESE: [&str; 5] = ["Hiragino Sans", "Hiragino Kaku Gothic ProN", "Yu Gothic", "Noto Sans JP", "Noto Sans CJK JP"];
    const CHINESE: [&str; 4] = ["PingFang SC", "Microsoft YaHei", "Noto Sans SC", "Noto Sans CJK SC"];
    const LATIN: [&str; 5] = ["Helvetica Neue", "Arial", "Segoe UI", "DejaVu Sans", "Libertinus Serif"];
    let first: &[&[&'static str]] = match lang {
        "ja" => &[&JAPANESE, &KOREAN, &CHINESE],
        "zh" => &[&CHINESE, &JAPANESE, &KOREAN],
        _ => &[&KOREAN, &JAPANESE, &CHINESE],
    };
    first.iter().flat_map(|f| f.iter().copied()).chain(LATIN).collect()
}

fn file_id(path: &str) -> FileId {
    FileId::new(RootedPath::new(VirtualRoot::Project, VirtualPath::new(path).expect("valid virtual path")))
}

/// Fonts installed on this computer plus Typst's own, found once per run.
fn fonts() -> &'static FontStore {
    static FONTS: OnceLock<FontStore> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut store = FontStore::new();
        store.extend(typst_kit::fonts::system());
        store.extend(typst_kit::fonts::embedded());
        store
    })
}

/// What Typst may see: the template, the data as `/data.json`, and the files under `assets`.
struct Page {
    library: LazyHash<Library>,
    main: Source,
    data: Bytes,
    assets: PathBuf,
}

impl World for Page {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        fonts().book()
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() { Ok(self.main.clone()) } else { Err(FileError::AccessDenied) }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main.id() {
            return Ok(Bytes::from_string(self.main.clone()));
        }
        if id == file_id("/data.json") {
            return Ok(self.data.clone());
        }
        let rooted = id.get();
        if rooted.root() != &VirtualRoot::Project {
            return Err(FileError::AccessDenied);
        }
        // `realize` refuses paths that climb out of `assets`.
        let path = rooted.vpath().realize(&self.assets)?;
        std::fs::read(&path).map(Bytes::new).map_err(|e| FileError::from_io(e, &path))
    }

    fn font(&self, index: usize) -> Option<Font> {
        fonts().font(index)
    }

    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Chapter, Frame, Origin, Segment};

    fn sample(text: &str) -> (VideoMeta, Summary, Vec<Section>, Transcript) {
        let meta = VideoMeta {
            title: text.into(),
            channel: Some(text.into()),
            duration: 3725.0,
            upload_date: Some("20240131".into()),
            origin: Origin::YouTube { id: "JdgjWfve1DY".into() },
            has_video: true,
        };
        let chapter = Chapter { start: 0.0, end: 65.0, title: text.into(), summary: text.into(), key_points: vec![text.into()] };
        let summary = Summary {
            title: text.into(),
            one_line: text.into(),
            overview: format!("{text}\n\n{text}"),
            key_points: vec![text.into()],
            chapters: vec![chapter.clone()],
        };
        let frame = Frame { time: 30.0, path: "frames/0001.jpg".into(), hash: 0 };
        let sections = vec![Section { chapter, frames: vec![frame] }];
        let transcript = Transcript { language: "ko".into(), segments: vec![Segment { start: 3.0, end: 5.0, text: text.into() }] };
        (meta, summary, sections, transcript)
    }

    fn assets_with_frame() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frames")).unwrap();
        std::fs::write(dir.path().join("frames/0001.jpg"), include_bytes!("../tests/fixtures/frame.jpg")).unwrap();
        dir
    }

    fn pdf_for(text: &str, lang: &str) -> Vec<u8> {
        let (meta, summary, sections, transcript) = sample(text);
        let lang: LangCode = lang.parse().unwrap();
        let doc = Document { meta: &meta, summary: &summary, sections: &sections, transcript: &transcript, lang: &lang, translated_from: None };
        typeset(&doc, assets_with_frame().path()).unwrap()
    }

    #[test]
    fn typesets_a_pdf_with_the_scene_image() {
        let pdf = pdf_for("영상 요약 테스트", "ko");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(pdf.windows(13).any(|w| w == b"/Subtype /Ima" || w == b"/Subtype/Imag"), "the frame is embedded");
    }

    #[test]
    fn video_text_is_shown_as_text_not_run_as_typst() {
        // Typst markup and code in a title must not be evaluated, e.g. reading another file.
        let pdf = pdf_for("#read(\"/etc/passwd\") *bold* $x$ <label> @ref", "en");
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn images_outside_the_assets_folder_are_refused() {
        let (meta, summary, mut sections, transcript) = sample("x");
        sections[0].frames[0].path = "../outside.jpg".into();
        let lang: LangCode = "ko".parse().unwrap();
        let doc = Document { meta: &meta, summary: &summary, sections: &sections, transcript: &transcript, lang: &lang, translated_from: None };
        let err = typeset(&doc, assets_with_frame().path()).unwrap_err().to_string();
        assert!(err.contains("outside") || err.contains("root"), "{err}");
    }
}
