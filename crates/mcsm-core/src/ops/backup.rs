//! World backups: `tar --zstd` archives of the level directory (and its
//! `_nether` / `_the_end` siblings if a setup keeps them separate).
//!
//! `zstd` compresses fast enough to run inline and Minecraft worlds are highly
//! compressible, so we shell out to the system `tar`, which every Arch install
//! has, rather than pulling in a tar+zstd crate stack.
//!
//! Restoring requires the server to be stopped — extracting over a live world
//! corrupts region files. The caller is responsible for that ordering.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tokio::process::Command;

use crate::error::{Error, Result};
use crate::paths::Paths;
use crate::properties::Properties;
use crate::util::{dir_size, format_compact_utc, read_to_string_opt};

/// One archive in the `backups/` directory.
#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub file_name: String,
    pub created: SystemTime,
    pub size_bytes: u64,
}

/// The level directories that exist for `level_name`, relative to the server dir.
fn world_dirs(server_dir: &Path, level_name: &str) -> Vec<String> {
    [
        level_name.to_string(),
        format!("{level_name}_nether"),
        format!("{level_name}_the_end"),
    ]
    .into_iter()
    .filter(|name| server_dir.join(name).is_dir())
    .collect()
}

/// The world directory name from `server.properties` (`level-name`), or
/// `"world"` if it is unset or the file does not exist yet.
#[must_use]
pub fn level_name(paths: &Paths) -> String {
    read_to_string_opt(&paths.server_file("server.properties"))
        .ok()
        .flatten()
        .and_then(|text| Properties::parse(&text).get("level-name").map(str::to_string))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "world".to_string())
}

/// Uncompressed size of the world on disk, for a pre-flight space check.
pub fn world_size_bytes(paths: &Paths, level_name: &str) -> Result<u64> {
    let mut total = 0;
    for name in world_dirs(&paths.server, level_name) {
        total += dir_size(&paths.server.join(name))?;
    }
    Ok(total)
}

/// Filename prefix for automatic backups, so retention can target them without
/// ever touching a manual backup.
const AUTO_PREFIX: &str = "auto-world-";
const MANUAL_PREFIX: &str = "world-";

impl BackupEntry {
    /// Whether this archive was made by the automatic-backup timer.
    #[must_use]
    pub fn is_automatic(&self) -> bool {
        self.file_name.starts_with(AUTO_PREFIX)
    }
}

/// Existing backups in `dir`, newest first. A missing directory is empty.
pub fn list(dir: &Path) -> Result<Vec<BackupEntry>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".tar.zst") {
            continue;
        }
        let meta = entry.metadata().map_err(|e| Error::io(entry.path(), e))?;
        entries.push(BackupEntry {
            path: entry.path(),
            file_name: name,
            created: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size_bytes: meta.len(),
        });
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.created));
    Ok(entries)
}

/// Create a new archive of the current world into `backup_dir`.
///
/// Callers that back up a *running* server should send `save-all flush` and
/// wait a moment first; this function only archives what is on disk. `auto`
/// marks the archive as one made by the automatic-backup timer (see
/// [`prune_auto`]).
pub async fn create(
    paths: &Paths,
    backup_dir: &Path,
    level_name: &str,
    auto: bool,
) -> Result<BackupEntry> {
    std::fs::create_dir_all(backup_dir).map_err(|e| Error::io(backup_dir, e))?;
    let dirs = world_dirs(&paths.server, level_name);
    if dirs.is_empty() {
        return Err(Error::msg(format!(
            "no world directory named `{level_name}` to back up"
        )));
    }

    let prefix = if auto { AUTO_PREFIX } else { MANUAL_PREFIX };
    let file_name = format!("{prefix}{}.tar.zst", format_compact_utc(SystemTime::now()));
    let archive = backup_dir.join(&file_name);
    let partial = archive.with_extension("zst.part");

    let mut cmd = Command::new("tar");
    cmd.arg("--zstd")
        .arg("-cf")
        .arg(&partial)
        .arg("-C")
        .arg(&paths.server);
    for dir in &dirs {
        cmd.arg(dir);
    }
    run(cmd, "tar (create backup)").await?;

    std::fs::rename(&partial, &archive).map_err(|e| Error::io(&archive, e))?;
    let meta = std::fs::metadata(&archive).map_err(|e| Error::io(&archive, e))?;
    Ok(BackupEntry {
        path: archive,
        file_name,
        created: meta.modified().unwrap_or_else(|_| SystemTime::now()),
        size_bytes: meta.len(),
    })
}

