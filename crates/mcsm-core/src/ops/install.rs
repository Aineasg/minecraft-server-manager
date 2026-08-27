//! Installing (or re-installing) the server: fetch the vanilla server jar and
//! the Fabric launcher jar into the content-addressed cache, place them in the
//! server directory, and write `eula.txt`.

use std::path::Path;

use crate::error::{Error, Result};
use crate::hash::sha1_hex;
use crate::net::{fabric, mojang, Http};
use crate::paths::Paths;
use crate::util::write_atomic;

/// The version triple to install.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub minecraft_version: String,
    pub loader_version: String,
    pub installer_version: String,
    /// Writing `eula=true` requires the user to have accepted the Minecraft EULA.
    pub accept_eula: bool,
}

/// Progress callback payload.
#[derive(Debug, Clone)]
pub enum InstallProgress {
    /// A new phase started.
    Step(String),
    /// Byte progress for the current download.
    Download {
        name: String,
        downloaded: u64,
        total: Option<u64>,
    },
}

/// Run the full install. Idempotent: cached jars that already verify are reused.
pub async fn install(
    http: &Http,
    paths: &Paths,
    plan: &InstallPlan,
    mut progress: impl FnMut(InstallProgress),
) -> Result<()> {
    paths.ensure_dirs()?;

    // --- vanilla server jar -------------------------------------------------
    progress(InstallProgress::Step(format!(
        "Resolving Minecraft {} server download",
        plan.minecraft_version
    )));
    let manifest = mojang::version_manifest(http).await?;
    let server_dl = mojang::server_download(http, &manifest, &plan.minecraft_version).await?;

    let vanilla_cache = paths
        .cache
        .join(format!("minecraft-server-{}.jar", plan.minecraft_version));
    ensure_cached(
        http,
        "Mojang",
        &server_dl.url,
        &vanilla_cache,
        Some((&server_dl.sha1, server_dl.size)),
        "minecraft-server.jar",
        &mut progress,
    )
    .await?;
    copy_into_place(&vanilla_cache, &paths.server_file("server.jar"))?;

    // --- Fabric launcher jar ---------------------------------------------------
    progress(InstallProgress::Step(
        "Downloading Fabric server launcher".into(),
    ));
    let fabric_url = fabric::server_launcher_jar_url(
        &plan.minecraft_version,
        &plan.loader_version,
        &plan.installer_version,
    );
    let fabric_cache = paths.cache.join(format!(
        "fabric-server-{}-{}-{}.jar",
        plan.minecraft_version, plan.loader_version, plan.installer_version
    ));
    ensure_cached(
        http,
        "Fabric meta",
        &fabric_url,
        &fabric_cache,
        None,
        "fabric-server-launch.jar",
        &mut progress,
    )
    .await?;
    copy_into_place(
        &fabric_cache,
        &paths.server_file("fabric-server-launch.jar"),
    )?;

    // --- eula.txt ------------------------------------------------------------
    progress(InstallProgress::Step("Writing eula.txt".into()));
    let eula_body = format!(
        "# Managed by minecraft-server-manager\n# https://aka.ms/MinecraftEULA\neula={}\n",
        plan.accept_eula
    );
    write_atomic(&paths.server_file("eula.txt"), eula_body.as_bytes())?;

    Ok(())
}

/// Download `url` to `dest` unless a valid copy is already cached.
async fn ensure_cached(
    http: &Http,
    service: &'static str,
    url: &str,
    dest: &Path,
    verify: Option<(&str, u64)>,
    display_name: &str,
    progress: &mut impl FnMut(InstallProgress),
) -> Result<()> {
    if dest.is_file() && cache_is_valid(dest, verify) {
        progress(InstallProgress::Step(format!(
            "Using cached {display_name}"
        )));
        return Ok(());
    }

    let name = display_name.to_string();
    http.download_to_file(service, url, dest, |downloaded, total| {
        progress(InstallProgress::Download {
            name: name.clone(),
            downloaded,
            total,
        });
    })
    .await?;

    if !cache_is_valid(dest, verify) {
        let _ = std::fs::remove_file(dest);
        return Err(Error::msg(format!(
            "{display_name} failed verification after download"
        )));
    }
    Ok(())
}

fn cache_is_valid(path: &Path, verify: Option<(&str, u64)>) -> bool {
    let Some((expected_sha1, expected_size)) = verify else {
        // No checksum available (Fabric): accept only a non-empty file that
        // actually begins with ZIP magic. An error page from a captive portal
        // or proxy must not be cached forever as `fabric-server-launch.jar`.
        return std::fs::metadata(path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
            && starts_with_zip_magic(path);
    };
    let size_ok = std::fs::metadata(path)
        .map(|m| m.len() == expected_size)
        .unwrap_or(false);
    size_ok
        && sha1_hex(path)
            .map(|got| got.eq_ignore_ascii_case(expected_sha1))
            .unwrap_or(false)
}

/// Whether the first four bytes of `path` are the ZIP local-file header
/// (`PK\x03\x04`) — every jar is a zip, no jar is anything else.
fn starts_with_zip_magic(path: &Path) -> bool {
    use std::io::Read as _;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map(|_| magic == [0x50, 0x4b, 0x03, 0x04])
        .unwrap_or(false)
}

fn copy_into_place(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::copy(from, to).map_err(|e| Error::io(to, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fabric_cache_entries_must_look_like_a_zip() {
        let dir = std::env::temp_dir().join(format!("mcsm-install-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let html = dir.join("launcher.jar");
        std::fs::write(&html, b"<html>captive portal</html>").unwrap();
        assert!(!starts_with_zip_magic(&html));
        assert!(!cache_is_valid(&html, None), "an error page must not pass");

        let zip = dir.join("real.jar");
        std::fs::write(&zip, b"PK\x03\x04 followed by real archive bytes").unwrap();
        assert!(starts_with_zip_magic(&zip));
        assert!(cache_is_valid(&zip, None));

        let truncated = dir.join("tiny.jar");
        std::fs::write(&truncated, b"PK").unwrap();
        assert!(!starts_with_zip_magic(&truncated));

        std::fs::remove_dir_all(&dir).ok();
    }
}
