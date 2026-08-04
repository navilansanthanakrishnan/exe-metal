# Contributing

Thanks for looking. This project has an unusual property that shapes how it is
built: **almost every hard-won fact in it cost someone hours of confusion**,
because the failures here are silent. A missing FreeType renders no text and
reports nothing. A disabled `mscoree` kills every .NET game with a registry
error three layers from the cause. A bottle on the wrong sync primitive burns
four times the CPU and looks fine.

So the guiding rule is: **write down what actually happened, and prove it.**

## Getting set up

```sh
git clone https://github.com/NavilanSanthanakrishnan/exe-metal
cd exe-metal
cargo build --release           # with the window
cargo build --no-default-features   # CLI only, builds in seconds
cargo test                      # unit tests: no network, no Wine, no fixtures
./tests/smoke.sh                # integration, against your real install
```

You need macOS 14+ on Apple Silicon and Rosetta 2. There is no Intel path and
there is not going to be one — the whole design rests on translating DirectX to
Metal on an Apple GPU.

## Before you open a PR

```sh
cargo fmt
cargo clippy --all-targets      # must be clean, CI enforces it
cargo test
./tests/smoke.sh                # if you touched anything that runs Wine
```

CI runs `fmt --check`, `clippy -D warnings`, both build configurations, and the
unit tests. It cannot run the integration suite, because that needs a real Wine
install and a real GPU — so if your change touches launching, prefixes,
renderers, or Steam, run `./tests/smoke.sh` yourself and say so in the PR.

## What makes a good change here

**Verify by running it, not by reading the diff.** "This should work" is not a
result. The README and commit history are full of measured numbers because a
plausible-looking change in this codebase can silently do nothing.

**A diagnostic that cries wolf is worse than no diagnostic.** `diagnose`
signatures must be narrow. One early version matched `NO CONNECTION` for the
two-Steam-clients case, which also matches `SteamServerConnectFailure_t No
Connection Reconnect` — routine noise appearing dozens of times in a healthy
session. If you add a signature, add a regression test holding a **real healthy
log** that must not trip it.

**Settings are frozen at bottle creation.** Anything you add to `Perf` or
`WindowCfg` needs a migration in `exe-metal repair`, or existing bottles keep the
old value forever.

**Say what you could not verify.** A PR that says "renderer selection is correct
but no 32-bit game has actually rendered a frame through it" is more useful than
one that implies otherwise.

## Reporting a bug

Run this first and paste the output — it answers most of the questions a
maintainer would otherwise have to ask:

```sh
exe-metal doctor
exe-metal diagnose <bottle>
exe-metal status <bottle>       # while the problem is happening
```

If a game fails to start, the Wine log is far more informative than the exit
code:

```sh
WINEDEBUG=+err,+seh exe-metal run <bottle> <exe> 2>&1 | tail -50
```

## Adding a runtime or renderer

`src/catalog.rs` is the single source of truth. Every entry carries a `notes`
field recording **what actually happened when it was tested**, including the
combinations that fail, so the tool can steer people away from dead ends rather
than letting them rediscover them.

If you add a runtime, fill in `winemetal`, `win32`, `msync` and
`needs_frameworks` honestly — `check_pairing` uses them to reject combinations
before anything is downloaded, and getting one wrong turns a clear up-front
error into a confusing runtime failure.

## Code style

Rust defaults via `rustfmt`. Comments explain **why**, not what — particularly
where the reason is non-obvious, which here is most places. If a line exists
because of a specific failure, name the failure.

## Scope

Things that belong here: making Windows software run well on Apple Silicon,
diagnosing why it does not, and being honest about the limits.

Things that do not: DRM circumvention, anti-cheat evasion, or anything that
helps run software you did not obtain legitimately. Kernel-level anti-cheat does
not work here and PRs attempting to change that will be declined.

## Code of conduct

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
