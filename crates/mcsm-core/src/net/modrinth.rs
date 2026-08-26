//! Modrinth API v2 (`api.modrinth.com/v2`): mod search, version listings,
//! hash-based identification of local jars, bulk update checks, and dependency
//! resolution.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::net::client::Http;

const BASE: &str = "https://api.modrinth.com/v2";
const SERVICE: &str = "Modrinth";

/// One row in a search response.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchHit {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    /// Latest Minecraft version the project supports, as reported by search.
    #[serde(default)]
    pub latest_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub total_hits: u64,
    pub offset: u64,
    pub limit: u64,
}

/// Inputs for a mod search, scoped to the installed server.
#[derive(Debug, Clone)]
pub struct SearchParams {
    pub query: String,
    pub limit: u32,
    pub offset: u32,
    pub minecraft_version: String,
    /// Loader name, always `"fabric"` here but kept explicit.
    pub loader: String,
    /// Exclude mods that only run on the client.
    pub server_side_only: bool,
}

impl SearchParams {
    #[must_use]
    pub fn new(query: impl Into<String>, minecraft_version: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: 20,
            offset: 0,
            minecraft_version: minecraft_version.into(),
            loader: "fabric".to_string(),
            server_side_only: true,
        }
    }

    /// Build the `facets` query parameter Modrinth expects: an array of AND
    /// groups, each group an array of OR alternatives.
    #[must_use]
    pub fn facets_json(&self) -> String {
        let mut groups: Vec<Vec<String>> = vec![
            vec![format!("project_type:{}", "mod")],
            vec![format!("categories:{}", self.loader)],
            vec![format!("versions:{}", self.minecraft_version)],
        ];
        if self.server_side_only {
            groups.push(vec![
                "server_side:required".to_string(),
                "server_side:optional".to_string(),
            ]);
        }
        serde_json::to_string(&groups).expect("facet strings always serialise")
    }
}

/// A published version of a project.
#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    /// RFC 3339; sorts lexicographically for `Z`-offset timestamps.
    #[serde(default)]
    pub date_published: String,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    pub files: Vec<VersionFile>,
}

impl Version {
    /// The file to download: the one marked `primary`, else the first.
    #[must_use]
    pub fn primary_file(&self) -> Option<&VersionFile> {
        self.files
            .iter()
            .find(|f| f.primary)
            .or_else(|| self.files.first())
    }

    #[must_use]
    pub fn supports(&self, mc: &str, loader: &str) -> bool {
        self.game_versions.iter().any(|v| v == mc)
            && self.loaders.iter().any(|l| l == loader)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionFile {
    pub hashes: FileHashes,
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileHashes {
    pub sha1: String,
    pub sha512: String,
}

/// How one project depends on another.
#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    pub dependency_type: DependencyType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyType {
    Required,
    Optional,
    Incompatible,
    /// Bundled inside the parent jar; nothing to install.
    Embedded,
}

/// Result of walking a version's dependency graph.
#[derive(Debug, Default, Clone)]
pub struct Resolution {
    /// The requested version plus every `required` dependency, de-duplicated by
    /// project. The first element is the originally requested version.
    pub to_install: Vec<Version>,
    /// `optional` dependencies, offered to the user but not added automatically.
    pub optional: Vec<String>,
    /// Projects the graph marks `incompatible`; the caller must check these
    /// against what is already installed.
    pub incompatible: Vec<String>,
}

/// Abstraction over "fetch versions for a project" so the resolver can be unit
/// tested without a network.
pub trait VersionResolver {
    fn project_versions(
        &self,
        project_id: &str,
        mc: &str,
        loader: &str,
    ) -> impl std::future::Future<Output = Result<Vec<Version>>>;

    fn get_version(
        &self,
        version_id: &str,
    ) -> impl std::future::Future<Output = Result<Version>>;
}

/// Pick the newest version compatible with `mc` + `loader`.
#[must_use]
pub fn choose_version<'a>(versions: &'a [Version], mc: &str, loader: &str) -> Option<&'a Version> {
    versions
        .iter()
        .filter(|v| v.supports(mc, loader))
        .max_by(|a, b| a.date_published.cmp(&b.date_published))
}

/// Walk `root`'s dependency graph, collecting everything that must be installed.
///
/// * `required` dependencies are resolved recursively (breadth-first, with a
///   visited-set so cycles and diamonds terminate).
/// * `optional` and `incompatible` are recorded, not followed.
/// * `embedded` is ignored.
pub async fn resolve<R: VersionResolver>(
    resolver: &R,
    root: Version,
    mc: &str,
    loader: &str,
) -> Result<Resolution> {
    let mut out = Resolution::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queued: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<Version> = VecDeque::new();

    if !root.project_id.is_empty() {
        queued.insert(root.project_id.clone());
    }
    queue.push_back(root);

    while let Some(version) = queue.pop_front() {
        if !version.project_id.is_empty() && !seen.insert(version.project_id.clone()) {
            continue;
        }

        for dep in &version.dependencies {
            match dep.dependency_type {
                DependencyType::Optional => {
                    if let Some(pid) = &dep.project_id {
                        out.optional.push(pid.clone());
                    }
                }
                DependencyType::Incompatible => {
                    if let Some(pid) = &dep.project_id {
                        out.incompatible.push(pid.clone());
                    }
                }
                DependencyType::Embedded => {}
                DependencyType::Required => {
                    let resolved = resolve_required(resolver, dep, mc, loader).await?;
                    let pid = resolved.project_id.clone();
                    if pid.is_empty() || (!seen.contains(&pid) && queued.insert(pid)) {
                        queue.push_back(resolved);
                    }
                }
            }
        }
        out.to_install.push(version);
    }

    out.optional.retain(|pid| !seen.contains(pid));
    out.optional.sort();
    out.optional.dedup();
    out.incompatible.sort();
    out.incompatible.dedup();
    Ok(out)
}

