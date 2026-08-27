//! Persisted application state: `data/state.toml`.
//!
//! This is the *only* file the app writes outside the server directory itself,
//! and it is deliberately small and human-readable — every field maps to
//! something the user set in the GUI, and editing it by hand is a supported way
//! to change configuration.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::memory::{MemoryBudget, DEFAULT_TOTAL_MIB};
use crate::util::{read_to_string_opt, write_atomic};

/// Bump when a field is renamed or removed so old files can be migrated.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub schema: u32,

    /// Minecraft version the server is installed for, e.g. `"1.21.4"`.
    pub minecraft_version: Option<String>,
    /// Fabric loader version, e.g. `"0.16.9"`.
    pub loader_version: Option<String>,
    /// Fabric installer version used to build the launcher jar URL, e.g. `"1.0.1"`.
    pub installer_version: Option<String>,
    /// Show Minecraft snapshots in the version picker.
    pub allow_snapshots: bool,

    /// The user has accepted the Minecraft EULA. The server will not start
    /// until this is true; setting it writes `eula=true` to `eula.txt`.
    pub eula_accepted: bool,

    pub memory: MemorySettings,

    /// Path to the `java` binary. `None` means "find `java` on `PATH`".
    pub java_path: Option<PathBuf>,
    pub gc_preset: GcPreset,
    /// Extra JVM arguments appended verbatim, for anything the GUI doesn't model.
    pub extra_jvm_args: Vec<String>,

    /// Restart the server automatically if it exits unexpectedly (never after
    /// an out-of-memory kill).
    pub auto_restart: bool,

    /// Take a world backup automatically every this many minutes while the app
    /// is open. `0` disables it.
    pub auto_backup_minutes: u64,
    /// How many automatic backups to keep; the oldest beyond this are pruned.
    /// `0` keeps them all.
    pub auto_backup_keep: u64,

    /// Where world backups are written. `None` means "use the default"
    /// (`~/Documents/Minecraft Server Manager Backups`). Stored explicitly once
    /// resolved so the location is never forgotten, even if the app folder is
    /// deleted and recreated elsewhere.
    pub backup_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySettings {
    /// Hard ceiling for app + JVM + world combined, in MiB.
    pub total_mib: u64,
    /// Requested heap size in MiB. `None` lets the budget pick a safe default.
    pub xmx_mib: Option<u64>,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            total_mib: DEFAULT_TOTAL_MIB,
            xmx_mib: None,
        }
    }
}

/// The outcome of [`AppState::resolve_backup_dir`].
#[derive(Debug, Clone)]
pub struct ResolvedBackupDir {
    /// The directory backups will actually be written to.
    pub path: std::path::PathBuf,
    /// `Some(configured)` when the configured folder was unusable and
    /// [`path`](Self::path) is the `<data>/backups` fallback; `None` when the
    /// configured folder is fine and in use.
    pub fell_back_from: Option<std::path::PathBuf>,
}

/// Garbage-collector tuning preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum GcPreset {
    /// Aikar's widely-used G1 flags, *minus* `AlwaysPreTouch` (which would
    /// commit the whole heap at startup and trip the cgroup cap).
    #[default]
    Aikar,
    /// Just `-Xms`/`-Xmx`, nothing else.
    Basic,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            minecraft_version: None,
            loader_version: None,
            installer_version: None,
            allow_snapshots: false,
            eula_accepted: false,
            memory: MemorySettings::default(),
            java_path: None,
            gc_preset: GcPreset::default(),
            extra_jvm_args: Vec::new(),
            auto_restart: true,
            auto_backup_minutes: 0,
            auto_backup_keep: 10,
            backup_dir: None,
        }
    }
}

impl AppState {
    /// Load from `path`, returning [`AppState::default`] if the file is absent.
    pub fn load(path: &Path) -> Result<Self> {
        let Some(text) = read_to_string_opt(path)? else {
            return Ok(Self::default());
        };
        let mut state: Self = toml::from_str(&text).map_err(|source| Error::Toml {
            path: path.to_path_buf(),
            source,
        })?;
        state.migrate();
        Ok(state)
    }

