# Steam

## Setting it up

```sh
exe-metal steam setup            # bottle + client, in one step
exe-metal steam launch <bottle>
exe-metal steam logs <bottle>    # far more informative than Wine's output
```

The client needs **Wine 10.0 or newer**. Apple's Game Porting Toolkit bundles
D3DMetal but its Wine base is 7.7, and the current client hangs at bootstrap on
it forever — 0% CPU, no download, no error. If Steam hangs, the Wine build is
too old; nothing else you change will help.

A fresh bottle needs **two launches**. The bootstrapper installs the 32-bit
client, discovers the client is now 64-bit, fetches that too, logs
`Update complete, launching Steam...` and exits without leaving a client
running. `exe-metal steam launch` handles the relaunch.

## Launching games

**Do not launch a Steam game by its executable path.** A game shipping
`steam_api.dll` re-invokes the client and exits:

```
Startup - Steam Client launched with: "…\steam.exe" steam://run/504230
```

So you get no game, plus an extra Steam client. Repeat it and you have several —
this is the entire "duplicate Steam" problem.

```sh
exe-metal play ".../steamapps/common/Celeste/Celeste.exe"
# → this is a Steam game (Celeste), so Steam has to start it
# → asking the running Steam client to launch app 504230
```

The appid is resolved by matching `installdir` in `appmanifest_*.acf` rather
than the folder name, because those differ: Project Playtime installs as
`Poppy Playtime - Multiplayer`.

## Games inherit the bottle

A game launched by Steam is a child process inside the same prefix, so it gets
the whole stack. Verified by inspecting a process Steam launched itself:

```
lsof -p $(pgrep steamwebhelper.exe)
  118 files open under engines/sikarugir-10.0+d3dmetal
  bound to bottles/cosmos-steam/pfx
```

The tuned environment reaches it too — `D3DM_ENABLE_METALFX`,
`MTL_SHADER_CACHE_PATH`, `ROSETTA_ADVERTISE_AVX`, `WINEDEBUG=-all`.

## The 32-bit caveat

**The renderer is fixed per bottle**, and Steam does the launching, so
`probe`'s per-game renderer choice never gets consulted for a Steam title.

In a `d3dmetal` bottle:

| Game | Loads | Result |
|---|---|---|
| 64-bit | D3DMetal (106 KB shim) | Metal, as intended |
| 32-bit | **Wine's `wined3d`** (413 KB) | no Metal, slow fallback |

D3DMetal ships only `ddraw.dll` for `i386-windows`; DXMT ships the full 32-bit
set. So a library with 32-bit titles in it wants a second bottle:

```sh
exe-metal create steam32 --runtime sikarugir-10.0 --renderer dxmt
exe-metal steam install steam32
```

## One account, one session

A Steam account holds exactly one session. If the native macOS client is signed
in, the bottled one logs in and is dropped immediately with `NO CONNECTION`,
which looks like a networking bug and is not one.

A leftover `ipcserver` helper from a closed native client is harmless — only
`steam_osx` itself takes the session.

## Anti-cheat

Kernel-level anti-cheat — Vanguard, EAC in kernel mode, BattlEye — does not run
here, and nothing free reproduces it. This is the genuine moat behind
CrossOver's paywall and it is out of scope for this project.
