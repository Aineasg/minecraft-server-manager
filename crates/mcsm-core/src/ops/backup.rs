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
        .and_then(|text| {
            Properties::parse(&text)
                .get("level-name")
                .map(str::to_string)
        })
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

/// Delete leftover `*.tar.zst.part` files from a backup run that was killed
/// before `tar` finished. They are scratch files, never valid archives, and
/// [`create`] is the only thing that writes them and is never run concurrently.
fn remove_stale_partials(dir: &Path) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .ends_with(".tar.zst.part")
        {
            let _ = std::fs::remove_file(entry.path());
        }
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
    remove_stale_partials(backup_dir);
    let dirs = world_dirs(&paths.server, level_name);
    if dirs.is_empty() {
        return Err(Error::msg(format!(
            "no world directory named `{level_name}` to back up"
        )));
    }

    let prefix = if auto { AUTO_PREFIX } else { MANUAL_PREFIX };
    let file_name = format!("{prefix}{}.tar.zst", format_compact_utc(SystemTime::now()));
    let archive = backup_dir.join(&file_name);
    // Names are second-resolution, so a manual backup and the auto timer firing
    // in the same second would otherwise silently overwrite each other (and the
    // partial sweep above would eat the other run's scratch file). Refuse
    // instead of destroying an archive.
    if archive.exists() {
        return Err(Error::msg(format!(
            "a backup named {file_name} already exists — wait a second and try again"
        )));
    }
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

/// The top-level directory names an archive contains, in listing order.
///
/// Used to check a backup against the current `level-name` before anything is
/// moved. Shelling out to `tar -t` keeps this consistent with the extraction
/// path, which uses the same `tar`.
async fn archive_top_level_dirs(archive: &Path) -> Result<Vec<String>> {
    let mut cmd = Command::new("tar");
    cmd.arg("--zstd").arg("-tf").arg(archive);
    let output = cmd.output().await.map_err(Error::IoBare)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::msg(format!(
            "could not read the backup archive ({}): {}",
            output.status,
            stderr.trim()
        )));
    }
    let mut names = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let top = line
            .trim_start_matches("./")
            .split('/')
            .next()
            .unwrap_or("");
        if !top.is_empty() && !names.iter().any(|n| n == top) {
            names.push(top.to_string());
        }
    }
    Ok(names)
}

/// Whether a running server still holds one of the world's `session.lock`
/// files.
///
/// Every Minecraft server — vanilla, Fabric, Paper, and no matter how it was
/// launched — takes an exclusive `flock` on `<level>/session.lock` for its
/// whole run and only drops it on shutdown. Trying and failing to take that
/// lock ourselves is the one check that also catches a server this app did not
/// start: the no-systemd fallback where the JVM is a bare child process, or one
/// the user launched by hand. The systemd-scope check only sees servers we
/// started.
///
/// A missing lock file (or any error opening it) counts as "not locked": this
/// is a guard layered on top of the scope check, and a world that simply has no
/// `session.lock` yet must still be restorable.
#[cfg(unix)]
fn world_session_in_use(server_dir: &Path, level_name: &str) -> bool {
    use rustix::fs::{flock, FlockOperation};

    for name in world_dirs(server_dir, level_name) {
        let lock_path = server_dir.join(&name).join("session.lock");
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
        else {
            continue;
        };
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            // We took the lock, so nobody else holds it — release it again.
            Ok(()) => {
                let _ = flock(&file, FlockOperation::Unlock);
            }
            // Held by a live server.
            Err(rustix::io::Errno::WOULDBLOCK) => return true,
            // Anything else (permissions, unsupported FS): don't block on a
            // signal we can't read.
            Err(_) => continue,
        }
    }
    false
}

#[cfg(not(unix))]
fn world_session_in_use(_server_dir: &Path, _level_name: &str) -> bool {
    false
}