async fn resolve_required<R: VersionResolver>(
    resolver: &R,
    dep: &Dependency,
    mc: &str,
    loader: &str,
) -> Result<Version> {
    if let Some(vid) = &dep.version_id {
        return resolver.get_version(vid).await;
    }
    let pid = dep.project_id.as_deref().ok_or_else(|| {
        Error::Dependency("a required dependency names neither a project nor a version".into())
    })?;
    let candidates = resolver.project_versions(pid, mc, loader).await?;
    choose_version(&candidates, mc, loader)
        .cloned()
        .ok_or_else(|| {
            Error::Dependency(format!(
                "required dependency `{pid}` has no build for Minecraft {mc} / {loader}"
            ))
        })
}

/// Client over the Modrinth REST API.
#[derive(Debug, Clone)]
pub struct Modrinth {
    http: Http,
}

impl Modrinth {
    #[must_use]
    pub fn new(http: Http) -> Self {
        Self { http }
    }

    pub async fn search(&self, params: &SearchParams) -> Result<SearchResults> {
        let url = format!(
            "{BASE}/search?query={}&limit={}&offset={}&index=relevance&facets={}",
            urlencode(&params.query),
            params.limit,
            params.offset,
            urlencode(&params.facets_json()),
        );
        self.http.get_json(SERVICE, &url).await
    }

    pub async fn get_version(&self, version_id: &str) -> Result<Version> {
        self.http
            .get_json(SERVICE, &format!("{BASE}/version/{version_id}"))
            .await
    }

    pub async fn project_versions(
        &self,
        project: &str,
        mc: &str,
        loader: &str,
    ) -> Result<Vec<Version>> {
        let url = format!(
            "{BASE}/project/{project}/version?loaders=[\"{loader}\"]&game_versions=[\"{mc}\"]"
        );
        self.http.get_json(SERVICE, &urlencode_query(&url)).await
    }

    /// Identify local jars by SHA-512: returns `hash -> Version` for the ones
    /// Modrinth recognises. Jars added by hand are matched too.
    pub async fn versions_by_hash(
        &self,
        sha512_hashes: &[String],
    ) -> Result<HashMap<String, Version>> {
        if sha512_hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let body = HashLookup {
            hashes: sha512_hashes,
            algorithm: "sha512",
        };
        self.http
            .post_json(SERVICE, &format!("{BASE}/version_files"), &body)
            .await
    }

    /// For each hash, the latest version compatible with `mc` + `loader`
    /// (Modrinth's bulk update endpoint). Absent from the map means up to date
    /// or unknown.
    pub async fn check_updates(
        &self,
        sha512_hashes: &[String],
        mc: &str,
        loader: &str,
    ) -> Result<HashMap<String, Version>> {
        if sha512_hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let body = UpdateLookup {
            hashes: sha512_hashes,
            algorithm: "sha512",
            loaders: [loader],
            game_versions: [mc],
        };
        self.http
            .post_json(SERVICE, &format!("{BASE}/version_files/update"), &body)
            .await
    }
}

impl VersionResolver for Modrinth {
    async fn project_versions(
        &self,
        project_id: &str,
        mc: &str,
        loader: &str,
    ) -> Result<Vec<Version>> {
        Modrinth::project_versions(self, project_id, mc, loader).await
    }

    async fn get_version(&self, version_id: &str) -> Result<Version> {
        Modrinth::get_version(self, version_id).await
    }
}

#[derive(Serialize)]
struct HashLookup<'a> {
    hashes: &'a [String],
    algorithm: &'a str,
}

