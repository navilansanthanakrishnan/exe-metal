# exe-metal

Run Windows games and applications on Apple Silicon macOS, using only free
components.

```sh
curl -fsSL https://raw.githubusercontent.com/NavilanSanthanakrishnan/exe-metal/main/install.sh | sh
exe-metal play "/path/to/game.exe"
```

That is the whole thing. `play` reads the executable, works out whether it needs
DirectX 9, 11 or 12, whether it is 32- or 64-bit, which of five renderers can
actually run it, and creates the bottle. You do not need to know that D3DMetal
has no 32-bit build, or that DXMT requires a Wine that exports `winemetal`.

Requires macOS 14 (Sonoma) or newer, Apple Silicon, and Rosetta 2. There is no
Intel path — the whole design rests on translating DirectX to Metal on an Apple
GPU.

## Install

The installer checks your machine, installs Rust if you do not have it, builds,
installs to `~/.local/bin`, and runs `exe-metal setup`. From nothing to a
working bottle in about 40 seconds on an M5 Pro.

```sh
curl -fsSL https://raw.githubusercontent.com/NavilanSanthanakrishnan/exe-metal/main/install.sh | sh
```

Or take a [prebuilt binary](https://github.com/NavilanSanthanakrishnan/exe-metal/releases):

```sh
shasum -a 256 -c exe-metal-*.tar.gz.sha256
tar -xzf exe-metal-*.tar.gz
install -m 0755 exe-metal-*/exe-metal ~/.local/bin/
xattr -d com.apple.quarantine ~/.local/bin/exe-metal   # ad-hoc signed, not notarised
exe-metal setup
```

Or build it:

```sh
git clone https://github.com/NavilanSanthanakrishnan/exe-metal
cd exe-metal
cargo build --release                 # with the window
cargo build --no-default-features     # CLI only, builds in seconds
./target/release/exe-metal setup
```

If Rosetta is missing, `setup` says so rather than escalating on your behalf:

```sh
sudo softwareupdate --install-rosetta --agree-to-license
```

## Quick start

```sh
exe-metal ui                        # a window: pick an .exe, boot it
exe-metal play "/path/to/game.exe"  # same, from the terminal
exe-metal probe "/path/to/game.exe" # what would be used, without running it
exe-metal status <bottle>           # is it working right now?
exe-metal diagnose <bottle>         # why isn't it?
```

```console
$ exe-metal probe ".../Celeste.exe"
  binary    32-bit x86
  graphics  none named in the binary
  runtime   sikarugir-10.0
  renderer  dxmt  — 32-bit, so D3DMetal is out (64-bit only)
  bottle    celeste  (will be created)
```

Steam works too, and Steam games are handled properly — a game shipping
`steam_api.dll` cannot be launched by path, so `play` hands it to the client
that owns the library. See [docs/steam.md](docs/steam.md).

```sh
exe-metal steam setup
```

## Library and Mac app

```sh
exe-metal open ~/Downloads/setup.exe   # install it; what it installs joins the library
exe-metal library                      # installed titles, exe-metal and CrossOver bottles
exe-metal launch <id>                  # Steam games go to Steam, Blizzard games to Battle.net
exe-metal quit <id>                    # WM_CLOSE, so the game saves its settings
exe-metal shortcut <id> --desktop      # a native .app for the Dock and Spotlight
./packaging/make-app.sh --install      # EXE Metal.app: double-click any .exe
```

CrossOver bottles are a second backend: titles that only run under
CrossOver's patched Wine are listed and launched through `cxstart`. The library
index lives beside the bottles at
`~/Library/Application Support/exe-metal/library.json`.

## What this actually is

CrossOver's performance is not magic. It is three layers, and every one of them
is obtainable for free:

| Layer | CrossOver 26 | exe-metal |
|---|---|---|
| x86-64 → ARM64 | Rosetta 2 | Rosetta 2 — identical |
| Windows API | Wine 11 + CodeWeavers' patches | **the same tree, built from their published source** |
| DirectX 11/12 → Metal | D3DMetal 3.0 | **D3DMetal 3.0** — identical |
| DirectX 10/11 → Metal | DXMT 0.72 | DXMT 0.74 — newer |

Two things make this possible.

**D3DMetal is Apple's, not CodeWeavers'.** CodeWeavers licenses it *from Apple*,
and Apple gives the identical library away in the Game Porting Toolkit. Both
copies report `CFBundleShortVersionString 3.0`.

**Wine is LGPL, so CrossOver's patches are public.** CodeWeavers must publish
the source for the Wine inside CrossOver, and they do. The `winecx-26.1.0`
runtime is that source compiled — their ~6,000 patches, msync included.

**But it did not make anything faster.** A plain Sikarugir Wine 10.0 beat
CrossOver's own tree on both halves of the benchmark below. The paid product's
advantage is not raw speed in the Wine layer, and buying it for that reason is a
mistake in both directions.

What is genuinely behind the paywall: per-game profiles, QA, support, and the
anti-cheat work in CrossOver 26. That last one is the real moat, and nothing
free reproduces it.

## Measured

`bench/dx12bench.cpp` on an M5 Pro, three interleaved passes, warm shader cache:

| | GPU-bound (1080p, 256-iteration PS) | CPU/API-bound (5000 draws/frame) |
|---|---|---|
| `sikarugir-10.0` | **222 fps** | **604k draws/s** |
| `winecx-26.1.0` | 185 fps | 582k draws/s |

D3D12 through D3DMetal reports feature level **12_2** (DirectX 12 Ultimate) with
ray tracing available.

### esync versus msync

One flag, same bottle, same client:

| | esync | msync |
|---|---|---|
| `steam.exe` CPU | 540–580% | **141–188%** |
| System load | 18.3 ↑ | **7.4 ↓** |
| Download | stalled | **~293 Mbps** |

Recorded as an observation, not a proven cause — one variable changed and four
things improved together, twice versus once. `exe-metal repair` migrates
existing bottles.

> Read the benchmark carefully: it is synthetic. It proves the D3D12 path works
> and roughly where the ceiling is; it does not predict any specific title.

## Hardware

Detected, not assumed — three things genuinely differ across the line:

| | M1 / M2 | M3 and later |
|---|---|---|
| Hardware ray tracing | **no** | yes |
| `dxr` worth enabling | no — costs framerate for nothing | yes |
| MetalFX upscaling | yes | yes |

The generation is parsed from the CPU brand string rather than matched against a
list, so a chip newer than this table identifies correctly. **Rosetta only
advertises AVX on macOS 15+**, so `doctor` reports it unavailable on 14 rather
than implying a speedup that is not happening.

## Documentation

- [Troubleshooting](docs/troubleshooting.md) — every failure hit so far, and its fix
- [Steam](docs/steam.md) — setup, launching games, the 32-bit caveat
- [Architecture](docs/architecture.md) — runtimes, renderers, engines and bottles
- [Observations](docs/observations.md) — measurements and open issues from live use
- [Roadmap](ROADMAP.md) and [changelog](CHANGELOG.md)
- [Third-party licences](THIRD-PARTY.md) — read this before shipping anything
  built on this

## Project status

Pre-1.0.

Verified working:

- Wine 10.0 boots a WoW64 prefix; 32- and 64-bit binaries both run
- The Windows Steam client installs, updates, logs in and holds the session
- 3D rendering: Unigine Heaven through DXMT → Metal at 2560×1440 with MetalFX
- A Windows Vulkan binary creating a `VkDevice` on the Apple GPU
- Celeste, a real .NET game, launched through Steam
- Fresh install in 41.8s unattended; 132 unit + 40 integration tests

Not yet verified:

- **Very few games have been run.** Every game report is genuinely useful —
  [file one](https://github.com/NavilanSanthanakrishnan/exe-metal/issues/new?template=game_report.yml),
  including titles that work.
- **Frame pacing.** Average fps is measured; 1% lows are not, and those are what
  make a game feel bad.
- `cnc-ddraw` has never run anything.
- **Kernel-level anti-cheat does not work** and is out of scope.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The short version: failures here are
silent, so *verify by running it* — a diff that reads correctly can still do
nothing at all.

## Licence

MIT — see [LICENSE](LICENSE).

exe-metal is an orchestration layer and owns none of what it downloads.
**D3DMetal is Apple's, distributed under Game Porting Toolkit terms that
describe evaluation and development use.** It is free to obtain; it is not free
of terms. See [THIRD-PARTY.md](THIRD-PARTY.md) before shipping anything built on
this. If that is a problem, `--renderer dxmt` and `--renderer dxvk` are open
source and carry no such restriction.

This project runs software you already own. It contains no game content and no
DRM circumvention.

## Credits

[Gcenx](https://github.com/Gcenx) for the macOS Wine builds, the GPTK repackage
and Sikarugir · [3Shain](https://github.com/3Shain) for DXMT · Apple for
D3DMetal · [mirpo](https://github.com/mirpo) for identifying Wine 10.0 as the
build that actually runs the Steam client.
