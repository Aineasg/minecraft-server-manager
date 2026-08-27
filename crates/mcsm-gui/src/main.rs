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

/// Answer `--version` / `--help` before any GTK, logging or filesystem work, so
/// they work headless (e.g. over SSH) and never create or read the data
/// directory. Returns `Some(exit_code)` when `main` should exit immediately.
fn handle_cli_args() -> Option<i32> {
    // The GUI takes no real arguments; anything here is an informational flag.
    match std::env::args().nth(1).as_deref() {
        None => None,
        Some("--version" | "-V") => {
            println!("mcsm {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        Some("--help" | "-h") => {
            print!(concat!(
                "Minecraft Server Manager — desktop app for a Fabric Minecraft server.\n\n",
                "Usage: mcsm [--version | --help]\n\n",
                "With no arguments it opens the window. Configuration:\n",
                "  MCSM_ROOT   directory to keep the server, world and settings in\n",
                "  MCSM_LOG    log filter, e.g. `debug` or `info,mcsm=trace`\n",
            ));
            Some(0)
        }
        Some(other) => {
            eprintln!("mcsm: unknown option {other:?} (try --help)");
            Some(2)
        }
    }
}

fn main() -> anyhow::Result<()> {
    if let Some(code) = handle_cli_args() {
        std::process::exit(code);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("MCSM_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info,mcsm=debug")),
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
    tracing::info!(backups = %context.backup_dir().display(), "backup folder");

    let app = RelmApp::new(APP_ID);
    // Match the installed .desktop / hicolor icon so the window and taskbar
    // show the app icon.
    relm4::gtk::Window::set_default_icon_name(APP_ID);
    app.run::<App>(AppInit { context });
    Ok(())
}
