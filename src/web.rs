//! `ytdoc serve`: a one-page local web UI over the same pipeline the CLI runs.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{DefaultBodyLimit, Multipart, Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::lang::{LANGUAGES, LangCode};
use crate::pipeline::{Event, Options, Outcome, Progress, Stage, run_pipeline};
use crate::source::Source;

const PAGE: &str = include_str!("../templates/web.html");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StageStatus {
    Pending,
    Running,
    Done,
    Cached,
    Skipped,
    Failed,
}

struct StageRow {
    stage: Stage,
    status: StageStatus,
    detail: Option<String>,
    started: Option<Instant>,
    elapsed: Option<f64>,
}

struct Job {
    source: Source,
    translate: Option<LangCode>,
    /// The uploaded file, deleted once the pipeline has its own copy in the cache.
    upload: Option<PathBuf>,
    state: JobState,
    stages: Vec<StageRow>,
    created: Instant,
    finished: Option<Instant>,
    error: Option<String>,
    title: Option<String>,
    pdf: Option<PathBuf>,
}

struct App {
    defaults: Options,
    port: u16,
    jobs: Mutex<HashMap<u64, Job>>,
    /// Ids in submission order; the single worker takes them one at a time.
    queue: mpsc::UnboundedSender<u64>,
    next_id: AtomicU64,
}

pub fn serve(port: u16, defaults: Options) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async move {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = Arc::new(App { defaults, port, jobs: Mutex::new(HashMap::new()), queue: tx, next_id: AtomicU64::new(1) });
        tokio::spawn(worker(app.clone(), rx));
        let router = Router::new()
            .route("/", get(page))
            .route("/api/jobs", post(create_job).layer(DefaultBodyLimit::disable()))
            .route("/api/jobs/{id}", get(job_status))
            .route("/api/jobs/{id}/pdf", get(job_pdf))
            .with_state(app);
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("{addr}에서 열 수 없습니다. 다른 프로그램이 쓰고 있다면 --port로 다른 번호를 고르세요"))?;
        eprintln!("ytdoc 웹 페이지: http://{addr}  (끝내려면 Ctrl+C)");
        axum::serve(listener, router).await?;
        Ok(())
    })
}

async fn worker(app: Arc<App>, mut rx: mpsc::UnboundedReceiver<u64>) {
    while let Some(id) = rx.recv().await {
        let app = app.clone();
        let _ = tokio::task::spawn_blocking(move || run_job(&app, id)).await;
    }
}

fn run_job(app: &App, id: u64) {
    let (source, opts, upload) = {
        let mut jobs = app.jobs.lock().unwrap();
        let job = jobs.get_mut(&id).expect("queued job exists");
        job.state = JobState::Running;
        let opts = Options {
            translate: job.translate.clone(),
            output: Some(app.defaults.cache_dir.join("outputs").join(format!("{id}.pdf"))),
            ..app.defaults.clone()
        };
        (job.source.clone(), opts, job.upload.clone())
    };
    let result = run_pipeline(&source, &opts, &JobProgress { app, id });
    if let Some(upload) = upload.as_deref().and_then(Path::parent) {
        let _ = std::fs::remove_dir_all(upload);
    }
    let mut jobs = app.jobs.lock().unwrap();
    let job = jobs.get_mut(&id).expect("running job exists");
    job.finished = Some(Instant::now());
    match result {
        Ok(done) => {
            job.state = JobState::Done;
            job.title = Some(done.title);
            job.pdf = Some(done.pdf);
        }
        Err(err) => {
            job.state = JobState::Failed;
            job.error = Some(format!("{err:#}"));
            for row in job.stages.iter_mut().filter(|r| r.status == StageStatus::Running) {
                row.status = StageStatus::Failed;
                row.elapsed = row.started.map(|s| s.elapsed().as_secs_f64());
            }
        }
    }
}

struct JobProgress<'a> {
    app: &'a App,
    id: u64,
}

