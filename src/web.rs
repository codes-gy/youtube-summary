//! `ytdoc serve`: a one-page local web UI over the same pipeline the CLI runs.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
use crate::pipeline::{Event, Finished, Options, Outcome, Progress, Stage, run_pipeline};
use crate::source::Source;

const PAGE: &str = include_str!("../templates/web.html");

/// Default for `ytdoc serve --max-upload-mb`: long lectures run to a few GB.
pub const DEFAULT_MAX_UPLOAD_MB: u64 = 4096;
/// Room for the multipart boundaries and the small text fields next to the file.
const FORM_OVERHEAD: u64 = 1024 * 1024;

/// What the worker runs for one job; tests swap in one that fails or panics.
type PipelineFn = fn(&Source, &Options, &dyn Progress) -> Result<Finished>;

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
    /// The uploaded file, deleted once the job has ended.
    upload: Option<PathBuf>,
    state: JobState,
    stages: Vec<StageRow>,
    created: Instant,
    finished: Option<Instant>,
    error: Option<String>,
    title: Option<String>,
    pdf: Option<PathBuf>,
}

impl Job {
    fn queued(source: Source, translate: Option<LangCode>, upload: Option<PathBuf>) -> Self {
        let stages = Stage::ALL
            .iter()
            .map(|&stage| StageRow { stage, status: StageStatus::Pending, detail: None, started: None, elapsed: None })
            .collect();
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
        }
    }
}

struct App {
    defaults: Options,
    port: u16,
    /// Largest video file a job may upload, in bytes.
    max_upload: u64,
    jobs: Mutex<HashMap<u64, Job>>,
    /// Ids in submission order; the single worker takes them one at a time.
    queue: mpsc::UnboundedSender<u64>,
    /// The last id handed out. Ids are microseconds since the Unix epoch, so a restarted
    /// server does not start over at 1 and overwrite an earlier run's `outputs/<id>.pdf`.
    last_id: AtomicU64,
    pipeline: PipelineFn,
}

impl App {
    fn new(defaults: Options, port: u16, max_upload: u64, queue: mpsc::UnboundedSender<u64>, pipeline: PipelineFn) -> Self {
        App { defaults, port, max_upload, jobs: Mutex::new(HashMap::new()), queue, last_id: AtomicU64::new(0), pipeline }
    }

    /// A job that panicked while holding the lock must not take every later request down with it.
    fn jobs(&self) -> MutexGuard<'_, HashMap<u64, Job>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A new id: larger than every id this process handed out and, unless the clock went
    /// backwards, every id an earlier run handed out. Stays below 2^53, so JavaScript keeps it exact.
    fn next_id(&self) -> u64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0);
        let mut last = self.last_id.load(Ordering::Relaxed);
        loop {
            let id = now.max(last + 1);
            match self.last_id.compare_exchange_weak(last, id, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return id,
                Err(actual) => last = actual,
            }
        }
    }
}

pub fn serve(port: u16, max_upload_mb: u64, defaults: Options) -> Result<()> {
    anyhow::ensure!(max_upload_mb > 0, "--max-upload-mb는 1 이상이어야 합니다");
    let max_upload = max_upload_mb.saturating_mul(1024 * 1024);
    tokio::runtime::Runtime::new()?.block_on(async move {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = Arc::new(App::new(defaults, port, max_upload, tx, run_pipeline));
        tokio::spawn(worker(app.clone(), rx));
        let router = router(app.clone());
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("{addr}에서 열 수 없습니다. 다른 프로그램이 쓰고 있다면 --port로 다른 번호를 고르세요"))?;
        eprintln!("ytdoc 웹 페이지: http://{addr}  (끝내려면 Ctrl+C, 업로드 최대 {max_upload_mb} MB)");
        if let Some(vault) = &app.defaults.obsidian {
            eprintln!("요약을 옵시디언 노트로도 저장합니다: {}", vault.display());
        }
        axum::serve(listener, router).await?;
        Ok(())
    })
}

fn router(app: Arc<App>) -> Router {
    let body_limit = usize::try_from(app.max_upload.saturating_add(FORM_OVERHEAD)).unwrap_or(usize::MAX);
    Router::new()
        .route("/", get(page))
        .route("/api/jobs", post(create_job).layer(DefaultBodyLimit::max(body_limit)))
        .route("/api/jobs/{id}", get(job_status))
        .route("/api/jobs/{id}/pdf", get(job_pdf))
        .layer(axum::middleware::from_fn_with_state(app.clone(), local_host_only))
        .with_state(app)
}

