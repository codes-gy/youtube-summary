use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::claude::{Llm, ask};
use crate::domain::{Segment, Transcript};
use crate::lang::LangCode;

pub const MAX_SEGMENTS: usize = 150;
/// Keeps each answer well under the output limit even for verbose target languages.
pub const MAX_CHARS: usize = 6000;
const PARALLEL_CALLS: usize = 4;

const SYSTEM_PROMPT: &str = "You translate video transcripts from {source} into {target}.
The input is a JSON list of lines, each with an index `i` and its `text`.
Return every line exactly once, with the same `i`, translated into {target}.
Do not merge, split, skip, or reorder lines, and add no commentary. Lines are consecutive speech,
so use neighbouring lines for context, but keep each translation on its own line.";

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Line {
    pub i: usize,
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TranslatedBatch {
    pub lines: Vec<Line>,
}

/// Group consecutive segment indices so each call stays small. An oversized segment gets its own batch.
pub fn batches(segments: &[Segment], max_segments: usize, max_chars: usize) -> Vec<Vec<usize>> {
    let mut groups = vec![];
    let mut current: Vec<usize> = vec![];
    let mut chars = 0;
    for (i, seg) in segments.iter().enumerate() {
        let len = seg.text.chars().count();
        if !current.is_empty() && (current.len() >= max_segments || chars + len > max_chars) {
            groups.push(std::mem::take(&mut current));
            chars = 0;
        }
        current.push(i);
        chars += len;
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

/// Every requested index exactly once, nothing extra, nothing blank.
pub fn check_batch(expected: &[usize], answer: &TranslatedBatch) -> Result<(), String> {
    let mut seen: HashMap<usize, usize> = HashMap::new();
    for line in &answer.lines {
        *seen.entry(line.i).or_default() += 1;
    }
    let missing: Vec<usize> = expected.iter().copied().filter(|i| !seen.contains_key(i)).collect();
    let mut extra: Vec<usize> = seen.keys().copied().filter(|i| !expected.contains(i)).collect();
    let mut duplicated: Vec<usize> = seen.iter().filter(|(_, n)| **n > 1).map(|(i, _)| *i).collect();
    let blank: Vec<usize> = answer.lines.iter().filter(|l| l.text.trim().is_empty()).map(|l| l.i).collect();
    extra.sort_unstable();
    duplicated.sort_unstable();
    let problems: Vec<String> = [
        ("missing i", missing),
        ("unexpected i", extra),
        ("duplicated i", duplicated),
        ("empty text for i", blank),
    ]
    .into_iter()
    .filter(|(_, ids)| !ids.is_empty())
    .map(|(name, ids)| format!("{name}: {ids:?}"))
    .collect();
    if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
}

/// Run `work` on every group, a few at a time, stopping at the first error.
pub fn run_parallel(groups: &[Vec<usize>], work: impl Fn(&[usize]) -> Result<()> + Sync) -> Result<()> {
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..PARALLEL_CALLS.min(groups.len()))
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    while let Some(group) = groups.get(next.fetch_add(1, Ordering::Relaxed)) {
                        work(group)?;
                    }
                    Ok(())
                })
            })
            .collect();
        workers.into_iter().try_for_each(|w| w.join().expect("worker panicked"))
    })
}

/// Translate each segment's text into `target`, keeping the original timestamps.
pub fn translate(llm: &dyn Llm, transcript: &Transcript, target: &LangCode) -> Result<Transcript> {
    let source = transcript.language.parse::<LangCode>().map_or_else(|_| transcript.language.clone(), |l| l.english_name());
    let system = SYSTEM_PROMPT
        .replace("{source}", &source)
        .replace("{target}", &target.english_name());
    let segments = &transcript.segments;
    let groups = batches(segments, MAX_SEGMENTS, MAX_CHARS);
    let texts: Mutex<HashMap<usize, String>> = Mutex::new(HashMap::new());

    let run_batch = |indices: &[usize]| -> Result<()> {
        let input: Vec<Line> = indices.iter().map(|&i| Line { i, text: segments[i].text.clone() }).collect();
        let answer: TranslatedBatch = ask(llm, &system, &serde_json::to_string(&input)?, |a| check_batch(indices, a))?;
        let mut texts = texts.lock().unwrap();
        texts.extend(answer.lines.into_iter().map(|l| (l.i, l.text.trim().to_string())));
        Ok(())
    };
    run_parallel(&groups, run_batch)?;

    let mut texts = texts.into_inner().unwrap();
    let segments = segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            Ok(Segment { text: texts.remove(&i).context("번역 결과에 빠진 줄이 있습니다")?, ..seg.clone() })
        })
        .collect::<Result<_>>()?;
    Ok(Transcript { language: target.to_string(), segments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::tests::Scripted;

    fn seg(text: &str) -> Segment {
        Segment { start: 0.0, end: 1.0, text: text.into() }
    }

    fn batch(lines: &[(usize, &str)]) -> TranslatedBatch {
        TranslatedBatch { lines: lines.iter().map(|&(i, t)| Line { i, text: t.into() }).collect() }
    }

    #[test]
    fn batches_split_on_count_and_size() {
        let segs: Vec<Segment> = ["aaaa", "bb", "cc", "dddddddd", "e"].iter().map(|t| seg(t)).collect();
        assert_eq!(batches(&segs, 2, 100), vec![vec![0, 1], vec![2, 3], vec![4]]);
        assert_eq!(batches(&segs, 10, 6), vec![vec![0, 1], vec![2], vec![3], vec![4]]);
        assert!(batches(&[], 10, 10).is_empty());
    }

    #[test]
    fn check_batch_names_every_problem() {
        assert!(check_batch(&[0, 1], &batch(&[(1, "b"), (0, "a")])).is_ok());
        let err = check_batch(&[0, 1, 2], &batch(&[(0, "a"), (0, "a"), (5, "x"), (2, " ")])).unwrap_err();
        assert_eq!(err, "missing i: [1]; unexpected i: [5]; duplicated i: [0]; empty text for i: [2]");
    }

    #[test]
    fn retries_a_bad_batch_and_keeps_timestamps() {
        let transcript = Transcript {
            language: "ko".into(),
            segments: vec![
                Segment { start: 0.0, end: 2.0, text: "안녕하세요".into() },
                Segment { start: 2.0, end: 5.0, text: "반갑습니다".into() },
            ],
        };
        let llm = Scripted::new(&[
            r#"{"lines":[{"i":0,"text":"Hello"}]}"#,
            r#"{"lines":[{"i":0,"text":"Hello"},{"i":1,"text":" Nice to meet you "}]}"#,
        ]);
        let out = translate(&llm, &transcript, &"en".parse().unwrap()).unwrap();
        assert_eq!(out.language, "en");
        assert_eq!(out.segments[1], Segment { start: 2.0, end: 5.0, text: "Nice to meet you".into() });
        assert!(llm.prompts.lock().unwrap()[1].contains("missing i: [1]"));
    }

    #[test]
    fn fails_when_a_batch_stays_bad() {
        let transcript = Transcript { language: "ko".into(), segments: vec![seg("하나")] };
        let llm = Scripted::new(&[r#"{"lines":[]}"#, r#"{"lines":[{"i":0,"text":""}]}"#]);
        assert!(translate(&llm, &transcript, &"en".parse().unwrap()).is_err());
    }
}
