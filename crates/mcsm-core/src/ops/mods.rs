//! The `mods/` directory: enumerating what is installed, toggling mods on and
//! off, and installing new ones (with their required dependencies) from Modrinth.
//!
//! A mod is "disabled" by renaming `foo.jar` to `foo.jar.disabled` — Fabric
//! ignores it, but it stays put so it can be re-enabled without downloading
//! again.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::hash::sha512_hex;
use crate::modmeta::{self, ModMeta};
use crate::net::modrinth::{self, Modrinth, Resolution, Version};
use crate::net::Http;
use crate::paths::Paths;

const DISABLED_SUFFIX: &str = ".disabled";

/// One jar in the `mods/` directory.
#[derive(Debug, Clone)]
pub struct InstalledMod {
    /// Absolute path to the jar (with or without the `.disabled` suffix).
    pub path: PathBuf,
    /// The active filename, e.g. `sodium-fabric-0.5.8.jar`.
    pub jar_name: String,
    pub enabled: bool,
    /// Identity from the jar's `fabric.mod.json`, if it could be read.
    pub meta: Option<ModMeta>,
    pub sha512: String,
}

impl InstalledMod {
    /// Best available human label.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.meta
            .as_ref()
            .and_then(|m| m.name.as_deref().or(Some(m.id.as_str())))
            .unwrap_or(&self.jar_name)
    }

    #[must_use]
    pub fn version_label(&self) -> Option<&str> {
        self.meta.as_ref().and_then(|m| m.version.as_deref())
    }
}

/// Enumerate `mods/`, hashing and reading metadata for each jar.
pub fn scan(paths: &Paths) -> Result<Vec<InstalledMod>> {
    let dir = &paths.mods;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut mods = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let (jar_name, enabled) = match name.strip_suffix(DISABLED_SUFFIX) {
            Some(base) if base.ends_with(".jar") => (base.to_string(), false),
            _ if name.ends_with(".jar") => (name.clone(), true),
            _ => continue,
        };

        let sha512 = sha512_hex(&path)?;
        let meta = modmeta::read_from_jar(&path).ok();
        mods.push(InstalledMod {
            path,
            jar_name,
            enabled,
            meta,
            sha512,
        });
    }

    mods.sort_by_key(|m| m.display_name().to_lowercase());
    Ok(mods)
}

/// Enable or disable a mod by (un)suffixing its filename. Returns the new path.
pub fn set_enabled(target: &InstalledMod, enabled: bool) -> Result<PathBuf> {
    if target.enabled == enabled {
        return Ok(target.path.clone());
    }
    let new_path = if enabled {
        target.path.with_file_name(&target.jar_name)
    } else {
        target
            .path
            .with_file_name(format!("{}{DISABLED_SUFFIX}", target.jar_name))
    };
    std::fs::rename(&target.path, &new_path).map_err(|e| Error::io(&new_path, e))?;
    Ok(new_path)
}

/// Permanently delete a mod jar.
pub fn remove(target: &InstalledMod) -> Result<()> {
    std::fs::remove_file(&target.path).map_err(|e| Error::io(&target.path, e))
}

/// Download one Modrinth [`Version`]'s primary file into `mods/`, via the cache.
/// Returns the installed path, or `None` if that exact file is already present.
pub async fn install_version(
    http: &Http,
    paths: &Paths,
    version: &Version,
) -> Result<Option<PathBuf>> {
    let file = version
        .primary_file()
        .ok_or_else(|| Error::msg(format!("Modrinth version {} has no file", version.id)))?;

    // `filename` and `hashes.sha512` come straight off the network. Both are
    // joined onto our directories, so neither is trusted as-is: a filename
    // carrying a separator (or `..`) would escape `cache/` and `mods/`, and a
    // short hash would panic the prefix slice below.
    let filename = safe_file_name(&file.filename)?;
    let hash_prefix = file.hashes.sha512.get(..16).ok_or_else(|| {
        Error::msg(format!(
            "Modrinth returned a malformed SHA-512 for {filename}"
        ))
    })?;

    // Already installed (same content)?
    if scan(paths)?.iter().any(|m| m.sha512 == file.hashes.sha512) {
        return Ok(None);
    }

    let cache_path = paths.cache.join(format!("{hash_prefix}-{filename}"));
    if !cache_path.is_file() {
        http.download_to_file("Modrinth CDN", &file.url, &cache_path, |_, _| {})
            .await?;
    }
    if sha512_hex(&cache_path)? != file.hashes.sha512 {
        let _ = std::fs::remove_file(&cache_path);
        return Err(Error::msg(format!(
            "{filename} failed SHA-512 verification"
        )));
    }

    let dest = paths.mods.join(filename);
    std::fs::create_dir_all(&paths.mods).map_err(|e| Error::io(&paths.mods, e))?;
    std::fs::copy(&cache_path, &dest).map_err(|e| Error::io(&dest, e))?;
    Ok(Some(dest))
}

