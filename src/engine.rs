//! Composing a runtime with a renderer.
//!
//! A renderer is not a plugin the way DXVK is on Linux — the Metal backends
//! replace Wine's own `d3d11.dll`/`dxgi.dll` *and* drop unix-side `.so` shims
//! next to them. So an engine is a copy of the base runtime with those files
//! overlaid. APFS cloning makes that copy nearly free.

use crate::install;
use crate::paths::{self, Layout};
use crate::util;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Build `engines/<runtime>+<renderer>` if it does not already exist.
pub fn ensure(layout: &Layout, runtime_id: &str, renderer_id: &str) -> Result<PathBuf> {
    let dest = layout.engine(runtime_id, renderer_id);
    if dest.join(".exe-metal-engine-ok").exists() {
        return Ok(dest);
    }

    install::ensure_for(layout, runtime_id, renderer_id)?;

    let base = layout.runtime(runtime_id);
    util::ensure_exists(&base, &format!("runtime '{runtime_id}'"))?;

    println!("composing engine {runtime_id}+{renderer_id}");
    util::clone_tree(&base, &dest).context("cloning base runtime")?;

    let wine_lib = paths::find_wine_lib(&dest).context("locating wine lib directory")?;

    // 1. Shared dylibs.
    //
    //    `lib/` next to the wine binaries covers everything Wine dlopens by
    //    bare soname — FreeType above all, which the CrossOver build does not
    //    bundle. `env::build` points DYLD_FALLBACK_LIBRARY_PATH here.
    //
    //    The Sikarugir layout needs two more copies: its `wine` resolves
    //    @rpath to the directory holding the bundle, its `wineserver` to the
    //    bundle's own lib/.
    if install::needs_frameworks(runtime_id) {
        util::overlay(&layout.frameworks(), &wine_lib)?;
        if install::is_wswine_layout(&dest) {
            util::overlay(&layout.frameworks(), &dest)?;
            util::overlay(&layout.frameworks(), &dest.join("wswine.bundle/lib"))?;
        }
    }

    // 2. Renderer overlay: Windows DLLs per architecture, unix shims, and the
    //    macOS frameworks they dlopen.
    let renderer = layout.renderer(renderer_id);

    let external = renderer.join("external");
    if external.exists() {
        util::overlay(&external, &wine_lib.join("external"))?;
    }

    let rwine = renderer.join("wine");
    if rwine.exists() {
        for arch in ["i386-windows", "x86_64-windows", "x86_64-unix"] {
            let src = rwine.join(arch);
            if src.exists() {
                util::overlay(&src, &wine_lib.join("wine").join(arch))?;
            }
        }
    }

    // 3. Overlaying files invalidates the signature of any bundle they landed
    //    in, and macOS will not load an invalidly-signed framework.
    util::sign_tree(&dest)?;

    std::fs::write(
        dest.join(".exe-metal-engine-ok"),
        format!("{runtime_id}+{renderer_id}\n"),
    )?;
    println!("  engine ready ({})", util::human_size(&dest));
    Ok(dest)
}

pub fn list(layout: &Layout) -> Vec<String> {
    util::dir_names(&layout.engines())
}

pub fn remove(layout: &Layout, runtime_id: &str, renderer_id: &str) -> Result<()> {
    let dest = layout.engine(runtime_id, renderer_id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
        println!("removed engine {runtime_id}+{renderer_id}");
    }
    Ok(())
}
