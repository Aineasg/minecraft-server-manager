//! A curated description of the common `server.properties` keys: their type,
//! valid range, default, and a one-line explanation.
//!
//! The GUI uses this to render a typed form (toggles, spinners, dropdowns) with
//! helpful tooltips. Keys **not** in this catalogue still round-trip fine
//! through [`crate::properties::Properties`]; the GUI just shows them as plain
//! text fields in an "Other" section.

/// How a value should be presented and validated.
#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    Bool,
    Int { min: i64, max: i64 },
    Choice(&'static [&'static str]),
    Text,
}

/// Metadata for one `server.properties` key.
#[derive(Debug, Clone, Copy)]
pub struct PropDef {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub default: &'static str,
    pub help: &'static str,
}

impl PropDef {
    #[must_use]
    pub fn find(key: &str) -> Option<&'static PropDef> {
        CATALOG.iter().find(|d| d.key == key)
    }
}

/// Whether the server only picks this key up at (re)start.
///
/// Most gameplay keys (`difficulty`, `gamemode`, `pvp`, `view-distance`,
/// `spawn-protection`, …) are re-applied by the dedicated server on every
/// launch, so editing them never needs a world reset — only a restart. The keys
/// below are the ones that genuinely do nothing until the next start.
#[must_use]
pub fn restart_required(key: &str) -> bool {
    matches!(
        key,
        "server-port"
            | "server-ip"
            | "online-mode"
            | "level-name"
            | "level-seed"
            | "level-type"
            | "generator-settings"
            | "enable-rcon"
            | "rcon.port"
            | "rcon.password"
            | "enable-query"
            | "query.port"
            | "network-compression-threshold"
            | "prevent-proxy-connections"
            | "rate-limit"
    )
}

use FieldKind::{Bool, Choice, Int, Text};