impl Progress for JobProgress<'_> {
    fn event(&self, event: Event) {
        let mut jobs = self.app.jobs.lock().unwrap();
        let Some(job) = jobs.get_mut(&self.id) else { return };
        fn row(job: &mut Job, stage: Stage) -> &mut StageRow {
            job.stages.iter_mut().find(|r| r.stage == stage).expect("every stage has a row")
        }
        match event {
            Event::Started(stage) => {
                let r = row(job, stage);
                r.status = StageStatus::Running;
                r.started = Some(Instant::now());
            }
            Event::Detail(stage, msg) => row(job, stage).detail = Some(msg),
            Event::Finished(stage, elapsed, outcome) => {
                let r = row(job, stage);
                r.status = match outcome {
                    Outcome::Done => StageStatus::Done,
                    Outcome::Cached => StageStatus::Cached,
                    Outcome::Skipped => StageStatus::Skipped,
                };
                r.detail = None;
                r.elapsed = Some(elapsed.as_secs_f64());
            }
        }
    }
}

async fn page() -> Html<String> {
    let options: String = LANGUAGES
        .iter()
        .map(|l| format!("<option value=\"{}\"{}>{}</option>", l.code, if l.code == "ko" { " selected" } else { "" }, l.native))
        .collect();
    Html(PAGE.replace("<!--LANGUAGE_OPTIONS-->", &options))
}

fn bad_request(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.into() }))).into_response()
}

/// Another website open in the same browser must not be able to start jobs.
fn same_origin(headers: &HeaderMap, port: u16) -> bool {
    match headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        None => true,
        Some(origin) => [format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}")].contains(&origin.to_string()),
    }
}

async fn create_job(State(app): State<Arc<App>>, headers: HeaderMap, mut form: Multipart) -> Response {
    if !same_origin(&headers, app.port) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "다른 사이트에서 온 요청은 받지 않습니다." }))).into_response();
    }
    let id = app.next_id.fetch_add(1, Ordering::Relaxed);
    let upload_dir = app.defaults.cache_dir.join("uploads").join(id.to_string());
    let (mut url, mut upload, mut translate) = (None::<String>, None::<PathBuf>, None::<LangCode>);
    loop {
        let field = match form.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => return bad_request(format!("업로드가 중간에 끊겼습니다: {e}")),
        };
        match field.name().unwrap_or_default() {
            "url" => url = field.text().await.ok().filter(|u| !u.trim().is_empty()),
            "translate" => {
                let code = field.text().await.unwrap_or_default();
                if !code.is_empty() {
                    match code.parse() {
                        Ok(lang) => translate = Some(lang),
                        Err(e) => return bad_request(e),
                    }
                }
            }
            "file" => match save_upload(field, &upload_dir).await {
                Ok(path) => upload = Some(path),
                Err(e) => {
                    let _ = tokio::fs::remove_dir_all(&upload_dir).await;
                    return bad_request(format!("파일을 받지 못했습니다: {e:#}"));
                }
            },
            _ => {}
        }
    }
    let source = match (upload.clone(), url) {
        (Some(path), _) => Source::LocalFile { path },
        (None, Some(url)) => match Source::parse_youtube(&url) {
            Some(source) => source,
            None => return bad_request("YouTube 영상 주소가 아닙니다. 예: https://www.youtube.com/watch?v=..."),
        },
        (None, None) => return bad_request("YouTube 주소를 넣거나 영상 파일을 골라 주세요."),
    };
    let stages = Stage::ALL
        .iter()
        .map(|&stage| StageRow { stage, status: StageStatus::Pending, detail: None, started: None, elapsed: None })
        .collect();
    app.jobs.lock().unwrap().insert(
        id,
        Job {
            source,
            translate,
            upload,
            state: JobState::Queued,
            stages,
            created: Instant::now(),
            finished: None,
            error: None,
            title: None,
            pdf: None,
        },
    );
    let _ = app.queue.send(id);
    Json(json!({ "id": id })).into_response()
}