/// Restore a backup over the current world. **The server must be stopped.**
///
/// The current world directories are moved aside to `<name>.pre-restore`
/// (replacing any previous such copy) before extraction, so a bad restore is
/// recoverable.
pub async fn restore(paths: &Paths, backup: &BackupEntry, level_name: &str) -> Result<()> {
    if !backup.path.is_file() {
        return Err(Error::msg(format!(
            "backup {} is missing",
            backup.file_name
        )));
    }

    for name in [
        level_name.to_string(),
        format!("{level_name}_nether"),
        format!("{level_name}_the_end"),
    ] {
        let live = paths.server.join(&name);
        if live.is_dir() {
            let stash = paths.server.join(format!("{name}.pre-restore"));
            if stash.exists() {
                std::fs::remove_dir_all(&stash).map_err(|e| Error::io(&stash, e))?;
            }
            std::fs::rename(&live, &stash).map_err(|e| Error::io(&live, e))?;
        }
    }

    let mut cmd = Command::new("tar");
    cmd.arg("--zstd")
        .arg("-xf")
        .arg(&backup.path)
        .arg("-C")
        .arg(&paths.server);
    run(cmd, "tar (restore backup)").await
}

/// Permanently delete one backup archive.
pub fn delete(entry: &BackupEntry) -> Result<()> {
    match std::fs::remove_file(&entry.path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(&entry.path, e)),
    }
}

/// Delete the oldest automatic backups in `dir`, keeping the `keep` most recent.
///
/// `keep == 0` keeps everything. Manual backups are never touched. Returns how
/// many files were removed.
pub fn prune_auto(dir: &Path, keep: usize) -> Result<usize> {
    if keep == 0 {
        return Ok(0);
    }
    let mut removed = 0;
    for stale in list(dir)?
        .into_iter()
        .filter(BackupEntry::is_automatic)
        .skip(keep)
    {
        delete(&stale)?;
        removed += 1;
    }
    Ok(removed)
}

async fn run(mut cmd: Command, what: &str) -> Result<()> {
    let output = cmd.output().await.map_err(Error::IoBare)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(Error::msg(format!(
        "{what} failed ({}): {}",
        output.status,
        stderr.trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_dirs_only_lists_existing() {
        let root = std::env::temp_dir().join(format!("mcsm-bk-{}", std::process::id()));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        std::fs::create_dir_all(paths.server.join("world")).unwrap();
        std::fs::create_dir_all(paths.server.join("world_the_end")).unwrap();

        let dirs = world_dirs(&paths.server, "world");
        assert!(dirs.contains(&"world".to_string()));
        assert!(dirs.contains(&"world_the_end".to_string()));
        assert!(!dirs.contains(&"world_nether".to_string()));

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn create_then_list_then_restore_roundtrip() {
        let root = std::env::temp_dir().join(format!(
            "mcsm-bk-rt-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        let region = paths.server.join("world").join("region");
        std::fs::create_dir_all(&region).unwrap();
        std::fs::write(region.join("r.0.0.mca"), b"chunkdata").unwrap();

        let bdir = paths.backups.clone();
        let entry = create(&paths, &bdir, "world", false).await.unwrap();
        assert!(entry.path.is_file());
        assert!(!entry.is_automatic());
        assert_eq!(list(&bdir).unwrap().len(), 1);

        std::fs::write(region.join("r.0.0.mca"), b"CORRUPTED").unwrap();
        restore(&paths, &entry, "world").await.unwrap();
        let restored = std::fs::read(region.join("r.0.0.mca")).unwrap();
        assert_eq!(restored, b"chunkdata");
        assert!(paths.server.join("world.pre-restore").is_dir());

        std::fs::remove_dir_all(&root).ok();
    }

    fn touch_backup(dir: &std::path::Path, name: &str, mtime_offset_secs: u64) {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let when = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(1_700_000_000 + mtime_offset_secs);
        file.set_modified(when).unwrap();
    }

    #[test]
    fn delete_removes_the_file_and_tolerates_a_missing_one() {
        let root = std::env::temp_dir().join(format!("mcsm-bk-del-{}", std::process::id()));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        touch_backup(&paths.backups, "world-20260101-000000.tar.zst", 0);

        let entry = list(&paths.backups).unwrap().pop().unwrap();
        delete(&entry).unwrap();
        assert!(list(&paths.backups).unwrap().is_empty());
        delete(&entry).unwrap(); // second time is a no-op

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prune_auto_keeps_newest_and_spares_manual_backups() {
        let root = std::env::temp_dir().join(format!("mcsm-bk-prune-{}", std::process::id()));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();

        for i in 0..5 {
            touch_backup(&paths.backups, &format!("auto-world-2026010{}-000000.tar.zst", i + 1), i * 100);
        }
        touch_backup(&paths.backups, "world-20260101-120000.tar.zst", 50);

        let removed = prune_auto(&paths.backups, 2).unwrap();
        assert_eq!(removed, 3);

        let remaining: Vec<String> = list(&paths.backups).unwrap().into_iter().map(|e| e.file_name).collect();
        assert_eq!(remaining.iter().filter(|n| n.starts_with("auto-world-")).count(), 2);
        assert!(remaining.iter().any(|n| n == "world-20260101-120000.tar.zst"));
        // keep == 0 is a no-op
        assert_eq!(prune_auto(&paths.backups, 0).unwrap(), 0);

        std::fs::remove_dir_all(&root).ok();
    }
}
