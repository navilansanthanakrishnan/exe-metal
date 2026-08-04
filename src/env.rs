//! Environment assembly.
//!
//! This is where most of the performance actually comes from. The translation
//! layers are already fast; what ruins framerate in practice is leaving Wine's
//! debug channels on, missing AVX, or throwing away the shader cache between
//! runs.

use crate::bottle::Bottle;
use crate::paths::{self, Layout};
use std::collections::BTreeMap;

pub fn build(layout: &Layout, bottle: &Bottle) -> BTreeMap<String, String> {
    let mut e: BTreeMap<String, String> = BTreeMap::new();

    e.insert(
        "WINEPREFIX".into(),
        bottle.prefix(layout).to_string_lossy().into(),
    );

    // Wine dlopens some libraries by bare soname — `libfreetype.6.dylib` most
    // importantly — which ignores the binary's own rpath and searches this
    // path instead. The CrossOver build ships no FreeType, so without this it
    // starts normally and then draws no text anywhere.
    let engine = layout.engine(&bottle.runtime, &bottle.renderer);
    if let Ok(lib) = paths::find_wine_lib(&engine) {
        e.insert(
            "DYLD_FALLBACK_LIBRARY_PATH".into(),
            format!("{}:/usr/local/lib:/usr/lib", lib.display()),
        );
    }

    // Wine's debug channels are formatted and written even when nothing reads
    // them. On a busy frame this alone can halve throughput.
    e.insert("WINEDEBUG".into(), "-all".into());

    // Suppress the Mono/Gecko installer prompts; games do not need them and the
    // dialogs block an unattended launch.
    //
    // The bottle's own overrides are appended rather than allowed to replace
    // this. WINEDLLOVERRIDES is one variable holding a list, so a bottle that
    // needed to disable a single DLL would otherwise drop the suppression above
    // and get a three-minute wait on a dialog nobody can see.
    let mut overrides = vec!["mscoree,mshtml=".to_string()];
    overrides.extend(
        bottle
            .dll_overrides
            .iter()
            .map(|(dll, to)| format!("{dll}={to}")),
    );
    e.insert("WINEDLLOVERRIDES".into(), overrides.join(";"));

    // --- CPU -------------------------------------------------------------
    if bottle.perf.avx {
        // Rosetta hides AVX by default. Modern engines check for it and take
        // markedly better code paths when it is present.
        e.insert("ROSETTA_ADVERTISE_AVX".into(), "1".into());
    }

    // Thread synchronisation. msync is the newer Mach-based path; esync is the
    // widely tested one. Enabling both is not meaningful, so msync wins.
    if bottle.perf.msync {
        e.insert("WINEMSYNC".into(), "1".into());
    } else if bottle.perf.esync {
        e.insert("WINEESYNC".into(), "1".into());
    }

    // --- Graphics --------------------------------------------------------
    match bottle.renderer.as_str() {
        "d3dmetal" => {
            if bottle.perf.metalfx {
                e.insert("D3DM_ENABLE_METALFX".into(), "1".into());
            }
            if bottle.perf.dxr {
                e.insert("D3DM_SUPPORT_DXR".into(), "1".into());
            }
        }
        "dxmt" => {
            if bottle.perf.metalfx {
                e.insert("DXMT_METALFX_SPATIAL_SWAPCHAIN".into(), "1".into());
            }
        }
        "dxvk" | "d9vk" => {
            if bottle.perf.hud {
                e.insert("DXVK_HUD".into(), "fps,gpuload".into());
            }
            // MoltenVK sits underneath; let it recover instead of hard-failing.
            e.insert("MVK_CONFIG_RESUME_LOST_DEVICE".into(), "1".into());
        }
        _ => {}
    }

    if bottle.perf.hud {
        // Apple's Metal overlay works regardless of which backend is driving.
        e.insert("MTL_HUD_ENABLED".into(), "1".into());
    }

    // --- Shader cache ----------------------------------------------------
    // Persisting compiled pipelines across runs is the difference between a
    // stuttery first hour and a smooth one. Keep it outside the prefix so
    // rebuilding the prefix does not throw it away.
    let cache = layout.shader_cache(&bottle.name);
    let _ = std::fs::create_dir_all(&cache);
    e.insert(
        "MTL_SHADER_CACHE_PATH".into(),
        cache.to_string_lossy().into(),
    );
    e.insert("DXVK_STATE_CACHE_PATH".into(), cache.to_string_lossy().into());
    e.insert("__GL_SHADER_DISK_CACHE_PATH".into(), cache.to_string_lossy().into());

    // Some 32-bit titles need the full 4 GB address space.
    e.insert("WINE_LARGE_ADDRESS_AWARE".into(), "1".into());

    // User overrides win.
    for (k, v) in &bottle.env {
        e.insert(k.clone(), v.clone());
    }
    e
}

/// Render the environment as shell assignments, for `exe-metal env`.
pub fn as_shell(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(k, v)| format!("export {k}={}", shell_quote(v)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "._-/:=".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