/// Stream one multipart file field to disk without holding it in memory.
async fn save_upload(mut field: axum::extract::multipart::Field<'_>, dir: &Path) -> Result<PathBuf> {
    let name = field
        .file_name()
        .and_then(|n| Path::new(n).file_name())
        .map(|n| n.to_string_lossy().replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_"))
        .filter(|n| !n.is_empty() && !n.starts_with('.'))
        .unwrap_or_else(|| "upload.mp4".into());
    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(name);
    let mut file = tokio::fs::File::create(&path).await?;
    let mut size = 0u64;
    while let Some(chunk) = field.chunk().await? {
        size += chunk.len() as u64;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    anyhow::ensure!(size > 0, "빈 파일입니다");
    Ok(std::path::absolute(&path)?)
}

async fn job_status(State(app): State<Arc<App>>, UrlPath(id): UrlPath<u64>) -> Response {
    let jobs = app.jobs.lock().unwrap();
    let Some(job) = jobs.get(&id) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "그런 작업이 없습니다. 서버가 다시 시작되었을 수 있습니다." }))).into_response();
    };
    let ahead = jobs
        .iter()
        .filter(|(other, j)| **other < id && matches!(j.state, JobState::Queued | JobState::Running))
        .count();
    Json(job_view(id, job, ahead)).into_response()
}

fn job_view(id: u64, job: &Job, ahead: usize) -> Value {
    let stages: Vec<Value> = job
        .stages
        .iter()
        .map(|r| {
            let elapsed = r.elapsed.or_else(|| match r.status {
                StageStatus::Running => r.started.map(|s| s.elapsed().as_secs_f64()),
                _ => None,
            });
            json!({ "key": r.stage, "label": r.stage.label(), "status": r.status, "detail": r.detail, "elapsed": elapsed })
        })
        .collect();
    let end = job.finished.unwrap_or_else(Instant::now);
    let label = match &job.source {
        Source::YouTube { id } => Source::youtube_url(id),
        Source::LocalFile { path } => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let translate = job.translate.as_ref().and_then(|l| LANGUAGES.iter().find(|x| x.code == l.as_str())).map(|l| l.native);
    json!({
        "id": id,
        "label": label,
        "translate": translate,
        "state": job.state,
        "ahead": ahead,
        "stages": stages,
        "elapsed": end.duration_since(job.created).as_secs_f64(),
        "error": job.error,
        "title": job.title,
    })
}

async fn job_pdf(State(app): State<Arc<App>>, UrlPath(id): UrlPath<u64>) -> Response {
    let (pdf, title) = {
        let jobs = app.jobs.lock().unwrap();
        match jobs.get(&id) {
            Some(Job { pdf: Some(pdf), title, .. }) => (pdf.clone(), title.clone().unwrap_or_default()),
            _ => return (StatusCode::NOT_FOUND, "PDF가 아직 없습니다.").into_response(),
        }
    };
    let bytes = match tokio::fs::read(&pdf).await {
        Ok(bytes) => bytes,
        Err(_) => return (StatusCode::NOT_FOUND, "PDF 파일이 지워졌습니다.").into_response(),
    };
    let disposition = format!("attachment; filename=\"ytdoc.pdf\"; filename*=UTF-8''{}", percent_encode(&pdf_file_name(&title)));
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/pdf"));
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

fn pdf_file_name(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { ' ' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean: String = clean.chars().take(80).collect();
    if clean.is_empty() { "ytdoc.pdf".into() } else { format!("{clean}.pdf") }
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_name_is_safe_and_readable() {
        assert_eq!(pdf_file_name("HTML: caniuse / 호환성?"), "HTML caniuse 호환성.pdf");
        assert_eq!(pdf_file_name("  "), "ytdoc.pdf");
        assert_eq!(percent_encode("가 a.pdf"), "%EA%B0%80%20a.pdf");
    }

    #[test]
    fn only_local_origins_may_post() {
        let mut headers = HeaderMap::new();
        assert!(same_origin(&headers, 8000));
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://127.0.0.1:8000"));
        assert!(same_origin(&headers, 8000));
        headers.insert(header::ORIGIN, HeaderValue::from_static("https://evil.example"));
        assert!(!same_origin(&headers, 8000));
    }
}
