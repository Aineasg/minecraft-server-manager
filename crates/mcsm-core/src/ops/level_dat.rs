//! Reading and patching the handful of world settings that live in the world's
//! `level.dat` rather than in `server.properties`.
//!
//! `level.dat` is a single gzip-compressed NBT document. Its root is an unnamed
//! compound with one key, `Data`, which holds (among much else):
//!
//! * `hardcore` — `TAG_Byte`, 0/1. **Only** written by `server.properties` at
//!   world creation; changing it afterwards means editing this file.
//! * `Difficulty` — `TAG_Byte`, 0..=3 (peaceful/easy/normal/hard).
//! * `DifficultyLocked` — `TAG_Byte`, 0/1.
//!
//! The server holds `level.dat` open and rewrites it on autosave and shutdown,
//! so callers must ensure the server is stopped before [`write`].

use std::io::{Read as _, Write as _};
use std::path::PathBuf;

use fastnbt::Value;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::error::{Error, Result};
use crate::paths::Paths;
use crate::util::write_atomic;

/// The three world-locked settings this module can edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldSettings {
    pub hardcore: bool,
    /// 0 = peaceful, 1 = easy, 2 = normal, 3 = hard.
    pub difficulty: u8,
    pub difficulty_locked: bool,
}

/// Path to `<level_name>/level.dat` inside the server directory.
fn level_dat_path(paths: &Paths, level_name: &str) -> PathBuf {
    paths.server.join(level_name).join("level.dat")
}

/// Whether this world has a `level.dat` yet (i.e. it has been generated).
#[must_use]
pub fn exists(paths: &Paths, level_name: &str) -> bool {
    level_dat_path(paths, level_name).is_file()
}

/// Read the world settings, or `Ok(None)` if the world has not been generated.
pub fn read(paths: &Paths, level_name: &str) -> Result<Option<WorldSettings>> {
    let path = level_dat_path(paths, level_name);
    let Some(raw) = read_nbt(&path)? else {
        return Ok(None);
    };
    let root: Value = fastnbt::from_bytes(&raw).map_err(nbt_err("parse level.dat"))?;
    let Value::Compound(map) = &root else {
        return Err(Error::msg("level.dat root is not a compound"));
    };
    let Some(Value::Compound(data)) = map.get("Data") else {
        return Err(Error::msg("level.dat has no `Data` compound"));
    };
    Ok(Some(WorldSettings {
        hardcore: byte(data, "hardcore") != 0,
        difficulty: byte(data, "Difficulty").clamp(0, 3) as u8,
        difficulty_locked: byte(data, "DifficultyLocked") != 0,
    }))
}

/// Patch the world settings in place. The server must be stopped.
///
/// The previous `level.dat` is copied to `level.dat.bak` first.
pub fn write(paths: &Paths, level_name: &str, settings: &WorldSettings) -> Result<()> {
    let path = level_dat_path(paths, level_name);
    let Some(raw) = read_nbt(&path)? else {
        return Err(Error::msg(
            "this world has no level.dat yet — start the server once to generate it",
        ));
    };

    let mut root: Value = fastnbt::from_bytes(&raw).map_err(nbt_err("parse level.dat"))?;
    {
        let Value::Compound(map) = &mut root else {
            return Err(Error::msg("level.dat root is not a compound"));
        };
        let Some(Value::Compound(data)) = map.get_mut("Data") else {
            return Err(Error::msg("level.dat has no `Data` compound"));
        };
        data.insert("hardcore".into(), Value::Byte(i8::from(settings.hardcore)));
        data.insert(
            "Difficulty".into(),
            Value::Byte(settings.difficulty.min(3) as i8),
        );
        data.insert(
            "DifficultyLocked".into(),
            Value::Byte(i8::from(settings.difficulty_locked)),
        );
    }

    let nbt = fastnbt::to_bytes(&root).map_err(nbt_err("serialise level.dat"))?;
    let gz = gzip(&nbt)?;

    // Back up the current file before overwriting.
    let backup = path.with_extension("dat.bak");
    std::fs::copy(&path, &backup).map_err(|e| Error::io(&backup, e))?;
    write_atomic(&path, &gz)
}

