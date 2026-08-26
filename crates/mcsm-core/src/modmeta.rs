//! Reading identity metadata out of a mod jar, and hashing jars for Modrinth
//! lookups.
//!
//! A Fabric mod jar carries a `fabric.mod.json` at its root; a Quilt mod jar
//! carries `quilt.mod.json`. Both are (loosely) JSON. We pull out just enough to
//! label the mod in the UI: id, display name, version.

use std::io::Read as _;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

pub use crate::hash::sha512_hex;

/// Identity of a mod, as declared inside its jar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModMeta {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize)]
struct FabricModJson {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct QuiltModJson {
    quilt_loader: QuiltLoader,
}

#[derive(Deserialize)]
struct QuiltLoader {
    id: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    metadata: Option<QuiltMetadata>,
}

#[derive(Deserialize)]
struct QuiltMetadata {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// Read `fabric.mod.json` (or `quilt.mod.json`) from a jar on disk.
pub fn read_from_jar(path: &Path) -> Result<ModMeta> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| Error::ModArchive {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;

    if let Some(text) = read_entry(&mut zip, "fabric.mod.json") {
        let parsed: FabricModJson = parse_lenient(&text).map_err(|e| Error::ModArchive {
            path: path.to_path_buf(),
            reason: format!("fabric.mod.json: {e}"),
        })?;
        return Ok(ModMeta {
            id: parsed.id,
            name: parsed.name,
            version: clean_version(parsed.version),
            description: parsed.description,
        });
    }

    if let Some(text) = read_entry(&mut zip, "quilt.mod.json") {
        let parsed: QuiltModJson = parse_lenient(&text).map_err(|e| Error::ModArchive {
            path: path.to_path_buf(),
            reason: format!("quilt.mod.json: {e}"),
        })?;
        let meta = parsed.quilt_loader.metadata;
        return Ok(ModMeta {
            id: parsed.quilt_loader.id,
            name: meta.as_ref().and_then(|m| m.name.clone()),
            version: clean_version(parsed.quilt_loader.version),
            description: meta.and_then(|m| m.description),
        });
    }

    Err(Error::ModArchive {
        path: path.to_path_buf(),
        reason: "no fabric.mod.json or quilt.mod.json".into(),
    })
}

fn read_entry<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    let mut text = String::new();
    entry.read_to_string(&mut text).ok()?;
    Some(text)
}

/// Parse JSON, retrying once with `//` and `/* */` comments stripped — some
/// `fabric.mod.json` files use them even though strict JSON forbids it.
fn parse_lenient<T: for<'de> Deserialize<'de>>(
    text: &str,
) -> std::result::Result<T, serde_json::Error> {
    match serde_json::from_str(text) {
        Ok(v) => Ok(v),
        Err(_) => serde_json::from_str(&strip_json_comments(text)),
    }
}

fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Drop unresolved build placeholders like `"${version}"`.
fn clean_version(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.contains("${"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn jar_with(entry: &str, body: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "mcsm-jar-{}-{}-{}.jar",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            entry.replace('.', "_")
        ));
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>(entry, zip::write::FileOptions::default())
            .unwrap();
        zip.write_all(body.as_bytes()).unwrap();
        zip.finish().unwrap();
        path
    }

    #[test]
    fn reads_fabric_mod_json() {
        let path = jar_with(
            "fabric.mod.json",
            r#"{"schemaVersion":1,"id":"sodium","name":"Sodium","version":"0.5.8"}"#,
        );
        let meta = read_from_jar(&path).unwrap();
        assert_eq!(meta.id, "sodium");
        assert_eq!(meta.name.as_deref(), Some("Sodium"));
        assert_eq!(meta.version.as_deref(), Some("0.5.8"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tolerates_comments_in_fabric_mod_json() {
        let path = jar_with(
            "fabric.mod.json",
            "{\n  // the mod id\n  \"id\": \"lithium\",\n  \"version\": \"0.12.1\"\n}",
        );
        let meta = read_from_jar(&path).unwrap();
        assert_eq!(meta.id, "lithium");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn drops_unresolved_version_placeholder() {
        let path = jar_with(
            "fabric.mod.json",
            r#"{"id":"broken","version":"${version}"}"#,
        );
        let meta = read_from_jar(&path).unwrap();
        assert_eq!(meta.version, None);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn strip_comments_keeps_string_slashes() {
        let src = r#"{"url":"https://example.com","x":1 /* c */}"#;
        let out = strip_json_comments(src);
        assert!(out.contains("https://example.com"));
        assert!(!out.contains("/* c */"));
    }
}
