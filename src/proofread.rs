//! Corrects speech-recognition slips in the source transcript, before it is translated or summarized.

use std::sync::Mutex;

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::claude::{Llm, ask};
use crate::domain::{Segment, Transcript};
use crate::translate::{Line, MAX_CHARS, MAX_SEGMENTS, batches, run_parallel};

const SYSTEM_PROMPT: &str = "You correct speech-recognition errors in a video transcript in {language}.
The input is a JSON list of lines, each with an index `i` and its `text`. Lines are consecutive speech,
so use neighbouring lines to tell what was meant.
Fix only words or terms that were clearly misheard, such as a Korean transcript giving \"대역복\" for \"대역폭\",
\"무혈성\" for \"무결성\", or \"제 요청\" for \"재요청\" when the context means a re-request.
Keep the wording, spacing, punctuation and tone of everything else. Never summarize, merge, split or reorder lines,
and never rewrite a line that is already correct.
Return only the lines you changed, each with its `index` (the line's `i`) and the full corrected `text`.
Return an empty list when nothing needs fixing.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fix {
    /// The `i` of the corrected line.
    pub index: usize,
    /// The full corrected text of that line.
    pub text: String,
}

/// The changed segments only; this is what the stage caches.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct TranscriptFixes {
    pub fixes: Vec<Fix>,
}

/// Ask for corrections to each batch of segments.
pub fn find_fixes(llm: &dyn Llm, transcript: &Transcript) -> Result<TranscriptFixes> {
    let language = transcript.language.parse::<crate::lang::LangCode>().map_or_else(|_| transcript.language.clone(), |l| l.english_name());
    let system = SYSTEM_PROMPT.replace("{language}", &language);
    let segments = &transcript.segments;
    let found: Mutex<Vec<Fix>> = Mutex::new(vec![]);
    run_parallel(&batches(segments, MAX_SEGMENTS, MAX_CHARS), |indices| {
        let input: Vec<Line> = indices.iter().map(|&i| Line { i, text: segments[i].text.clone() }).collect();
        let answer: TranscriptFixes = ask(llm, &system, &serde_json::to_string(&input)?, |_| Ok(()))?;
        let mut found = found.lock().unwrap();
        found.extend(answer.fixes.into_iter().filter(|f| indices.contains(&f.index)));
        Ok(())
    })?;
    let mut fixes = found.into_inner().unwrap();
    fixes.sort_by_key(|f| f.index);
    fixes.dedup_by_key(|f| f.index);
    Ok(TranscriptFixes { fixes })
}

/// The transcript with each fix applied by segment index; out-of-range indexes and blank texts are ignored.
pub fn apply(transcript: &Transcript, fixes: &TranscriptFixes) -> Transcript {
    let mut segments = transcript.segments.clone();
    for fix in fixes.fixes.iter().filter(|f| !f.text.trim().is_empty()) {
        if let Some(seg) = segments.get_mut(fix.index) {
            *seg = Segment { text: fix.text.trim().to_string(), ..seg.clone() };
        }
    }
    Transcript { language: transcript.language.clone(), segments }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::tests::Scripted;

    fn transcript(texts: &[&str]) -> Transcript {
        let segments = texts.iter().enumerate().map(|(i, t)| Segment { start: i as f64, end: i as f64 + 1.0, text: (*t).into() }).collect();
        Transcript { language: "ko".into(), segments }
    }

    #[test]
    fn corrects_only_the_lines_the_model_returns() {
        let t = transcript(&["대역복이 넓다", "그대로 둔다", "제 요청을 보낸다"]);
        let llm = Scripted::new(&[r#"{"fixes":[{"index":0,"text":"대역폭이 넓다"},{"index":2,"text":"재요청을 보낸다"}]}"#]);
        let fixes = find_fixes(&llm, &t).unwrap();
        assert_eq!(fixes.fixes.len(), 2);
        let fixed = apply(&t, &fixes);
        let texts: Vec<&str> = fixed.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["대역폭이 넓다", "그대로 둔다", "재요청을 보낸다"]);
        assert_eq!(fixed.segments[2].start, 2.0, "timestamps are kept");
        assert!(llm.prompts.lock().unwrap()[0].contains(r#""i":2"#));
    }

    #[test]
    fn ignores_out_of_range_and_blank_fixes() {
        let t = transcript(&["하나", "둘"]);
        let fixes = TranscriptFixes {
            fixes: vec![Fix { index: 9, text: "x".into() }, Fix { index: 0, text: " ".into() }, Fix { index: 1, text: "둘!".into() }],
        };
        assert_eq!(apply(&t, &fixes), transcript(&["하나", "둘!"]));
    }

    #[test]
    fn drops_fixes_for_lines_outside_the_batch() {
        let t = transcript(&["하나"]);
        let llm = Scripted::new(&[r#"{"fixes":[{"index":5,"text":"x"}]}"#]);
        assert!(find_fixes(&llm, &t).unwrap().fixes.is_empty());
    }

    #[test]
    fn schema_is_closed_with_both_fields_required() {
        let schema = crate::claude::schema_for::<TranscriptFixes>();
        let fix = &schema["properties"]["fixes"]["items"];
        assert_eq!(fix["additionalProperties"], false);
        assert_eq!(fix["required"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn an_empty_answer_changes_nothing() {
        let t = transcript(&["하나"]);
        let llm = Scripted::new(&[r#"{"fixes":[]}"#]);
        assert_eq!(apply(&t, &find_fixes(&llm, &t).unwrap()), t);
    }
}
