use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoMeta {
    pub title: String,
    pub channel: Option<String>,
    pub duration: f64,
    /// YYYYMMDD, as yt-dlp reports it.
    pub upload_date: Option<String>,
    pub origin: Origin,
    /// False for audio-only files, which get a document without scene images.
    #[serde(default = "yes")]
    pub has_video: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    YouTube { id: String },
    LocalFile { file_name: String },
}

impl VideoMeta {
    /// The page a reader can open; local files have none.
    pub fn watch_url(&self) -> Option<String> {
        match &self.origin {
            Origin::YouTube { id } => Some(format!("https://www.youtube.com/watch?v={id}")),
            Origin::LocalFile { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// Whisper language code of `segments`, e.g. "ko".
    pub language: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub time: f64,
    /// Relative to the cache work dir, so the HTML next to it can reference it.
    pub path: String,
    /// 64-bit difference hash, used to drop near-duplicates.
    pub hash: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameSet {
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Chapter {
    /// Chapter start, seconds from video start
    pub start: f64,
    /// Chapter end, seconds from video start
    pub end: f64,
    pub title: String,
    /// 2-5 sentence summary of this chapter
    pub summary: String,
    pub key_points: Vec<String>,
}

/// What the summarizer must return. Also the JSON schema sent to Claude.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    /// Document title in the output language
    pub title: String,
    /// One-sentence summary of the whole video
    pub one_line: String,
    /// One or two paragraph overview
    pub overview: String,
    /// 3-7 most important takeaways
    pub key_points: Vec<String>,
    /// Consecutive chapters covering the video in order
    pub chapters: Vec<Chapter>,
}

impl Summary {
    /// The rules the schema cannot express.
    pub fn check(&self) -> Result<(), String> {
        if self.chapters.is_empty() {
            return Err("summary needs at least one chapter".into());
        }
        for c in &self.chapters {
            if c.end <= c.start {
                return Err(format!(
                    "chapter \"{}\": end {} must be after start {}",
                    c.title, c.end, c.start
                ));
            }
        }
        if self.chapters.windows(2).any(|w| w[1].start < w[0].start) {
            return Err("chapters must be in chronological order".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Section {
    pub chapter: Chapter,
    pub frames: Vec<Frame>,
}

/// Give each chapter the frames inside its time range, thinned to an even spread.
pub fn assign_frames(chapters: &[Chapter], frames: &[Frame], max_per_chapter: usize) -> Vec<Section> {
    chapters
        .iter()
        .enumerate()
        .map(|(i, chapter)| {
            let is_last = i + 1 == chapters.len();
            let inside: Vec<&Frame> = frames
                .iter()
                .filter(|f| f.time >= chapter.start && (f.time < chapter.end || is_last))
                .collect();
            Section {
                chapter: chapter.clone(),
                frames: spread(&inside, max_per_chapter).into_iter().cloned().collect(),
            }
        })
        .collect()
}

/// Pick `n` items evenly spaced across the slice, keeping order.
pub fn spread<T: Copy>(items: &[T], n: usize) -> Vec<T> {
    if items.len() <= n {
        return items.to_vec();
    }
    let step = items.len() as f64 / n as f64;
    (0..n).map(|i| items[(step * i as f64 + step / 2.0) as usize]).collect()
}

/// Seconds as M:SS, or H:MM:SS for an hour or more.
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(start: f64, end: f64) -> Chapter {
        Chapter {
            start,
            end,
            title: "t".into(),
            summary: "s".into(),
            key_points: vec![],
        }
    }

    fn frame(time: f64) -> Frame {
        Frame { time, path: format!("frames/{time}.jpg"), hash: 0 }
    }

    fn summary(chapters: Vec<Chapter>) -> Summary {
        Summary {
            title: "t".into(),
            one_line: "o".into(),
            overview: "v".into(),
            key_points: vec![],
            chapters,
        }
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(65.9), "1:05");
        assert_eq!(clock(370.0), "6:10");
        assert_eq!(clock(3600.0), "1:00:00");
        assert_eq!(clock(3725.0), "1:02:05");
    }

    #[test]
    fn frames_go_to_the_chapter_containing_them() {
        let chapters = [chapter(0.0, 10.0), chapter(10.0, 20.0)];
        let frames = [frame(1.0), frame(10.0), frame(19.0), frame(25.0)];
        let sections = assign_frames(&chapters, &frames, 5);
        let times: Vec<Vec<f64>> = sections
            .iter()
            .map(|s| s.frames.iter().map(|f| f.time).collect())
            .collect();
        // A frame past the last chapter's end still lands in the last chapter.
        assert_eq!(times, vec![vec![1.0], vec![10.0, 19.0, 25.0]]);
    }

    #[test]
    fn max_images_picks_an_even_spread() {
        let chapters = [chapter(0.0, 100.0)];
        let frames: Vec<Frame> = (0..10).map(|i| frame(i as f64 * 10.0)).collect();
        let sections = assign_frames(&chapters, &frames, 2);
        let times: Vec<f64> = sections[0].frames.iter().map(|f| f.time).collect();
        assert_eq!(times, vec![20.0, 70.0]);
        assert!(assign_frames(&chapters, &frames, 0)[0].frames.is_empty());
    }

    #[test]
    fn summary_check_rejects_bad_chapters() {
        assert!(summary(vec![chapter(0.0, 5.0), chapter(5.0, 9.0)]).check().is_ok());
        assert!(summary(vec![]).check().unwrap_err().contains("at least one"));
        assert!(summary(vec![chapter(5.0, 5.0)]).check().unwrap_err().contains("after start"));
        assert!(
            summary(vec![chapter(5.0, 9.0), chapter(0.0, 5.0)])
                .check()
                .unwrap_err()
                .contains("chronological")
        );
    }
}
