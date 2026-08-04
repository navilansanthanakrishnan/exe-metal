# exe-metal

Run Windows games and applications on Apple Silicon macOS, using only free
components.

Built and verified on **macOS 26.5, Apple M5 Pro, 24 GB**.

```
curl -fsSL .../install.sh | sh      # Rust, build, install, setup — one command
exe-metal ui                        # a window: pick an .exe, boot it
exe-metal play "/path/to/game.exe"  # same thing, no window
```

Or, driving it by hand:

```
exe-metal setup                    # everything: runtime, renderers, engine, bottle
exe-metal steam setup              # bottle + Windows Steam, running
exe-metal run mygame "C:\Games\thing.exe" --hud
```

`play` reads the executable's PE header and its DLL references, then picks the
runtime and renderer itself — you do not have to know that D3DMetal has no
32-bit build, or that DXMT needs a Wine exporting `winemetal`. `exe-metal probe`
shows the same reasoning without running anything:

```
$ exe-metal probe .../x86launcher.exe
  binary    32-bit x86
  graphics  none named in the binary
  runtime   sikarugir-10.0
  renderer  dxmt  — 32-bit, so D3DMetal is out (64-bit only)
  bottle    x86launcher  (will be created)
```

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
the source for the Wine inside CrossOver, and they do, at
`media.codeweavers.com/pub/crossover/source/`. The `winecx-26.1.0` runtime is
that source compiled — their ~6,000 patches, msync included.

**But it did not make anything faster.** See the measurements below: a plain
Sikarugir Wine 10.0 beat CrossOver's own tree on both the GPU-bound and the
CPU-bound half of the test. The paid product's advantage is not raw speed in
the Wine layer, and buying it for that reason is a mistake in both directions.

What is genuinely behind the paywall: per-game profiles, QA, support, and the
anti-cheat work in CrossOver 26 (nProtect GameGuard, EAC, BattlEye), which
CodeWeavers describes as proprietary. That last one is the real moat.

## Concepts

- **runtime** — a Wine build. Provides the Windows API. Pristine, never modified.
- **renderer** — a DirectX→Metal translation layer, applied as an overlay.
- **engine** — runtime + renderer, composed. Built on first use and shared.
- **bottle** — a Wine prefix plus its config. One per game is the sane default.

Engines are composed with APFS clonefile, so a 700 MB Wine bundle clones in
under a second and costs almost no extra disk until the overlay rewrites a
handful of DLLs.

## Choosing a runtime

| Runtime | Wine | Use it when |
|---|---|---|
| `sikarugir-10.0` | 10.0 | **Default.** Fastest measured, ships `winemetal` (so DXMT works) and GStreamer (so cutscenes play). |
| `winecx-26.1.0` | 11.0 (CrossOver 26) | Compatibility fallback for titles the default cannot run. Newer Wine and msync, but slower here. No FreeType or GStreamer of its own — exe-metal supplies FreeType; in-game video will not play. |
| `wine-11.14-staging` | 11.14 | Newest upstream. No Metal backend at all — for non-graphical Windows apps. |
| `gptk-3.0` | 7.7 | Apple's toolkit. Too old for the Steam client; direct exe launches only. |

## Choosing a renderer

| Renderer | APIs | Use it when |
|---|---|---|
| `d3dmetal` | DX 11, 12 | **Default.** The only DirectX 12 backend, so anything modern. 64-bit only. |
| `dxmt` | DX 10, 11 | Open source. Handles 32-bit games, which D3DMetal cannot. |
| `dxvk` | DX 10, 11 | Via Vulkan/MoltenVK. Slower, but renders things the others refuse. |
| `d9vk` | DX 9 | Older titles. |
| `cnc-ddraw` | DirectDraw | Legacy 2D. |

`exe-metal catalog` prints this with the caveats attached.

## Measured

`bench/dx12bench.cpp` is a small DirectX 12 workload built for this. Cross-compile
it with `x86_64-w64-mingw32-g++ -O2 -std=c++17 -o dx12bench.exe bench/dx12bench.cpp
-ld3d12 -ldxgi -ld3dcompiler -luuid -lole32`, then `exe-metal run <bottle> dx12bench.exe`.

