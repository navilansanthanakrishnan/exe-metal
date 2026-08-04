#!/bin/bash
#
# exe-metal integration tests.
#
# bash rather than sh: iterating paths safely needs `read -d ''`, and Steam
# installs under "Program Files (x86)", where word-splitting silently compares
# nothing and looks like a skip.
#
#   ./tests/smoke.sh                 # against the real ~/Library install
#   ./tests/smoke.sh --fresh         # against a throwaway HOME (downloads ~1.4 GB)
#
# The --fresh mode is the one that proves a new machine works: Layout::discover
# reads $HOME, so pointing it at an empty directory produces a genuinely
# untouched install without a VM.
#
# Every check prints PASS or FAIL and the script exits non-zero if any failed.

set -u

BIN="${EXE_METAL_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/release/exe-metal}"
FRESH=0
[ "${1:-}" = "--fresh" ] && FRESH=1

PASS=0
FAIL=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass() { PASS=$((PASS + 1)); printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf '  \033[31mFAIL\033[0m  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; }
head_() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# ok <name> <command...>  — passes when the command exits 0
ok() { name="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name" "exit $?"; fi; }

# no <name> <command...>  — passes when the command exits NON-zero
no() { name="$1"; shift; if "$@" >/dev/null 2>&1; then fail "$name" "expected failure, got success"; else pass "$name"; fi; }

# has <name> <needle> <command...> — passes when output contains needle
has() {
    name="$1"; needle="$2"; shift 2
    out="$("$@" 2>&1)"
    case "$out" in
        *"$needle"*) pass "$name" ;;
        *) fail "$name" "expected '$needle' in output" ;;
    esac
}

[ -x "$BIN" ] || { echo "no binary at $BIN — run: cargo build --release"; exit 1; }

if [ "$FRESH" = "1" ]; then
    export HOME="$TMP/home"
    mkdir -p "$HOME"
    head_ "fresh install into $HOME"
    printf '  downloading ~1.4 GB, several minutes\n'
    if "$BIN" setup >"$TMP/setup.log" 2>&1; then
        pass "setup completed"
    else
        fail "setup completed" "see $TMP/setup.log"
        tail -20 "$TMP/setup.log"
        exit 1
    fi
    # Running it again must be safe and must not re-download.
    if "$BIN" setup >"$TMP/setup2.log" 2>&1; then
        pass "setup is idempotent"
    else
        fail "setup is idempotent"
    fi
    grep -q "already installed" "$TMP/setup2.log" \
        && pass "second setup reused the cache" \
        || fail "second setup reused the cache" "it re-downloaded"
fi

# ---------------------------------------------------------------- basics ----

head_ "command surface"
ok  "--version"            "$BIN" --version
ok  "doctor"               "$BIN" doctor
ok  "catalog"              "$BIN" catalog
ok  "list"                 "$BIN" list
ok  "engine list"          "$BIN" engine list
ok  "recent"               "$BIN" recent
ok  "prune"                "$BIN" prune
ok  "setup --dry-run"      "$BIN" setup --dry-run
no  "unknown command"      "$BIN" definitely-not-a-command
no  "show missing bottle"  "$BIN" show no-such-bottle-here
no  "run missing bottle"   "$BIN" run no-such-bottle-here foo.exe

head_ "machine detection"
has "reports an Apple chip"   "chip:"          "$BIN" doctor
has "reports core split"      "performance"    "$BIN" doctor
has "reports capabilities"    "capabilities"   "$BIN" doctor
has "reports MetalFX"         "MetalFX"        "$BIN" doctor
has "reports ray tracing"     "ray tracing"    "$BIN" doctor

# ------------------------------------------------------------ exe probing ----

head_ "probe: malformed input is rejected, not guessed"

: > "$TMP/empty.exe"
no  "0-byte file rejected"        "$BIN" probe "$TMP/empty.exe"
has "0-byte says why" "empty"     "$BIN" probe "$TMP/empty.exe"

printf 'this is not an executable at all' > "$TMP/text.exe"
no  "non-PE file rejected"        "$BIN" probe "$TMP/text.exe"
has "non-PE says why" "MZ"        "$BIN" probe "$TMP/text.exe"

no  "missing file rejected"       "$BIN" probe "$TMP/does-not-exist.exe"

# A directory is not a file; must not be treated as one.
mkdir -p "$TMP/adir.exe"
no  "directory rejected"          "$BIN" probe "$TMP/adir.exe"

# MZ header but truncated before the PE header.
printf 'MZ' > "$TMP/truncated.exe"
dd if=/dev/zero bs=1 count=60 >> "$TMP/truncated.exe" 2>/dev/null
no  "truncated PE rejected"       "$BIN" probe "$TMP/truncated.exe"

# ------------------------------------------------- probe against file(1) ----

head_ "probe: bitness agrees with file(1)"

