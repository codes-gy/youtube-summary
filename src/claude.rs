use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub const API_MODEL: &str = "claude-opus-5-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";

/// What the user picks with `--summarizer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendChoice {
    /// api when ANTHROPIC_API_KEY is set, claude-cli otherwise
    Auto,
    Api,
    ClaudeCli,
}

/// A resolved way to reach Claude.
pub enum Backend {
    Api { key: String },
    Cli { program: PathBuf },
}

/// One structured-output request. Returns the JSON text Claude produced.
pub trait Llm: Sync {
    fn complete(&self, system: &str, prompt: &str, schema: &Value) -> Result<String>;
}

impl BackendChoice {
    pub fn resolve(self) -> Result<Backend> {
        let key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty());
        match (self, key) {
            (BackendChoice::Api | BackendChoice::Auto, Some(key)) => Ok(Backend::Api { key }),
            (BackendChoice::Api, None) => bail!("--summarizer api에는 ANTHROPIC_API_KEY 환경 변수가 필요합니다."),
            (BackendChoice::ClaudeCli | BackendChoice::Auto, _) => find_claude_cli(),
        }
    }
}

fn find_claude_cli() -> Result<Backend> {
    let home_local: Vec<PathBuf> = crate::platform::home_dir().map(|h| h.join(".local").join("bin")).into_iter().collect();
    crate::platform::find_program("claude", &home_local)
        .map(|program| Backend::Cli { program })
        .context("`claude` 명령을 찾을 수 없습니다. Claude Code를 설치해 로그인하거나 ANTHROPIC_API_KEY를 설정하세요.")
}

impl Llm for Backend {
    fn complete(&self, system: &str, prompt: &str, schema: &Value) -> Result<String> {
        match self {
            Backend::Api { key } => complete_api(key, system, prompt, schema),
            Backend::Cli { program } => complete_cli(program, system, prompt, schema),
        }
    }
}

/// Ask for a `T`, validate it with serde and `check`, and retry once with the
/// rejection reason appended when the answer does not hold up.
pub fn ask<T, C>(llm: &dyn Llm, system: &str, prompt: &str, check: C) -> Result<T>
where
    T: DeserializeOwned + JsonSchema,
    C: Fn(&T) -> Result<(), String>,
{
    let schema = schema_for::<T>();
    let mut feedback = String::new();
    for _ in 0..2 {
        let text = llm.complete(system, &format!("{prompt}{feedback}"), &schema)?;
        let verdict = serde_json::from_str::<T>(&text)
            .map_err(|e| e.to_string())
            .and_then(|value| check(&value).map(|()| value));
        match verdict {
            Ok(value) => return Ok(value),
            Err(reason) => {
                feedback = format!(
                    "\n\nYour previous answer was rejected: {reason}\nReturn a corrected answer."
                );
            }
        }
    }
    bail!("Claude가 두 번 연속 잘못된 형식으로 답했습니다:{}", feedback.lines().nth(2).unwrap_or_default())
}

/// JSON schema derived from the Rust type, in the subset structured outputs accept:
/// every object closed (`additionalProperties: false`) with all its properties required.
pub fn schema_for<T: JsonSchema>() -> Value {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator();
    let mut schema = generator.into_root_schema_for::<T>().to_value();
    close_objects(&mut schema);
    schema
}

fn close_objects(node: &mut Value) {
    let Value::Object(map) = node else { return };
    for keyword in ["$schema", "title", "format", "minimum", "maximum"] {
        map.remove(keyword);
    }
    if let Some(Value::Object(props)) = map.get_mut("properties") {
        props.values_mut().for_each(close_objects);
        let required: Vec<Value> = props.keys().cloned().map(Value::String).collect();
        map.insert("required".into(), Value::Array(required));
        map.insert("additionalProperties".into(), Value::Bool(false));
    }
    if let Some(items) = map.get_mut("items") {
        close_objects(items);
    }
    for combinator in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = map.get_mut(combinator) {
            variants.iter_mut().for_each(close_objects);
        }
    }
}

pub fn api_request_body(system: &str, prompt: &str, schema: &Value) -> Value {
    json!({
        "model": API_MODEL,
        "max_tokens": 16000,
        "system": system,
        "messages": [{"role": "user", "content": prompt}],
        "output_config": {"format": {"type": "json_schema", "schema": schema}},
    })
}

