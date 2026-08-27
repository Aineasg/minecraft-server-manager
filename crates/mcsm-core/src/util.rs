//! Small filesystem and formatting helpers used across the crate.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

/// Write `bytes` to `path` atomically: write a sibling temp file, `fsync`, then
/// rename over the target. A crash mid-write can never leave a half-written
/// `server.properties` or `state.toml`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;

    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));

    let mut file = std::fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
    file.write_all(bytes).map_err(|e| Error::io(&tmp, e))?;
    file.sync_all().map_err(|e| Error::io(&tmp, e))?;
    drop(file);

    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
}

/// Whether `dir` can be created (when missing) and written into.
///
/// Probes by creating and deleting a marker file rather than trusting the
/// directory's permission bits: on Unix those miss a read-only mount, a path
/// owned by someone else, an unmounted drive, or a `dir` that is actually a
/// regular file. Used to decide whether a configured backup folder is still
/// usable before every backup, so a path saved on another machine (or one
/// whose drive is absent) falls back instead of failing silently.
#[must_use]
pub fn dir_is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".mcsm-write-test");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Read a file to a string, returning `Ok(None)` if it does not exist.
pub fn read_to_string_opt(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Recursively sum the size of every regular file under `dir`. A missing
/// directory is zero, not an error.
pub fn dir_size(dir: &Path) -> Result<u64> {
    if !dir.exists() {
        return Ok(0);
    }
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).map_err(|e| Error::io(&current, e))? {
            let entry = entry.map_err(|e| Error::io(&current, e))?;
            let file_type = entry.file_type().map_err(|e| Error::io(entry.path(), e))?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    Ok(total)
}

/// Format a `SystemTime` as `YYYYMMDD-HHMMSS` in UTC, for use in filenames.
///
/// Uses Howard Hinnant's `days_from_civil` inverse; no timezone database or
/// external crate needed.
#[must_use]
pub fn format_compact_utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_is_writable_creates_missing_dirs_and_probes() {
        let root = std::env::temp_dir().join(format!("mcsm-wr-{}", std::process::id()));
        let nested = root.join("a").join("b");
        assert!(!nested.exists());
        assert!(dir_is_writable(&nested), "should create the tree and pass");
        assert!(nested.is_dir());
        // The probe file must not be left behind.
        assert!(!nested.join(".mcsm-write-test").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dir_is_writable_is_false_when_the_path_is_a_file() {
        let root = std::env::temp_dir().join(format!("mcsm-wr-file-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let as_file = root.join("not-a-dir");
        std::fs::write(&as_file, b"x").unwrap();

        assert!(!dir_is_writable(&as_file));

        std::fs::remove_dir_all(&root).ok();
    }
}