What D3D12 reports through D3DMetal on an M5 Pro:

```
adapter    AMD Compatibility Mode
vram       18186 MB
feature    level 12_2          <- DirectX 12 Ultimate
raytracing yes
```

Three interleaved passes per runtime, both on `d3dmetal`, warm shader cache:

| | GPU-bound (1080p, 256-iteration PS) | CPU/API-bound (5000 draws/frame) |
|---|---|---|
| `sikarugir-10.0` | **222 fps** (225/219/221) | **604k draws/s** (602k/605k/605k) |
| `winecx-26.1.0` | 185 fps (194/195/166) | 582k draws/s (564k/593k/591k) |

### esync versus msync, on the Steam client

One flag, measured on the same bottle running the same client:

| | esync | msync |
|---|---|---|
| `steam.exe` CPU | 540–580% | **141–188%** (peak 236%) |
| System load | 18.3, climbing | **7.4, falling** |
| Download | stalled at 0 MB/15s | **+732 MB/20s (~293 Mbps)** |
| Window | vanished twice, ~2 min in | stable for the session |

Twice on esync the client rendered its store UI, then MoltenVK tore down its
`VkInstance`, the window disappeared, and `steam.exe` was left reparented to
init burning six cores while the download made no progress. One variable
changed and four things improved together — strong, but twice-versus-once is
not a controlled experiment, and it is recorded here as an observation rather
than a proven cause.

`exe-metal repair` migrates existing bottles, which is worth doing: settings are
frozen at bottle creation and never revisited otherwise.

Read this carefully before drawing conclusions from it. It is a synthetic test:
one triangle, or 5000 tiny draws. Real games are bound by texture streaming,
state changes and compute, none of which it exercises. It proves the D3D12 path
works and roughly where the ceiling is; it does not predict any specific title.

The first pass of any comparison is worthless — cold shader compilation cost
made CrossOver's build look 46% *faster* until the caches warmed up and the
result inverted.

## Hardware

Detected rather than assumed, because three things genuinely differ across the
line and each one changes what you should turn on:

| | M1 / M2 | M3 and later |
|---|---|---|
| Hardware ray tracing | **no** — RT units arrived with M3 | yes |
| MetalFX upscaling | yes (macOS 13+) | yes |
| `D3DM_SUPPORT_DXR` worth it | no, it costs framerate for nothing | yes |

The generation is parsed out of `machdep.cpu.brand_string` rather than matched
against a list, so a chip newer than this table identifies correctly and
inherits the M3+ answers. `exe-metal doctor` prints all of it, and `diagnose`
warns if a bottle has `dxr = true` on silicon that cannot honour it.

One more that is easy to get wrong: **Rosetta only advertises AVX on macOS 15
and later.** `ROSETTA_ADVERTISE_AVX=1` is accepted and ignored on 14, so doctor
reports it as unavailable there rather than implying a speedup that is not
happening.

## Steam games must be launched by Steam

A game shipping `steam_api.dll` does not run when you launch its executable
directly. It re-invokes the client and exits:

```
Startup - Steam Client launched with: "…\steam.exe" steam://run/504230
```

So launching by path gets you no game and **an extra Steam client**. Do it a
few times and you have several — which is where "duplicate Steam" comes from,
and it is not a bug in Steam.

`exe-metal play` detects this, finds the appid from the manifest, and hands the
game to the client that already owns the library:

```
$ exe-metal play ".../steamapps/common/Celeste/Celeste.exe"
this is a Steam game (Celeste), so Steam has to start it
asking the running Steam client to launch app 504230
```

The appid comes from matching `installdir` in `appmanifest_*.acf` rather than
the folder name, because those differ — Project Playtime installs as
`Poppy Playtime - Multiplayer`.

## .NET games need wine-mono, and it is not installed by default