/// Read `level.dat`, transparently gunzipping. Returns `Ok(None)` if absent.
fn read_nbt(path: &std::path::Path) -> Result<Option<Vec<u8>>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path, e)),
    };
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        GzDecoder::new(&bytes[..])
            .read_to_end(&mut out)
            .map_err(|e| Error::io(path, e))?;
        Ok(Some(out))
    } else {
        // Rare, but some tools store level.dat uncompressed.
        Ok(Some(bytes))
    }
}

fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).map_err(Error::IoBare)?;
    encoder.finish().map_err(Error::IoBare)
}

fn byte(map: &std::collections::HashMap<String, Value>, key: &str) -> i64 {
    match map.get(key) {
        Some(Value::Byte(b)) => i64::from(*b),
        Some(Value::Short(s)) => i64::from(*s),
        Some(Value::Int(i)) => i64::from(*i),
        _ => 0,
    }
}

fn nbt_err(what: &'static str) -> impl Fn(fastnbt::error::Error) -> Error {
    move |e| Error::msg(format!("{what}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_level_dat(paths: &Paths, level: &str, settings: WorldSettings) {
        let mut data = HashMap::new();
        data.insert(
            "hardcore".to_string(),
            Value::Byte(i8::from(settings.hardcore)),
        );
        data.insert(
            "Difficulty".to_string(),
            Value::Byte(settings.difficulty as i8),
        );
        data.insert(
            "DifficultyLocked".to_string(),
            Value::Byte(i8::from(settings.difficulty_locked)),
        );
        data.insert("LevelName".to_string(), Value::String(level.to_string()));

        let mut root = HashMap::new();
        root.insert("Data".to_string(), Value::Compound(data));
        let nbt = fastnbt::to_bytes(&Value::Compound(root)).unwrap();

        let dir = paths.server.join(level);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("level.dat"), gzip(&nbt).unwrap()).unwrap();
    }

    fn temp_paths() -> Paths {
        let root = std::env::temp_dir().join(format!(
            "mcsm-leveldat-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        paths
    }

    #[test]
    fn read_missing_world_is_none() {
        let paths = temp_paths();
        assert!(read(&paths, "world").unwrap().is_none());
        assert!(!exists(&paths, "world"));
        std::fs::remove_dir_all(&paths.root).ok();
    }

    #[test]
    fn round_trips_hardcore_and_difficulty() {
        let paths = temp_paths();
        make_level_dat(
            &paths,
            "world",
            WorldSettings {
                hardcore: false,
                difficulty: 1,
                difficulty_locked: false,
            },
        );

        let before = read(&paths, "world").unwrap().unwrap();
        assert!(!before.hardcore);
        assert_eq!(before.difficulty, 1);

        write(
            &paths,
            "world",
            &WorldSettings {
                hardcore: true,
                difficulty: 3,
                difficulty_locked: true,
            },
        )
        .unwrap();

        let after = read(&paths, "world").unwrap().unwrap();
        assert!(after.hardcore);
        assert_eq!(after.difficulty, 3);
        assert!(after.difficulty_locked);

        // Other keys are preserved and a backup was made.
        assert!(paths.server.join("world").join("level.dat.bak").is_file());
        let raw = read_nbt(&paths.server.join("world").join("level.dat"))
            .unwrap()
            .unwrap();
        // Valid NBT root framing the vanilla server expects: TAG_Compound (0x0a).
        assert_eq!(raw.first(), Some(&0x0a));
        let root: Value = fastnbt::from_bytes(&raw).unwrap();
        let Value::Compound(map) = root else { panic!() };
        let Some(Value::Compound(data)) = map.get("Data") else {
            panic!()
        };
        assert!(matches!(data.get("LevelName"), Some(Value::String(s)) if s == "world"));

        std::fs::remove_dir_all(&paths.root).ok();
    }

    #[test]
    fn write_without_a_world_errors() {
        let paths = temp_paths();
        let err = write(
            &paths,
            "world",
            &WorldSettings {
                hardcore: true,
                difficulty: 2,
                difficulty_locked: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no level.dat"));
        std::fs::remove_dir_all(&paths.root).ok();
    }
}
