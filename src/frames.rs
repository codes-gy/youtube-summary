use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use image::imageops::FilterType;

use crate::domain::Frame;
use crate::media::{run, stderr_tail};

const SCENE_THRESHOLD: f64 = 0.3;
/// Seconds; avoids bursts of frames during transitions.
const MIN_GAP: f64 = 4.0;
/// Seconds; guarantees coverage of static talking-head stretches.
const MAX_GAP: f64 = 30.0;
/// Of 64 bits; at or below this two frames look the same.
pub const MAX_HASH_DISTANCE: u32 = 6;

/// Candidate key frames: scene changes, plus one frame whenever MAX_GAP passes without one.
pub fn extract(video: &Path, work: &Path) -> Result<Vec<Frame>> {
    let dir = work.join("frames");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let select = format!(
        "isnan(prev_selected_t)+gte(t-prev_selected_t,{MAX_GAP})+gt(scene,{SCENE_THRESHOLD})*gte(t-prev_selected_t,{MIN_GAP})"
    );
    let out = run(Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(video)
        .args(["-vf", &format!("scale='min(960,iw)':-2,select='{select}',showinfo")])
        .args(["-fps_mode", "vfr", "-q:v", "4"])
        .arg(dir.join("%04d.jpg")))?;
    if !out.status.success() {
        bail!("ffmpeg로 장면을 뽑지 못했습니다: {}", stderr_tail(&out));
    }
    let times = showinfo_times(&String::from_utf8_lossy(&out.stderr));
    let mut files: Vec<_> = fs::read_dir(&dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    if files.len() != times.len() {
        bail!("ffmpeg가 장면 {}장을 저장했지만 시각은 {}개만 알려 주었습니다", files.len(), times.len());
    }
    let mut frames = Vec::with_capacity(files.len());
    for (time, file) in times.into_iter().zip(files) {
        let img = image::open(&file).with_context(|| format!("{} 읽기 실패", file.display()))?;
        let name = file.file_name().unwrap().to_string_lossy();
        frames.push(Frame { time, path: format!("frames/{name}"), hash: dhash(&img) });
    }
    Ok(drop_near_duplicates(frames, MAX_HASH_DISTANCE))
}

fn showinfo_times(stderr: &str) -> Vec<f64> {
    stderr
        .split("pts_time:")
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Difference hash: shrink to 9x8 grey and record whether each pixel is brighter than its right neighbour.
pub fn dhash(img: &image::DynamicImage) -> u64 {
    let small = img.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let bit = small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0];
            hash = (hash << 1) | u64::from(bit);
        }
    }
    hash
}

/// Keep each frame only if it looks different from every frame already kept.
pub fn drop_near_duplicates(mut frames: Vec<Frame>, max_distance: u32) -> Vec<Frame> {
    frames.sort_by(|a, b| a.time.total_cmp(&b.time));
    let mut kept: Vec<Frame> = vec![];
    for frame in frames {
        if kept.iter().all(|k| (k.hash ^ frame.hash).count_ones() > max_distance) {
            kept.push(frame);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgb, RgbImage};

    fn frame(time: f64, hash: u64) -> Frame {
        Frame { time, path: String::new(), hash }
    }

    fn gradient(width: u32, flip: bool) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(width, width * 9 / 16, |x, _| {
            let v = (x * 255 / width) as u8;
            let v = if flip { 255 - v } else { v };
            Rgb([v, v, v])
        }))
    }

    #[test]
    fn dhash_ignores_scale_but_sees_content() {
        let a = dhash(&gradient(960, false));
        let b = dhash(&gradient(320, false));
        let c = dhash(&gradient(960, true));
        assert!((a ^ b).count_ones() <= MAX_HASH_DISTANCE);
        assert!((a ^ c).count_ones() > MAX_HASH_DISTANCE);
    }

    #[test]
    fn near_duplicates_are_dropped_in_time_order() {
        let frames = vec![frame(30.0, 0b1111), frame(0.0, 0), frame(10.0, 0b11), frame(20.0, u64::MAX)];
        let kept: Vec<f64> = drop_near_duplicates(frames, 2).iter().map(|f| f.time).collect();
        // 10s differs from 0s by 2 bits (duplicate); 30s differs from 0s by 4 bits (kept).
        assert_eq!(kept, vec![0.0, 20.0, 30.0]);
    }

    #[test]
    fn reads_times_from_showinfo() {
        let log = "[Parsed_showinfo_2 @ 0x1] n:   0 pts:      0 pts_time:0       duration:512\n\
                   [Parsed_showinfo_2 @ 0x1] n:   1 pts: 491520 pts_time:32.5    duration:512";
        assert_eq!(showinfo_times(log), vec![0.0, 32.5]);
    }
}
