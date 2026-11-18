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

fn complete_api(key: &str, system: &str, prompt: &str, schema: &Value) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()?;
    let response = client
        .post(API_URL)
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .json(&api_request_body(system, prompt, schema))
        .send()
        .context("Anthropic API에 연결하지 못했습니다")?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or(body);
        bail!("Anthropic API 오류 ({status}): {message}");
    }
    api_reply_text(serde_json::from_str(&body).context("Anthropic API 응답을 읽지 못했습니다")?)
}

fn complete_cli(program: &PathBuf, system: &str, prompt: &str, schema: &Value) -> Result<String> {
    let mut child = Command::new(program)
        .args(["-p", "--output-format", "json", "--json-schema"])
        .arg(schema.to_string())
        .arg("--system-prompt")
        .arg(system)
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
    child.stdin.take().expect("piped stdin").write_all(prompt.as_bytes())?;
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("claude -p 실패 ({}): {}", out.status, tail(if stderr.trim().is_empty() { &stdout } else { &stderr }));
    }
    cli_reply_text(&stdout)
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
        "chapters":[{"start":0,"end":10,"title":"c","summary":"s","key_points":["p"]}]}"#;

    fn ask_summary(llm: &Scripted) -> Result<Summary> {
        ask(llm, "sys", "prompt", Summary::check)
    }

    #[test]
    fn schema_is_closed_and_fully_required() {
        let schema = schema_for::<Summary>();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"].as_array().unwrap().len(), 5);
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
}
