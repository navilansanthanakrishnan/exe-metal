use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// On-disk layout for everything exe-metal manages.
///
/// Nothing lives outside this root except the binary itself, so uninstalling is
/// `rm -rf` on one directory.
///
/// ```text
/// ~/Library/Application Support/exe-metal/
///   cache/                      downloaded tarballs, installers
///   runtimes/<id>/              base Wine bundles, pristine
///   renderers/<id>/             DirectX->Metal overlays
///   frameworks/                 shared dylibs the Wine bundles link against
///   engines/<runtime>+<rend>/   composed runtime+renderer (APFS clone)
///   bottles/<name>/             Wine prefix + exe-metal.toml
/// ```
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn discover() -> Result<Self> {
        let home = std::env::var("HOME").map_err(|_| anyhow!("HOME is not set"))?;
        let root = PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("exe-metal");
        Ok(Self { root })
    }

    pub fn ensure(&self) -> Result<()> {
        for d in [
            self.cache(),
            self.runtimes(),
            self.renderers(),
            self.frameworks(),
            self.engines(),
            self.bottles(),
        ] {
            std::fs::create_dir_all(&d)?;
        }
        Ok(())
    }

    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }
    pub fn runtimes(&self) -> PathBuf {
        self.root.join("runtimes")
    }
    pub fn renderers(&self) -> PathBuf {
        self.root.join("renderers")
    }
    pub fn frameworks(&self) -> PathBuf {
        self.root.join("frameworks")
    }
    pub fn engines(&self) -> PathBuf {
        self.root.join("engines")
    }
    pub fn bottles(&self) -> PathBuf {
        self.root.join("bottles")
    }

    pub fn runtime(&self, id: &str) -> PathBuf {
        self.runtimes().join(id)
    }
    pub fn renderer(&self, id: &str) -> PathBuf {
        self.renderers().join(id)
    }
    pub fn bottle(&self, name: &str) -> PathBuf {
        self.bottles().join(name)
    }

    /// Engines are keyed by the pair they were composed from, so a given
    /// combination is built at most once and shared by every bottle using it.
    pub fn engine(&self, runtime: &str, renderer: &str) -> PathBuf {
        self.engines().join(format!("{runtime}+{renderer}"))
    }

    /// Shader caches are per bottle. Warm caches are the single biggest factor
    /// in whether a game stutters on first play, so they must survive a
    /// prefix rebuild.
    pub fn shader_cache(&self, bottle: &str) -> PathBuf {
        self.root.join("shadercache").join(bottle)
    }
}

/// Locate the `wine` / `wineserver` binaries inside a runtime or engine tree.
///
/// The upstream builds disagree on layout: Sikarugir engines use
/// `wswine.bundle/bin`, WineHQ and GPTK use `*.app/Contents/Resources/wine/bin`.
/// Both are supported so runtimes can be mixed freely.
pub fn find_wine_bin(tree: &Path) -> Result<PathBuf> {
    let candidates = [
        tree.join("wswine.bundle/bin"),
        tree.join("Contents/Resources/wine/bin"),
        tree.join("bin"),
    ];
    for c in candidates {
        if c.join("wine").is_file() || c.join("wine64").is_file() {
            return Ok(c);
        }
    }
    // Fall back to a shallow scan for app bundles dropped in whole.
    if let Ok(entries) = std::fs::read_dir(tree) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let c = p.join("Contents/Resources/wine/bin");
                if c.join("wine").is_file() || c.join("wine64").is_file() {
                    return Ok(c);
                }
                let c = p.join("bin");
                if c.join("wine").is_file() {
                    return Ok(c);
                }
            }
        }
    }
    Err(anyhow!(
        "no wine binary found under {} — is the runtime installed?",
        tree.display()
    ))
}

/// The `lib/wine` directory holding the per-architecture DLL trees that a
/// renderer overlay drops files into.
pub fn find_wine_lib(tree: &Path) -> Result<PathBuf> {
    let bin = find_wine_bin(tree)?;
    let lib = bin
        .parent()
        .ok_or_else(|| anyhow!("malformed wine tree"))?
        .join("lib");
    if lib.join("wine").is_dir() {
        return Ok(lib);
    }
    Err(anyhow!("no lib/wine under {}", lib.display()))
}

/// Preferred wine entrypoint. Newer builds ship a single `wine` that handles
/// both bitnesses through WoW64; GPTK-era builds only ship `wine64`.
pub fn wine_exe(bin: &Path) -> PathBuf {
    let unified = bin.join("wine");
    if unified.is_file() {
        unified
    } else {
        bin.join("wine64")
    }
}
