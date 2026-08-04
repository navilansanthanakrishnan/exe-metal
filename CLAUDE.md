# exe-metal

Rust CLI + GUI that runs Windows games on Apple Silicon macOS by composing a
Wine runtime with a DirectX→Metal renderer. Free components only — see README
for the layer-by-layer comparison against CrossOver.

## Build and test

```sh
cargo build --release           # with the window (~420 crates, minutes first time)
cargo build --no-default-features   # CLI only, seconds
cargo test                      # 38 unit tests, no fixtures or network needed
./tests/smoke.sh                # 40 integration checks, incl. a soak pass
./tests/smoke.sh --fresh        # same, into a throwaway HOME (downloads ~1.4 GB)
./install.sh                    # full bootstrap: Rust, build, install, setup
```

`--fresh` is how a new machine is simulated without a VM: `Layout::discover`
reads `$HOME`, so pointing it at an empty directory gives a genuinely untouched
install. A full fresh setup takes ~42s and ends at 3.6 GB on disk.

The binary lands at `target/release/exe-metal`.

## CI gates — run these before pushing

CI is strict and will reject anything that fails them:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
shellcheck --severity=warning install.sh tests/smoke.sh
```

MSRV is **1.92**, set by egui/eframe and enforced by a CI job. CI cannot run
`tests/smoke.sh` — it needs real Wine and a real GPU — so run it yourself for
anything touching launching, prefixes, renderers or Steam, and say so in the PR.

## Releasing

Tag `v*` and push; `.github/workflows/release.yml` verifies, builds, packages
with a SHA-256, confirms the packaged binary runs, and publishes.

```sh
# bump version in Cargo.toml, add a CHANGELOG entry, then:
git tag -a v0.2.0 -m "..." && git push origin v0.2.0
```

## Where things live

Public-facing docs are in `docs/`; this file is the internal guide.
`docs/observations.md` holds measurements and open issues from live use.
`ROADMAP.md` leads with the project's real gap: too few games verified.

## Verifying a change

Tests cover the pure logic. Anything touching Wine has to be run:

```sh
./target/release/exe-metal doctor            # machine + capability report
./target/release/exe-metal probe <exe>       # bitness, DirectX, chosen renderer
./target/release/exe-metal diagnose <bottle> # log signatures + silent failures
./target/release/exe-metal play --bottle sk <exe>   # reuse a bottle, fast
./target/release/exe-metal ui                # the window
```

`bottles/sk` is a small existing bottle useful for quick launches. Creating a
new one clones ~700 MB and re-signs it, so pass `--bottle` when testing.

Cross-check `probe` against `file(1)` — they must agree on bitness.

## Layout

| File | What it owns |
|---|---|
| `catalog.rs` | Known runtimes/renderers, download URLs, pairing rules |
| `paths.rs` | On-disk layout; locating `wine`/`lib` in differing bundle shapes |
| `engine.rs` | Composing runtime + renderer via APFS clone, then re-signing |
| `bottle.rs` | Prefix creation and per-bottle config |
| `env.rs` | The environment that produces the framerate |
| `hw.rs` | M1–M5 detection and capability gates |
| `probe.rs` | PE header + DLL-reference reading; renderer choice |
| `auto.rs` | `play`: decide → create → check → launch → record → explain |
| `diagnose.rs` | Failure signatures and the silent-failure checks |
| `ui.rs` | egui window; all slow work on a worker thread |

## Things that will bite

- **D3DMetal is 64-bit only.** No 32-bit binary may ever be routed to it;
  `probe.rs` has a test asserting this across every API combination.
- **DXMT needs `winemetal`**, which only `sikarugir-10.0` exports.
  `catalog::check_pairing` rejects the rest before anything downloads.
- **Renderer overlays must preserve symlinks** — `util::overlay` uses `cp -R`
  on `src/.` deliberately. Dereferencing produces an engine that looks right
  and does not work.
- **Overlaying invalidates code signatures.** `engine::ensure` re-signs after
  every overlay or macOS refuses to load the frameworks.
- **Diagnostic signatures must be narrow.** A matcher that fires on healthy
  logs is worse than no matcher. `NO CONNECTION` was one such false positive;
  see the regression test in `diagnose.rs` holding a real healthy log.
- **Never widen a `Match::Any` needle without a healthy-log test.**
- **A renderer is per bottle, not per game.** Steam launches games itself, so
  everything in a Steam bottle gets that bottle's renderer. D3DMetal ships no
  32-bit `d3d11.dll`, so 32-bit Steam games in a `d3dmetal` bottle silently
  fall back to wined3d. Use a separate `dxmt` bottle for those.
- **Never disable `mscoree`.** It is the .NET loader; turning it off silently
  breaks every XNA/FNA/MonoGame title. The Mono/Gecko dialogs it was suppressing
  only appear when the installer is absent, and the runtimes bundle it.
- **Steam games cannot be launched by path.** `steam_api.dll` next to an exe
  means it will re-invoke `steam://run/<appid>` and exit, leaving an extra
  client behind. Route through `steam::run_app`.
- **Settings are frozen at bottle creation.** Anything added to `Perf` or
  `WindowCfg` needs a migration in `exe-metal repair`, or existing bottles keep
  the old value forever — that is how a bottle stayed on esync at 4x the CPU.
- **`ps` shows Wine processes by their Windows path**, so matching on the unix
  prefix finds the loader and misses every game. See `status::processes`.
- **`probe` must reject a damaged PE, not shrug at it.** An MZ header with no
  valid PE header means a truncated download; accepting it as `Unknown` creates
  a bottle and defers the failure into Wine where the cause is invisible.

## Testing GUI changes

Use the `computer-use` skill — never AppleScript/System Events, which steal the
real cursor and keyboard.
