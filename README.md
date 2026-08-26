# Minecraft Server Manager

A native GTK4 / libadwaita desktop app for running a single **Fabric** Minecraft
server on Linux: install the server, manage mods from Modrinth, edit
`server.properties` and the player-access files through a real form, take world
backups, and watch the server's memory against a hard ceiling.

Everything the app touches lives in **one directory** — this repository. Delete
the folder and nothing is left behind (bar the GTK toolkit's own shared caches,
which every GTK app on your system uses).

```
minecraft-server-manager/
├── crates/
│   ├── mcsm-core/      logic, config, HTTP clients, process control — no GUI, fully unit-tested
│   └── mcsm-gui/       thin Relm4 layer: one module per page
├── data/               ← everything the app writes at runtime (git-ignored)
│   ├── state.toml      app settings (human-editable)
│   ├── server/         the server: jars, world, mods/, config/, *.json
│   ├── cache/          downloaded jars, reused across reinstalls
│   ├── backups/        world-YYYYMMDD-HHMMSS.tar.zst
│   └── logs/
└── target/             Rust build output (git-ignored)
```

## Requirements

- Rust ≥ 1.85, and GTK 4 + libadwaita development files (`gtk4`, `libadwaita`
  on Arch — they include headers).
- A JVM appropriate for your Minecraft version (`java` on `PATH`, or point at
  one in Settings). Minecraft 1.21.x wants Java 21+.
- `systemd --user` for the hard memory cap (standard on a normal desktop
  session). Without it the server still runs, but the ceiling becomes advisory.
- `tar` with `zstd` support (`tar --zstd`) and the `zstd` binary, for backups.

## Build & run

```sh
cargo run -p mcsm-gui        # debug
cargo build --release -p mcsm-gui && ./target/release/mcsm
```

The app finds its root by walking up from the executable to this repository
(the `Cargo.toml` containing `mcsm`). To run the binary from elsewhere, set
`MCSM_ROOT=/path/to/minecraft-server-manager`.

Logging: `MCSM_LOG=debug cargo run -p mcsm-gui`.

## First run

1. **Settings** → pick a Minecraft version and Fabric loader → **Install**.
2. **Settings** → accept the Minecraft EULA.
3. Adjust the memory ceiling if 9 GiB isn't right for your machine.
4. **Dashboard** → **Start**.

## Testing

```sh
cargo test --workspace        # unit tests (no network)
cargo clippy --workspace --all-targets
```

## How the memory ceiling works

You set one number — the total for **app + JVM + world**. From it the app derives
(see `crates/mcsm-core/src/memory.rs`):

| | default at 9 GiB |
|---|---|
| systemd scope `MemoryMax` (kernel SIGKILL backstop) | 8 GiB |
| systemd scope `MemoryHigh` (reclaim pressure first) | 7 GiB |
| `-Xmx` (projected RSS ≈ `Xmx×1.25 + 512 MiB`, kept under `MemoryHigh`) | 5 GiB |
| `-Xmx` slider maximum | 5.5 GiB |

The server JVM runs inside a transient `systemd-run --user --scope` unit, so the
kernel enforces the cap on the whole process tree and the live memory readout is
a single `memory.current` read rather than a fragile `/proc` sum. Aikar's GC
flags are offered but `-XX:+AlwaysPreTouch` is dropped — it would commit the
whole heap at startup and trip the cap. The scope is a backstop that should
never fire; auto-restart never runs after an OOM kill.
