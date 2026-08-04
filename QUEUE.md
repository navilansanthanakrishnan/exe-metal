# Work queue

Feature set requested: one-command install, M1–M5 support, auto error
detection, and a simple GUI. All landed and verified.

## Done

- [x] `hw.rs` — Apple Silicon generation/variant detection, capability gates
- [x] `probe.rs` — PE header bitness + DirectX sniffing, renderer choice
- [x] `diagnose.rs` — 11 log signatures + the silent-failure checks
- [x] `recent.rs` — persisted recent-executable list
- [x] `setup.rs` — `exe-metal setup`, idempotent, one command
- [x] `auto.rs` — `play`/`probe` commands
- [x] `ui.rs` — egui window: pick exe, boot, recent list
- [x] `install.sh` — curl-able bootstrap
- [x] README, AGENTS.md

Verified: 30 tests pass; `probe` agrees with `file(1)` on both bitnesses; a
Windows Vulkan binary reached the GPU (MoltenVK → Apple M5 Pro, exit 0); the
window opens, inspects, and boots via the worker thread.

## Next

1. **Run an actual game.** Still the biggest gap — the README has said so
   since before this work and it is still true. Everything so far is a
   benchmark, a driver-query tool, and the Steam client. A free Steam title
   (TF2) would exercise texture streaming, audio, input, save files and a
   launcher in one go.
2. **Exercise the 32-bit paths.** `probe` routes 32-bit to DXMT and DX9 to
   d9vk, and refuses D3DMetal — but no 32-bit game has rendered a frame
   through them. A 32-bit freeware title would close this.
3. **`cnc-ddraw` has never run anything.** Installed, selectable, unproven.
4. **Frame pacing.** Average fps is measured; 1% lows are not, and those are
   what make a game feel bad.
5. Signature coverage grows from real failures only — add one when something
   actually breaks, with a healthy-log regression test alongside it.