/// Reduce a filename from a remote API to a bare file-name component, refusing
/// anything that would resolve outside the directory it is joined onto.
fn safe_file_name(raw: &str) -> Result<&str> {
    let rejected = || {
        Error::msg(format!(
            "Modrinth returned an unusable file name `{raw}`; refusing to write it"
        ))
    };
    if raw.contains('/') || raw.contains('\\') || raw.contains('\0') {
        return Err(rejected());
    }
    let name = raw.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err(rejected());
    }
    Ok(name)
}

/// Replace an installed mod with a newer Modrinth [`Version`].
///
/// Installs the new jar, then deletes the old one **only if** the update landed
/// under a different filename. When the new version keeps the same name,
/// [`install_version`] has already overwritten it in place and deleting `old`
/// would take the update with it. A no-op (`None` from `install_version`, i.e.
/// the new content was already present) still removes the stale jar.
///
/// The one exception is `old` *being* the requested version already: then the
/// "already present" no-op refers to `old` itself and removing it would delete
/// the mod outright. Today's callers cannot reach that — [`check_updates`] drops
/// any version whose file is already installed — but this is a public API, so
/// the identity case is checked here rather than relied on upstream.
pub async fn update(
    http: &Http,
    paths: &Paths,
    old: &InstalledMod,
    new_version: &Version,
) -> Result<()> {
    if new_version
        .primary_file()
        .is_some_and(|f| f.hashes.sha512 == old.sha512)
    {
        return Ok(()); // already this version: nothing to install, nothing to delete
    }
    let installed = install_version(http, paths, new_version).await?;
    if should_remove_old(installed.as_deref(), &old.path) {
        remove(old)?;
    }
    Ok(())
}

/// Whether the previous jar at `old_path` should be deleted after an update
/// installed to `new_path` (`None` = nothing was written because the content
/// was already present).
fn should_remove_old(new_path: Option<&std::path::Path>, old_path: &std::path::Path) -> bool {
    new_path != Some(old_path)
}

/// Install a resolved dependency set. Returns the paths that were newly written.
pub async fn apply_resolution(
    http: &Http,
    paths: &Paths,
    resolution: &Resolution,
) -> Result<Vec<PathBuf>> {
    let mut installed = Vec::new();
    for version in &resolution.to_install {
        if let Some(path) = install_version(http, paths, version).await? {
            installed.push(path);
        }
    }
    Ok(installed)
}

/// Resolve and install a project's newest compatible version plus dependencies.
pub async fn install_project(
    http: &Http,
    modrinth: &Modrinth,
    paths: &Paths,
    project_id: &str,
    mc: &str,
    loader: &str,
) -> Result<Resolution> {
    let candidates = modrinth.project_versions(project_id, mc, loader).await?;
    let root = modrinth::choose_version(&candidates, mc, loader)
        .cloned()
        .ok_or_else(|| {
            Error::Dependency(format!(
                "`{project_id}` has no build for Minecraft {mc} / {loader}"
            ))
        })?;
    let resolution = modrinth::resolve(modrinth, root, mc, loader).await?;
    apply_resolution(http, paths, &resolution).await?;
    Ok(resolution)
}

