//! Error type shared across the core crate.
//!
//! We use one flat enum rather than per-module error types: the set of things
//! that can go wrong is small, the GUI only ever needs a human-readable string,
//! and a single type keeps `?` working everywhere without conversion boilerplate.

use std::path::PathBuf;

/// Result alias used throughout `mcsm-core`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// An i/o error with no meaningful path (pipes, process stdio, ...).
    #[error("i/o error: {0}")]
    IoBare(#[from] std::io::Error),

    #[error("could not locate the manager root directory: {0}")]
    RootNotFound(String),

    #[error("HTTP request to {url} failed: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("{service} returned HTTP {status} for {url}")]
    HttpStatus {
        service: &'static str,
        status: u16,
        url: String,
    },

    #[error("could not parse {what}: {source}")]
    Json {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("could not parse TOML in {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not serialise TOML: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("invalid mod archive {path}: {reason}")]
    ModArchive { path: PathBuf, reason: String },

    #[error("the server is already running (systemd scope {unit} is active)")]
    ServerAlreadyRunning { unit: String },

    #[error("the server is not running")]
    ServerNotRunning,

    #[error("failed to launch the server process: {0}")]
    Spawn(String),

    /// A dependency graph from Modrinth could not be satisfied.
    #[error("dependency resolution failed: {0}")]
    Dependency(String),

    #[error("{0}")]
    Message(String),
}

impl Error {
    /// Build an [`Error::Io`] that remembers which file was involved.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn msg(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}