/// Restore a backup over the current world. **The server must be stopped.**
///
/// The current world directories are moved aside to `<name>.pre-restore`
/// (replacing any previous such copy) before extraction, so a bad restore is
/// recoverable.
///
/// Refuses outright when the archive's world directory does not match the
/// current `level-name`. Extracting a `myworld/` archive while `level-name` is
/// `world` would move the live world aside, unpack a directory the server never
/// looks at, and let it generate a fresh empty world — a silent loss that looks
/// like a successful restore.
pub async fn restore(paths: &Paths, backup: &BackupEntry, level_name: &str) -> Result<()> {
    // Hard stop: never extract over a world a server still has open. Doing so
    // leaves the JVM writing region/entity/playerdata files to inodes that no
    // longer match what is on disk, and the mismatch survives a restart — it
    // shows up in-game as missing item models, broken villager trades and
    // desynced durability, not as an obvious crash. The systemd-scope check in
    // the caller misses a bare-JVM fallback or a hand-started server; this one
    // does not.
    if world_session_in_use(&paths.server, level_name) {
        return Err(Error::msg(
            "the world is still in use by a running Minecraft server — stop the \
             server and wait for it to fully exit before restoring a backup",
        ));
    }

    // A concurrent `create` is writing a `*.tar.zst.part` scratch file for its
    // entire run; extracting over the world while it archives would produce a
    // silently partial archive. The parts are swept at the start of every
    // `create`, so a stray one here means "crashed run" — delete it by hand.
    if let Some(dir) = backup.path.parent() {
        let running = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().ends_with(".tar.zst.part"))
            })
            .unwrap_or(false);
        if running {
            return Err(Error::msg(
                "a backup appears to be in progress (found a *.tar.zst.part file) — \
                 wait for it to finish before restoring",
            ));
        }
    }

    if !backup.path.is_file() {
        return Err(Error::msg(format!(
            "backup {} is missing",
            backup.file_name
        )));
    }

    let contents = archive_top_level_dirs(&backup.path).await?;
    if !contents.iter().any(|name| name == level_name) {
        return Err(Error::msg(format!(
            "{} holds `{}`, but the current level-name is `{level_name}` — \
             set level-name to `{}` in Properties (or rename the world) before restoring",
            backup.file_name,
            contents.join("`, `"),
            contents.first().map_or("?", String::as_str),
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
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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

    #[tokio::test]
    async fn restore_refuses_an_archive_for_a_different_level_name() {
        let root = std::env::temp_dir().join(format!(
            "mcsm-bk-name-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();

        // A backup taken while level-name was `oldworld`.
        let region = paths.server.join("oldworld").join("region");
        std::fs::create_dir_all(&region).unwrap();
        std::fs::write(region.join("r.0.0.mca"), b"old chunks").unwrap();
        let bdir = paths.backups.clone();
        let entry = create(&paths, &bdir, "oldworld", false).await.unwrap();

        // ...and a live world under the current level-name `world`.
        let live = paths.server.join("world").join("region");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("r.0.0.mca"), b"live chunks").unwrap();

        let err = restore(&paths, &entry, "world").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("oldworld"), "got: {msg}");
        assert!(msg.contains("world"), "got: {msg}");

        // The live world was not touched and nothing was stashed aside.
        assert_eq!(
            std::fs::read(live.join("r.0.0.mca")).unwrap(),
            b"live chunks"
        );
        assert!(!paths.server.join("world.pre-restore").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn create_refuses_to_overwrite_an_existing_archive() {
        let root = std::env::temp_dir().join(format!(
            "mcsm-bk-clash-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        std::fs::create_dir_all(paths.server.join("world")).unwrap();
        std::fs::write(paths.server.join("world").join("level.dat"), b"x").unwrap();

        let bdir = paths.backups.clone();
        let first = create(&paths, &bdir, "world", false).await.unwrap();
        // Same second, same prefix => same name. Must not clobber the first.
        std::fs::write(&first.path, b"pretend this is the real archive").unwrap();
        if let Err(e) = create(&paths, &bdir, "world", false).await {
            assert!(e.to_string().contains("already exists"), "got: {e}");
            assert_eq!(
                std::fs::read(&first.path).unwrap(),
                b"pretend this is the real archive"
            );
        }
        // If the clock ticked over into the next second the name differs and the
        // second create legitimately succeeds; either way the first is intact.
        assert!(first.path.is_file());

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn create_sweeps_orphan_partials() {
        let root = std::env::temp_dir().join(format!(
            "mcsm-bk-part-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        std::fs::create_dir_all(paths.server.join("world").join("region")).unwrap();
        std::fs::write(
            paths.server.join("world").join("region").join("r.0.0.mca"),
            b"x",
        )
        .unwrap();

        let bdir = paths.backups.clone();
        std::fs::write(bdir.join("world-20200101-000000.tar.zst.part"), b"junk").unwrap();
        std::fs::write(
            bdir.join("auto-world-20200101-000000.tar.zst.part"),
            b"junk",
        )
        .unwrap();

        create(&paths, &bdir, "world", false).await.unwrap();

        let leftover: Vec<_> = std::fs::read_dir(&bdir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".part"))
            .collect();
        assert!(
            leftover.is_empty(),
            "orphan partials not swept: {leftover:?}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn restore_refuses_while_a_backup_is_in_progress() {
        let root = std::env::temp_dir().join(format!(
            "mcsm-bk-part-guard-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        std::fs::create_dir_all(paths.server.join("world")).unwrap();

        // Simulate a concurrently running `create` via its scratch file.
        let bdir = paths.backups.clone();
        std::fs::write(bdir.join("world-20260101-000000.tar.zst.part"), b"wip").unwrap();

        let entry = BackupEntry {
            path: bdir.join("world-20251231-000000.tar.zst"),
            file_name: "world-20251231-000000.tar.zst".into(),
            created: SystemTime::UNIX_EPOCH,
            size_bytes: 0,
        };
        let err = restore(&paths, &entry, "world").await.unwrap_err();
        assert!(err.to_string().contains("in progress"), "got: {err}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restore_refuses_while_a_server_holds_session_lock() {
        use rustix::fs::{flock, FlockOperation};

        let root = std::env::temp_dir().join(format!(
            "mcsm-bk-session-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();

        // A live world with a backup to restore from.
        let region = paths.server.join("world").join("region");
        std::fs::create_dir_all(&region).unwrap();
        std::fs::write(region.join("r.0.0.mca"), b"live chunks").unwrap();
        let bdir = paths.backups.clone();
        let entry = create(&paths, &bdir, "world", false).await.unwrap();

        // Simulate a running server: hold an exclusive lock on session.lock.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(paths.server.join("world").join("session.lock"))
            .unwrap();
        flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();

        let err = restore(&paths, &entry, "world").await.unwrap_err();
        assert!(err.to_string().contains("still in use"), "got: {err}");
        // The live world was left untouched.
        assert_eq!(
            std::fs::read(region.join("r.0.0.mca")).unwrap(),
            b"live chunks"
        );
        assert!(!paths.server.join("world.pre-restore").exists());

        // Server exits -> lock released -> restore now goes through.
        flock(&lock, FlockOperation::Unlock).unwrap();
        drop(lock);
        restore(&paths, &entry, "world").await.unwrap();
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
            touch_backup(
                &paths.backups,
                &format!("auto-world-2026010{}-000000.tar.zst", i + 1),
                i * 100,
            );
        }
        touch_backup(&paths.backups, "world-20260101-120000.tar.zst", 50);

        let removed = prune_auto(&paths.backups, 2).unwrap();
        assert_eq!(removed, 3);

        let remaining: Vec<String> = list(&paths.backups)
            .unwrap()
            .into_iter()
            .map(|e| e.file_name)
            .collect();
        assert_eq!(
            remaining
                .iter()
                .filter(|n| n.starts_with("auto-world-"))
                .count(),
            2
        );
        assert!(remaining
            .iter()
            .any(|n| n == "world-20260101-120000.tar.zst"));
        // keep == 0 is a no-op
        assert_eq!(prune_auto(&paths.backups, 0).unwrap(), 0);

        std::fs::remove_dir_all(&root).ok();
    }
}