    /// Bring a just-parsed state up to [`SCHEMA_VERSION`].
    ///
    /// Every field is `#[serde(default)]`, so a *new* field absent from an older
    /// file is already filled in by the time we get here — no work needed for
    /// the common "newer binary, older file" case. This is the single place to
    /// handle a field **rename** or a change in how a value is interpreted,
    /// keyed on the stored [`schema`](Self::schema): add one `if self.schema <
    /// N` block per bump of [`SCHEMA_VERSION`].
    ///
    /// A file written by a *newer* build (`schema > SCHEMA_VERSION`, i.e. the
    /// user downgraded) is kept and used as-is rather than rejected — locking
    /// someone out of their own settings would be worse. Unknown keys were
    /// already dropped by the parser, so re-saving will not preserve fields the
    /// newer version added.
    fn migrate(&mut self) {
        // Snapshot the on-disk version first: the migration arms below branch on
        // it, and the last line of this function overwrites `self.schema`.
        let from = self.schema;

        if from > SCHEMA_VERSION {
            tracing::warn!(
                file_schema = from,
                supported = SCHEMA_VERSION,
                "state.toml was written by a newer version; loading best-effort \
                 and settings unknown to this build will be lost on the next save"
            );
        }

        // No field renames yet. When SCHEMA_VERSION is bumped, add an arm here
        // (they must test `from`, not `self.schema`):
        //   if from < 2 { self.new_name = std::mem::take(&mut self.old_name); }

        self.schema = SCHEMA_VERSION;
    }

    /// Persist to `path` atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        write_atomic(path, text.as_bytes())
    }

    /// The resolved memory budget for the current settings.
    #[must_use]
    pub fn budget(&self) -> MemoryBudget {
        MemoryBudget::new(self.memory.total_mib, self.memory.xmx_mib)
    }

    /// Where backups are written: the explicit setting, or the default.
    ///
    /// This is the *configured* location and does no I/O — it can point at a
    /// folder that no longer exists or is not writable. Callers about to write
    /// a backup should use [`resolve_backup_dir`](Self::resolve_backup_dir).
    #[must_use]
    pub fn backup_dir(&self, paths: &crate::Paths) -> std::path::PathBuf {
        self.backup_dir
            .clone()
            .unwrap_or_else(|| paths.default_backup_dir())
    }

    /// The backup directory to actually use right now, healing a broken one.
    ///
    /// The configured [`backup_dir`](Self::backup_dir) can stop working long
    /// after it was set — its drive is unmounted, it came from another machine,
    /// it was hand-edited wrong. Rather than let every backup fail forever,
    /// probe it: if it cannot be created or written to, fall back to
    /// `<data>/backups` (the app made that directory, so it is always usable).
    /// The configured setting is left untouched, so backups return to it on
    /// their own once it works again.
    #[must_use]
    pub fn resolve_backup_dir(&self, paths: &crate::Paths) -> ResolvedBackupDir {
        let configured = self.backup_dir(paths);
        if crate::util::dir_is_writable(&configured) {
            return ResolvedBackupDir {
                path: configured,
                fell_back_from: None,
            };
        }
        let fallback = paths.backups.clone();
        let _ = std::fs::create_dir_all(&fallback);
        ResolvedBackupDir {
            path: fallback,
            fell_back_from: Some(configured),
        }
    }

    /// The `java` invocation to use.
    #[must_use]
    pub fn java_command(&self) -> PathBuf {
        self.java_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("java"))
    }

    /// Everything is installed and the EULA is accepted, so a start is possible.
    #[must_use]
    pub fn ready_to_launch(&self) -> bool {
        self.eula_accepted
            && self.minecraft_version.is_some()
            && self.loader_version.is_some()
            && self.installer_version.is_some()
    }

    /// JVM arguments that precede `-jar fabric-server-launch.jar nogui`.
    #[must_use]
    pub fn jvm_args(&self) -> Vec<String> {
        let budget = self.budget();
        let mut args = vec![
            format!("-Xms{}M", budget.xms_mib),
            format!("-Xmx{}M", budget.xmx_mib),
        ];
        if self.gc_preset == GcPreset::Aikar {
            args.extend(AIKAR_FLAGS.iter().map(|s| (*s).to_string()));
        }
        args.extend(self.extra_jvm_args.iter().cloned());
        args
    }
}

