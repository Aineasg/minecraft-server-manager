<div align="center">

<img src="packaging/icons/128.png" alt="" width="96" height="96">

# Minecraft Server Manager

A native GTK4 / libadwaita desktop app for running a single **Fabric** Minecraft
server on Linux.

[![CI](https://github.com/aineasg/minecraft-server-manager/actions/workflows/ci.yml/badge.svg)](https://github.com/aineasg/minecraft-server-manager/actions/workflows/ci.yml)
[![Licence: AGPL-3.0-or-later](https://img.shields.io/badge/licence-AGPL--3.0--or--later-blue.svg)](LICENSE)

</div>

Install and run a server, manage mods from Modrinth with dependency resolution,
edit `server.properties` through a real form, apply ops/whitelist/bans live,
schedule world backups, flip **hardcore** on an existing world, and watch the
server's memory against a hard ceiling the kernel enforces.

<!-- Add a screenshot at docs/screenshot.png and uncomment:
<div align="center"><img src="docs/screenshot.png" alt="Screenshot" width="820"></div>
-->

## Features

- **Dashboard** — start/stop/restart, live console with command input, a live
  memory meter for the whole process tree, and auto-restart (never after an OOM).
- **Mods** — search Modrinth (server-side Fabric mods for your version), install
  with required dependencies pulled in, enable/disable without deleting, bulk
  update check. Recognises jars you dropped in by hand.
- **Properties** — a typed form for `server.properties` (comment- and
  order-preserving writer), a raw section for anything unmodelled, and a
  **World (level.dat)** group to toggle **hardcore** / difficulty / difficulty
  lock on an already-generated world.
- **Player access** — ops, whitelist and bans. Applied **live** as console
  commands when the server is running; written to JSON when it is stopped.
- **Backups** — `tar --zstd` archives of the world. Scheduled automatic backups
  (with a retention count), one-click restore, delete. A running server is
  flushed with `save-all` first. Backups default to your **Documents** folder so
  deleting the app never loses a world.
- **Files** — a plain-text editor for anything under the data folder.
- **Settings** — Minecraft + Fabric version install (no Fabric-installer step),
  the memory budget, GC flags, Java path, EULA, backup folder.

## Install

### Script (Arch, Debian/Ubuntu, Fedora)

```sh
git clone https://github.com/aineasg/minecraft-server-manager
cd minecraft-server-manager
./install.sh
```

It installs the build/runtime dependencies with your package manager (asking for
`sudo` only for that step), builds a release binary, and installs it plus a
desktop entry and icon into `~/.local`. Then launch **Minecraft Server Manager**
from your app menu. `./install.sh --uninstall` removes it; your worlds and
settings are never touched.

### Arch (AUR-style)

```sh
cd packaging && makepkg -si          # uninstall: sudo pacman -R minecraft-server-manager
```

### From source, no install

```sh
cargo run --release -p mcsm-gui
```

**Runtime needs:** a JRE (Java 21+ for modern Minecraft), `tar` with `--zstd`
plus the `zstd` binary, and `systemd --user` for the hard memory cap (without it
the server still runs, but the ceiling becomes advisory).

### Upgrading

Pull (or re-clone) the new version and run `./install.sh` again. It rebuilds and
replaces the binary in `~/.local/bin`; the app menu entry is pinned to that path
so it always launches the version you just installed. Your data directory
(`~/.local/share/MinecraftServerManager/` — world, jars, mods, `state.toml`) is
never read or touched by the installer. Confirm the build with
`~/.local/bin/mcsm --version` (the absolute path — a plain `mcsm` may still hit
an older copy earlier on your `PATH`).

If you installed with `makepkg -si` instead, upgrade with `makepkg -si` — don't
mix the two, or an old `/usr/bin/mcsm` can shadow the new one (the script warns
when it detects this).

Downgrading is best-effort: an older build loads a `state.toml` written by a
newer one, but settings the newer version added are dropped the next time
settings are saved. The world is unaffected.

## Where things live

The app keeps **everything in one folder** (call it `<root>`):

- run from a cloned repo → `<root>` is `./data/` inside the checkout
- installed → `<root>` is `~/.local/share/MinecraftServerManager/`
- or set `MCSM_ROOT=/some/path` → `<root>` is that path

```
<root>/
├── state.toml            settings (human-editable)
├── server/               jars, world, mods/, config/, ops.json, whitelist.json, …
│   └── logs/             the server's own rolling logs, written by the JVM
├── cache/                downloaded jars, reused across reinstalls
└── logs/                 reserved for an app log; nothing writes here yet
```

**Backups** are the one exception: they default to
`~/Documents/Minecraft Server Manager Backups/` so a deleted app folder never
loses a world. Change the location in **Settings → Backups**; it is recorded in
`state.toml`.

## First run

1. **Settings** → pick a Minecraft version and Fabric loader → **Install**.
2. **Settings** → accept the Minecraft EULA.
3. Adjust the memory ceiling if 9 GiB isn't right for your machine.
4. **Dashboard** → **Start**.

`MCSM_LOG=debug mcsm` for verbose logging.

## How the memory ceiling works

You set one number — the total for **app + JVM + world**. From it the app derives
(`crates/mcsm-core/src/memory.rs`):

| | default at 9 GiB |
|---|---|
| systemd scope `MemoryMax` (kernel SIGKILL backstop) | 8 GiB |
| systemd scope `MemoryHigh` (reclaim pressure first) | 7 GiB |
| `-Xmx` (projected RSS ≈ `Xmx × 1.25 + 512 MiB`, kept under `MemoryHigh`) | 5 GiB |
| `-Xmx` slider maximum | 5.5 GiB |

The server JVM runs inside a transient `systemd-run --user --scope` unit, so the
kernel enforces the cap on the whole process tree and the live readout is one
`memory.current` read rather than a fragile `/proc` sum. Aikar's GC flags are
offered but `-XX:+AlwaysPreTouch` is dropped — it would commit the whole heap at
startup and trip the cap. The scope is a backstop that should never fire;
auto-restart never runs after an OOM kill.

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # unit tests, no network
```

`crates/mcsm-core` is all logic and has no GTK dependency; `crates/mcsm-gui` is a
thin Relm4 layer, one module per page. See [`DESIGN.md`](DESIGN.md) and
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Licence

[GNU Affero General Public License v3.0 or later](LICENSE). This is free software
with **no warranty**. If you distribute a modified version — or make one
available to others over a network — you must release your changes under the
AGPL too.