#[derive(Deserialize)]
pub struct ApiResponse {
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

pub fn api_reply_text(response: ApiResponse) -> Result<String> {
    match response.stop_reason.as_deref() {
        Some("refusal") => bail!("Claude가 이 영상의 처리를 거절했습니다."),
        Some("max_tokens") => bail!("Claude의 답이 출력 한도에 걸려 끝나지 않았습니다."),
        _ => {}
    }
    response
        .content
        .into_iter()
        .find(|b| b.kind == "text")
        .and_then(|b| b.text)
        .context("Claude의 답에 글이 없습니다.")
}

/// How many times one API request is retried after a rate limit (429), an overload (529)
/// or a server error (5xx), on top of the first try.
const API_MAX_RETRIES: u32 = 4;
/// Backoff before retry n (0-based) is up to `API_BACKOFF_BASE * 2^n`, capped at `API_BACKOFF_MAX`.
const API_BACKOFF_BASE: Duration = Duration::from_secs(2);
const API_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A `retry-after` longer than this is not worth waiting for in one run.
const API_RETRY_AFTER_MAX: Duration = Duration::from_secs(300);

/// One try at the API: the answer, a failure worth retrying, or a failure that is final.
enum Attempt<T> {
    Done(T),
    Retry { error: anyhow::Error, retry_after: Option<Duration> },
    Fatal(anyhow::Error),
}

/// Statuses that mean "try again later": rate limited, overloaded, or the server broke.
fn is_retryable(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 529 || status.is_server_error()
}

/// The `retry-after` header in seconds (Anthropic sends whole seconds).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    value.parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0).map(Duration::from_secs_f64)
}

/// Wait before retry `retry` (0-based): exponential backoff with full jitter (`jitter` in 0..1),
/// but never shorter than what the server asked for in `retry-after`.
fn backoff_delay(retry: u32, retry_after: Option<Duration>, jitter: f64) -> Duration {
    let exp = API_BACKOFF_BASE.saturating_mul(2u32.saturating_pow(retry)).min(API_BACKOFF_MAX);
    let jittered = exp.mul_f64(jitter.clamp(0.0, 1.0));
    match retry_after {
        Some(asked) => asked.min(API_RETRY_AFTER_MAX).max(jittered),
        None => jittered,
    }
}

/// A random fraction in 0..1, without pulling in a random-number crate.
fn jitter() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    (hasher.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// Run `attempt` until it succeeds, fails for good, or has been retried `max_retries` times.
fn with_retries<T>(
    max_retries: u32,
    mut attempt: impl FnMut() -> Attempt<T>,
    mut sleep: impl FnMut(Duration),
    mut jitter: impl FnMut() -> f64,
) -> Result<T> {
    let mut retry = 0;
    loop {
        match attempt() {
            Attempt::Done(value) => return Ok(value),
            Attempt::Fatal(error) => return Err(error),
            Attempt::Retry { error, .. } if retry >= max_retries => {
                return Err(error.context(format!("{}번 다시 시도했지만 실패했습니다", max_retries)));
            }
            Attempt::Retry { error, retry_after } => {
                let wait = backoff_delay(retry, retry_after, jitter());
                eprintln!("  Claude API가 잠시 응답하지 못했습니다 ({error:#}). {:.0}초 뒤 다시 시도합니다.", wait.as_secs_f64());
                sleep(wait);
                retry += 1;
            }
        }
    }
}

fn complete_api(key: &str, system: &str, prompt: &str, schema: &Value) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()?;
    let body = api_request_body(system, prompt, schema);
    let text = with_retries(API_MAX_RETRIES, || api_attempt(&client, key, &body), std::thread::sleep, jitter)?;
    api_reply_text(serde_json::from_str(&text).context("Anthropic API 응답을 읽지 못했습니다")?)
}

/// One POST. Connection failures are retried too; a timeout after 15 minutes is not.
fn api_attempt(client: &reqwest::blocking::Client, key: &str, body: &Value) -> Attempt<String> {
    let response = match client
        .post(API_URL)
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .json(body)
        .send()
    {
        Ok(response) => response,
        Err(e) if e.is_connect() => {
            return Attempt::Retry { error: anyhow::Error::new(e).context("Anthropic API에 연결하지 못했습니다"), retry_after: None };
        }
        Err(e) => return Attempt::Fatal(anyhow::Error::new(e).context("Anthropic API에 연결하지 못했습니다")),
    };
    let status = response.status();
    let wait = retry_after(response.headers());
    let text = match response.text() {
        Ok(text) => text,
        Err(e) => return Attempt::Fatal(anyhow::Error::new(e).context("Anthropic API 응답을 받지 못했습니다")),
    };
    if status.is_success() {
        return Attempt::Done(text);
    }
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or(text);
    let error = anyhow::anyhow!("Anthropic API 오류 ({status}): {message}");
    if is_retryable(status) { Attempt::Retry { error, retry_after: wait } } else { Attempt::Fatal(error) }
}

