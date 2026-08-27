# Design

Decisions and structure for the Minecraft Server Manager. Written to be read
alongside the code, not instead of it.

## Goals

- **One folder.** Source, build output, and all runtime data under a single
  directory. Deleting it is a complete uninstall. Nothing in `~/.config`,
  `~/.cache`, `dconf`, or `~/.local`. The GTK toolkit's own shared caches are
  the one thing outside our control and are shared with every other GTK app.
- **Native.** GTK4 + libadwaita, so it fits a GNOME/KDE desktop. Rust, single
  binary.
- **Self-contained runtime.** No panel, no web server, no daemon. The app is
  the UI and the supervisor.
- **Hard memory ceiling.** App + JVM + world must fit in a user-set limit
  (default 9 GiB), enforced by the kernel, not hoped for.
- **Human-editable code and config.** `state.toml` and `server.properties` are
  meant to be opened in an editor; the code is split into small, focused,
  tested modules.

## Crate split

| crate | contains | tested |
|---|---|---|
| `mcsm-core` | paths, `state.toml`, `server.properties` round-trip, memory-budget math, access-file models, Fabric/Mojang/Modrinth HTTP clients, jar hashing + `fabric.mod.json` reading, install/mods/backup/server-process operations | unit-tested, no network |
| `mcsm-gui` | `main.rs`, a `Context` shared by every page, and one Relm4 component per page | logic lives in core; only pure helpers are tested here |

`mcsm-core` has no GTK dependency, so its tests run in milliseconds and the
logic can be reasoned about without a display.

## Directory layout (`mcsm_core::paths::Paths`)

`Paths` is the single source of truth. Root resolution order:

1. `$MCSM_ROOT` if set (created if missing) — used directly as the data dir.
2. **Portable mode** — the *executable itself* is in a checkout: it sits under a
   `target/` build directory with a `Cargo.toml` mentioning `mcsm` above it, or a
   `.mcsm-root` marker file sits beside it (or in a parent). → `<repo>/data/`.
   The working directory is deliberately **not** consulted, so an installed
   binary launched from inside a clone still resolves to the installed root, not
   the clone.
3. **Installed mode** — none of the above, so the binary lives in `~/.local/bin`
   or `/usr/bin` → `~/.local/share/MinecraftServerManager/` (`$XDG_DATA_HOME`).
   Still one self-contained folder, just not next to the binary.
4. The directory containing the executable (last resort).

In portable mode runtime state is nested in `<root>/data/` so it stays out of
the source tree; in every other case it sits directly in the resolved directory
(`paths.data == paths.root`). `Paths::with_root` builds the first layout,
`Paths::with_data_dir` the second.

`<data>/logs/` is created but **currently unused**: the app itself logs to
stderr (`MCSM_LOG=debug`), and the server's own rolling logs are written by the
JVM into `<data>/server/logs/`, that being its working directory. The directory
is reserved so the layout does not shift if app file-logging is added.

`./data/` is git-ignored but it is **live user data** — a Minecraft world plus
the server jars, mods and `state.toml`. Nothing in the tooling and no one
working in this repo should ever `rm -rf data`, `git clean -fdx`, or bulk-
overwrite it. A clean-slate run goes to a throwaway root
(`MCSM_ROOT="$(mktemp -d)"`), never by clearing `data/`. See `CONTRIBUTING.md`.

**Backups are the one thing that leaves the folder.** `state.backup_dir` (an
`Option`, pinned to the resolved default on first run so it is never forgotten)
points at `~/Documents/Minecraft Server Manager Backups/` by default — chosen so
that deleting or moving the app folder never loses a world. It is editable in
Settings and falls back to `<data>/backups` when there is no `~/Documents`.

## Packaging & install

`install.sh` (repo root, POSIX `sh`) detects Arch / Debian / Fedora from
`/etc/os-release`, installs the build + runtime dependencies via the native
package manager (`sudo` only for that step), installs the Rust toolchain via
`rustup` if `cargo` is absent, `cargo build --release`, then drops the binary,
`.desktop` and hicolor icons into `~/.local`. `--uninstall` reverses it and
never touches `<root>`. `packaging/PKGBUILD` is the AUR-style equivalent
installing under `/usr`.