`env.rs` used to set `WINEDLLOVERRIDES=mscoree,mshtml=` to suppress the Mono
and Gecko installer dialogs. `mscoree=` disables the .NET loader outright, so
**every .NET game failed** — Celeste, Stardew Valley, Terraria, anything
XNA/FNA/MonoGame. The symptom is not obvious:

```
err:mscoree:LoadLibraryShim error reading registry key for installroot
```

and the game exits before drawing a frame.

The runtimes bundle wine-mono already, so nothing needs downloading — it just
has to be installed into the prefix, and `wineboot -u` will not do it after the
fact. `exe-metal repair <bottle>` installs it; new bottles get it at creation.

## Window, pointer and monitors

Four complaints share one cause — Wine capturing the display:

- the pointer cannot leave the game window
- the window cannot be dragged to a second monitor
- cmd-tab does not switch away cleanly
- resizing fights back

`CaptureDisplaysForFullscreen` takes the screen away from the window server, so
macOS can no longer composite over it, move it, or hand off the pointer. Bottles
now default it off, along with `UseSystemCursor` and
`UseSystemKeyboardShortcuts` on.

```
exe-metal window <bottle> --no-capture               # the default
exe-metal window <bottle> --virtual-desktop --size 1920x1080
```

Virtual desktop is the stronger option: the game renders into an ordinary
resizable macOS window, so moving and resizing become the window server's job
rather than Wine's.

## Is it working right now?

```
$ exe-metal status cosmos-steam
cosmos-steam   sikarugir-10.0 + d3dmetal   msync
  window   native windows · display not captured · retina
  Celeste.exe            pid 71089     48.5%    1177 MB
  gameoverlayui64.exe    pid 71149      1.0%     120 MB
  steam.exe              pid 70878      0.7%     265 MB
  library  13823 MB  (idle)
  load     3.18 2.95 3.98
```

Throughput is sampled twice rather than read once, because a single reading
cannot tell a download moving at 40 MB/s from one that stopped ten minutes ago.
A process reparented to init *and* above 200% CPU is flagged as a spinning
orphan — both conditions are required, because a busy game always has a live
wineserver as its parent and so is never orphaned.

## Games launched from the bottled Steam

**Yes, they run through exe-metal.** Steam launches a game as a child process
inside the same Wine prefix, so it inherits the whole stack. Verified by
inspecting a process Steam launched itself, rather than one exe-metal launched:

```
$ lsof -p $(pgrep -f steamwebhelper.exe)
  118 files open under engines/sikarugir-10.0+d3dmetal
  216 files open under bottles/cosmos-steam
  bound to bottles/cosmos-steam/pfx
```

The tuned environment reaches it too — `WINEPREFIX`, `D3DM_ENABLE_METALFX`,
`MTL_SHADER_CACHE_PATH`, `ROSETTA_ADVERTISE_AVX`, `WINEESYNC` and
`WINEDEBUG=-all` are all present in a child launched inside the bottle.

**But the renderer is fixed per bottle, and that is a real limitation.** A
bottle is created with one renderer, and every game Steam launches into it gets
that one. There is no per-game choice, because Steam — not exe-metal — is doing
the launching.

That matters most for bitness. In a `d3dmetal` bottle:

| Game | `d3d11.dll` it loads | Result |
|---|---|---|
| 64-bit | D3DMetal (106 KB shim) | Metal, as intended |
| 32-bit | **Wine's builtin wined3d** (413 KB) | no Metal — the slow fallback |

D3DMetal ships only `ddraw.dll` for `i386-windows`; DXMT ships a full 32-bit
set (`d3d11.dll`, `d3d10core.dll`, `dxgi.dll`, `winemetal.dll`). So for a
library with 32-bit titles in it, a second Steam bottle is worth having:

```
exe-metal create steam32 --runtime sikarugir-10.0 --renderer dxmt
exe-metal steam install steam32
```

Anti-cheat is the other hard stop — kernel-level anti-cheat does not run here
regardless of bottle.

## When something breaks

`exe-metal diagnose` scans a bottle's logs and configuration for the failure
signatures in this README and names the fix. `run` and `play` do it
automatically on a non-zero exit, so a bare exit code is never the whole answer.