/// For every installed jar Modrinth recognises, the newest compatible version
/// when it differs from what is installed. Keyed by the installed jar's SHA-512.
pub async fn check_updates(
    modrinth: &Modrinth,
    installed: &[InstalledMod],
    mc: &str,
    loader: &str,
) -> Result<HashMap<String, Version>> {
    let hashes: Vec<String> = installed.iter().map(|m| m.sha512.clone()).collect();
    let mut latest = modrinth.check_updates(&hashes, mc, loader).await?;
    // Drop entries that resolve to the file we already have.
    let have: std::collections::HashSet<&str> =
        installed.iter().map(|m| m.sha512.as_str()).collect();
    latest.retain(|_, v| {
        v.primary_file()
            .map(|f| !have.contains(f.hashes.sha512.as_str()))
            .unwrap_or(false)
    });
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::Path;

    fn write_jar(dir: &Path, name: &str, mod_id: &str) {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("fabric.mod.json", zip::write::FileOptions::default())
            .unwrap();
        write!(
            zip,
            r#"{{"id":"{mod_id}","name":"{mod_id}","version":"1.0.0"}}"#
        )
        .unwrap();
        zip.finish().unwrap();
    }

    fn temp_paths() -> Paths {
        let root = std::env::temp_dir().join(format!(
            "mcsm-mods-{}-{}",
            std::process::id(),
            fastrand_like()
        ));
        let paths = Paths::with_root(&root);
        paths.ensure_dirs().unwrap();
        paths
    }

    fn fastrand_like() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn scan_classifies_enabled_and_disabled() {
        let paths = temp_paths();
        write_jar(&paths.mods, "alpha.jar", "alpha");
        write_jar(&paths.mods, "beta.jar.disabled", "beta");

        let mods = scan(&paths).unwrap();
        assert_eq!(mods.len(), 2);
        let alpha = mods.iter().find(|m| m.jar_name == "alpha.jar").unwrap();
        let beta = mods.iter().find(|m| m.jar_name == "beta.jar").unwrap();
        assert!(alpha.enabled);
        assert!(!beta.enabled);
        assert_eq!(beta.display_name(), "beta");

        std::fs::remove_dir_all(&paths.root).ok();
    }

    #[test]
    fn toggle_round_trips_the_suffix() {
        let paths = temp_paths();
        write_jar(&paths.mods, "gamma.jar", "gamma");
        let mods = scan(&paths).unwrap();
        let gamma = &mods[0];

        let disabled = set_enabled(gamma, false).unwrap();
        assert!(disabled.to_string_lossy().ends_with("gamma.jar.disabled"));
        assert!(!paths.mods.join("gamma.jar").exists());

        let rescanned = scan(&paths).unwrap();
        let again = set_enabled(&rescanned[0], true).unwrap();
        assert!(again.to_string_lossy().ends_with("gamma.jar"));

        std::fs::remove_dir_all(&paths.root).ok();
    }

    #[test]
    fn update_keeps_old_jar_only_when_new_reused_its_path() {
        let same = std::path::Path::new("/m/sodium.jar");
        let other = std::path::Path::new("/m/sodium-0.6.jar");

        // New version wrote a different filename: delete the old one.
        assert!(should_remove_old(Some(other), same));
        // New version overwrote the same file in place: keep it (it *is* the update).
        assert!(!should_remove_old(Some(same), same));
        // Nothing was written (content already present): still drop the stale jar.
        // `update` only reaches this with a *different* jar holding that content;
        // the case where `old` itself is the requested version short-circuits
        // before `install_version` (see `update`).
        assert!(should_remove_old(None, same));
    }

    #[test]
    fn remote_file_names_cannot_escape_the_mods_directory() {
        assert_eq!(safe_file_name("sodium-0.6.jar").unwrap(), "sodium-0.6.jar");
        for hostile in [
            "../../../.bashrc",
            "sub/dir.jar",
            "..\\windows.jar",
            "..",
            ".",
            "",
            "   ",
        ] {
            assert!(
                safe_file_name(hostile).is_err(),
                "`{hostile}` should be rejected"
            );
        }
    }
}