/// How long one `claude -p` call may run before it is killed. A long transcript takes a
/// few minutes; this only stops a hung process. Override with `YTDOC_CLAUDE_TIMEOUT_SECS`.
const CLI_TIMEOUT: Duration = Duration::from_secs(900);

fn cli_timeout() -> Duration {
    std::env::var("YTDOC_CLAUDE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(Duration::from_secs)
        .unwrap_or(CLI_TIMEOUT)
}

fn complete_cli(program: &PathBuf, system: &str, prompt: &str, schema: &Value) -> Result<String> {
    complete_cli_with_timeout(program, system, prompt, schema, cli_timeout())
}

fn complete_cli_with_timeout(program: &PathBuf, system: &str, prompt: &str, schema: &Value, timeout: Duration) -> Result<String> {
    // A file, not an argument: on Windows `claude` may be `claude.cmd`, and Rust refuses
    // to pass multi-line arguments to batch files.
    let system_file = tempfile::NamedTempFile::new()?;
    std::fs::write(system_file.path(), system)?;
    let mut command = Command::new(program);
    // Its own process group, so a timeout can stop whatever it started as well.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .args(["-p", "--output-format", "json", "--json-schema"])
        .arg(schema.to_string())
        .arg("--system-prompt-file")
        .arg(system_file.path())
        .args([
            "--tools",
            "",
            "--no-session-persistence",
            "--disable-slash-commands",
            "--strict-mcp-config",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("{} 실행 실패", program.display()))?;
    // Feed stdin and drain stdout/stderr on their own threads: a long prompt fills the pipe,
    // and a process that stops reading must not block us past the timeout.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let prompt = prompt.as_bytes().to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&prompt));
    let stdout = read_in_background(child.stdout.take().expect("piped stdout"));
    let stderr = read_in_background(child.stderr.take().expect("piped stderr"));

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            kill_tree(&mut child);
            // The reader threads finish once the pipes close; they are not waited for.
            bail!(
                "claude -p가 {}초 안에 끝나지 않아 멈췄습니다 (YTDOC_CLAUDE_TIMEOUT_SECS로 바꿀 수 있습니다)",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if let Ok(Err(e)) = writer.join() {
        // A process that exits early closes stdin; its own exit status says why.
        if status.success() {
            return Err(anyhow::Error::new(e).context("claude -p에 프롬프트를 보내지 못했습니다"));
        }
    }
    let stdout = String::from_utf8_lossy(&stdout);
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        bail!("claude -p 실패 ({status}): {}", tail(if stderr.trim().is_empty() { &stdout } else { &stderr }));
    }
    cli_reply_text(&stdout)
}

fn read_in_background(mut pipe: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// Stop the process and what it started (`claude.cmd` runs node under cmd.exe on Windows).
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // The child leads its own process group (see `process_group(0)` above).
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Unwrap the envelope printed by `claude -p --output-format json --json-schema`.
pub fn cli_reply_text(stdout: &str) -> Result<String> {
    let envelope: Value = serde_json::from_str(stdout).context("claude -p 출력이 JSON이 아닙니다")?;
    if envelope["is_error"].as_bool() == Some(true) {
        bail!("claude -p 오류: {}", envelope["result"].as_str().unwrap_or("알 수 없음"));
    }
    match &envelope["structured_output"] {
        Value::Null => Ok(envelope["result"].as_str().unwrap_or("null").to_string()),
        payload => Ok(payload.to_string()),
    }
}

fn tail(text: &str) -> &str {
    let text = text.trim();
    let cut = text.len().saturating_sub(1500);
    let start = (cut..=text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    &text[start..]
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::domain::Summary;
    use std::sync::Mutex;

    /// Replays canned answers and records the prompts it saw.
    pub struct Scripted {
        pub answers: Mutex<Vec<String>>,
        pub prompts: Mutex<Vec<String>>,
    }

    impl Scripted {
        pub fn new(answers: &[&str]) -> Self {
            Scripted {
                answers: Mutex::new(answers.iter().rev().map(|s| s.to_string()).collect()),
                prompts: Mutex::new(vec![]),
            }
        }
    }

    impl Llm for Scripted {
        fn complete(&self, _system: &str, prompt: &str, _schema: &Value) -> Result<String> {
            self.prompts.lock().unwrap().push(prompt.to_string());
            self.answers.lock().unwrap().pop().context("no more answers")
        }
    }

    const GOOD: &str = r#"{"title":"T","one_line":"O","overview":"V","key_points":["k"],
        "chapters":[{"start":0,"end":10,"title":"c","summary":"s","key_points":["p"]}],
        "concepts":[{"name":"a","meaning":"m"}],"relations":[],"comparisons":[]}"#;

    fn ask_summary(llm: &Scripted) -> Result<Summary> {
        ask(llm, "sys", "prompt", Summary::check)
    }

    #[test]
    fn schema_is_closed_and_fully_required() {
        let schema = schema_for::<Summary>();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"].as_array().unwrap().len(), 8);
        let chapter = &schema["properties"]["chapters"]["items"];
        assert_eq!(chapter["additionalProperties"], false);
        assert_eq!(chapter["required"].as_array().unwrap().len(), 5);
        assert!(chapter["properties"]["start"]["description"].is_string());
        let text = schema.to_string();
        for banned in ["$ref", "$schema", "\"format\""] {
            assert!(!text.contains(banned), "{banned} in {text}");
        }
    }

    #[test]
    fn valid_answer_is_accepted_first_time() {
        let llm = Scripted::new(&[GOOD]);
        assert_eq!(ask_summary(&llm).unwrap().title, "T");
        assert_eq!(llm.prompts.lock().unwrap().len(), 1);
    }

    #[test]
    fn invalid_answer_is_retried_with_the_reason() {
        let backwards = GOOD.replace(r#""start":0,"end":10"#, r#""start":10,"end":0"#);
        let llm = Scripted::new(&[&backwards, GOOD]);
        assert_eq!(ask_summary(&llm).unwrap().title, "T");
        let prompts = llm.prompts.lock().unwrap();
        assert!(prompts[1].starts_with("prompt\n\nYour previous answer was rejected: chapter"));
    }

    #[test]
    fn gives_up_after_two_bad_answers() {
        let llm = Scripted::new(&["not json", r#"{"title":"T"}"#]);
        let err = ask_summary(&llm).unwrap_err().to_string();
        assert!(err.contains("두 번 연속"), "{err}");
        assert!(err.contains("missing field"), "{err}");
    }

    #[test]
    fn api_body_has_structured_output_and_no_sampling_knobs() {
        let body = api_request_body("s", "p", &json!({"type": "object"}));
        assert_eq!(body["model"], API_MODEL);
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        for knob in ["thinking", "temperature", "top_p"] {
            assert!(body.get(knob).is_none(), "{knob}");
        }
    }

    #[test]
    fn api_reply_skips_thinking_and_stops_on_refusal() {
        let reply: ApiResponse = serde_json::from_value(json!({
            "stop_reason": "end_turn",
            "content": [{"type": "thinking", "thinking": "..."}, {"type": "text", "text": "{}"}]
        }))
        .unwrap();
        assert_eq!(api_reply_text(reply).unwrap(), "{}");
        let refused: ApiResponse =
            serde_json::from_value(json!({"stop_reason": "refusal", "content": []})).unwrap();
        assert!(api_reply_text(refused).unwrap_err().to_string().contains("거절"));
    }

    #[test]
    fn cli_envelope_prefers_structured_output() {
        assert_eq!(cli_reply_text(r#"{"structured_output":{"a":1},"result":"x"}"#).unwrap(), r#"{"a":1}"#);
        assert_eq!(cli_reply_text(r#"{"result":"{\"a\":2}"}"#).unwrap(), r#"{"a":2}"#);
        assert!(cli_reply_text(r#"{"is_error":true,"result":"no login"}"#).is_err());
    }

    /// On Windows `claude` may be a batch file, which cannot take multi-line arguments.
    #[test]
    fn cli_backend_works_with_a_multi_line_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let program = if cfg!(windows) {
            let p = dir.path().join("claude.cmd");
            std::fs::write(&p, "@echo off\r\nmore > nul\r\necho {\"structured_output\":{\"ok\":true}}\r\n").unwrap();
            p
        } else {
            let p = dir.path().join("claude");
            std::fs::write(&p, "#!/bin/sh\ncat > /dev/null\necho '{\"structured_output\":{\"ok\":true}}'\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            p
        };
        let schema = serde_json::json!({ "type": "object", "properties": { "ok": { "type": "boolean" } } });
        let reply = complete_cli(&program, "first line\nsecond line with \"quotes\" and 100%", "prompt\nbody", &schema).unwrap();
        assert_eq!(reply, r#"{"ok":true}"#);
    }

    /// A `claude` that never answers is stopped after the timeout instead of hanging the job.
    #[test]
    fn cli_backend_is_killed_after_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let program = if cfg!(windows) {
            let p = dir.path().join("claude.cmd");
            std::fs::write(&p, "@echo off\r\n\"%SystemRoot%\\System32\\waitfor.exe\" /t 60 ytdocneversignalled > nul 2>&1\r\necho {\"structured_output\":{}}\r\n").unwrap();
            p
        } else {
            let p = dir.path().join("claude");
            std::fs::write(&p, "#!/bin/sh\nsleep 60\necho '{\"structured_output\":{}}'\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            p
        };
        let started = std::time::Instant::now();
        let err = complete_cli_with_timeout(&program, "s", "p", &json!({}), Duration::from_secs(1)).unwrap_err();
        assert!(err.to_string().contains("1초 안에 끝나지 않아"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(20), "took {:?}", started.elapsed());
    }

    #[test]
    fn only_rate_limits_overloads_and_server_errors_are_retried() {
        for (code, retry) in [(429, true), (529, true), (500, true), (503, true), (400, false), (401, false), (404, false)] {
            assert_eq!(is_retryable(reqwest::StatusCode::from_u16(code).unwrap()), retry, "{code}");
        }
    }

    #[test]
    fn backoff_grows_is_capped_and_honours_retry_after() {
        assert_eq!(backoff_delay(0, None, 1.0), Duration::from_secs(2));
        assert_eq!(backoff_delay(2, None, 1.0), Duration::from_secs(8));
        assert_eq!(backoff_delay(10, None, 1.0), API_BACKOFF_MAX);
        assert_eq!(backoff_delay(3, None, 0.5), Duration::from_secs(8));
        assert_eq!(backoff_delay(0, None, 0.0), Duration::ZERO);
        // The server's retry-after wins over a shorter backoff, but is capped.
        assert_eq!(backoff_delay(0, Some(Duration::from_secs(30)), 1.0), Duration::from_secs(30));
        assert_eq!(backoff_delay(0, Some(Duration::from_secs(3600)), 1.0), API_RETRY_AFTER_MAX);
        for _ in 0..100 {
            let j = jitter();
            assert!((0.0..1.0).contains(&j), "{j}");
        }
    }

    #[test]
    fn retry_after_header_is_read_in_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(reqwest::header::RETRY_AFTER, "12".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(12)));
        headers.insert(reqwest::header::RETRY_AFTER, "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn transient_errors_are_retried_until_success() {
        let mut answers = vec![
            Attempt::Retry { error: anyhow::anyhow!("529 overloaded"), retry_after: None },
            Attempt::Retry { error: anyhow::anyhow!("429"), retry_after: Some(Duration::from_secs(7)) },
            Attempt::Done("ok"),
        ]
        .into_iter();
        let mut slept = vec![];
        let result = with_retries(4, || answers.next().unwrap(), |d| slept.push(d), || 1.0).unwrap();
        assert_eq!(result, "ok");
        assert_eq!(slept, vec![Duration::from_secs(2), Duration::from_secs(7)]);
    }

    #[test]
    fn retries_are_bounded_and_fatal_errors_are_not_retried() {
        let mut tries = 0;
        let mut slept = 0;
        let err = with_retries::<()>(
            3,
            || {
                tries += 1;
                Attempt::Retry { error: anyhow::anyhow!("500"), retry_after: None }
            },
            |_| slept += 1,
            || 0.0,
        )
        .unwrap_err();
        assert_eq!((tries, slept), (4, 3));
        assert!(format!("{err:#}").contains("500"), "{err:#}");

        let mut tries = 0;
        let err = with_retries::<()>(
            3,
            || {
                tries += 1;
                Attempt::Fatal(anyhow::anyhow!("400 bad request"))
            },
            |_| panic!("must not sleep"),
            || 0.0,
        )
        .unwrap_err();
        assert_eq!(tries, 1);
        assert!(err.to_string().contains("400"));
    }
}
