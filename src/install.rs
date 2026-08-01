use crate::catalog;
use crate::paths::Layout;
use crate::util;
use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// Download and unpack a base Wine build into `runtimes/<id>`.
pub fn runtime(layout: &Layout, id: &str) -> Result<()> {
    let spec = catalog::runtime(id).ok_or_else(|| anyhow!("unknown runtime '{id}'"))?;
    let dest = layout.runtime(id);
    if dest.exists() {
        println!("runtime '{id}' already installed ({})", util::human_size(&dest));
        return Ok(());
    }

    println!("installing runtime '{id}' (wine {})", spec.wine_version);
    let archive = layout.cache().join(spec.archive);
    util::download(spec.url, &archive)?;

    let staging = layout.cache().join(format!(".staging-{id}"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    util::extract(&archive, &staging)?;

    std::fs::create_dir_all(layout.runtimes())?;
    std::fs::rename(&staging, &dest)?;

    println!("  sign    ad-hoc signing (builds are unsigned)");
    util::sign_adhoc(&dest)?;

    println!("installed '{id}' -> {}", dest.display());
    Ok(())
}

/// Unpack the wrapper template once and harvest both the renderer overlays and
/// the shared dylibs from it.
pub fn renderers_and_frameworks(layout: &Layout) -> Result<()> {
    let archive = layout.cache().join(catalog::TEMPLATE_ARCHIVE);
    let unpacked = layout.cache().join("template");
    let app = unpacked.join(catalog::TEMPLATE_APP);

    if !app.exists() {
        util::download(catalog::TEMPLATE_URL, &archive)?;
        util::extract(&archive, &unpacked)?;
    }

    let src_frameworks = app.join("Contents/Frameworks");
    util::ensure_exists(&src_frameworks, "wrapper Frameworks directory")?;

    // Shared dylibs: gnutls, freetype, GStreamer and friends. The Wine bundles
    // link these by rpath and will not start without them.
    if !layout.frameworks().join("libinotify.0.dylib").exists() {
        println!("installing shared frameworks");
        util::clone_tree(&src_frameworks, &layout.frameworks())?;
    }

    let src_renderers = src_frameworks.join("renderer");
    util::ensure_exists(&src_renderers, "renderer directory")?;

    for spec in catalog::RENDERERS {
        let src = src_renderers.join(spec.id);
        if !src.exists() {
            eprintln!("  warn    template has no renderer '{}'", spec.id);
            continue;
        }
        let dest = layout.renderer(spec.id);
        if dest.exists() {
            continue;
        }
        println!("installing renderer '{}' ({})", spec.id, spec.apis);
        util::clone_tree(&src, &dest)?;
    }
    Ok(())
}

/// Fetch the official Windows Steam installer.
pub fn steam_installer(layout: &Layout) -> Result<std::path::PathBuf> {
    let dest = layout.cache().join(catalog::STEAM_INSTALLER);
    util::download(catalog::STEAM_INSTALLER_URL, &dest)?;
    Ok(dest)
}

/// Make sure everything a bottle needs is on disk.
pub fn ensure_for(layout: &Layout, runtime_id: &str, renderer_id: &str) -> Result<()> {
    if let Err(e) = catalog::check_pairing(runtime_id, renderer_id) {
        bail!(e);
    }
    if !layout.runtime(runtime_id).exists() {
        runtime(layout, runtime_id)?;
    }
    if !layout.renderer(renderer_id).exists() || !layout.frameworks().exists() {
        renderers_and_frameworks(layout)?;
    }
    Ok(())
}

/// Does this runtime need the wrapper's shared dylibs overlaid onto it?
///
/// Recorded per runtime in the catalog rather than sniffed, because the failure
/// is quiet: a build missing FreeType starts fine and then renders no text at
/// all, which reads as a font bug rather than a missing library.
pub fn needs_frameworks(runtime_id: &str) -> bool {
    catalog::runtime(runtime_id).is_some_and(|r| r.needs_frameworks)
}

/// True for the Sikarugir engine layout, where `wine` and `wineserver` resolve
/// `@rpath` differently and the dylibs must therefore exist in two places.
pub fn is_wswine_layout(tree: &Path) -> bool {
    tree.join("wswine.bundle").is_dir()
}
