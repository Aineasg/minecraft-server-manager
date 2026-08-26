# Contributing

Thanks for taking a look. This is a small project — issues and PRs are welcome.

## Building

```sh
git clone https://github.com/aineasg/minecraft-server-manager
cd minecraft-server-manager
./install.sh --skip-deps    # or just: cargo run -p mcsm-gui
```

You need GTK 4 + libadwaita development files, `pkg-config`, OpenSSL headers,
`zstd`, and a recent Rust toolchain. `./install.sh` installs those for
Arch / Debian / Fedora.

> **`./data/` is a live Minecraft world, not scratch.** It is git-ignored but
> holds the world, jars, mods and `state.toml` whenever you run from a checkout.
> Never `rm -rf data` or `git clean -fdx` in this repo. For a clean-slate run,
> point the app elsewhere: `MCSM_ROOT="$(mktemp -d)" cargo run -p mcsm-gui`.

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs exactly these three. All of them must pass.

## Layout

- `crates/mcsm-core` — all logic, no GTK, fully unit-tested. Put new behaviour
  here with a test.
- `crates/mcsm-gui` — a thin Relm4 layer, one module per page under `src/ui/`.
- `DESIGN.md` explains why things are the way they are — worth a read, and worth
  updating when a design decision changes.

## Style

- Match the surrounding code. Small, focused modules.
- Keep `mcsm-core` free of GTK types so its tests stay fast.
- User-facing errors are strings a person can act on.

## Licence

By contributing you agree that your work is licensed under **AGPL-3.0-or-later**,
the same as the rest of the project.