It also checks the failures that produce **no log line at all** — a missing
FreeType being the worst of them, because nothing errors and the game simply
renders no text anywhere.

A note on signature quality: these are deliberately narrow. An early version
matched `NO CONNECTION` for the two-Steam-clients case, which also matches
`SteamServerConnectFailure_t No Connection Reconnect` — routine reconnect noise
that appears dozens of times in a completely healthy session. It now matches
only Steam's own `LoggedInElsewhere` result, and there is a regression test
holding a real healthy log to keep it that way.

## Verified working

- Wine 10.0 boots a WoW64 prefix; 32- and 64-bit Windows binaries both run.
- The **Windows Steam client** installs, updates (~1.5 GB), reaches the login
  window, and connects to Valve over WebSocket. `steamwebhelper` (CEF, 64-bit)
  starts cleanly — no `0x3008`.
- **3D rendering works.** Unigine Heaven renders through DXMT → Metal at
  2560x1440 with MetalFX spatial upscaling from 1280x720, on Rosetta.
- **Steam logs in.** `RecvMsgClientLogOnResponse() : 'OK'` against a real
  account, with the session held open — not just a login window on screen.
- **Vulkan reaches the GPU.** A 64-bit Windows Vulkan binary run through
  `play` loaded MoltenVK 1.4.1, enumerated 153 extensions, and created a
  `VkDevice` on the Apple M5 Pro. Exit code 0. Wine's own MoltenVK path, with
  no DirectX renderer involved.
- **Binary detection is exact.** `probe` agrees with `file(1)` on both
  bitnesses across a 32/64-bit pair of the same program.
- Engine composition, bottle creation, prune, doctor.

## Not yet verified

- **No real game has been run.** Everything above is a synthetic benchmark, a
  driver-query tool and the Steam client. A shipping title exercises texture
  streaming, audio, input, save files and a launcher — none of which this has
  touched.
- **The 32-bit renderer paths are chosen but unproven.** `probe` correctly
  routes 32-bit binaries to DXMT and DirectX 9 to d9vk, and refuses to pair
  D3DMetal with a 32-bit exe — but no 32-bit game has actually rendered a frame
  through them. Heaven exercised DXMT at 64-bit only.
- **`cnc-ddraw` has never been exercised at all.** It is installed and
  selectable; nothing has run on it.
- **Nothing is known about frame pacing under load.** Average fps is the easy
  number; what makes a game feel bad is the 1% low, and a 10-second synthetic
  loop cannot show that.
- **Anti-cheat.** Kernel-level anti-cheat (Vanguard, EAC in kernel mode,
  BattlEye) will not run. CrossOver 26 solved parts of this with proprietary
  work; nothing free reproduces it.
- **In-game video on `winecx-26.1.0`.** That build has no GStreamer, so intro
  and cutscene playback will fail. Use `sikarugir-10.0` if a game needs it.

## Things that cost hours, written down

**CrossOver's Wine is free — you just have to compile it.** Wine is LGPL, so
CodeWeavers must publish the source for the Wine inside CrossOver, and they do.
Prebuilt macOS binaries exist at `srimanachanta/winecx-dist`. This is not a
clone or a reimplementation; it is their tree. Anyone telling you the free
stack is stuck on upstream Wine has not checked.

**That build ships no FreeType, and the failure is silent.** It starts, boots a
prefix, runs — and draws no text anywhere, which reads as a font bug. Wine
`dlopen`s `libfreetype.6.dylib` by bare soname, which ignores the binary's
rpath entirely, so dropping the dylib next to the binaries is not enough.
`DYLD_FALLBACK_LIBRARY_PATH` has to point at it. exe-metal does both.

**A fresh prefix will hang forever on a dialog you cannot see.** `wineboot` on
a new prefix offers to install Mono and Gecko and then blocks on GUI input. It
looks exactly like a hang. `WINEDLLOVERRIDES=mscoree,mshtml=` turns a 3-minute
timeout into a 7-second boot.

