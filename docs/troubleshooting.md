# Troubleshooting

Start here:

```sh
exe-metal doctor              # the machine, and what is installed
exe-metal diagnose <bottle>   # log signatures + the silent failures
exe-metal status <bottle>     # what is happening right now
exe-metal repair <bottle>     # migrate settings frozen at creation
```

`repair` is worth running first on any bottle made by an older version. Settings
are written once at creation and never revisited, so a bottle can sit on a worse
default indefinitely without anything mentioning it.

---

## The game exits immediately and says nothing

**Almost always .NET.** Anything built on XNA, FNA or MonoGame — Celeste,
Stardew Valley, Terraria — needs wine-mono. With Wine's channels on you will
see:

```
err:mscoree:LoadLibraryShim error reading registry key for installroot
```

```sh
exe-metal repair <bottle>
```

The installer is bundled with the runtime, so this needs no download. Note that
`wineboot -u` does **not** fix it: Mono is only installed when a prefix is
first created.

## Nothing renders any text

FreeType. Wine `dlopen`s `libfreetype.6.dylib` by bare soname, which ignores the
binary's rpath, so dropping the dylib next to the binaries is not enough — it
has to be on `DYLD_FALLBACK_LIBRARY_PATH`. The failure is completely silent: the
game starts, runs, and draws no text anywhere, which reads as a font bug.

`exe-metal diagnose` checks for this directly. To fix, rebuild the engine:

```sh
exe-metal engine remove <runtime> <renderer>
```

## The pointer will not leave the game / I cannot drag it to my other monitor

One setting. `CaptureDisplaysForFullscreen` takes the display away from the
window server, so macOS can no longer composite over it, move it, or hand off
the pointer. It also breaks cmd-tab and makes resizing fight back.

```sh
exe-metal window <bottle> --no-capture           # the default since 0.1.0
exe-metal window <bottle> --virtual-desktop --size 1920x1080
```

Virtual desktop is the stronger option — the game renders into an ordinary
resizable macOS window, so moving and resizing become the window server's
problem rather than Wine's.

## Two Steam windows appear

You launched a Steam game by its executable path. A game shipping
`steam_api.dll` re-invokes the client and exits:

```
Startup - Steam Client launched with: "…\steam.exe" steam://run/504230
```

So a direct launch gets you no game and one extra client, every time. Use
`exe-metal play`, which detects this and hands the game to the client that
already owns the library.

## Steam hangs at 0% and never downloads

The Wine build is too old. Apple's Game Porting Toolkit bundles D3DMetal but its
Wine base is 7.7, from 2022, and the current Steam client sits at bootstrap
forever on it. Nothing else you change will help.

```sh
exe-metal create <bottle> --runtime sikarugir-10.0 --force
```

## Steam says "Unexpected Transport Error (0x3008)"

`steamwebhelper` was rejected. Same cause as above — Wine 10.0 is the oldest
base verified working.

## Steam logs in and is immediately disconnected

A Steam account holds exactly one session. Quit the native macOS client and any
other machine, then launch again.

Note that a *lingering* `ipcserver` helper from a closed native client is
harmless — only the client itself (`steam_osx`) takes the session.

## Something is burning CPU and nothing is on screen

A wineserver died badly and left its children reparented to init, where they
spin forever. Observed at 512–580% CPU with a stalled download and no window.

```sh
exe-metal status <bottle>     # flags it as ORPHANED AND SPINNING
exe-metal kill <bottle>
exe-metal prune --force
```

These are hard to spot manually because `ps` shows their *Windows* path, so
grepping for the install directory finds nothing.

## A 32-bit installer crashes

```
NtRaiseException ... unable to dispatch exception
```

32-bit Inno Setup installers hit an SEH dispatch bug in Wine 11.14's new WoW64.
NSIS installers, including Steam's, are fine.

Either use `--runtime sikarugir-10.0`, or extract on the macOS side and skip the
installer entirely:

```sh
brew install innoextract && innoextract setup.exe
```

## A 32-bit game runs but performs badly

Check what it is actually loading:

```sh
exe-metal probe <game exe>
```

D3DMetal has **no 32-bit build** — it ships only `ddraw.dll` for
`i386-windows`. In a `d3dmetal` bottle a 32-bit game silently falls back to
Wine's `wined3d`, which is far slower. DXMT ships the full 32-bit set:

```sh
exe-metal create <name> --runtime sikarugir-10.0 --renderer dxmt
```

This is invisible when the game comes from Steam, because Steam launches it into
whatever bottle it lives in.

## A DLL is missing

```
err:module:import_dll Library ... not found
```

Usually a redistributable. Most games ship theirs in a `_CommonRedist` folder —
run the `vcredist` installer inside it.

## macOS refuses to load a library

```
code signature ... not valid for use in process
```

Overlaying files invalidates the signature of the bundle they landed in.
Rebuild the engine, which re-signs it:

```sh
exe-metal engine remove <runtime> <renderer>
```

## Getting more detail

The exit code alone rarely explains anything. Wine's channels do:

```sh
WINEDEBUG=+err,+seh exe-metal run <bottle> <exe> 2>&1 | tail -50
```

Steam's own logs say far more than Wine's:

```sh
exe-metal steam logs <bottle>
```

Leave `WINEDEBUG` at its default (`-all`) for normal play — Wine formats debug
output even when nothing reads it, and leaving channels on is the single easiest
way to halve your framerate.

## Still stuck

Open a [bug report](https://github.com/NavilanSanthanakrishnan/exe-metal/issues/new?template=bug_report.yml)
with the output of `doctor`, `diagnose` and `status`. Those three answer most of
what anyone would otherwise have to ask you.
