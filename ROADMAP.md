# Roadmap

No dates. Ordered by how much each would actually change things.

## 1. Run more games

The honest state of this project: **very few titles have been verified.** Every
measurement in the README is a synthetic benchmark, a driver-query tool, the
Steam client, or one .NET platformer. A shipping game exercises texture
streaming, audio, input, save files and a launcher, and almost none of that has
been touched.

This is the single most valuable contribution anyone can make, and it needs no
Rust:
[file a game report](https://github.com/NavilanSanthanakrishnan/exe-metal/issues/new?template=game_report.yml).
**Titles that work matter as much as titles that do not** — right now there is
no compatibility data at all.

A per-title compatibility table, generated from those reports, is the goal.

## 2. Close the 32-bit gap

`probe` routes 32-bit binaries to DXMT and DirectX 9 to d9vk, and refuses to
pair D3DMetal with a 32-bit exe — but **no 32-bit game has actually rendered a
frame through those paths.** Heaven exercised DXMT at 64-bit only, and
`cnc-ddraw` has never run anything at all.

Worse, it is invisible for Steam titles: the renderer is fixed per bottle and
Steam does the launching, so a 32-bit game in a `d3dmetal` bottle silently falls
back to `wined3d`.

- `diagnose` should scan a Steam bottle's library for 32-bit executables and
  warn that they will not get Metal
- `exe-metal steam setup --32bit` to create a dxmt bottle for those libraries
- Longer term: per-game DLL overrides inside one prefix, so a single Steam
  bottle serves both. Meaningfully harder, possibly not worth it.

## 3. Frame pacing

Average fps is the easy number. What makes a game feel bad is the 1% low, and a
10-second synthetic loop cannot show it. Needs a real title, a real session, and
a frametime histogram rather than a mean.

## 4. Prove the msync result properly

Switching one bottle from esync to msync dropped Steam from 540–580% CPU to
141–188%, unstalled a download, and stopped the window vanishing. That is the
largest performance result this project has produced — and it is one variable
changed with four things improving together, observed twice versus once.

A controlled run — three launches per setting, same conditions — would turn a
strong observation into a fact. Until then the README says so.

## 5. Distribution

- Homebrew tap, so `brew install exe-metal` works
- Notarised binaries, so the quarantine dance goes away
- Signed releases

## 6. Diagnostics

Signatures should grow from real failures only, never speculation. Each one
needs a regression test holding a **healthy** log that must not trip it — a
matcher that cries wolf is worse than no matcher, because it trains people to
ignore the one that matters.

Open ideas:

- Detect a game window that never appears despite the process being healthy
- Warn when a bottle's renderer cannot serve a game already installed in it
- Surface Wine's own `err:` channels selectively rather than all-or-nothing

## Explicitly out of scope

- **Intel Macs.** The whole design rests on an Apple GPU.
- **Kernel-level anti-cheat.** Vanguard, EAC in kernel mode, BattlEye. Nothing
  free reproduces this and this project will not try.
- **DRM circumvention**, or anything for running software you did not obtain
  legitimately.
