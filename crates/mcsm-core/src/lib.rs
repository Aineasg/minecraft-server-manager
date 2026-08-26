//! Core logic for the Minecraft Server Manager.
//!
//! This crate has no GUI dependency. It knows how to:
//!
//! * lay out and locate the self-contained manager directory ([`paths`]);
//! * read and write `server.properties` without disturbing comments or order
//!   ([`properties`], [`properties_catalog`]);
//! * turn a single memory ceiling into JVM and cgroup limits ([`memory`]);
//! * persist app settings ([`state`]);
//! * edit the player-access JSON files ([`access`]);
//! * talk to Fabric, Mojang and Modrinth ([`net`]);
//! * install jars, manage mods, take backups, and supervise the server
//!   process ([`ops`]).
//!
//! The GUI crate (`mcsm-gui`) is a thin Relm4 layer over these pieces.

pub mod access;
pub mod error;
pub mod hash;
pub mod memory;
pub mod modmeta;
pub mod net;
pub mod ops;
pub mod paths;
pub mod properties;
pub mod properties_catalog;
pub mod state;
pub mod util;

pub use error::{Error, Result};
pub use paths::Paths;
pub use state::AppState;
