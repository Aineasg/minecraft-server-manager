//! Where everything lives on disk.
//!
//! The manager is deliberately *self-contained*: a single directory holds every
//! byte the app writes at runtime — server jars, the world, mods, backups,
//! logs, app state. Deleting that one directory is a complete uninstall. When
//! run from a repo checkout it is `<repo>/data/` (keeping runtime state out of
//! the source tree); installed, it is the manager's own data directory.
//!
//! [`Paths`] is the single source of truth for that layout. Nothing else in the
//! codebase joins path segments by hand.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Resolved absolute locations for every directory and key file the app uses.
///
/// `data` is the directory that directly holds `state.toml`, `server/`,
/// `cache/`, `backups/` and `logs/`. In **portable mode** (a repo checkout run
/// in place) `data` is `<root>/data` so runtime state stays out of the source
/// tree; in **installed mode** and under an explicit `$MCSM_ROOT` the manager
/// owns the whole directory, so `data == root`.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The manager root — the directory you can delete to remove everything.
    pub root: PathBuf,
    /// Parent of all runtime state (`== root`, or `<root>/data` in portable mode).
    pub data: PathBuf,
    /// `<data>/state.toml` — persisted [`crate::state::AppState`].
    pub state_file: PathBuf,
    /// `<data>/server` — the working directory the server JVM runs in.
    pub server: PathBuf,
    /// `<data>/server/mods`.
    pub mods: PathBuf,
    /// `<data>/server/config` — per-mod config files.
    pub mod_config: PathBuf,
    /// `<data>/cache` — downloaded jars, keyed by content hash, reused across reinstalls.
    pub cache: PathBuf,
    /// `<data>/backups` — world archives.
    pub backups: PathBuf,
    /// `<data>/logs` — rotated server logs plus the app's own log.
    pub logs: PathBuf,
}

impl Paths {
    /// Build the layout with `data` as the directory that *directly* holds
    /// `state.toml`, `server/`, ... Used for installed mode and an explicit
    /// `$MCSM_ROOT`, where the manager owns the whole directory.
    pub fn with_data_dir(data: impl Into<PathBuf>) -> Self {
        let data = data.into();
        Self {
            state_file: data.join("state.toml"),
            server: data.join("server"),
            mods: data.join("server").join("mods"),
            mod_config: data.join("server").join("config"),
            cache: data.join("cache"),
            backups: data.join("backups"),
            logs: data.join("logs"),
            root: data.clone(),
            data,
        }
    }

    /// Build the layout for a repo checkout run in place: runtime data lives in
    /// `<root>/data/` so it stays out of the source tree.
    ///
    /// Callers that want discovery should go through [`Paths::discover`].
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut paths = Self::with_data_dir(root.join("data"));
        paths.root = root;
        paths
    }

    /// Find the manager root automatically.
    ///
    /// Resolution order:
    /// 1. `$MCSM_ROOT`, if set — used directly as the data directory.
    /// 2. **Portable mode:** the *executable itself* sits in a repo checkout —
    ///    it is under a `target/` build directory with a `Cargo.toml`
    ///    mentioning `mcsm` above it, or a `.mcsm-root` marker file sits beside
    ///    it (or in a parent). Data goes in `<repo>/data/`. The working
    ///    directory is deliberately **not** consulted: an installed binary run
    ///    from inside a clone must still use the installed root, not the clone.
    /// 3. **Installed mode:** `$XDG_DATA_HOME/MinecraftServerManager`
    ///    (default `~/.local/share/MinecraftServerManager`) — one
    ///    self-contained folder, just not next to the binary.
    /// 4. Last resort: the directory containing the executable.
    pub fn discover() -> Result<Self> {
        if let Some(env_root) = std::env::var_os("MCSM_ROOT") {
            let root = PathBuf::from(env_root);
            std::fs::create_dir_all(&root).map_err(|e| {
                Error::RootNotFound(format!("cannot use $MCSM_ROOT {}: {e}", root.display()))
            })?;
            return Ok(Self::with_data_dir(root));
        }

        let exe = std::env::current_exe().ok();

        if let Some(exe_dir) = exe.as_deref().and_then(Path::parent) {
            let built_in_place = exe
                .as_deref()
                .is_some_and(|e| e.components().any(|c| c.as_os_str() == "target"));
            if let Some(root) = walk_up_for_root(exe_dir, built_in_place) {
                return Ok(Self::with_root(root));
            }
        }

        if let Some(data_home) = dirs::data_dir() {
            return Ok(Self::with_data_dir(
                data_home.join("MinecraftServerManager"),
            ));
        }

        if let Some(dir) = exe.as_deref().and_then(Path::parent) {
            return Ok(Self::with_data_dir(dir));
        }

        Err(Error::RootNotFound(
            "set $MCSM_ROOT to choose where the manager keeps its data".into(),
        ))
    }

    /// The default location for world backups: `~/Documents/Minecraft Server
    /// Manager Backups`, falling back to `<data>/backups` when there is no
    /// Documents directory. Used when the user has not set an explicit path.
    #[must_use]
    pub fn default_backup_dir(&self) -> PathBuf {
        match dirs::document_dir() {
            Some(docs) => docs.join("Minecraft Server Manager Backups"),
            None => self.backups.clone(),
        }
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

    /// `<data>/server/<name>` — a file directly in the server directory.
    pub fn server_file(&self, name: &str) -> PathBuf {
        self.server.join(name)
    }
}