**Wine version is the whole game for Steam.** Apple's Game Porting Toolkit
bundles D3DMetal but its Wine base is **7.7, from 2022**. The current Steam
client hangs at bootstrap on it — it downloads nothing and sits at 0% CPU
forever. Wine 10.0 downloads the client at full speed. If Steam hangs, the Wine
build is too old; nothing else you change will help.

**Steam needs two launches on a fresh bottle.** The bootstrapper installs the
32-bit client, then discovers the client is now 64-bit, fetches that too, logs
`Update complete, launching Steam...` and exits without leaving a client
running. `exe-metal steam launch` relaunches automatically until the client
opens a connection log.

**DXMT needs a Wine that exports `winemetal`.** On stock Wine it fails with
`Failed to create metal view, it seems like your Wine has no exported symbols
needed by DXMT`. `exe-metal` refuses that pairing up front rather than letting
you find out at runtime.

**Orphaned Wine services will quietly eat your machine.** When a `wineserver`
dies badly its `rpcss.exe`, `services.exe` and `plugplay.exe` are reparented to
init and spin forever. Five of them here pushed load average to 65 and made
every measurement worthless. They are hard to spot because `ps` shows their
*Windows* path, so grepping for the install directory finds nothing.
`exe-metal prune` finds them by that signature.

**32-bit Inno Setup installers crash on Wine 11.14's new WoW64**
(`NtRaiseException ... unable to dispatch exception` — SEH dispatch). NSIS
installers, including Steam's, are fine. Workaround: extract with
`innoextract` on the macOS side and skip the installer.

**Renderer overlays must preserve symlinks.** The unix-side `d3d11.so` is a
symlink to `../../external/libd3dshared.dylib`; dereferencing it produces an
engine that looks correct and does not work.

**Two Steam clients cannot share one account.** If the native macOS Steam is
signed in, the bottled client logs in and is dropped immediately with
"NO CONNECTION". `exe-metal` warns when it sees the native client running.

## Performance knobs

Set per bottle in `exe-metal.toml`, applied by `exe-metal env`:

- `WINEDEBUG=-all` — always on. Wine formats debug output even when nothing
  reads it; leaving channels enabled is the single easiest way to halve your
  framerate.
- `ROSETTA_ADVERTISE_AVX=1` — Rosetta hides AVX by default; modern engines take
  better code paths when they can see it.
- `WINEESYNC` / `WINEMSYNC` — thread synchronisation.
- `D3DM_ENABLE_METALFX` — MetalFX upscaling. Verified working.
- `D3DM_SUPPORT_DXR` — ray tracing. Off by default; it costs performance.
- Shader caches live outside the prefix, so rebuilding a prefix does not throw
  away compiled pipelines.

Turn on **macOS Game Mode** for real sessions — the Metal HUD reported it `Off`
during testing, and it raises scheduling priority for CPU and GPU.

## Layout

Everything lives in one directory; uninstalling is one `rm -rf`.

```
~/Library/Application Support/exe-metal/
  cache/        downloads
  runtimes/     Wine builds, pristine
  renderers/    DirectX→Metal overlays
  frameworks/   shared dylibs the Wine builds link against
  engines/      composed runtime+renderer (APFS clones)
  bottles/      prefixes + exe-metal.toml
  shadercache/  survives prefix rebuilds
```

## Licensing, honestly

Wine is LGPL. DXMT is LGPL. **D3DMetal is Apple's**, distributed under the Game
Porting Toolkit terms, which describe evaluation and development use. It is
redistributed inside the Sikarugir wrapper template that exe-metal downloads.
That is how every free tool in this space obtains it. It is free to get; it is
not free of terms, and this is worth knowing before shipping anything built on
it.

Sikarugir itself currently ships **without a licence file**, so it grants no
formal rights. exe-metal downloads its official release artefacts rather than
vendoring them.

## Credits

Gcenx, for the macOS Wine builds, the GPTK repackage and Sikarugir.
3Shain, for DXMT. Apple, for D3DMetal. mirpo's
`windows-steam-on-apple-silicon`, which identified Wine 10.0 as the engine that
actually runs the Steam client.
