//! Known-good runtimes and renderers.
//!
//! Every entry here was installed and exercised on macOS 26.5 / Apple M5 Pro.
//! The `notes` field records what actually happened, including the
//! combinations that fail, so the tool can steer callers away from them
//! instead of letting them rediscover the same dead ends.

pub struct Runtime {
    pub id: &'static str,
    pub url: &'static str,
    pub archive: &'static str,
    pub wine_version: &'static str,
    /// True when the build exposes the `winemetal` entry points DXMT needs.
    pub winemetal: bool,
    /// True when 32-bit Windows binaries work (Steam's client is 32-bit).
    pub win32: bool,
    /// True when `ntdll` carries CrossOver's Mach-semaphore synchronisation.
    /// Cheaper than esync, and on builds without eventfd it is the only option.
    pub msync: bool,
    /// True when the build relies on the wrapper's shared dylibs instead of
    /// carrying its own. Getting this wrong shows up as a missing FreeType (no
    /// text renders anywhere) or a dyld failure at startup.
    pub needs_frameworks: bool,
    pub notes: &'static str,
}

pub struct Renderer {
    pub id: &'static str,
    /// DirectX versions this backend translates.
    pub apis: &'static str,
    pub notes: &'static str,
}

/// Base Wine builds.
pub const RUNTIMES: &[Runtime] = &[
    Runtime {
        id: "winecx-26.1.0",
        url: "https://github.com/srimanachanta/winecx-dist/releases/download/v26.1.0/winecx-26.1.0-osx64.tar.gz",
        archive: "winecx-26.1.0-osx64.tar.gz",
        wine_version: "11.0 (CrossOver 26)",
        winemetal: false,
        win32: true,
        msync: true,
        needs_frameworks: true,
        notes: "CrossOver 26's own Wine, built from the LGPL source CodeWeavers is obliged to \
                publish — their ~6,000 patches, msync included. Measured SLOWER than \
                sikarugir-10.0 here: 185 vs 222 fps GPU-bound, 582k vs 604k draw calls/s. \
                Keep it for compatibility on titles the default cannot run, not for speed. \
                Ships no FreeType or GStreamer: exe-metal supplies FreeType, but in-game \
                video will not play.",
    },
    Runtime {
        id: "sikarugir-10.0",
        url: "https://github.com/Sikarugir-App/Engines/releases/download/v1.0/WS12WineSikarugir10.0_6.tar.xz",
        archive: "WS12WineSikarugir10.0_6.tar.xz",
        wine_version: "10.0",
        winemetal: true,
        win32: true,
        msync: true,
        needs_frameworks: true,
        notes: "Default, and the fastest measured here on both the GPU- and CPU-bound halves \
                of bench/dx12bench. Runs the modern Steam client end to end. The only runtime \
                shipping winemetal, so the only one DXMT runs on. Carries GStreamer, so \
                in-game video works.",
    },
    Runtime {
        id: "wine-11.14-staging",
        url: "https://github.com/Gcenx/macOS_Wine_builds/releases/download/11.14/wine-staging-11.14-osx64.tar.xz",
        archive: "wine-staging-11.14-osx64.tar.xz",
        wine_version: "11.14",
        winemetal: false,
        win32: true,
        msync: false,
        needs_frameworks: false,
        notes: "Newest upstream Wine. No Metal backend and no winemetal, so DirectX falls back \
                to wined3d. 32-bit Inno Setup installers crash under its WoW64 (SEH dispatch \
                failure).",
    },
    Runtime {
        id: "gptk-3.0",
        url: "https://github.com/Gcenx/game-porting-toolkit/releases/download/Game-Porting-Toolkit-3.0-2/game-porting-toolkit-3.0-2.tar.xz",
        archive: "game-porting-toolkit-3.0-2.tar.xz",
        wine_version: "7.7",
        winemetal: false,
        win32: true, // via wine32on64
        msync: false,
        needs_frameworks: false,
        notes: "Apple's Game Porting Toolkit, bundles D3DMetal. Wine base is 7.7 (2022): the \
                current Steam client hangs at bootstrap on it. Fine for launching a game exe \
                directly, not for running Steam.",
    },
];

/// DirectX -> Metal translation layers, applied as overlays onto a runtime.
///
/// All of these are extracted from the Sikarugir wrapper template, which is the
/// only place they are distributed together and version-matched.
pub const RENDERERS: &[Renderer] = &[
    Renderer {
        id: "d3dmetal",
        apis: "DirectX 11, 12",
        notes: "Apple's D3DMetal 3.0 — the same library CrossOver licenses. The only backend \
                with DirectX 12, so it is the one heavy modern titles need. 64-bit only.",
    },
    Renderer {
        id: "dxmt",
        apis: "DirectX 10, 11",
        notes: "Open source (LGPL). Handles 32-bit as well as 64-bit. Requires a runtime that \
                exposes winemetal — stock Wine fails with 'no exported symbols needed by DXMT'.",
    },
    Renderer {
        id: "dxvk",
        apis: "DirectX 10, 11",
        notes: "Translates to Vulkan, then MoltenVK to Metal. An extra hop, so slower than \
                D3DMetal, but it renders things the others refuse.",
    },
    Renderer {
        id: "d9vk",
        apis: "DirectX 9",
        notes: "For older titles. DirectX 9 over Vulkan.",
    },
    Renderer {
        id: "cnc-ddraw",
        apis: "DirectDraw",
        notes: "Legacy 2D titles.",
    },
];

/// The wrapper template is the distribution vehicle for every renderer plus the
/// shared dylibs the Wine bundles link against.
pub const TEMPLATE_URL: &str =
    "https://github.com/Sikarugir-App/Wrapper/releases/download/v1.0/Template-1.0.11.tar.xz";
pub const TEMPLATE_ARCHIVE: &str = "Template-1.0.11.tar.xz";
pub const TEMPLATE_APP: &str = "Template-1.0.11.app";

pub const STEAM_INSTALLER_URL: &str =
    "https://cdn.akamai.steamstatic.com/client/installer/SteamSetup.exe";
pub const STEAM_INSTALLER: &str = "SteamSetup.exe";

pub const DEFAULT_RUNTIME: &str = "sikarugir-10.0";
pub const DEFAULT_RENDERER: &str = "d3dmetal";

pub fn runtime(id: &str) -> Option<&'static Runtime> {
    RUNTIMES.iter().find(|r| r.id == id)
}

pub fn renderer(id: &str) -> Option<&'static Renderer> {
    RENDERERS.iter().find(|r| r.id == id)
}

/// Reject pairings that are known not to work before anything is downloaded.
pub fn check_pairing(runtime_id: &str, renderer_id: &str) -> Result<(), String> {
    let rt = runtime(runtime_id).ok_or_else(|| format!("unknown runtime '{runtime_id}'"))?;
    renderer(renderer_id).ok_or_else(|| format!("unknown renderer '{renderer_id}'"))?;

    if renderer_id == "dxmt" && !rt.winemetal {
        return Err(format!(
            "runtime '{runtime_id}' (wine {}) does not export winemetal, which DXMT requires.\n\
             Use --runtime sikarugir-10.0, or pick --renderer d3dmetal.",
            rt.wine_version
        ));
    }
    Ok(())
}
