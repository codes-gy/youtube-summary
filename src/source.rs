use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;

/// Where the video comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    YouTube { id: String },
    LocalFile { path: PathBuf },
}

static ID_IN_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:https?://)?(?:(?:www|m|music)\.)?(?:youtube\.com/(?:watch\?(?:.*&)?v=|shorts/|embed/|live/|v/)|youtu\.be/)([A-Za-z0-9_-]{11})(?:[?&#/].*)?$",
    )
    .unwrap()
});
static BARE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").unwrap());

impl Source {
    /// An existing file wins; otherwise the argument must be a YouTube URL or video id.
    pub fn parse(arg: &str) -> Result<Source> {
        let arg = arg.trim();
        let path = Path::new(arg);
        if path.is_file() {
            return Ok(Source::LocalFile { path: std::path::absolute(path)? });
        }
        if let Some(source) = Source::parse_youtube(arg) {
            return Ok(source);
        }
        if path.exists() {
            bail!("'{arg}'은(는) 폴더입니다. 영상 파일을 지정해 주세요.");
        }
        bail!("'{arg}'은(는) YouTube 주소도 아니고 있는 파일도 아닙니다.");
    }

    /// A YouTube URL or bare video id; never a file path.
    pub fn parse_youtube(arg: &str) -> Option<Source> {
        let arg = arg.trim();
        let id = match ID_IN_URL.captures(arg) {
            Some(m) => m[1].to_string(),
            None if BARE_ID.is_match(arg) => arg.to_string(),
            None => return None,
        };
        Some(Source::YouTube { id })
    }

    pub fn youtube_url(id: &str) -> String {
        format!("https://www.youtube.com/watch?v={id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt(arg: &str) -> Option<String> {
        match Source::parse(arg) {
            Ok(Source::YouTube { id }) => Some(id),
            _ => None,
        }
    }

    #[test]
    fn youtube_url_forms() {
        let id = Some("JdgjWfve1DY".to_string());
        for url in [
            "https://www.youtube.com/watch?v=JdgjWfve1DY",
            "https://youtube.com/watch?feature=share&v=JdgjWfve1DY&t=30s",
            "youtube.com/watch?v=JdgjWfve1DY",
            "https://m.youtube.com/watch?v=JdgjWfve1DY",
            "https://youtu.be/JdgjWfve1DY",
            "https://youtu.be/JdgjWfve1DY?si=abc",
            "https://www.youtube.com/shorts/JdgjWfve1DY",
            "https://www.youtube.com/embed/JdgjWfve1DY",
            "https://www.youtube.com/live/JdgjWfve1DY?feature=share",
            "JdgjWfve1DY",
            "  https://youtu.be/JdgjWfve1DY  ",
        ] {
            assert_eq!(yt(url), id, "{url}");
        }
    }

    #[test]
    fn rejects_non_youtube_and_missing_files() {
        for arg in [
            "https://vimeo.com/watch?v=JdgjWfve1DY",
            "https://www.youtube.com/watch?v=short",
            "no-such-file.mp4",
            "",
        ] {
            let err = Source::parse(arg).unwrap_err().to_string();
            assert!(err.contains("YouTube 주소도 아니고"), "{arg}: {err}");
        }
    }

    #[test]
    fn existing_file_is_local_even_if_named_like_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("JdgjWfve1DY");
        std::fs::write(&file, b"x").unwrap();
        let parsed = Source::parse(file.to_str().unwrap()).unwrap();
        assert_eq!(parsed, Source::LocalFile { path: std::path::absolute(&file).unwrap() });
        assert!(Source::parse(dir.path().to_str().unwrap()).unwrap_err().to_string().contains("폴더"));
    }
}
