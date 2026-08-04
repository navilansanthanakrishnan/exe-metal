#!/bin/sh
#
# exe-metal installer.
#
#   curl -fsSL https://raw.githubusercontent.com/<you>/exe-metal/main/install.sh | sh
#
# or, from a checkout:
#
#   ./install.sh
#
# Builds the binary, puts it on PATH, and runs `exe-metal setup`, which
# downloads the Wine runtime and every renderer and leaves a bottle ready.
#
# Deliberately POSIX sh with `set -e`: this is the one script that has to work
# on a machine where nothing has been set up yet.

set -eu

REPO_URL="${EXE_METAL_REPO:-https://github.com/NavilanSanthanakrishnan/exe-metal.git}"
PREFIX="${EXE_METAL_PREFIX:-$HOME/.local/bin}"
BRANCH="${EXE_METAL_BRANCH:-main}"

say()  { printf '\033[1m%s\033[0m\n' "$*"; }
info() { printf '  %s\n' "$*"; }
die()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------- checks ----

say "exe-metal installer"

[ "$(uname -s)" = "Darwin" ] || die "this is macOS only (uname reports $(uname -s))"

ARCH="$(uname -m)"
if [ "$ARCH" != "arm64" ]; then
    die "exe-metal targets Apple Silicon; this machine reports $ARCH.
       DirectX is translated to Metal on Apple GPUs — there is no Intel path."
fi

OS_MAJOR="$(sw_vers -productVersion | cut -d. -f1)"
if [ "$OS_MAJOR" -lt 14 ]; then
    die "macOS 14 (Sonoma) or newer is required; this is $(sw_vers -productVersion)"
fi

info "macOS $(sw_vers -productVersion) on $(sysctl -n machdep.cpu.brand_string)"

# Xcode command line tools supply git, clang and the linker Rust needs.
if ! xcode-select -p >/dev/null 2>&1; then
    say "installing Xcode command line tools"
    info "a system dialog will appear — accept it, then re-run this script"
    xcode-select --install || true
    die "waiting on the command line tools install"
fi

# ------------------------------------------------------------------ rust ----

if ! command -v cargo >/dev/null 2>&1; then
    say "installing Rust"
    info "exe-metal is built from source; this is the only build dependency"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # rustup writes here regardless of --no-modify-path.
    . "$HOME/.cargo/env"
fi
command -v cargo >/dev/null 2>&1 || die "cargo is still not on PATH after installing Rust"
info "cargo $(cargo --version | awk '{print $2}')"

# ----------------------------------------------------------------- build ----

# Work in place when run from a checkout; otherwise fetch one.
if [ -f "Cargo.toml" ] && grep -q '^name = "exe-metal"' Cargo.toml 2>/dev/null; then
    SRC="$(pwd)"
    say "building from this checkout"
else
    SRC="${TMPDIR:-/tmp}/exe-metal-build"
    say "fetching source"
    rm -rf "$SRC"
    git clone --depth 1 --branch "$BRANCH" "$REPO_URL" "$SRC" \
        || die "could not clone $REPO_URL"
fi

say "building (a few minutes the first time — the window pulls a graphics stack)"
( cd "$SRC" && cargo build --release ) || die "build failed"

BIN="$SRC/target/release/exe-metal"
[ -x "$BIN" ] || die "build reported success but produced no binary at $BIN"

# --------------------------------------------------------------- install ----

mkdir -p "$PREFIX"
install -m 0755 "$BIN" "$PREFIX/exe-metal"
say "installed $PREFIX/exe-metal"

case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *)
        info "$PREFIX is not on your PATH. Add it:"
        # Name the file the user's shell actually reads.
        case "$(basename "${SHELL:-/bin/zsh}")" in
            zsh)  RC="$HOME/.zshrc" ;;
            bash) RC="$HOME/.bash_profile" ;;
            *)    RC="your shell profile" ;;
        esac
        printf '\n    echo '"'"'export PATH="%s:$PATH"'"'"' >> %s\n\n' "$PREFIX" "$RC"
        ;;
esac

# ----------------------------------------------------------------- setup ----

say "running setup"
"$PREFIX/exe-metal" setup

# Rosetta is the one step needing admin, so it is checked last and loudly
# rather than silently escalating mid-install.
if [ ! -f /Library/Apple/usr/libexec/oah/libRosettaRuntime ]; then
    printf '\n'
    say "one step left"
    info "Rosetta 2 executes the x86-64 code, and installing it needs admin:"
    printf '\n    sudo softwareupdate --install-rosetta --agree-to-license\n\n'
fi

printf '\n'
say "done"
info "exe-metal ui                     open the window"
info "exe-metal play /path/to/game.exe  run something straight away"
