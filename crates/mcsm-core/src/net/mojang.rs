//! Mojang endpoints: the version manifest (to find the vanilla server jar) and
//! the username → UUID lookup (for populating ops/whitelist on an online-mode
//! server).

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::net::client::Http;

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const PROFILE_URL: &str = "https://api.mojang.com/users/profiles/minecraft";
const SERVICE: &str = "Mojang";

#[derive(Debug, Clone, Deserialize)]
pub struct VersionManifest {
    pub latest: Latest,
    pub versions: Vec<ManifestVersion>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestVersion {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// URL of the per-version metadata document.
    pub url: String,
}

impl VersionManifest {
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&ManifestVersion> {
        self.versions.iter().find(|v| v.id == id)
    }
}

/// The `downloads.server` entry from a per-version metadata document.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerDownload {
    pub url: String,
    pub sha1: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct VersionDoc {
    downloads: Downloads,
}

#[derive(Deserialize)]
struct Downloads {
    server: Option<ServerDownload>,
}

/// Fetch the list of all Minecraft versions.
pub async fn version_manifest(http: &Http) -> Result<VersionManifest> {
    http.get_json(SERVICE, MANIFEST_URL).await
}

/// Resolve the vanilla dedicated-server jar download for a version id.
pub async fn server_download(
    http: &Http,
    manifest: &VersionManifest,
    version_id: &str,
) -> Result<ServerDownload> {
    let entry = manifest.find(version_id).ok_or_else(|| {
        Error::msg(format!(
            "Minecraft version `{version_id}` is not in the manifest"
        ))
    })?;
    let doc: VersionDoc = http.get_json(SERVICE, &entry.url).await?;
    doc.downloads.server.ok_or_else(|| {
        Error::msg(format!(
            "Minecraft `{version_id}` has no dedicated server download"
        ))
    })
}

#[derive(Deserialize)]
struct Profile {
    id: String,
}

/// Look up a player's UUID by username on an online-mode account server.
/// Returns `Ok(None)` when the name has no paid account.
pub async fn lookup_uuid(http: &Http, name: &str) -> Result<Option<String>> {
    let url = format!("{PROFILE_URL}/{name}");
    match http.get_json::<Profile>(SERVICE, &url).await {
        Ok(profile) => Ok(Some(dash_uuid(&profile.id))),
        Err(Error::HttpStatus { status, .. }) if status == 404 || status == 204 => Ok(None),
        Err(e) => Err(e),
    }
}

/// Insert dashes into a 32-hex-character UUID: `8-4-4-4-12`.
fn dash_uuid(hex: &str) -> String {
    if hex.len() != 32 {
        return hex.to_string();
    }
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashes_a_bare_uuid() {
        assert_eq!(
            dash_uuid("069a79f444e94726a5befca90e38aaf5"),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
    }

    #[test]
    fn manifest_find_locates_a_version() {
        let json = r#"{
            "latest": {"release": "1.21.4", "snapshot": "25w03a"},
            "versions": [
                {"id": "1.21.4", "type": "release", "url": "https://example/1.21.4.json"},
                {"id": "25w03a", "type": "snapshot", "url": "https://example/25w03a.json"}
            ]
        }"#;
        let m: VersionManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.find("1.21.4").unwrap().kind, "release");
        assert!(m.find("1.0.0-nonexistent").is_none());
    }
}
