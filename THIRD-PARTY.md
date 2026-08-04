# Third-party components

exe-metal is an orchestration layer. It is MIT licensed, but it downloads and
composes components it does not own, each under its own terms. Nothing in this
repository vendors them — they are fetched from their official release
artefacts at install time.

| Component | Licence | Source |
|---|---|---|
| Wine | LGPL-2.1-or-later | <https://winehq.org> |
| Wine (CrossOver tree) | LGPL-2.1-or-later | <https://media.codeweavers.com/pub/crossover/source/> |
| DXMT | LGPL / MIT | <https://github.com/3Shain/dxmt> |
| DXVK / D9VK | zlib | <https://github.com/doitsujin/dxvk> |
| cnc-ddraw | LGPL-2.1 | <https://github.com/FunkyFr3sh/cnc-ddraw> |
| wine-mono | MIT | <https://github.com/madewokherd/wine-mono> |
| **D3DMetal** | **Apple, proprietary** | Game Porting Toolkit terms |
| Steam client | Valve, proprietary | <https://store.steampowered.com/> |

## The one to actually read

**D3DMetal is Apple's.** CodeWeavers licenses it from Apple; Apple distributes
the identical library in the Game Porting Toolkit. It reaches your machine
inside the Sikarugir wrapper template that exe-metal downloads.

Apple's Game Porting Toolkit terms describe **evaluation and development use**.
It is free to obtain; it is not free of terms. If you intend to ship, sell, or
distribute anything built on this, read them first — this project cannot grant
you rights it does not have.

If that is a problem for your use case, `--renderer dxmt` and `--renderer dxvk`
are open source and carry no such restriction. DXMT covers DirectX 10/11 at both
bitnesses; only DirectX 12 genuinely requires D3DMetal.

## Sikarugir

The [Sikarugir](https://github.com/Sikarugir-App) wrapper template and engine
builds currently ship **without a licence file**, and therefore grant no formal
rights. exe-metal downloads their official release artefacts rather than
redistributing them, which keeps the question between you and the upstream
project rather than routing it through this one.

## Wine and the LGPL

Wine is LGPL, which is why CrossOver's ~6,000 patches are public and why the
`winecx-26.1.0` runtime can exist at all. exe-metal does not modify Wine — it
clones a pristine build and overlays renderer DLLs onto the copy. The original
trees under `runtimes/` are never written to.

## Games

exe-metal runs software you already have. It contains no game content, no DRM
circumvention, and no means of obtaining games you do not own. Bring your own,
legitimately.
