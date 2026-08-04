# Observed issues and fixes

Written from a live session running the Windows Steam client and downloading
games. Everything here is something that actually happened, with the numbers
that were measured at the time — not a wishlist.

## The headline finding: esync was costing 4x CPU and killing the window

The `cosmos-steam` bottle was created before `bottle::create` learned to prefer
msync, so it carried `esync = true`. Switching that one flag:

| | esync | msync |
|---|---|---|
| `steam.exe` CPU | 540–580% | **141–188%** |
| System load average | 18.28, climbing | **7.38, falling** |
| Download throughput | stalled — 0 MB in 15s | **+732 MB in 20s (~293 Mbps)** |
| Steam window | vanished twice, ~2 min in | stable |
| Symptom | orphaned process spinning at `ppid=1` | normal |

Twice in a row on esync, the sequence was identical: window appears, renders
the full store UI, MoltenVK tears down its `VkInstance`, window disappears, and
`steam.exe` is left reparented to init burning six cores while the download
makes no progress. CEF itself was healthy throughout — `cef_log.txt` showed
only benign Wine winsock noise, and the newest crash dump was two days old.

This is the README's documented orphan hazard, reproduced live. It had never
been attributed to esync before.

**Caveat on causality.** One variable changed and four things improved
together, twice observed versus once. That is strong but not conclusive — a
clean A/B would relaunch three times on each setting and compare. Worth doing
before treating "esync causes this" as established.

---

## Issues, in the order they are worth fixing

### 1. Existing bottles never get migrated to better defaults — HIGH

`bottle::create` picks msync for runtimes that support it, but that decision is
frozen into `exe-metal.toml` at creation. A bottle made before that logic keeps
esync forever, silently, and nothing ever mentions it. This is what cost six
cores above.

**Fix.** Have `doctor` and `diagnose` compare each bottle's `[perf]` against
what the catalog recommends for its runtime, and report the drift:

```
WARN  bottle 'cosmos-steam' uses esync, but sikarugir-10.0 supports msync,
      which costs measurably less CPU.
      fix: exe-metal tune cosmos-steam        (or edit [perf] by hand)
```

Add an `exe-metal tune <bottle>` that rewrites `[perf]` to current
recommendations, leaving user-set `[env]` alone. Cheap, and it would have made
this session's problem a one-liner instead of an investigation.

### 2. Nothing detects a spinning orphan — HIGH

`prune` finds orphaned `services.exe`, `rpcss.exe` and `plugplay.exe` by name.
It does not find an orphaned `steam.exe`, or an orphaned game. `diagnose` has
no signature for it either, because the failure produces no log line at all —
the process is running perfectly happily, just doing nothing at 580%.

**Fix.** A health check keyed on behaviour rather than name: any process inside
a bottle's prefix whose `ppid` is 1 **and** whose CPU is sustained above a
threshold across two samples a second apart. That is exactly the signature seen
twice here, and it cannot false-positive on a busy game, because a game with a
live wineserver is not reparented to init.

Surface it in `diagnose`, and have `prune` offer to kill it.

### 3. No way to see whether a bottle is healthy *while* it runs — HIGH

Everything exe-metal reports is point-in-time and structural: what is
installed, what is configured. There is no answer to "is this working right
now", which is the only question that mattered for two hours of this session.
Detecting the stall took manual `ps`, `du` sampling and `lsof`.

**Fix.** `exe-metal status <bottle>`:

```
cosmos-steam   sikarugir-10.0 + d3dmetal   msync
  steam.exe      188% cpu   605 MB   up 4m12s
  window         yes
  downloading    Project Playtime  3367 MB  (+36 MB/s)
  load average   7.38
```

Sampled twice a second apart so throughput is real. Nearly all the plumbing
already exists in `diagnose`.

### 4. The native-Steam warning is still a false positive — MEDIUM

`warn_about_native_steam` greps for `Steam.AppBundle`, which matches a leftover
`ipcserver` helper that lingers long after the native client quits. It fired on
every launch this session and was wrong every time — the bottled client logged
in and held the session fine.

**Fix.** Match the actual client process (`steam_osx`) rather than the bundle
path. Warning about something harmless three times trains people to ignore the
one time it matters.

### 5. `steam launch` blocks for up to 5 minutes in the foreground — MEDIUM

Without `--detach` it waits on `wait_until_ready` for 300s. Anything with a
shorter timeout kills it mid-launch, which is how one Steam session died here.

**Fix.** Either default `steam launch` to detached (the client is a GUI app
that should outlive the shell), or print the timeout up front so the wait is
not mistaken for a hang.

### 6. The smoke suite tests starting, not staying up — MEDIUM

All 36 checks passed the whole time this was broken. They verify a process
launches and exits 0; nothing watches a process that is *supposed* to keep
running. The entire failure mode lived in that gap.

**Fix.** A soak check: launch, wait 60s, then assert the process is alive, its
CPU is below a ceiling, and it still owns a window. That single test would have
caught this before it was ever noticed by hand.

### 7. 32-bit Steam games get no Metal — MEDIUM, documented but unsolved

Already in the README: D3DMetal ships only `ddraw.dll` for `i386-windows`, so a
32-bit game launched from a `d3dmetal` Steam bottle silently loads Wine's
`wined3d` (413 KB) instead of D3DMetal (106 KB). Steam does the launching, so
`probe`'s per-game renderer choice never gets consulted.

**Fix, in increasing order of effort:**
- `diagnose` should scan a Steam bottle's `steamapps/common` for 32-bit game
  executables and warn that they will not get Metal in this bottle.
- `exe-metal steam setup --32bit` creating a dxmt bottle for those libraries.
- Longer term: per-game DLL overrides inside one prefix, so a single Steam
  bottle can serve both. Meaningfully harder, and possibly not worth it.

### 8. Nothing captures the win — LOW

The msync-versus-esync difference above is the largest single performance
result this project has produced, and it exists only in a chat log. It belongs
in the README's measured section next to the dx12bench numbers, with the
caveat about causality attached.

---

## Optimisations not yet tried

Ordered by expected value, none of them measured yet:

- **macOS Game Mode.** The README already notes the Metal HUD reported it
  `Off` during benchmarking. It raises CPU and GPU scheduling priority, and on
  a machine that just hit load 18 that is not nothing.
- **`WINEDEBUG` is already `-all`** — confirmed correct in the resolved env.
- **Shader cache is already outside the prefix** — confirmed.
- **`-cef-disable-gpu` for the Steam client.** Currently always on. It is the
  right call for stability, but it may be why the store renders slowly; worth
  measuring with it off now that msync has freed up five cores.
- **`ROSETTA_ADVERTISE_AVX` is set and this machine honours it** (macOS 26.5,
  so ≥15). No action, but worth knowing it is genuinely active here and inert
  for anyone on 14.
- **Download throughput is not the bottleneck.** ~293 Mbps measured, matching
  what the client itself reported. Whatever else is slow, it is not the network.

## Still the biggest gap

No game has rendered a frame. Project Playtime is downloading as this is
written, which will close it — and being a Unity title it should be 64-bit, so
it exercises the D3DMetal path rather than the unproven 32-bit one. Celeste,
queued behind it, is the more interesting test precisely because it is likely
32-bit and will land on the fallback described in issue 7.
