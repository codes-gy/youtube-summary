use std::fmt::Write;

use anyhow::Result;

use crate::claude::{Llm, ask};
use crate::domain::{Summary, Transcript, VideoMeta, clock};
use crate::lang::LangCode;

const SYSTEM_PROMPT: &str = "You turn video transcripts into concise, well-structured study documents.
Write every human-readable field in {language}, even when the transcript is in another language.
Split the video into 3-8 consecutive chapters that follow topic changes. Chapter times are in seconds,
taken from the transcript timestamps; together the chapters cover the video from start to end without gaps.
Be faithful to what is said. Do not invent facts that are not in the transcript.";

pub fn summarize(llm: &dyn Llm, meta: &VideoMeta, transcript: &Transcript, lang: &LangCode) -> Result<Summary> {
    let system = SYSTEM_PROMPT.replace("{language}", &lang.english_name());
    ask(llm, &system, &build_prompt(meta, transcript), Summary::check)
}

fn build_prompt(meta: &VideoMeta, transcript: &Transcript) -> String {
    let mut prompt = format!("Video title: {}\n", meta.title);
    if let Some(channel) = &meta.channel {
        let _ = writeln!(prompt, "Channel: {channel}");
    }
    let _ = write!(
        prompt,
        "Duration: {:.0} seconds ({})\nSpoken language detected: {}\n\nTranscript, one line per segment, with start time in seconds:\n",
        meta.duration,
        clock(meta.duration),
        transcript.language
    );
    for seg in &transcript.segments {
        let _ = writeln!(prompt, "[{:.1}s] {}", seg.start, seg.text);
    }
    prompt
}