/// Walk up from `start` looking for something that marks the manager root.
///
/// A `.mcsm-root` marker file always counts. A `Cargo.toml` mentioning `mcsm`
/// only counts when `allow_cargo` is set — i.e. the caller already established
/// that the executable was built in place — so an installed binary that merely
/// happens to run inside some unrelated Rust project is not fooled.
fn walk_up_for_root(start: &Path, allow_cargo: bool) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if dir.join(".mcsm-root").is_file() {
            return Some(dir.to_path_buf());
        }
        if allow_cargo {
            let cargo = dir.join("Cargo.toml");
            if cargo.is_file() {
                if let Ok(text) = std::fs::read_to_string(&cargo) {
                    if text.contains("mcsm") {
                        return Some(dir.to_path_buf());
                    }
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
    fn portable_layout_nests_under_data() {
        let p = Paths::with_root("/srv/mc");
        assert_eq!(p.root, Path::new("/srv/mc"));
        assert_eq!(p.data, Path::new("/srv/mc/data"));
        assert_eq!(p.server, Path::new("/srv/mc/data/server"));
        assert_eq!(p.mods, Path::new("/srv/mc/data/server/mods"));
        assert_eq!(p.state_file, Path::new("/srv/mc/data/state.toml"));
        assert!(p.cache.starts_with(&p.data));
        assert!(p.backups.starts_with(&p.data));
        assert!(p.logs.starts_with(&p.data));
    }

    #[test]
    fn installed_layout_sits_directly_in_the_data_dir() {
        let p = Paths::with_data_dir("/home/u/.local/share/MinecraftServerManager");
        assert_eq!(p.root, p.data);
        assert_eq!(
            p.state_file,
            Path::new("/home/u/.local/share/MinecraftServerManager/state.toml")
        );
        assert_eq!(
            p.server,
            Path::new("/home/u/.local/share/MinecraftServerManager/server")
        );
    }

    #[test]
    fn marker_file_is_found_walking_up_regardless_of_cargo() {
        let tmp = std::env::temp_dir().join(format!("mcsm-test-{}", std::process::id()));
        let nested = tmp.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(tmp.join(".mcsm-root"), b"").unwrap();

        assert_eq!(walk_up_for_root(&nested, false).unwrap(), tmp);
        assert_eq!(walk_up_for_root(&nested, true).unwrap(), tmp);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn cargo_toml_only_marks_the_root_when_built_in_place() {
        let tmp = std::env::temp_dir().join(format!("mcsm-cargo-{}", std::process::id()));
        let nested = tmp.join("target").join("release");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            tmp.join("Cargo.toml"),
            b"[workspace]\nmembers = [\"mcsm-core\"]\n",
        )
        .unwrap();

        // An installed binary that merely runs inside a Rust project: ignored.
        assert!(walk_up_for_root(&nested, false).is_none());
        // A binary actually built in this tree (exe under target/): portable.
        assert_eq!(walk_up_for_root(&nested, true).unwrap(), tmp);

        std::fs::remove_dir_all(&tmp).ok();
    }
}
