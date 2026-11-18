use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Return the stage result stored at `path`, computing and saving it if absent.
///
/// The JSON file is the stage's completion marker. It is written via rename, so a
/// crash mid-stage leaves no file and the next run redoes the stage.
pub fn cached<T, F>(path: &Path, compute: F) -> Result<(T, bool)>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Result<T>,
{
    if let Ok(text) = fs::read_to_string(path) {
        if let Ok(value) = serde_json::from_str(&text) {
            return Ok((value, true));
        }
    }
    let value = compute()?;
    write_atomic(path, serde_json::to_string_pretty(&value)?.as_bytes())?;
    Ok((value, false))
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).with_context(|| format!("{} 저장 실패", tmp.display()))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn computes_once_then_reads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stage.json");
        let calls = Cell::new(0);
        let compute = || {
            calls.set(calls.get() + 1);
            Ok(vec![1, 2, 3])
        };
        assert_eq!(cached(&path, compute).unwrap(), (vec![1, 2, 3], false));
        assert_eq!(cached(&path, compute).unwrap(), (vec![1, 2, 3], true));
        assert_eq!(calls.get(), 1);
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn failed_stage_leaves_no_marker_and_corrupt_marker_is_redone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stage.json");
        assert!(cached::<Vec<i32>, _>(&path, || anyhow::bail!("boom")).is_err());
        assert!(!path.exists());
        fs::write(&path, "{ half written").unwrap();
        assert_eq!(cached(&path, || Ok(vec![7])).unwrap(), (vec![7], false));
    }
}
