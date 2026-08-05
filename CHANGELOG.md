# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Steam and games rendered at half size on Retina displays.** `RetinaMode`
  made Wine report the full pixel resolution while it still laid out at 96 DPI,
  so windows were sized for half the panel and resizing only added scroll bars
  rather than reflowing. Bottles now set `HKCU\Control Panel\Desktop\LogPixels`
  to 192 (200%), which is real Windows DPI scaling — applications re-lay out
  instead of being stretched. `exe-metal repair` migrates existing bottles, and
  `exe-metal window <bottle> --scale <percent>` sets it explicitly.

### Changed

- **Windows applications now open in an ordinary macOS window by default.**
  They get real traffic lights, resize, move between monitors, and go full
  screen with the green button, and the pointer is never trapped. The guest
  keeps a fixed backbuffer and never learns the window changed, which is also
  why resizing no longer crashes games that only ever expected one resolution.
  The window is sized to the main display rather than a fixed guess.

  It also gives the guest a display-mode list. Without one, GoldSrc titles find
  no modes and fall back to 640x480 — which is why Cry of Fear opened as a tiny
  black square. `exe-metal window <bottle> --native` opts out for anything that
  wants to drive the display itself.

### Added

- `exe-metal ui --fullscreen`, plus a full-screen toggle in the window (F11,
  Escape to leave) and zoom controls (Cmd +/−/0). Zoom tracks the viewport by
  default so the interface keeps a constant physical size as the window grows,
  and stops tracking the moment it is adjusted by hand.

## [0.1.0] — 2026-08-03

First public release.

### Added

- **One-command setup.** `exe-metal setup` goes from nothing to a working
  bottle unattended — measured at 41.8s and 3.6 GB on an M5 Pro — and is
  idempotent, so an interrupted run can simply be repeated. `install.sh`
  bootstraps Rust and the binary as well.
- **Automatic renderer selection.** `exe-metal play <exe>` reads the PE header
  for bitness and scans for DLL references to determine the graphics API, then
  picks a runtime and renderer. Verified against `file(1)` across 25
  executables. `exe-metal probe` shows the same reasoning without running
  anything.
- **Hardware capability gates** for M1 through M5 and beyond. The generation is
  parsed from the CPU brand string rather than matched against a list, so newer
  chips identify correctly. Hardware ray tracing is gated to M3+, and Rosetta's
  AVX advertisement to macOS 15+, because enabling either where unsupported
  costs performance and returns nothing.
- **Diagnostics.** `exe-metal diagnose` matches eleven log signatures and, more
  usefully, checks the failures that produce no log line at all — a missing
  FreeType being the worst, since nothing errors and the game simply renders no
  text. `run` and `play` diagnose automatically on a non-zero exit.
- **`exe-metal status`** — live health: processes, CPU, memory, download
  throughput and load. Throughput is sampled twice, because one reading cannot
  distinguish 40 MB/s from stopped.
- **`exe-metal repair`** — migrates bottles to current recommended settings.
  Settings are frozen at creation and were never revisited otherwise.
- **`exe-metal window`** — pointer, display and multi-monitor behaviour,
  including a virtual-desktop mode.
- **A window.** `exe-metal ui` — pick an executable, boot it, and a list of
  recently played titles. Optional via the default-on `ui` feature;
  `--no-default-features` still builds the full CLI in seconds.
- **Integration suite.** `tests/smoke.sh`, 40 checks, with `--fresh` to run
  against a throwaway `HOME` — a genuinely untouched install without a VM.

### Fixed

- **.NET games could never run.** `WINEDLLOVERRIDES=mscoree,mshtml=` was set to
  suppress the Mono and Gecko installer dialogs, but `mscoree=` disables the
  .NET loader outright, so every XNA/FNA/MonoGame title — Celeste, Stardew
  Valley, Terraria — exited before drawing a frame with
  `err:mscoree:LoadLibraryShim error reading registry key for installroot`.
  The runtimes already bundle wine-mono; it is now installed into the prefix
  instead of suppressed.
- **Launching a Steam game by path produced duplicate Steam clients.** A game
  shipping `steam_api.dll` re-invokes the client with `steam://run/<appid>` and
  exits, so launching one directly yields no game and an extra client. `play`
  now resolves the appid from the manifest and hands the game to the client
  that owns the library.
- **Wine captured the display.** `CaptureDisplaysForFullscreen` takes the
  screen from the window server, which trapped the pointer inside the game,
  prevented dragging the window to a second monitor, broke cmd-tab, and made
  resizing fight back. Now off by default.
- **"Steam is up" could be reported for a client that was still starting or
  already dead.** Readiness checked that `connection_log.txt` existed, but it
  survives previous sessions; it now compares modification time against launch
  time.
- **A damaged executable was accepted rather than reported.** An `MZ` header
  with no valid PE header means a truncated download; it produced a bottle and
  deferred the failure into Wine, where the cause is invisible.
- **The native-Steam warning was a false positive.** It matched
  `Steam.AppBundle`, which also matches a lingering `ipcserver` helper, so it
  fired on every launch and was wrong every time.
- **A diagnostic matched healthy logs.** The two-clients signature matched
  `NO CONNECTION`, which also matches `SteamServerConnectFailure_t No
  Connection Reconnect` — routine reconnect noise appearing dozens of times in
  a perfectly healthy session. Narrowed to Steam's own `LoggedInElsewhere`,
  with a regression test holding a real healthy log.

### Performance

- Bottles now default to **msync** where the runtime supports it. On the Steam
  client this measured 141–188% CPU against 540–580% on esync, with system load
  falling from 18.3 to 7.4 and a stalled download resuming at ~293 Mbps.
  Recorded as an observation rather than a proven cause: one variable changed
  and four things improved together, twice versus once.

### Known limitations

- Kernel-level anti-cheat does not work and is not in scope.
- 32-bit games in a `d3dmetal` bottle silently fall back to Wine's `wined3d`,
  because D3DMetal ships no 32-bit DirectX DLLs. Use a `dxmt` bottle for those.
- Very few titles have been verified. See the game reports label.

[Unreleased]: https://github.com/NavilanSanthanakrishnan/exe-metal/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/NavilanSanthanakrishnan/exe-metal/releases/tag/v0.1.0
