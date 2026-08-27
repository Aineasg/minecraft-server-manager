#!/bin/sh
# Minecraft Server Manager — build & install for the current user.
#
#   ./install.sh              install deps, build --release, install to ~/.local
#   ./install.sh --skip-deps  skip the package-manager step
#   ./install.sh --uninstall  remove the installed files (never touches your data)
#   ./install.sh --help
#
# Supports Arch, Debian/Ubuntu and Fedora families. On anything else it prints
# the dependency list and carries on.

set -eu

APP_ID="dev.aineasg.MinecraftServerManager"
BIN="mcsm"
REPO_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

BIN_DIR="$HOME/.local/bin"
APP_DIR="$HOME/.local/share/applications"
ICON_DIR="$HOME/.local/share/icons/hicolor"

SKIP_DEPS=0
ACTION="install"

for arg in "$@"; do
	case "$arg" in
	--skip-deps) SKIP_DEPS=1 ;;
	--uninstall) ACTION="uninstall" ;;
	--help | -h)
		sed -n '2,11p' "$0" | sed 's/^#\{1,\} \{0,1\}//;s/^#$//'
		exit 0
		;;
	*)
		echo "unknown option: $arg (try --help)" >&2
		exit 2
		;;
	esac
done

say() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m==> %s\033[0m\n' "$*" >&2; }
die() {
	printf '\033[1;31m==> %s\033[0m\n' "$*" >&2
	exit 1
}

# --- privilege helper -------------------------------------------------------
as_root() {
	if [ "$(id -u)" -eq 0 ]; then
		"$@"
	elif command -v sudo >/dev/null 2>&1; then
		sudo "$@"
	else
		die "need root to install packages and 'sudo' is not available; install the deps yourself and re-run with --skip-deps"
	fi
}

# --- uninstall ------------------------------------------------------------------
if [ "$ACTION" = "uninstall" ]; then
	say "Removing installed files (your worlds and settings are left alone)"
	rm -f "$BIN_DIR/$BIN" "$APP_DIR/$APP_ID.desktop"
	rm -f "$ICON_DIR/scalable/apps/$APP_ID.svg"
	for s in 16 24 32 48 64 128 256 512; do
		rm -f "$ICON_DIR/${s}x${s}/apps/$APP_ID.png"
	done
	command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APP_DIR" 2>/dev/null || true
	command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qtf "$ICON_DIR" 2>/dev/null || true
	say "Done."
	exit 0
fi

# --- distro detection ---------------------------------------------------------
DISTRO="unknown"
if [ -r /etc/os-release ]; then
	# shellcheck disable=SC1091
	. /etc/os-release
	for id in ${ID:-} ${ID_LIKE:-}; do
		case "$id" in
		arch) DISTRO="arch"; break ;;
		debian | ubuntu) DISTRO="debian"; break ;;
		fedora | rhel) DISTRO="fedora"; break ;;
		esac
	done
fi

# --- dependencies -----------------------------------------------------------
install_deps() {
	need_rust=0
	command -v cargo >/dev/null 2>&1 || need_rust=1

	case "$DISTRO" in
	arch)
		pkgs="gtk4 libadwaita pkgconf openssl zstd tar"
		[ "$need_rust" -eq 1 ] && pkgs="$pkgs rust"
		command -v java >/dev/null 2>&1 || pkgs="$pkgs jre-openjdk"
		say "pacman -S --needed $pkgs"
		as_root pacman -S --needed --noconfirm $pkgs
		;;
	debian)
		pkgs="build-essential curl pkg-config libssl-dev libgtk-4-dev libadwaita-1-dev zstd tar"
		command -v java >/dev/null 2>&1 || pkgs="$pkgs default-jre"
		say "apt-get install $pkgs"
		as_root apt-get update
		as_root apt-get install -y $pkgs
		;;
	fedora)
		pkgs="gcc gcc-c++ curl pkgconf-pkg-config openssl-devel gtk4-devel libadwaita-devel zstd tar"
		command -v java >/dev/null 2>&1 || pkgs="$pkgs java-21-openjdk"
		say "dnf install $pkgs"
		as_root dnf install -y $pkgs
		;;
	*)
		warn "Unrecognised distro. Make sure you have: a C compiler, pkg-config, OpenSSL headers, GTK4 + libadwaita development files, zstd, tar, a JRE (Java 21+), and the Rust toolchain."
		;;
	esac

	if ! command -v cargo >/dev/null 2>&1; then
		say "Installing the Rust toolchain via rustup"
		curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
		# shellcheck disable=SC1091
		. "$HOME/.cargo/env"
	fi
}