reqwest is pinned to `native-tls` (system OpenSSL) rather than the default
rustls + aws-lc-rs stack, so the build needs only a C compiler + `libssl` — no
`cmake`/`clang` — which keeps the dependency list short on every distro.

## Server process supervision (`mcsm_core::ops::server`)

The JVM is launched as:

```
systemd-run --user --scope --unit=mcsm-server.scope \
  -p MemoryMax=<>M -p MemoryHigh=<>M -p MemorySwapMax=0 \
  --collect --quiet --working-directory=<data/server> \
  -- <java> <jvm args> -jar fabric-server-launch.jar nogui
```

- **Fixed unit name** so a JVM that outlived a crashed GUI can always be found
  (`scope_active`) and re-attached to or stopped, instead of starting a second
  JVM on the same world.
- `--scope` (not a service) keeps stdio wired to us for the console pane;
  `--collect` lets a failed scope be reused; `--quiet` keeps systemd chatter
  out of the log.
- **Log backpressure:** a reader task merges stdout+stderr and flushes batched
  lines on a 60 ms timer, capped at 500 lines per batch, so a modded startup
  spew cannot lock the UI.
- **Memory:** a 1 Hz task reads the scope's cgroup `memory.current` /
  `memory.peak` — one file read for the whole process tree, no `/proc` RSS
  summing.
- **OOM:** on exit the scope's `memory.events` `oom_kill` counter is checked.
  Auto-restart never runs after an OOM kill (that is a loop); the UI tells the
  user to lower the heap or drop mods.
- **No systemd:** falls back to a direct `java` child with no hard cap and a
  visible warning. The rest of the supervision is unchanged.

The module only *reports* via `ServerEvent`; restart policy lives in the
Dashboard page.

## Memory budget (`mcsm_core::memory`)

One ceiling in, concrete limits out. Encodes `JVM RSS ≈ Xmx × 1.25 + 512 MiB`
(metaspace, code cache, ~50-100 thread stacks, Netty direct buffers). `-Xmx` is
sized so projected RSS stays under `MemoryHigh`; the slider maximum keeps it
under `MemoryMax`. `-Xms` is set below `-Xmx` on purpose — with no
`AlwaysPreTouch` there is nothing to gain from committing the whole heap up
front, and a smaller initial commit is safer under the cap.

## `server.properties` (`mcsm_core::properties`)

Parsed into an ordered list of lines; only the value span of changed keys is
rewritten, every other line is reproduced byte-for-byte. Handles the real Java
`.properties` rules: `=` and `:` separators with surrounding whitespace,
backslash escapes, `\uXXXX` for non-Latin-1 (so a `§` colour code in the MOTD
survives a round trip), `#`/`!` comments. Line continuations are the one
unsupported feature — `server.properties` never uses them.

`properties_catalog` is a static table (key, label, type, range, default, help)
the GUI turns into a typed form. Keys not in the catalogue still round-trip;
the GUI shows them as plain text fields in an "Other" group.
`properties_catalog::restart_required(key)` marks the handful of keys the server
only reads at launch (ports, `online-mode`, `level-*`, RCON/query); the form
tags those rows. Everything else is re-applied by the dedicated server on every
start, so no `server.properties` change ever needs a world reset.

## World settings in `level.dat` (`mcsm_core::ops::level_dat`)

`hardcore`, `Difficulty` and `DifficultyLocked` live in the world's `level.dat`
(gzip-compressed NBT), not `server.properties` — `server.properties` only seeds
them at world creation. This module gunzips `level.dat`, patches those three
bytes in the `Data` compound via `fastnbt`, re-gzips, backs up the old file to
`level.dat.bak` and atomically writes. The Properties page exposes them as a
"World (level.dat)" group, disabled while the server runs (it holds the file
open and rewrites it on autosave).

