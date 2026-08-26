//! Where everything lives on disk.
//!
//! The manager is deliberately *self-contained*: one directory holds the source,
//! the build output, and — under `data/` — every byte the app writes at runtime
//! (server jars, the world, mods, backups, logs, app state). Deleting that one
//! directory is a complete uninstall.
//!
//! [`Paths`] is the single source of truth for that layout. Nothing else in the
//! codebase joins path segments by hand.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Resolved absolute locations for every directory and key file the app uses.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The manager root — the directory you can delete to remove everything.
    pub root: PathBuf,
    /// `<root>/data` — parent of all runtime state.
    pub data: PathBuf,
    /// `<root>/data/state.toml` — persisted [`crate::state::AppState`].
    pub state_file: PathBuf,
    /// `<root>/data/server` — the working directory the server JVM runs in.
    pub server: PathBuf,
    /// `<root>/data/server/mods`.
    pub mods: PathBuf,
    /// `<root>/data/server/config` — per-mod config files.
    pub mod_config: PathBuf,
    /// `<root>/data/cache` — downloaded jars, keyed by content hash, reused across reinstalls.
    pub cache: PathBuf,
    /// `<root>/data/backups` — world archives.
    pub backups: PathBuf,
    /// `<root>/data/logs` — rotated server logs plus the app's own log.
    pub logs: PathBuf,
}

impl Paths {
    /// Resolve the layout from an explicit root directory.
    ///
    /// The root is used as-is; callers that want discovery should go through
    /// [`Paths::discover`].
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let data = root.join("data");
        Self {
            state_file: data.join("state.toml"),
            server: data.join("server"),
            mods: data.join("server").join("mods"),
            mod_config: data.join("server").join("config"),
            cache: data.join("cache"),
            backups: data.join("backups"),
            logs: data.join("logs"),
            data,
            root,
        }
    }

    /// Find the manager root automatically.
    ///
    /// Resolution order:
    /// 1. `$MCSM_ROOT`, if set.
    /// 2. Walking up from the running executable, then from the current working
    ///    directory, looking for a `Cargo.toml` that belongs to this workspace
    ///    (contains the string `mcsm`) or a `.mcsm-root` marker file.
    /// 3. The directory containing the executable (the shipped-binary case).
    pub fn discover() -> Result<Self> {
        if let Some(env_root) = std::env::var_os("MCSM_ROOT") {
            let root = PathBuf::from(env_root);
            if root.is_dir() {
                return Ok(Self::with_root(root));
            }
            return Err(Error::RootNotFound(format!(
                "$MCSM_ROOT points at {}, which is not a directory",
                root.display()
            )));
        }

        let exe = std::env::current_exe().ok();
        let cwd = std::env::current_dir().ok();
        let starts = [
            exe.as_deref().and_then(Path::parent),
            cwd.as_deref(),
        ];

        for start in starts.into_iter().flatten() {
            if let Some(root) = walk_up_for_root(start) {
                return Ok(Self::with_root(root));
            }
        }

        if let Some(dir) = exe.as_deref().and_then(Path::parent) {
            return Ok(Self::with_root(dir));
        }

        Err(Error::RootNotFound(
            "set $MCSM_ROOT or run the binary from inside the project directory".into(),
        ))
    }

    /// Create `data/` and every subdirectory. Idempotent.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            &self.data,
            &self.server,
            &self.mods,
            &self.mod_config,
            &self.cache,
            &self.backups,
            &self.logs,
        ] {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        Ok(())
    }

    /// `<root>/data/server/<name>` — a file directly in the server directory.
    pub fn server_file(&self, name: &str) -> PathBuf {
        self.server.join(name)
    }
}

/// Walk up from `start` looking for something that identifies the manager root.
fn walk_up_for_root(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if dir.join(".mcsm-root").is_file() {
            return Some(dir.to_path_buf());
        }
        let cargo = dir.join("Cargo.toml");
        if cargo.is_file() {
            if let Ok(text) = std::fs::read_to_string(&cargo) {
                if text.contains("mcsm") {
                    return Some(dir.to_path_buf());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_all_under_data() {
        let p = Paths::with_root("/srv/mc");
        assert_eq!(p.data, Path::new("/srv/mc/data"));
        assert_eq!(p.server, Path::new("/srv/mc/data/server"));
        assert_eq!(p.mods, Path::new("/srv/mc/data/server/mods"));
        assert_eq!(p.state_file, Path::new("/srv/mc/data/state.toml"));
        assert!(p.cache.starts_with(&p.data));
        assert!(p.backups.starts_with(&p.data));
        assert!(p.logs.starts_with(&p.data));
    }

    #[test]
    fn discover_via_marker_file() {
        let tmp = std::env::temp_dir().join(format!("mcsm-test-{}", std::process::id()));
        let nested = tmp.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(tmp.join(".mcsm-root"), b"").unwrap();

        let found = walk_up_for_root(&nested).unwrap();
        assert_eq!(found, tmp);

        std::fs::remove_dir_all(&tmp).ok();
    }
}