async fn worker(app: Arc<App>, mut rx: mpsc::UnboundedReceiver<u64>) {
    while let Some(id) = rx.recv().await {
        let runner = app.clone();
        // A panic inside the pipeline would otherwise leave the job "running" forever
        // and its uploaded video on disk.
        if let Err(err) = tokio::task::spawn_blocking(move || run_job(&runner, id)).await {
            let reason = if err.is_panic() { panic_message(err.into_panic()) } else { err.to_string() };
            finish_job(&app, id, Err(format!("처리 중 예기치 못한 오류로 멈췄습니다: {reason}")));
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "알 수 없는 패닉".into())
}

fn run_job(app: &App, id: u64) {
    let (source, opts) = {
        let mut jobs = app.jobs();
        let Some(job) = jobs.get_mut(&id) else { return };
        job.state = JobState::Running;
        let opts = Options {
            translate: job.translate.clone(),
            output: Some(app.defaults.cache_dir.join("outputs").join(format!("{id}.pdf"))),
            ..app.defaults.clone()
        };
        (job.source.clone(), opts)
    };
    let result = (app.pipeline)(&source, &opts, &JobProgress { app, id });
    finish_job(app, id, result.map_err(|err| format!("{err:#}")));
}

/// Record how a job ended and delete its upload. Called once the pipeline returns or
/// panics; a job that has already ended is left as it is.
fn finish_job(app: &App, id: u64, result: std::result::Result<Finished, String>) {
    let upload = {
        let mut jobs = app.jobs();
        let Some(job) = jobs.get_mut(&id) else { return };
        if matches!(job.state, JobState::Done | JobState::Failed) {
            return;
        }
        job.finished = Some(Instant::now());
        match result {
            Ok(done) => {
                job.state = JobState::Done;
                job.title = Some(done.title);
                job.pdf = Some(done.pdf);
            }
            Err(err) => {
                job.state = JobState::Failed;
                job.error = Some(err);
                for row in job.stages.iter_mut().filter(|r| r.status == StageStatus::Running) {
                    row.status = StageStatus::Failed;
                    row.elapsed = row.started.map(|s| s.elapsed().as_secs_f64());
                }
            }
        }
        job.upload.take()
    };
    if let Some(dir) = upload.as_deref().and_then(Path::parent) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

struct JobProgress<'a> {
    app: &'a App,
    id: u64,
}

impl Progress for JobProgress<'_> {
    fn event(&self, event: Event) {
        let mut jobs = self.app.jobs();
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

async fn page(State(app): State<Arc<App>>) -> Html<String> {
    let options: String = LANGUAGES
        .iter()
        .map(|l| format!("<option value=\"{}\"{}>{}</option>", l.code, if l.code == "ko" { " selected" } else { "" }, l.native))
        .collect();
    Html(PAGE.replace("<!--LANGUAGE_OPTIONS-->", &options).replace("__MAX_UPLOAD_BYTES__", &app.max_upload.to_string()))
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

/// A website whose domain re-resolves to 127.0.0.1 (DNS rebinding) reaches this server
/// with its own name in `Host`; only our own names may read jobs and PDFs.
fn local_host(headers: &HeaderMap, port: u16) -> bool {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|host| [format!("127.0.0.1:{port}"), format!("localhost:{port}")].iter().any(|ok| ok == host))
}

async fn local_host_only(
    State(app): State<Arc<App>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !local_host(request.headers(), app.port) {
        return (StatusCode::FORBIDDEN, "ytdoc은 http://127.0.0.1 주소로만 엽니다.").into_response();
    }
    next.run(request).await
}

async fn create_job(State(app): State<Arc<App>>, headers: HeaderMap, form: Multipart) -> Response {
    if !same_origin(&headers, app.port) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "다른 사이트에서 온 요청은 받지 않습니다." }))).into_response();
    }
    let id = app.next_id();
    let upload_dir = app.defaults.cache_dir.join("uploads").join(id.to_string());
    let (source, translate, upload) = match read_job_form(form, &upload_dir, app.max_upload).await {
        Ok(parsed) => parsed,
        Err(response) => {
            // Whatever part of a file arrived before the request was turned down.
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            return response;
        }
    };
    app.jobs().insert(id, Job::queued(source, translate, upload));
    let _ = app.queue.send(id);
    Json(json!({ "id": id })).into_response()
}

type JobForm = (Source, Option<LangCode>, Option<PathBuf>);

