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
        let state: Self = toml::from_str(&text).map_err(|source| Error::Toml {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(state)
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
    #[must_use]
    pub fn backup_dir(&self, paths: &crate::Paths) -> std::path::PathBuf {
        self.backup_dir
            .clone()
            .unwrap_or_else(|| paths.default_backup_dir())
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
}
