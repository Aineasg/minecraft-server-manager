//! HTTP clients for the three services the manager talks to: Fabric metadata,
//! Mojang (version manifest + profiles) and Modrinth (mods).

pub mod client;
pub mod fabric;
pub mod modrinth;
pub mod mojang;

pub use client::Http;