async fn read_job_form(mut form: Multipart, upload_dir: &Path, max_upload: u64) -> std::result::Result<JobForm, Response> {
    let (mut url, mut upload, mut translate) = (None::<String>, None::<PathBuf>, None::<LangCode>);
    loop {
        let field = match form.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            // The whole request went over the body limit before the file field ended.
            Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => return Err(too_large(max_upload)),
            Err(e) => return Err(bad_request(format!("업로드가 중간에 끊겼습니다: {e}"))),
        };
        match field.name().unwrap_or_default() {
            "url" => url = field.text().await.ok().filter(|u| !u.trim().is_empty()),
            "translate" => {
                let code = field.text().await.unwrap_or_default();
                if !code.is_empty() {
                    match code.parse() {
                        Ok(lang) => translate = Some(lang),
                        Err(e) => return Err(bad_request(e)),
                    }
                }
            }
            "file" => match save_upload(field, upload_dir, max_upload).await {
                Ok(path) => upload = Some(path),
                Err(e) if e.is::<TooLarge>() => return Err(too_large(max_upload)),
                Err(e) => return Err(bad_request(format!("파일을 받지 못했습니다: {e:#}"))),
            },
            _ => {}
        }
    }
    let source = match (upload.clone(), url) {
        (Some(path), _) => Source::LocalFile { path },
        (None, Some(url)) => match Source::parse_youtube(&url) {
            Some(source) => source,
            None => return Err(bad_request("YouTube 영상 주소가 아닙니다. 예: https://www.youtube.com/watch?v=...")),
        },
        (None, None) => return Err(bad_request("YouTube 주소를 넣거나 영상 파일을 골라 주세요.")),
    };
    Ok((source, translate, upload))
}

/// An upload larger than `--max-upload-mb`.
#[derive(Debug)]
struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("파일이 너무 큽니다")
    }
}

impl std::error::Error for TooLarge {}

fn too_large(max_upload: u64) -> Response {
    let mb = max_upload / (1024 * 1024);
    let msg = format!("파일이 너무 큽니다. 최대 {mb} MB까지 올릴 수 있습니다 (ytdoc serve --max-upload-mb로 바꿀 수 있습니다).");
    (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({ "error": msg }))).into_response()
}

