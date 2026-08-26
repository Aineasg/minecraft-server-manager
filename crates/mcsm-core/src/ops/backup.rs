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
use crate::util::{dir_size, format_compact_utc};

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

/// Uncompressed size of the world on disk, for a pre-flight space check.
pub fn world_size_bytes(paths: &Paths, level_name: &str) -> Result<u64> {
    let mut total = 0;
    for name in world_dirs(&paths.server, level_name) {
        total += dir_size(&paths.server.join(name))?;
    }
    Ok(total)
}

/// Existing backups, newest first.
pub fn list(paths: &Paths) -> Result<Vec<BackupEntry>> {
    let dir = &paths.backups;
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

/// Create a new archive of the current world. The server should be stopped or
/// at least idle (a running server may write mid-archive, producing a slightly
/// inconsistent but usually loadable snapshot).
pub async fn create(paths: &Paths, level_name: &str) -> Result<BackupEntry> {
    paths.ensure_dirs()?;
    let dirs = world_dirs(&paths.server, level_name);
    if dirs.is_empty() {
        return Err(Error::msg(format!(
            "no world directory named `{level_name}` to back up"
        )));
    }

    let file_name = format!("world-{}.tar.zst", format_compact_utc(SystemTime::now()));
    let archive = paths.backups.join(&file_name);
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

        let entry = create(&paths, "world").await.unwrap();
        assert!(entry.path.is_file());
        assert_eq!(list(&paths).unwrap().len(), 1);

        std::fs::write(region.join("r.0.0.mca"), b"CORRUPTED").unwrap();
        restore(&paths, &entry, "world").await.unwrap();
        let restored = std::fs::read(region.join("r.0.0.mca")).unwrap();
        assert_eq!(restored, b"chunkdata");
        assert!(paths.server.join("world.pre-restore").is_dir());

        std::fs::remove_dir_all(&root).ok();
    }
}
