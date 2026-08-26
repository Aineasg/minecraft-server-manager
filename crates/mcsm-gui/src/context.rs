//! Shared state handed to every page: the on-disk layout, HTTP clients, and the
//! live [`AppState`] (settings).
//!
//! `AppState` lives behind `Rc<RefCell<_>>` because the GTK main loop is
//! single-threaded — every page runs on it, so there is no cross-thread access
//! and a mutex would only add ceremony. Background work never touches this;
//! it is handed owned copies of what it needs.

use std::cell::RefCell;
use std::rc::Rc;

use mcsm_core::net::modrinth::Modrinth;
use mcsm_core::net::Http;
use mcsm_core::ops::server::ServerConfig;
use mcsm_core::{AppState, Paths};

/// Filename of the Fabric launcher jar inside the server directory.
pub const LAUNCHER_JAR: &str = "fabric-server-launch.jar";

#[derive(Clone)]
pub struct Context {
    pub paths: Paths,
    pub http: Http,
    pub modrinth: Modrinth,
    pub state: Rc<RefCell<AppState>>,
}

impl Context {
    pub fn new(paths: Paths, state: AppState) -> anyhow::Result<Self> {
        let http = Http::new()?;
        Ok(Self {
            modrinth: Modrinth::new(http.clone()),
            http,
            paths,
            state: Rc::new(RefCell::new(state)),
        })
    }

    /// Persist the current [`AppState`] to `data/state.toml`.
    pub fn save_state(&self) -> mcsm_core::Result<()> {
        self.state.borrow().save(&self.paths.state_file)
    }

    /// `(minecraft_version, loader_version)` if the server is installed.
    pub fn installed_versions(&self) -> Option<(String, String)> {
        let s = self.state.borrow();
        Some((s.minecraft_version.clone()?, s.loader_version.clone()?))
    }

    /// A launch configuration built from the current settings.
    pub fn server_config(&self) -> ServerConfig {
        let s = self.state.borrow();
        ServerConfig {
            server_dir: self.paths.server.clone(),
            java: s.java_command(),
            jvm_args: s.jvm_args(),
            launcher_jar: LAUNCHER_JAR.to_string(),
            budget: s.budget(),
        }
    }
}
