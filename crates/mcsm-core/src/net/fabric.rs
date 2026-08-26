//! Fabric metadata service (`meta.fabricmc.net`).
//!
//! We only need three lists (game versions, loader versions, installer versions)
//! and one download URL. The `.../server/jar` endpoint returns a ready-to-run
//! launcher jar, so the app never has to run the interactive Fabric installer.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::net::client::Http;

const BASE: &str = "https://meta.fabricmc.net/v2";
const SERVICE: &str = "Fabric meta";

/// A Minecraft version Fabric supports.
#[derive(Debug, Clone, Deserialize)]
pub struct GameVersion {
    pub version: String,
    /// `true` for full releases, `false` for snapshots.
    pub stable: bool,
}

/// A Fabric loader release.
#[derive(Debug, Clone, Deserialize)]
pub struct LoaderVersion {
    pub version: String,
    pub stable: bool,
}

/// A Fabric installer release.
#[derive(Debug, Clone, Deserialize)]
pub struct InstallerVersion {
    pub version: String,
    pub stable: bool,
}

/// All Minecraft versions, newest first.
pub async fn game_versions(http: &Http) -> Result<Vec<GameVersion>> {
    http.get_json(SERVICE, &format!("{BASE}/versions/game"))
        .await
}

/// All loader versions, newest first.
pub async fn loader_versions(http: &Http) -> Result<Vec<LoaderVersion>> {
    http.get_json(SERVICE, &format!("{BASE}/versions/loader"))
        .await
}

/// All installer versions, newest first.
pub async fn installer_versions(http: &Http) -> Result<Vec<InstallerVersion>> {
    http.get_json(SERVICE, &format!("{BASE}/versions/installer"))
        .await
}

/// The newest loader version marked stable.
pub async fn latest_stable_loader(http: &Http) -> Result<String> {
    first_stable(
        loader_versions(http).await?,
        |v| (v.version, v.stable),
        "loader",
    )
}

/// The newest installer version marked stable.
pub async fn latest_stable_installer(http: &Http) -> Result<String> {
    first_stable(
        installer_versions(http).await?,
        |v| (v.version, v.stable),
        "installer",
    )
}

/// URL of the standalone server launcher jar for a given version triple.
#[must_use]
pub fn server_launcher_jar_url(game: &str, loader: &str, installer: &str) -> String {
    format!("{BASE}/versions/loader/{game}/{loader}/{installer}/server/jar")
}

fn first_stable<T>(
    items: Vec<T>,
    project: impl Fn(T) -> (String, bool),
    what: &str,
) -> Result<String> {
    items
        .into_iter()
        .map(project)
        .find_map(|(version, stable)| stable.then_some(version))
        .ok_or_else(|| Error::msg(format!("Fabric meta returned no stable {what} version")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_url_is_shaped_correctly() {
        assert_eq!(
            server_launcher_jar_url("1.21.4", "0.16.9", "1.0.1"),
            "https://meta.fabricmc.net/v2/versions/loader/1.21.4/0.16.9/1.0.1/server/jar"
        );
    }

    #[test]
    fn picks_first_stable_entry() {
        let loaders = vec![
            LoaderVersion {
                version: "0.17.0-beta".into(),
                stable: false,
            },
            LoaderVersion {
                version: "0.16.9".into(),
                stable: true,
            },
            LoaderVersion {
                version: "0.16.8".into(),
                stable: true,
            },
        ];
        let got = first_stable(loaders, |v| (v.version, v.stable), "loader").unwrap();
        assert_eq!(got, "0.16.9");
    }
}