STEAM="$HOME/Library/Application Support/exe-metal/bottles/cosmos-steam/pfx/drive_c/Program Files (x86)/Steam"
if [ -d "$STEAM" ]; then
    # Steam lives under "Program Files (x86)". Iterating find output with a
    # bare `for` splits on those spaces and silently compares nothing, which
    # reads as a skip rather than a bug — hence the null-delimited read.
    find "$STEAM" -name '*.exe' -print0 2>/dev/null > "$TMP/exes.z"

    checked=0
    mismatch=0
    bad=0
    seen=0
    while IFS= read -r -d '' f; do
        [ -f "$f" ] || continue
        seen=$((seen + 1))
        [ "$seen" -gt 25 ] && break
        want=""
        case "$(file -b "$f" 2>/dev/null)" in
            *"Intel 80386"*) want="32-bit x86" ;;
            *"x86-64"*)      want="64-bit x86-64" ;;
            *) continue ;;
        esac
        out="$("$BIN" probe "$f" 2>/dev/null)"
        got="$(printf '%s\n' "$out" | sed -n 's/^  binary  *//p' | head -1)"
        checked=$((checked + 1))
        if [ "$got" != "$want" ]; then
            mismatch=$((mismatch + 1))
            printf '        %s: file=%s exe-metal=%s\n' "$(basename "$f")" "$want" "$got"
        fi
        # D3DMetal has no 32-bit build; routing a 32-bit exe to it would
        # produce a bottle that cannot render.
        if [ "$want" = "32-bit x86" ]; then
            case "$out" in
                *"renderer  d3dmetal"*)
                    bad=$((bad + 1))
                    printf '        %s got d3dmetal at 32-bit\n' "$(basename "$f")" ;;
            esac
        fi
    done < "$TMP/exes.z"

    if [ "$checked" -eq 0 ]; then
        fail "bitness matches file(1)" "compared nothing — the loop found no executables"
    elif [ "$mismatch" -eq 0 ]; then
        pass "bitness matches file(1) on $checked executables"
    else
        fail "bitness matches file(1)" "$mismatch of $checked disagreed"
    fi

    head_ "probe: renderer selection is safe"
    if [ "$checked" -eq 0 ]; then
        fail "no 32-bit binary was routed to d3dmetal" "nothing was checked"
    elif [ "$bad" -eq 0 ]; then
        pass "no 32-bit binary was routed to d3dmetal (across $checked binaries)"
    else
        fail "no 32-bit binary was routed to d3dmetal" "$bad violations"
    fi
else
    printf '  \033[33mSKIP\033[0m  no cosmos-steam bottle to probe against\n'
fi

# ------------------------------------------------------------- diagnose ----

head_ "diagnose"
for b in $("$BIN" list 2>/dev/null | awk '{print $1}'); do
    # Exit code is 1 when findings exist, which is not a test failure — the
    # test is that it runs and produces output rather than panicking.
    out="$("$BIN" diagnose "$b" 2>&1)"
    case "$out" in
        *"nothing wrong found"*|*"fatal"*|*"warning"*)
            pass "diagnose $b produced a verdict" ;;
        *) fail "diagnose $b produced a verdict" "got: $(printf '%s' "$out" | head -2)" ;;
    esac
done

head_ "diagnose: healthy Steam logs are not misreported"
if [ -d "$STEAM/logs" ] && grep -qi "No Connection Reconnect" "$STEAM/logs/connection_log.txt" 2>/dev/null; then
    # This exact line was once a false positive for the two-clients signature.
    out="$("$BIN" diagnose cosmos-steam 2>&1)"
    case "$out" in
        *"signed in elsewhere"*) fail "reconnect noise is not an eviction" "false positive returned" ;;
        *) pass "reconnect noise is not an eviction" ;;
    esac
else
    printf '  \033[33mSKIP\033[0m  no reconnect noise present to test against\n'
fi

# ------------------------------------------------------------------ run ----

head_ "running a real Windows binary"
TARGET="$(find "$STEAM" -name 'vulkandriverquery64.exe' 2>/dev/null | head -1)"
BOTTLE="$("$BIN" list 2>/dev/null | awk 'NR==1{print $1}')"
if [ -n "$TARGET" ] && [ -n "$BOTTLE" ]; then
    ok  "launches and exits 0"  "$BIN" play --bottle "$BOTTLE" "$TARGET"
    has "reaches the Apple GPU" "Apple M" "$BIN" play --bottle "$BOTTLE" "$TARGET"
    has "recorded in recent"    "vulkandriverquery64" "$BIN" recent
else
    printf '  \033[33mSKIP\033[0m  no test binary or bottle available\n'
fi

head_ "bottle environment reaches child processes"
if [ -n "$BOTTLE" ]; then
    # A game launched by Steam is a child of steam.exe inside the same prefix,
    # so this is what it inherits.
    env_out="$("$BIN" run "$BOTTLE" 'C:\windows\system32\cmd.exe' /c set 2>/dev/null)"
    for var in WINEPREFIX MTL_SHADER_CACHE_PATH WINEDEBUG; do
        case "$env_out" in
            *"$var"*) pass "child inherits $var" ;;
            *) fail "child inherits $var" ;;
        esac
    done
fi

# ---------------------------------------------------------------- report ----

printf '\n\033[1m%d passed, %d failed\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