/// Aikar's flags for heaps up to ~12 GiB, with `-XX:+AlwaysPreTouch` removed on
/// purpose (see [`GcPreset::Aikar`]).
const AIKAR_FLAGS: &[&str] = &[
    "-XX:+UseG1GC",
    "-XX:+ParallelRefProcEnabled",
    "-XX:MaxGCPauseMillis=200",
    "-XX:+UnlockExperimentalVMOptions",
    "-XX:+DisableExplicitGC",
    "-XX:G1NewSizePercent=30",
    "-XX:G1MaxNewSizePercent=40",
    "-XX:G1HeapRegionSize=8M",
    "-XX:G1ReservePercent=20",
    "-XX:G1HeapWastePercent=5",
    "-XX:G1MixedGCCountTarget=4",
    "-XX:InitiatingHeapOccupancyPercent=15",
    "-XX:G1MixedGCLiveThresholdPercent=90",
    "-XX:G1RSetUpdatingPauseTimePercent=5",
    "-XX:SurvivorRatio=32",
    "-XX:+PerfDisableSharedMem",
    "-XX:MaxTenuringThreshold=1",
    "-Dusing.aikars.flags=https://mcflags.emc.gs",
    "-Daikars.new.flags=true",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_not_ready_to_launch() {
        assert!(!AppState::default().ready_to_launch());
    }

    #[test]
    fn round_trips_through_toml() {
        let s = AppState {
            minecraft_version: Some("1.21.4".into()),
            loader_version: Some("0.16.9".into()),
            installer_version: Some("1.0.1".into()),
            eula_accepted: true,
            memory: MemorySettings {
                total_mib: 8192,
                xmx_mib: None,
            },
            ..AppState::default()
        };

        let text = toml::to_string_pretty(&s).unwrap();
        let back: AppState = toml::from_str(&text).unwrap();

        assert_eq!(back.minecraft_version.as_deref(), Some("1.21.4"));
        assert!(back.ready_to_launch());
        assert_eq!(back.memory.total_mib, 8192);
    }

    #[test]
    fn jvm_args_lead_with_heap_then_gc_flags() {
        let s = AppState::default();
        let args = s.jvm_args();
        assert_eq!(args[0], "-Xms1024M");
        assert_eq!(args[1], "-Xmx5120M");
        assert!(args.iter().any(|a| a == "-XX:+UseG1GC"));
        assert!(
            !args.iter().any(|a| a.contains("AlwaysPreTouch")),
            "AlwaysPreTouch must not be present under the cgroup cap"
        );
    }

    #[test]
    fn basic_preset_is_heap_only() {
        let s = AppState {
            gc_preset: GcPreset::Basic,
            ..AppState::default()
        };
        assert_eq!(s.jvm_args(), vec!["-Xms1024M", "-Xmx5120M"]);
    }

    #[test]
    fn missing_file_yields_default() {
        let path = std::env::temp_dir().join("mcsm-nonexistent-state.toml");
        let _ = std::fs::remove_file(&path);
        let s = AppState::load(&path).unwrap();
        assert_eq!(s.schema, SCHEMA_VERSION);
    }

    #[test]
    fn old_file_is_stamped_to_current_schema_and_keeps_known_fields() {
        let dir = std::env::temp_dir().join(format!("mcsm-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");
        // schema 0, a real setting, and a key this build does not know.
        std::fs::write(
            &path,
            "schema = 0\neula_accepted = true\nsome_removed_field = 42\n",
        )
        .unwrap();

        let s = AppState::load(&path).unwrap();
        assert_eq!(s.schema, SCHEMA_VERSION, "migrate() stamps the version");
        assert!(s.eula_accepted, "recognised fields survive");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_backup_dir_uses_a_writable_configured_folder() {
        let root = std::env::temp_dir().join(format!("mcsm-rbd-ok-{}", std::process::id()));
        let paths = crate::Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        let chosen = root.join("my-backups");

        let state = AppState {
            backup_dir: Some(chosen.clone()),
            ..AppState::default()
        };
        let resolved = state.resolve_backup_dir(&paths);
        assert_eq!(resolved.path, chosen);
        assert!(resolved.fell_back_from.is_none());
        assert!(chosen.is_dir(), "a writable configured folder is created");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_backup_dir_falls_back_when_the_configured_folder_is_unusable() {
        let root = std::env::temp_dir().join(format!("mcsm-rbd-bad-{}", std::process::id()));
        let paths = crate::Paths::with_root(&root);
        paths.ensure_dirs().unwrap();

        // A regular file where a directory is expected: create_dir_all fails on it.
        let blocked = root.join("blocked");
        std::fs::write(&blocked, b"not a dir").unwrap();

        let state = AppState {
            backup_dir: Some(blocked.clone()),
            ..AppState::default()
        };
        let resolved = state.resolve_backup_dir(&paths);
        assert_eq!(resolved.path, paths.backups);
        assert_eq!(resolved.fell_back_from.as_deref(), Some(blocked.as_path()));
        assert!(paths.backups.is_dir());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn newer_file_loads_best_effort_without_error() {
        let dir = std::env::temp_dir().join(format!("mcsm-downgrade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");
        std::fs::write(&path, "schema = 9999\nauto_backup_keep = 3\n").unwrap();

        let s = AppState::load(&path).expect("a newer file must not be rejected");
        assert_eq!(s.schema, SCHEMA_VERSION);
        assert_eq!(s.auto_backup_keep, 3);

        std::fs::remove_dir_all(&dir).ok();
    }
}
