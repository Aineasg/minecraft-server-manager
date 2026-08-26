//! The four player-access JSON files Minecraft keeps in the server directory:
//! `ops.json`, `whitelist.json`, `banned-players.json`, `banned-ips.json`.
//!
//! These are plain JSON arrays. We model each row as a typed struct so the GUI
//! can present add/remove tables instead of a raw text editor. When the server
//! is running the GUI prefers to drive these through console commands (`op`,
//! `whitelist add`, ...) so the change takes effect live; when it is stopped it
//! edits the files directly, which is what this module is for.

use std::net::IpAddr;
use std::path::Path;

use md5::{Digest as _, Md5};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::{read_to_string_opt, write_atomic};

/// An entry in `ops.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpEntry {
    pub uuid: String,
    pub name: String,
    /// Permission level 1-4. Level 4 is full operator.
    pub level: u8,
    #[serde(default)]
    pub bypasses_player_limit: bool,
}

impl OpEntry {
    #[must_use]
    pub fn new(uuid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uuid: uuid.into(),
            name: name.into(),
            level: 4,
            bypasses_player_limit: false,
        }
    }
}

/// An entry in `whitelist.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhitelistEntry {
    pub uuid: String,
    pub name: String,
}

/// An entry in `banned-players.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannedPlayer {
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub created: String,
    #[serde(default = "unknown_source")]
    pub source: String,
    #[serde(default = "forever")]
    pub expires: String,
    #[serde(default = "banned_by_operator")]
    pub reason: String,
}

/// An entry in `banned-ips.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannedIp {
    pub ip: String,
    #[serde(default)]
    pub created: String,
    #[serde(default = "unknown_source")]
    pub source: String,
    #[serde(default = "forever")]
    pub expires: String,
    #[serde(default = "banned_by_operator")]
    pub reason: String,
}

fn unknown_source() -> String {
    "(Unknown)".to_string()
}
fn forever() -> String {
    "forever".to_string()
}
fn banned_by_operator() -> String {
    "Banned by an operator.".to_string()
}

/// Which access file to read/write, and its filename in the server directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessFile {
    Ops,
    Whitelist,
    BannedPlayers,
    BannedIps,
}

impl AccessFile {
    #[must_use]
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Ops => "ops.json",
            Self::Whitelist => "whitelist.json",
            Self::BannedPlayers => "banned-players.json",
            Self::BannedIps => "banned-ips.json",
        }
    }
}

/// Load a JSON array from `server_dir/<file>`. A missing file is an empty list.
pub fn load<T: DeserializeOwned>(server_dir: &Path, file: AccessFile) -> Result<Vec<T>> {
    let path = server_dir.join(file.file_name());
    let Some(text) = read_to_string_opt(&path)? else {
        return Ok(Vec::new());
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|source| Error::Json {
        what: "access list",
        source,
    })
}

/// Write a JSON array to `server_dir/<file>`, pretty-printed, atomically.
pub fn save<T: Serialize>(server_dir: &Path, file: AccessFile, entries: &[T]) -> Result<()> {
    let path = server_dir.join(file.file_name());
    let mut json = serde_json::to_vec_pretty(entries).map_err(|source| Error::Json {
        what: "access list",
        source,
    })?;
    json.push(b'\n');
    write_atomic(&path, &json)
}

/// The UUID an offline-mode server assigns to a username: version-3 (MD5) UUID
/// of the bytes `OfflinePlayer:<name>`. Lets the GUI add ops/whitelist entries
/// while the server is stopped and offline, with no network lookup.
#[must_use]
pub fn offline_uuid(name: &str) -> String {
    let mut hash = Md5::new();
    hash.update(format!("OfflinePlayer:{name}").as_bytes());
    let mut bytes: [u8; 16] = hash.finalize().into();

    // Stamp in the version (3) and RFC 4122 variant, exactly as Java's
    // `UUID.nameUUIDFromBytes` does.
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11],
        bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Validate a string as an IP address for the banned-IPs table.
pub fn parse_ip(text: &str) -> Result<IpAddr> {
    text.trim()
        .parse()
        .map_err(|_| Error::msg(format!("`{text}` is not a valid IP address")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_matches_known_java_output() {
        // Verified against `UUID.nameUUIDFromBytes("OfflinePlayer:Notch".getBytes())`.
        assert_eq!(
            offline_uuid("Notch"),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
        assert_eq!(offline_uuid("jeb_"), "a762f560-4fce-3236-812a-b80efff0b62b");
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = std::env::temp_dir().join(format!("mcsm-access-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ops: Vec<OpEntry> = load(&dir, AccessFile::Ops).unwrap();
        assert!(ops.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ops_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("mcsm-access-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ops = vec![OpEntry::new(offline_uuid("Steve"), "Steve")];
        save(&dir, AccessFile::Ops, &ops).unwrap();
        let back: Vec<OpEntry> = load(&dir, AccessFile::Ops).unwrap();
        assert_eq!(ops, back);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn banned_player_fills_defaults() {
        let json = r#"[{"uuid":"u","name":"Griefer"}]"#;
        let list: Vec<BannedPlayer> = serde_json::from_str(json).unwrap();
        assert_eq!(list[0].expires, "forever");
        assert_eq!(list[0].reason, "Banned by an operator.");
    }
}
