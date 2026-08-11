#!/bin/sh
# Build and install Argon40 on Raspberry Pi OS.
set -eu

usage() {
    cat <<'EOF'
Usage: ./install.sh [--no-enable] [--root PATH]

Build the release binaries with the locked Cargo dependency set, then install
them using argon40ctl's self-contained installer.

  --no-enable  Install files without enabling or starting argon40d
  --root PATH  Stage files below PATH instead of installing on this host;
               implies --no-enable and does not require root
  -h, --help   Show this help

Rust 1.85 or newer is required. Run this script as your normal user; it uses
sudo only for the final system installation.
EOF
}

die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

warn() {
    printf 'install.sh: warning: %s\n' "$*" >&2
}

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
INSTALL_ROOT=/
ENABLE_SERVICE=1

while [ "$#" -gt 0 ]; do
    case "$1" in
        --no-enable)
            ENABLE_SERVICE=0
            ;;
        --root)
            [ "$#" -ge 2 ] || die "--root requires a path"
            INSTALL_ROOT=$2
            ENABLE_SERVICE=0
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1 (try --help)"
            ;;
    esac
    shift
done

[ "$(uname -s)" = Linux ] || die "this installer supports Linux only"
case "$(uname -m)" in
    aarch64|armv7l|armv8l)
        ;;
    *)
        warn "this is intended for Raspberry Pi OS; detected architecture $(uname -m)"
        ;;
esac

command -v cargo >/dev/null 2>&1 || die "Cargo is required; install Rust 1.85 or newer from https://rustup.rs"
command -v rustc >/dev/null 2>&1 || die "rustc is required; install Rust 1.85 or newer from https://rustup.rs"

RUST_VERSION=$(rustc --version | awk '{print $2}')
RUST_MAJOR=$(printf '%s\n' "$RUST_VERSION" | awk -F. '{print $1}')
RUST_MINOR=$(printf '%s\n' "$RUST_VERSION" | awk -F. '{print $2}')
case "$RUST_MAJOR:$RUST_MINOR" in
    *[!0-9:]*|:*) die "could not parse rustc version: $RUST_VERSION" ;;
esac
if [ "$RUST_MAJOR" -lt 1 ] || { [ "$RUST_MAJOR" -eq 1 ] && [ "$RUST_MINOR" -lt 85 ]; }; then
    die "Rust 1.85 or newer is required; found $RUST_VERSION"
fi

if [ "$INSTALL_ROOT" = / ] && [ "$ENABLE_SERVICE" -eq 1 ] && command -v systemctl >/dev/null 2>&1; then
    for LEGACY_UNIT in argononed.service argoneond.service; do
        if systemctl is-active --quiet "$LEGACY_UNIT" 2>/dev/null; then
            die "$LEGACY_UNIT is active; stop and disable it first so two daemons cannot control the same hardware"
        fi
    done
fi

printf 'Building Argon40 release binaries with Rust %s...\n' "$RUST_VERSION"
cargo build --locked --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

CTL=$SCRIPT_DIR/target/release/argon40ctl
[ -x "$CTL" ] || die "release build did not create $CTL"

if [ "$INSTALL_ROOT" != / ]; then
    mkdir -p -- "$INSTALL_ROOT"
    "$CTL" install --root "$INSTALL_ROOT" --no-enable
elif [ "$(id -u)" -eq 0 ]; then
    if [ "$ENABLE_SERVICE" -eq 1 ]; then
        "$CTL" install
    else
        "$CTL" install --no-enable
    fi
else
    command -v sudo >/dev/null 2>&1 || die "sudo is required for installation under /; rerun as root or install sudo"
    if [ "$ENABLE_SERVICE" -eq 1 ]; then
        sudo "$CTL" install
    else
        sudo "$CTL" install --no-enable
    fi
fi

if [ "$INSTALL_ROOT" = / ]; then
    for DEVICE_GLOB in /dev/i2c-* /dev/gpiochip* /dev/serial0; do
        if [ ! -e "$DEVICE_GLOB" ]; then
            warn "$DEVICE_GLOB was not found; enable the required Raspberry Pi interface before hardware use"
        fi
    done
    printf '%s\n' \
        'Installation complete.' \
        'Power-button actions remain diagnostic-only until --button-actions is added to the systemd ExecStart override.' \
        'See README.md, especially "Power button and shutdown", before enabling destructive actions.'
fi