/// The catalogue, grouped loosely by topic in declaration order.
pub const CATALOG: &[PropDef] = &[
    PropDef {
        key: "motd",
        label: "MOTD",
        kind: Text,
        default: "A Minecraft Server",
        help: "Message shown in the multiplayer server list. Supports § colour codes.",
    },
    PropDef {
        key: "server-port",
        label: "Server port",
        kind: Int {
            min: 1,
            max: 65_535,
        },
        default: "25565",
        help: "TCP/UDP port the server listens on.",
    },
    PropDef {
        key: "max-players",
        label: "Max players",
        kind: Int {
            min: 0,
            max: 2_147_483_647,
        },
        default: "20",
        help: "Maximum simultaneous players.",
    },
    PropDef {
        key: "gamemode",
        label: "Game mode",
        kind: Choice(&["survival", "creative", "adventure", "spectator"]),
        default: "survival",
        help: "Default game mode for joining players.",
    },
    PropDef {
        key: "difficulty",
        label: "Difficulty",
        kind: Choice(&["peaceful", "easy", "normal", "hard"]),
        default: "easy",
        help: "World difficulty.",
    },
    PropDef {
        key: "hardcore",
        label: "Hardcore",
        kind: Bool,
        default: "false",
        help: "Players are set to spectator on death; difficulty is locked to hard.",
    },
    PropDef {
        key: "pvp",
        label: "PvP",
        kind: Bool,
        default: "true",
        help: "Allow players to damage each other.",
    },
    PropDef {
        key: "online-mode",
        label: "Online mode",
        kind: Bool,
        default: "true",
        help: "Authenticate players against Mojang. Turn off only on a trusted LAN.",
    },
    PropDef {
        key: "white-list",
        label: "Whitelist",
        kind: Bool,
        default: "false",
        help: "Only players on the whitelist may join.",
    },
    PropDef {
        key: "enforce-whitelist",
        label: "Enforce whitelist",
        kind: Bool,
        default: "false",
        help: "Kick online players who are removed from the whitelist.",
    },
    PropDef {
        key: "level-name",
        label: "Level name",
        kind: Text,
        default: "world",
        help: "Name of the world directory.",
    },
    PropDef {
        key: "level-seed",
        label: "Level seed",
        kind: Text,
        default: "",
        help: "Seed for world generation. Leave blank for random.",
    },
    PropDef {
        key: "level-type",
        label: "Level type",
        kind: Choice(&[
            "minecraft:normal",
            "minecraft:flat",
            "minecraft:large_biomes",
            "minecraft:amplified",
        ]),
        default: "minecraft:normal",
        help: "World generation preset.",
    },
    PropDef {
        key: "view-distance",
        label: "View distance",
        kind: Int { min: 2, max: 32 },
        default: "10",
        help: "Chunk radius sent to clients. Higher values cost more CPU and RAM.",
    },
    PropDef {
        key: "simulation-distance",
        label: "Simulation distance",
        kind: Int { min: 2, max: 32 },
        default: "10",
        help: "Chunk radius that receives entity/tick updates.",
    },
    PropDef {
        key: "spawn-protection",
        label: "Spawn protection",
        kind: Int {
            min: 0,
            max: 10_000,
        },
        default: "16",
        help: "Radius around spawn that non-ops cannot modify. 0 disables it.",
    },
    PropDef {
        key: "allow-nether",
        label: "Allow Nether",
        kind: Bool,
        default: "true",
        help: "Enable the Nether dimension.",
    },
    PropDef {
        key: "allow-flight",
        label: "Allow flight",
        kind: Bool,
        default: "false",
        help: "Permit flight mods/elytra hovering without a kick. Does not grant creative flight.",
    },
    PropDef {
        key: "force-gamemode",
        label: "Force game mode",
        kind: Bool,
        default: "false",
        help: "Reset players to the default game mode on join.",
    },
    PropDef {
        key: "enable-command-block",
        label: "Command blocks",
        kind: Bool,
        default: "false",
        help: "Allow command blocks to run.",
    },
    PropDef {
        key: "spawn-monsters",
        label: "Spawn monsters",
        kind: Bool,
        default: "true",
        help: "Allow hostile mobs to spawn.",
    },
    PropDef {
        key: "max-world-size",
        label: "Max world size",
        kind: Int {
            min: 1,
            max: 29_999_984,
        },
        default: "29999984",
        help: "World border radius in blocks.",
    },
    PropDef {
        key: "player-idle-timeout",
        label: "Idle timeout (min)",
        kind: Int {
            min: 0,
            max: 525_600,
        },
        default: "0",
        help: "Kick idle players after this many minutes. 0 disables it.",
    },
    PropDef {
        key: "resource-pack",
        label: "Resource pack URL",
        kind: Text,
        default: "",
        help: "URL of a server resource pack. Requires resource-pack-sha1 to be set.",
    },
    PropDef {
        key: "resource-pack-sha1",
        label: "Resource pack SHA-1",
        kind: Text,
        default: "",
        help: "Hex SHA-1 of the resource pack file, for client-side caching and integrity.",
    },
    PropDef {
        key: "require-resource-pack",
        label: "Require resource pack",
        kind: Bool,
        default: "false",
        help: "Disconnect players who reject the resource pack.",
    },
    PropDef {
        key: "enable-rcon",
        label: "Enable RCON",
        kind: Bool,
        default: "false",
        help: "Expose the remote console protocol. Leave off unless you need it.",
    },
    PropDef {
        key: "rcon.port",
        label: "RCON port",
        kind: Int {
            min: 1,
            max: 65_535,
        },
        default: "25575",
        help: "Port for RCON when enabled.",
    },
    PropDef {
        key: "rcon.password",
        label: "RCON password",
        kind: Text,
        default: "",
        help: "Password for RCON. Required if RCON is enabled.",
    },
    PropDef {
        key: "enable-query",
        label: "Enable query",
        kind: Bool,
        default: "false",
        help: "Expose the GameSpy4 query protocol for server-list tools.",
    },
    PropDef {
        key: "sync-chunk-writes",
        label: "Sync chunk writes",
        kind: Bool,
        default: "true",
        help: "fsync each chunk write. Safer on crash, slower on spinning disks.",
    },
    PropDef {
        key: "entity-broadcast-range-percentage",
        label: "Entity broadcast range %",
        kind: Int { min: 10, max: 1000 },
        default: "100",
        help: "Scales how far entities are sent to clients.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_keys_are_recognised() {
        assert!(restart_required("server-port"));
        assert!(restart_required("online-mode"));
        assert!(!restart_required("difficulty"));
        assert!(!restart_required("pvp"));
    }

    #[test]
    fn catalogue_has_no_duplicate_keys() {
        let mut keys: Vec<&str> = CATALOG.iter().map(|d| d.key).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "duplicate key in CATALOG");
    }

    #[test]
    fn int_defaults_sit_inside_their_range() {
        for def in CATALOG {
            if let FieldKind::Int { min, max } = def.kind {
                if let Ok(v) = def.default.parse::<i64>() {
                    assert!(
                        (min..=max).contains(&v),
                        "{} default {v} out of range",
                        def.key
                    );
                }
            }
        }
    }

    #[test]
    fn choice_defaults_are_listed_options() {
        for def in CATALOG {
            if let FieldKind::Choice(opts) = def.kind {
                assert!(
                    opts.contains(&def.default),
                    "{} default not in choices",
                    def.key
                );
            }
        }
    }
}
