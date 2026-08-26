//! Minecraft Server Manager — GTK4 / libadwaita front-end.

mod app;
mod context;
mod ui;

use anyhow::Context as _;
use mcsm_core::{AppState, Paths};
use relm4::RelmApp;
use tracing_subscriber::EnvFilter;

use crate::app::{App, AppInit};
use crate::context::Context;

const APP_ID: &str = "dev.aineasg.MinecraftServerManager";

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("MCSM_LOG").unwrap_or_else(|_| EnvFilter::new("info,mcsm=debug")),
        )
        .init();

    // Resolve the self-contained root and make sure every data directory exists.
    let paths = Paths::discover().context("locating the manager directory")?;
    paths
        .ensure_dirs()
        .context("creating the data directories")?;
    tracing::info!(root = %paths.root.display(), "manager root");

    let state = AppState::load(&paths.state_file).context("loading state.toml")?;
    let context = Context::new(paths.clone(), state).context("initialising HTTP clients")?;

    let app = RelmApp::new(APP_ID);
    app.run::<App>(AppInit { context });
    Ok(())
}
