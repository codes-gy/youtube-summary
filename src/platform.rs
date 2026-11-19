//! Everything that differs between macOS, Linux and Windows.

use std::path::PathBuf;

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
