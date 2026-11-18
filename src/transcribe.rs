use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::domain::{Segment, Transcript};

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const VAD_MODEL: &str = "ggml-silero-v5.1.2.bin";
const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

/// `--whisper-model` is either a path to a ggml file or a name from the whisper.cpp model repo.
pub fn model_file(model: &str, on_progress: &dyn Fn(String)) -> Result<PathBuf> {
    let as_path = Path::new(model);
    if as_path.is_file() {
        return Ok(as_path.to_path_buf());
    }
    if !model.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        bail!("'{model}' 모델 파일이 없습니다.");
    }
    let file = format!("ggml-{model}.bin");
    fetch(&format!("{MODEL_BASE_URL}/{file}"), &models_dir()?.join(&file), &format!("Whisper {model}"), on_progress)
}

fn models_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("YTDOC_MODEL_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(crate::platform::home_dir()?.join(".cache").join("ytdoc").join("models"))
}

/// Download `url` to `dest` once, via a `.part` file so an interrupted download is never mistaken for a model.
fn fetch(url: &str, dest: &Path, what: &str, on_progress: &dyn Fn(String)) -> Result<PathBuf> {
    if dest.is_file() {
        return Ok(dest.to_path_buf());
    }
    fs::create_dir_all(dest.parent().unwrap())?;
    let client = reqwest::blocking::Client::builder().timeout(None).build()?;
    // Hugging Face answers 502/503 now and then; a few seconds later it works.
    let mut attempt = 0;
    let mut response = loop {
        attempt += 1;
        let result = client.get(url).send();
        let transient = match &result {
            Ok(r) => r.status().is_server_error(),
            Err(e) => e.is_connect() || e.is_timeout(),
        };
        if !transient || attempt == 4 {
            break result.with_context(|| format!("{what} 모델을 내려받지 못했습니다"))?;
        }
        on_progress(format!("{what} 모델 서버가 응답하지 않아 다시 시도합니다 ({attempt}/3)"));
        std::thread::sleep(std::time::Duration::from_secs(2 * attempt));
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("{what} 모델이 없습니다. tiny, base, small, medium, large-v3, large-v3-turbo 중에서 고르세요.");
    }
    if !response.status().is_success() {
        bail!("{what} 모델을 내려받지 못했습니다: HTTP {}", response.status());
    }
    let total = response.content_length();
    let part = dest.with_extension("part");
    let mut out = fs::File::create(&part)?;
    let mut buf = vec![0u8; 1 << 16];
    let (mut done, mut shown) = (0u64, u64::MAX);
    loop {
        let n = response.read(&mut buf).with_context(|| format!("{what} 모델을 내려받다가 끊겼습니다"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        let mb = done >> 20;
        let pct = total.map(|t| done * 100 / t.max(1));
        let step = pct.unwrap_or(mb / 10);
        if step != shown {
            shown = step;
            on_progress(match total {
                Some(t) => format!("{what} 모델 내려받는 중 {}% ({mb}/{} MB)", pct.unwrap(), t >> 20),
                None => format!("{what} 모델 내려받는 중 {mb} MB"),
            });
        }
    }
    out.sync_all()?;
    if total.is_some_and(|t| t != done) {
        bail!("{what} 모델을 끝까지 받지 못했습니다. 다시 실행하세요.");
    }
    fs::rename(&part, dest)?;
    Ok(dest.to_path_buf())
}

/// Speech to text with whisper.cpp (Metal on Apple Silicon), language auto-detected,
/// silence skipped with the Silero VAD model.
pub fn transcribe(samples: &[f32], model: &Path, on_progress: &dyn Fn(String)) -> Result<Transcript> {
    let vad = fetch(VAD_URL, &models_dir()?.join(VAD_MODEL), "VAD", on_progress)?;
    whisper_rs::install_logging_hooks();
    let mut ctx_params = WhisperContextParameters::default();
    ctx_params.flash_attn(true);
    let ctx = WhisperContext::new_with_params(&model, ctx_params)
        .map_err(|e| anyhow::anyhow!("Whisper 모델을 열지 못했습니다 ({}): {e}", model.display()))?;
    let mut state = ctx.create_state().map_err(|e| anyhow::anyhow!("Whisper 준비 실패: {e}"))?;

    // Greedy with flash attention was about 25% faster than a 5-wide beam search on a
    // 6-minute Korean lecture, with 98.6% of the text identical (mostly fillers dropped).
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("auto"));
    params.set_n_threads(std::thread::available_parallelism().map_or(4, |n| n.get().min(8)) as i32);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    let vad_path = vad.to_string_lossy().into_owned();
    params.set_vad_model_path(Some(&vad_path));
    params.enable_vad(true);
    let (tx, rx) = std::sync::mpsc::channel::<i32>();
    params.set_progress_callback_safe(move |pct: i32| {
        let _ = tx.send(pct);
    });

    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| state.full(params, samples).map(|_| state));
        // whisper-rs keeps the callback (and `tx`) alive after `full` returns, so the
        // channel never closes; stop when the worker does.
        while !worker.is_finished() {
            if let Ok(pct) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
                on_progress(format!("받아쓰는 중 {pct}%"));
            }
        }
        worker.join().expect("whisper thread panicked")
    });
    let state = result.map_err(|e| anyhow::anyhow!("받아쓰기 실패: {e}"))?;

    let language = whisper_rs::get_lang_str(state.full_lang_id_from_state()).unwrap_or("unknown").to_string();
    let segments = state
        .as_iter()
        .filter_map(|seg| {
            let text = seg.to_str_lossy().ok()?.trim().to_string();
            (!text.is_empty()).then(|| Segment {
                start: seg.start_timestamp() as f64 / 100.0,
                end: seg.end_timestamp() as f64 / 100.0,
                text,
            })
        })
        .collect();
    Ok(Transcript { language, segments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    #[test]
    fn download_retries_a_flaky_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model.bin", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for status in ["502 Bad Gateway", "503 Service Unavailable", "200 OK"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                let body = if status.starts_with("200") { "model" } else { "" };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.bin");
        fetch(&url, &dest, "test", &|_| {}).unwrap();
        server.join().unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "model");
    }
}