## Player access (`mcsm_core::access` + GUI routing)

When the server is **running**, add/remove on the Player access page issues the
matching console command (`op`/`deop`/`whitelist add`/`whitelist remove`/`ban`/
`pardon`/`ban-ip`/`pardon-ip`) — routed to the Dashboard's stdin like backups —
so it takes effect immediately, then the JSON the server rewrote is re-read.
When **stopped**, the JSON is edited directly with an offline-mode UUID.
Running-state is checked per action via `scope_active()`.

## Mods (`mcsm_core::net::modrinth`, `mcsm_core::ops::mods`)

- **Search:** `GET /v2/search` with facets `project_type:mod`,
  `categories:fabric`, `versions:<mc>`, and `server_side:required|optional` to
  keep client-only mods out.
- **Dependencies:** a breadth-first walk with a visited-set (cycles and
  diamonds terminate). `required` is resolved recursively, `incompatible` is
  recorded for the caller to check against installed mods, `optional` is
  offered but not added, `embedded` is ignored.
- **Local jars:** identified by SHA-512 via `POST /v2/version_files`, so mods
  dropped in by hand still get enable/disable/update.
- **Updates:** one bulk `POST /v2/version_files/update` for the whole `mods/`
  directory.
- **Enable/disable:** rename `foo.jar` ⇄ `foo.jar.disabled`.

## Install (`mcsm_core::ops::install`)

No Fabric installer jar. `meta.fabricmc.net/v2/versions/loader/<mc>/<loader>/<installer>/server/jar`
returns a ready-to-run launcher jar; the vanilla server jar comes from the
Mojang piston manifest and is SHA-1 verified. Both are cached by name under
`data/cache/` and copied into `data/server/`.

## Backups (`mcsm_core::ops::backup`)

`tar --zstd` of the `<level-name>` directory (plus `_nether` / `_the_end`
siblings if a setup keeps them separate). Restore moves the live world aside to
`<name>.pre-restore` first, and refuses to run while the server scope is active.

Two things restore/create refuse rather than do:

- **Level-name mismatch.** `restore` lists the archive (`tar -tf`) and checks its
  top-level directory against the current `level-name` before moving anything.
  Unpacking a `myworld/` archive while `level-name` is `world` would stash the
  live world, write a directory the server never reads, and let it generate a
  fresh empty world — a silent loss that looks like success.
- **Name collisions.** Archive names are second-resolution, so a manual backup
  and the auto timer firing in the same second would land on one filename;
  `create` errors instead of overwriting an archive.

Creating a backup (manual button or the auto timer) is routed through the
Dashboard's live server handle: if the server is running it sends `save-all
flush` and waits ~2 s before archiving, so the snapshot is consistent without a
`save-off`. Automatic backups get an `auto-world-…` filename; `prune_auto` keeps
the newest N of those (configurable: 3/5/10/25/50/all) and never touches a
manual `world-…` archive. The timer lives on the Backups page and only runs
while the app is open — it is not a background daemon.

## GUI (`mcsm-gui`)

Relm4 (Elm-style) over gtk4-rs + libadwaita. `adw::NavigationSplitView` with a
sidebar `gtk::ListBox` switching pages in an `adw::ViewStack`. A top
`adw::Banner` prompts to install a server / accept the EULA.

`Context` (paths + HTTP clients + `Rc<RefCell<AppState>>`) is cloned into every
page. `AppState` is single-`RefCell` rather than a mutex because the GTK main
loop is single-threaded; background work is handed owned copies.

Pages that do I/O or network are full `Component`s and run it through
`sender.command(...)`; the results come back as messages. Pages with dynamic
row lists (Properties, Access, Mods, Backups) build their `adw::PreferencesGroup`s
imperatively and rebuild on change — simpler here than a factory.

## Deliberately out of scope for v1

- Multiple server instances (single server, by request).
- Background/daemon backups (the auto-backup timer runs only while the app is open).
- CurseForge (Modrinth only — clean API, no auth).
- Syntax highlighting in the raw file editor (plain monospace `TextView`).