/// Stream one multipart file field to disk without holding it in memory,
/// stopping as soon as it grows past `max` bytes.
async fn save_upload(mut field: axum::extract::multipart::Field<'_>, dir: &Path, max: u64) -> Result<PathBuf> {
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
    let too_big = |e: axum::extract::multipart::MultipartError| -> anyhow::Error {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE { TooLarge.into() } else { e.into() }
    };
    while let Some(chunk) = field.chunk().await.map_err(too_big)? {
        size += chunk.len() as u64;
        if size > max {
            return Err(TooLarge.into());
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    anyhow::ensure!(size > 0, "빈 파일입니다");
    Ok(std::path::absolute(&path)?)
}

async fn job_status(State(app): State<Arc<App>>, UrlPath(id): UrlPath<u64>) -> Response {
    let jobs = app.jobs();
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
        let jobs = app.jobs();
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
    use crate::claude::BackendChoice;
    use std::time::Duration;

    fn options(cache_dir: &Path) -> Options {
        Options {
            cache_dir: cache_dir.to_path_buf(),
            whisper_model: "tiny".into(),
            max_images: 1,
            summarizer: BackendChoice::Auto,
            translate: None,
            output: None,
            obsidian: None,
        }
    }

    fn panics(_: &Source, _: &Options, progress: &dyn Progress) -> Result<Finished> {
        progress.event(Event::Started(Stage::Transcribe));
        panic!("whisper blew up");
    }

    fn fails(_: &Source, _: &Options, _: &dyn Progress) -> Result<Finished> {
        anyhow::bail!("no network")
    }

    /// Queue one job with an uploaded file, run the worker, and wait for the job to end.
    async fn run_one(pipeline: PipelineFn) -> (tempfile::TempDir, Arc<App>, u64, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let app = Arc::new(App::new(options(dir.path()), 8000, 1 << 20, tx, pipeline));
        let id = app.next_id();
        let upload = dir.path().join("uploads").join(id.to_string()).join("video.mp4");
        std::fs::create_dir_all(upload.parent().unwrap()).unwrap();
        std::fs::write(&upload, b"video").unwrap();
        app.jobs().insert(id, Job::queued(Source::LocalFile { path: upload.clone() }, None, Some(upload.clone())));
        app.queue.send(id).unwrap();
        tokio::spawn(worker(app.clone(), rx));
        for _ in 0..200 {
            if matches!(app.jobs()[&id].state, JobState::Done | JobState::Failed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        (dir, app, id, upload)
    }

    #[tokio::test]
    async fn a_panicking_job_fails_and_its_upload_is_removed() {
        let (_dir, app, id, upload) = run_one(panics).await;
        let jobs = app.jobs();
        let job = &jobs[&id];
        assert_eq!(job.state, JobState::Failed);
        assert!(job.error.as_deref().unwrap().contains("whisper blew up"), "{:?}", job.error);
        assert!(job.finished.is_some());
        let transcribe = job.stages.iter().find(|r| r.stage == Stage::Transcribe).unwrap();
        assert_eq!(transcribe.status, StageStatus::Failed);
        assert!(!upload.parent().unwrap().exists(), "upload dir left behind");
    }

    #[tokio::test]
    async fn the_worker_keeps_going_after_a_panic() {
        let (_dir, app, _, _) = run_one(panics).await;
        // The lock was not left poisoned and later jobs still run.
        let id = app.next_id();
        app.jobs().insert(id, Job::queued(Source::YouTube { id: "abc".into() }, None, None));
        app.queue.send(id).unwrap();
        for _ in 0..200 {
            if app.jobs()[&id].state == JobState::Failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(app.jobs()[&id].state, JobState::Failed);
    }

    #[tokio::test]
    async fn a_failing_job_fails_and_its_upload_is_removed() {
        let (_dir, app, id, upload) = run_one(fails).await;
        assert_eq!(app.jobs()[&id].state, JobState::Failed);
        assert!(app.jobs()[&id].error.as_deref().unwrap().contains("no network"));
        assert!(!upload.parent().unwrap().exists());
    }

    #[test]
    fn job_ids_grow_and_do_not_restart_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let app = App::new(options(dir.path()), 8000, 1, tx, fails);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64;
        let ids: Vec<u64> = (0..1000).map(|_| app.next_id()).collect();
        assert!(ids[0] >= now, "ids are time based, so a restarted server does not reuse 1, 2, 3...");
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "ids are unique and increasing");
        // JavaScript numbers hold integers up to 2^53 exactly.
        assert!(*ids.last().unwrap() < 1u64 << 53);

        // A second server started later (same clock) hands out larger ids.
        let (tx, _rx) = mpsc::unbounded_channel();
        let restarted = App::new(options(dir.path()), 8000, 1, tx, fails);
        std::thread::sleep(Duration::from_millis(2));
        assert!(restarted.next_id() > ids[0]);
    }

    /// Start the real router on a free port and post a multipart body to it.
    async fn post_file(max_upload: u64, file_size: usize) -> (u16, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, _rx) = mpsc::unbounded_channel();
        let app = Arc::new(App::new(options(dir.path()), port, max_upload, tx, fails));
        tokio::spawn(async move { axum::serve(listener, router(app)).await });

        let boundary = "ytdocboundary";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"v.mp4\"\r\nContent-Type: video/mp4\r\n\r\n"
        )
        .into_bytes();
        body.extend(std::iter::repeat_n(b'x', file_size));
        body.extend(format!("\r\n--{boundary}--\r\n").into_bytes());
        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/api/jobs"))
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.text().await.unwrap(), dir)
    }

    #[tokio::test]
    async fn uploads_over_the_limit_are_refused_and_not_kept() {
        let (status, body, dir) = post_file(64 * 1024, 200 * 1024).await;
        assert_eq!(status, 413, "{body}");
        assert!(body.contains("max-upload-mb"), "{body}");
        let uploads = dir.path().join("uploads");
        let left = std::fs::read_dir(&uploads).map(|d| d.count()).unwrap_or(0);
        assert_eq!(left, 0, "a refused upload left files in {}", uploads.display());
    }

    #[tokio::test]
    async fn the_page_knows_the_upload_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let app = Arc::new(App::new(options(dir.path()), 8000, 65536, tx, fails));
        let Html(html) = page(State(app)).await;
        assert!(html.contains(r#"const MAX_UPLOAD_BYTES = Number("65536");"#));
        assert!(!html.contains("__MAX_UPLOAD_BYTES__"));
    }

    #[tokio::test]
    async fn uploads_under_the_limit_are_accepted() {
        let (status, body, _dir) = post_file(64 * 1024, 1024).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("\"id\""), "{body}");
    }

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

    #[test]
    fn only_local_host_names_are_served() {
        let mut headers = HeaderMap::new();
        assert!(!local_host(&headers, 8000));
        for (host, ok) in [("127.0.0.1:8000", true), ("localhost:8000", true), ("evil.example:8000", false), ("127.0.0.1:9000", false)] {
            headers.insert(header::HOST, HeaderValue::from_static(host));
            assert_eq!(local_host(&headers, 8000), ok, "{host}");
        }
    }
}