if [ "$SKIP_DEPS" -eq 0 ]; then
	install_deps
else
	say "Skipping dependency install"
	command -v cargo >/dev/null 2>&1 || die "cargo not found and --skip-deps was given"
fi

# --- build ----------------------------------------------------------------------
say "Building (release)…"
( cd "$REPO_DIR" && cargo build --release --locked -p mcsm-gui )
BUILT="$REPO_DIR/target/release/$BIN"
[ -x "$BUILT" ] || die "build did not produce $BUILT"

# --- install ------------------------------------------------------------------
say "Installing to ~/.local"
install -Dm755 "$BUILT" "$BIN_DIR/$BIN"

# The packaged .desktop ships a bare `Exec=mcsm`, which is right for a distro
# package (/usr/bin is always on PATH) but not here: the graphical session
# launches this from its own PATH, which usually lacks ~/.local/bin, so a menu
# click would fail — or worse, start a stale /usr/bin/mcsm from an old
# distro-package install that this user-level entry now shadows. Pin Exec to the
# binary we just wrote so the launcher always runs *this* version.
mkdir -p "$APP_DIR"
sed "s|^Exec=mcsm\$|Exec=$BIN_DIR/$BIN|" \
	"$REPO_DIR/packaging/$APP_ID.desktop" >"$APP_DIR/$APP_ID.desktop"
chmod 644 "$APP_DIR/$APP_ID.desktop"
# The rewrite is anchored to the literal `Exec=mcsm`. If that line ever changes
# shape the substitution silently does nothing and the menu entry launches
# whatever `mcsm` is on the session PATH again — fail loudly instead.
grep -q "^Exec=$BIN_DIR/$BIN\$" "$APP_DIR/$APP_ID.desktop" ||
	die "could not pin Exec= in $APP_ID.desktop (packaged file changed?)"

install -Dm644 "$REPO_DIR/packaging/$APP_ID.svg" "$ICON_DIR/scalable/apps/$APP_ID.svg"
for s in 16 24 32 48 64 128 256 512; do
	install -Dm644 "$REPO_DIR/packaging/icons/${s}.png" "$ICON_DIR/${s}x${s}/apps/$APP_ID.png"
done

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APP_DIR" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qtf "$ICON_DIR" 2>/dev/null || true

say "Installed. Launch it from your app menu, or run: $BIN"

# If an older install (e.g. `makepkg -si` → /usr/bin/mcsm) is still on PATH and
# comes first, typing `mcsm` in a terminal would run the stale binary even
# though this upgrade "succeeded". The menu entry is safe (absolute Exec=), but
# say something about the terminal case.
found=$(command -v "$BIN" 2>/dev/null || true)
if [ -n "$found" ] && [ "$found" != "$BIN_DIR/$BIN" ]; then
	warn "'$BIN' on your PATH is $found, not the copy just installed at $BIN_DIR/$BIN."
	warn "Remove the old one (e.g. 'sudo pacman -R minecraft-server-manager') or put $BIN_DIR first on PATH."
fi

case ":$PATH:" in
*":$HOME/.local/bin:"*) ;;
*) warn "$HOME/.local/bin is not on your PATH — add it to run 'mcsm' from a terminal." ;;
esac