#[derive(Serialize)]
struct UpdateLookup<'a> {
    hashes: &'a [String],
    algorithm: &'a str,
    loaders: [&'a str; 1],
    game_versions: [&'a str; 1],
}

/// Percent-encode a query-parameter value.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Encode only the part of `url` after the first `?`, leaving the path intact.
fn urlencode_query(url: &str) -> String {
    match url.split_once('?') {
        None => url.to_string(),
        Some((path, query)) => {
            let encoded: Vec<String> = query
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((k, v)) => format!("{k}={}", urlencode(v)),
                    None => pair.to_string(),
                })
                .collect();
            format!("{path}?{}", encoded.join("&"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(id: &str, project: &str, mc: &[&str], deps: Vec<Dependency>) -> Version {
        Version {
            id: id.into(),
            project_id: project.into(),
            name: id.into(),
            version_number: id.into(),
            game_versions: mc.iter().map(|s| (*s).to_string()).collect(),
            loaders: vec!["fabric".into()],
            date_published: format!("2025-01-01T00:00:0{}Z", id.len() % 10),
            dependencies: deps,
            files: vec![VersionFile {
                hashes: FileHashes {
                    sha1: "".into(),
                    sha512: format!("{id}-sha"),
                },
                url: format!("https://cdn/{id}.jar"),
                filename: format!("{id}.jar"),
                primary: true,
                size: 1,
            }],
        }
    }

    fn req(project: &str) -> Dependency {
        Dependency {
            version_id: None,
            project_id: Some(project.into()),
            file_name: None,
            dependency_type: DependencyType::Required,
        }
    }

    struct MapResolver {
        by_project: HashMap<String, Vec<Version>>,
    }

    impl VersionResolver for MapResolver {
        async fn project_versions(
            &self,
            project_id: &str,
            _mc: &str,
            _loader: &str,
        ) -> Result<Vec<Version>> {
            Ok(self.by_project.get(project_id).cloned().unwrap_or_default())
        }
        async fn get_version(&self, version_id: &str) -> Result<Version> {
            self.by_project
                .values()
                .flatten()
                .find(|v| v.id == version_id)
                .cloned()
                .ok_or_else(|| Error::Dependency(format!("no such version {version_id}")))
        }
    }

    #[test]
    fn facets_include_loader_version_and_server_side() {
        let p = SearchParams::new("sodium", "1.21.4");
        let f = p.facets_json();
        assert!(f.contains("project_type:mod"));
        assert!(f.contains("categories:fabric"));
        assert!(f.contains("versions:1.21.4"));
        assert!(f.contains("server_side:required"));
    }

    #[test]
    fn choose_version_prefers_newest_compatible() {
        let vs = vec![
            Version { date_published: "2024-01-01T00:00:00Z".into(), ..version("old", "p", &["1.21.4"], vec![]) },
            Version { date_published: "2025-06-01T00:00:00Z".into(), ..version("new", "p", &["1.21.4"], vec![]) },
            Version { date_published: "2025-09-01T00:00:00Z".into(), ..version("wrongmc", "p", &["1.20.1"], vec![]) },
        ];
        assert_eq!(choose_version(&vs, "1.21.4", "fabric").unwrap().id, "new");
    }

    #[tokio::test]
    async fn resolves_required_chain_and_dedups_diamond() {
        // root -> a, b ; a -> c ; b -> c   (c must appear exactly once)
        let mut by_project = HashMap::new();
        by_project.insert("a".to_string(), vec![version("a1", "a", &["1.21.4"], vec![req("c")])]);
        by_project.insert("b".to_string(), vec![version("b1", "b", &["1.21.4"], vec![req("c")])]);
        by_project.insert("c".to_string(), vec![version("c1", "c", &["1.21.4"], vec![])]);
        let resolver = MapResolver { by_project };

        let root = version("root1", "root", &["1.21.4"], vec![req("a"), req("b")]);
        let res = resolve(&resolver, root, "1.21.4", "fabric").await.unwrap();

        let projects: Vec<&str> = res.to_install.iter().map(|v| v.project_id.as_str()).collect();
        assert_eq!(projects[0], "root");
        assert_eq!(res.to_install.len(), 4, "root + a + b + c, c only once");
        assert_eq!(projects.iter().filter(|p| **p == "c").count(), 1);
    }

    #[tokio::test]
    async fn records_optional_and_incompatible_without_following_them() {
        let resolver = MapResolver { by_project: HashMap::new() };
        let root = version(
            "r1",
            "root",
            &["1.21.4"],
            vec![
                Dependency { dependency_type: DependencyType::Optional, ..req("nice-to-have") },
                Dependency { dependency_type: DependencyType::Incompatible, ..req("conflicts") },
                Dependency { dependency_type: DependencyType::Embedded, ..req("bundled") },
            ],
        );
        let res = resolve(&resolver, root, "1.21.4", "fabric").await.unwrap();
        assert_eq!(res.to_install.len(), 1);
        assert_eq!(res.optional, vec!["nice-to-have"]);
        assert_eq!(res.incompatible, vec!["conflicts"]);
    }

    #[tokio::test]
    async fn missing_required_build_is_an_error() {
        let resolver = MapResolver { by_project: HashMap::new() };
        let root = version("r1", "root", &["1.21.4"], vec![req("gone")]);
        let err = resolve(&resolver, root, "1.21.4", "fabric").await.unwrap_err();
        assert!(matches!(err, Error::Dependency(_)));
    }

    #[test]
    fn query_encoder_leaves_path_alone() {
        let got = urlencode_query("https://x/y?loaders=[\"fabric\"]&game_versions=[\"1.21.4\"]");
        assert_eq!(
            got,
            "https://x/y?loaders=%5B%22fabric%22%5D&game_versions=%5B%221.21.4%22%5D"
        );
    }
}
