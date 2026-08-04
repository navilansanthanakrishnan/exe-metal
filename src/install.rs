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
        println!(
            "runtime '{id}' already installed ({})",
            util::human_size(&dest)
        );
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

/// Locate a directory Wine ships its bundled installers in, across both bundle
/// layouts.
fn share_wine(tree: &Path, what: &str) -> Option<std::path::PathBuf> {
    for base in [
        tree.join("wswine.bundle/share/wine"),
        tree.join("Contents/Resources/wine/share/wine"),
        tree.join("share/wine"),
    ] {
        let p = base.join(what);
        if p.is_dir()
            && std::fs::read_dir(&p)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

/// Does this engine carry the wine-mono installer?
///
/// This matters more than it looks. Wine only shows the blocking "install
/// Mono?" dialog when it cannot find the installer locally; when the runtime
/// bundles it, `wineboot` installs it silently. So a bundled Mono means the
/// .NET loader can be left enabled, and every .NET game works.
pub fn bundles_mono(tree: &Path) -> bool {
    share_wine(tree, "mono").is_some()
}

pub fn bundles_gecko(tree: &Path) -> bool {
    share_wine(tree, "gecko").is_some()
}

/// Put wine-mono into a prefix that does not have it.
///
/// `wineboot -u` will not do this: the bundled Mono is only installed when a
/// prefix is first created, so a prefix built while `mscoree` was disabled
/// stays permanently without .NET no matter how many times it is rebooted.
///
/// The runtime ships Mono pre-extracted rather than as a single MSI, so this
/// mirrors what Wine's own first-boot path does — copy the tree into
/// `drive_c/windows/mono`, then run the small support MSI that writes the
/// registry keys `mscoree` looks for. Without that second step the files are
/// present and `mscoree` still reports "error reading registry key for
/// installroot".
pub fn mono_into_prefix(layout: &Layout, bottle: &crate::bottle::Bottle) -> Result<bool> {
    let engine = bottle.engine_dir(layout)?;
    let Some(src_root) = share_wine(&engine, "mono") else {
        return Ok(false);
    };

    // share/wine/mono/wine-mono-<version>/
    let Some(versioned) = std::fs::read_dir(&src_root)
        .ok()
        .and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.is_dir()))
    else {
        return Ok(false);
    };
    let name = versioned
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "wine-mono".into());

    let mono_dir = bottle.prefix(layout).join("drive_c/windows/mono");
    let dest = mono_dir.join(&name);
    if !dest.exists() {
        std::fs::create_dir_all(&mono_dir)?;
        util::clone_tree(&versioned, &dest)?;
    }

    // The support MSI registers the install root. Reference it by its Windows
    // path so msiexec resolves it inside the prefix.
    let msi_unix = dest.join("support/winemono-support.msi");
    if msi_unix.is_file() {
        let msi_win = format!("C:\\windows\\mono\\{name}\\support\\winemono-support.msi");
        let wine = bottle.wine(layout)?;
        let vars = crate::env::build(layout, bottle);
        let status = std::process::Command::new(&wine)
            .args(["msiexec", "/i", &msi_win, "/quiet"])
            .envs(&vars)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Ok(s) = status {
            if !s.success() {
                eprintln!("  warn    winemono-support.msi exited with {s}");
            }
        }
    }

    Ok(dest.is_dir())
}
