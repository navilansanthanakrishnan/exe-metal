# Architecture

## The three layers

Running a Windows game on an Apple GPU is three translations stacked, and
exe-metal owns none of them — it composes them.

| Layer | What does it | Provided by |
|---|---|---|
| x86-64 → ARM64 | Rosetta 2 | Apple, already on your machine |
| Windows API | Wine | LGPL, downloaded |
| DirectX → Metal | D3DMetal / DXMT / DXVK | downloaded |

## Concepts

- **runtime** — a Wine build. Pristine; never modified after install.
- **renderer** — a DirectX→Metal translation layer, applied as an overlay.
- **engine** — runtime + renderer, composed. Built on first use, shared by every
  bottle that uses the pair.
- **bottle** — a Wine prefix plus its config. One per game is the sane default.

A renderer is not a plugin the way DXVK is on Linux. The Metal backends
*replace* Wine's own `d3d11.dll` and `dxgi.dll` and drop unix-side `.so` shims
next to them, so an engine is a copy of the runtime with those files overlaid.

APFS cloning makes that copy nearly free: a ~700 MB Wine bundle clones in under a
second and costs almost no extra disk until the overlay rewrites a handful of
DLLs.

## On-disk layout

Everything lives under one directory. Uninstalling is one `rm -rf`.

```
~/Library/Application Support/exe-metal/
  cache/                      downloaded tarballs and installers
  runtimes/<id>/              base Wine builds, pristine
  renderers/<id>/             DirectX→Metal overlays
  frameworks/                 shared dylibs the Wine builds link against
  engines/<runtime>+<rend>/   composed, APFS clones
  bottles/<name>/
    exe-metal.toml            configuration
    pfx/                      the Wine prefix
    logs/
  shadercache/<bottle>/       survives prefix rebuilds
  recent.toml
```

Shader caches live **outside** the prefix deliberately. Warm caches are the
single biggest factor in whether a game stutters on first play, so rebuilding a
prefix must not throw them away.

## Modules

| File | Owns |
|---|---|
| `catalog.rs` | Known runtimes and renderers, download URLs, pairing rules |
| `paths.rs` | On-disk layout; locating `wine` across differing bundle shapes |
| `install.rs` | Downloading, extracting, wine-mono |
| `engine.rs` | Composing runtime + renderer, then re-signing |
| `bottle.rs` | Prefix creation and per-bottle config |
| `env.rs` | The environment that produces the framerate |
| `hw.rs` | Apple Silicon detection and capability gates |
| `probe.rs` | PE header and DLL-reference reading; renderer choice |
| `auto.rs` | `play`: decide → create → check → launch → record → explain |
| `diagnose.rs` | Failure signatures and silent-failure checks |
| `status.rs` | Live process and throughput sampling |
| `window.rs` | Pointer, display and multi-monitor behaviour |
| `steam.rs` | The Steam client, and Steam-owned games |
| `ui.rs` | The window; all slow work on a worker thread |

## How a launch is decided

```
exe-metal play game.exe
        │
        ├─ probe::inspect      COFF machine field → bitness
        │                      DLL-name scan → graphics API
        │
        ├─ probe::plan         bitness vetoes first: D3DMetal is 64-bit only,
        │                      so a 32-bit DX11 game must go to DXMT, which in
        │                      turn requires a winemetal-exporting runtime
        │
        ├─ steam::detect_app   steam_api.dll present? then the client must
        │                      launch it, not us
        │
        ├─ bottle              reuse the game's existing one, or create it
        ├─ diagnose            refuse to launch into a broken bottle
        ├─ run::exec           wine [explorer /desktop=…] game.exe
        └─ on failure          scan logs, name the cause
```

The DLL-name scan is deliberately a string search rather than an import-table
walk. Games routinely `LoadLibrary("d3d11.dll")` at runtime, and those never
appear in the import directory; a string scan catches both and cannot be thrown
off by an unusual section layout.

## Design rules

**Pristine runtimes.** `runtimes/` is never written to after install. Everything
mutable happens in a clone.

**Preserve symlinks when overlaying.** The unix-side `d3d11.so` is a symlink to
`../../external/libd3dshared.dylib`. Dereferencing it produces an engine that
looks correct and does not work.

**Re-sign after overlaying.** Replacing a DLL inside a signed framework
invalidates its signature, and macOS then refuses to load it.

**Reject bad pairings before downloading.** `catalog::check_pairing` knows DXMT
needs `winemetal`, so the error arrives in milliseconds rather than after a
700 MB download and a failed launch.

**Settings frozen at creation must be migratable.** Anything added to `Perf` or
`WindowCfg` needs a migration in `exe-metal repair`, or existing bottles keep
the old value forever.

**Diagnostics must be narrow.** A signature that fires on healthy logs is worse
than no signature, because it trains people to ignore the one that matters.
