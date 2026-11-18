//! Everything that differs between macOS, Linux and Windows.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn home_dir() -> Result<PathBuf> {
    std::env::home_dir().context("홈 폴더를 찾을 수 없습니다")
}

/// Names a program can have on disk: `claude` on Unix, `claude.exe` or `claude.cmd` on Windows.
fn file_names(name: &str) -> Vec<String> {
    if cfg!(windows) { vec![format!("{name}.exe"), format!("{name}.cmd")] } else { vec![name.to_string()] }
}

/// The first `name` found in `PATH`, then in `extra_dirs`.
pub fn find_program(name: &str, extra_dirs: &[PathBuf]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(extra_dirs.iter().cloned())
        .flat_map(|dir| file_names(name).into_iter().map(move |f| dir.join(f)))
        .find(|p| p.is_file())
}

/// How to install a command-line tool ytdoc runs, for this operating system.
pub fn install_hint(program: &str) -> String {
    let package = match (std::env::consts::OS, program) {
        ("macos", "ffprobe") => "ffmpeg",
        ("windows", "ffmpeg" | "ffprobe") => "Gyan.FFmpeg",
        ("windows", "yt-dlp") => "yt-dlp.yt-dlp DenoLand.Deno",
        (_, "yt-dlp") => "yt-dlp deno",
        _ => program,
    };
    match std::env::consts::OS {
        "macos" => format!("설치: brew install {package}"),
        "windows" => format!("설치: winget install {package}"),
        _ if program == "yt-dlp" => "설치 방법: https://github.com/yt-dlp/yt-dlp/wiki/Installation (deno도 필요합니다)".into(),
        _ => format!("설치 예: sudo apt install {package}"),
    }
}

/// Chrome, Chromium or Edge, wherever this OS usually puts it.
pub fn find_chrome() -> Option<PathBuf> {
    let fixed: Vec<PathBuf> = if cfg!(target_os = "macos") {
        ["Google Chrome", "Chromium", "Microsoft Edge"]
            .iter()
            .map(|app| PathBuf::from(format!("/Applications/{app}.app/Contents/MacOS/{app}")))
            .collect()
    } else if cfg!(windows) {
        let roots = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"].iter().filter_map(std::env::var_os);
        roots
            .flat_map(|root| {
                let root = PathBuf::from(root);
                [
                    root.join(r"Google\Chrome\Application\chrome.exe"),
                    root.join(r"Chromium\Application\chrome.exe"),
                    root.join(r"Microsoft\Edge\Application\msedge.exe"),
                ]
            })
            .collect()
    } else {
        Vec::new()
    };
    fixed.into_iter().find(|p| p.is_file()).or_else(|| {
        ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome", "microsoft-edge", "msedge"]
            .iter()
            .find_map(|name| find_program(name, &[]))
    })
}

/// `file://` URL for an absolute path, with every byte outside the unreserved set percent-encoded.
/// Windows paths (`C:\a\b`, or `\\?\C:\a\b` from `canonicalize`) become `file:///C:/a/b`.
pub fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text).replace('\\', "/");
    let (prefix, rest) = match text.as_bytes() {
        [drive, b':', ..] if drive.is_ascii_alphabetic() => (format!("file:///{}:", *drive as char), &text[2..]),
        _ => ("file://".to_string(), &text[..]),
    };
    let mut url = prefix;
    for b in rest.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_for_unix_and_windows_paths() {
        assert_eq!(file_url(Path::new("/tmp/a b/문서.html")), "file:///tmp/a%20b/%EB%AC%B8%EC%84%9C.html");
        assert_eq!(file_url(Path::new(r"C:\Users\me\doc.html")), "file:///C:/Users/me/doc.html");
        assert_eq!(file_url(Path::new(r"\\?\D:\캐시\doc.html")), "file:///D:/%EC%BA%90%EC%8B%9C/doc.html");
    }
}
