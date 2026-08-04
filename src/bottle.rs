use crate::catalog;
use crate::engine;
use crate::paths::{self, Layout};
use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Bottle {
    pub name: String,
    pub runtime: String,
    pub renderer: String,
    /// Windows version reported to the application.
    #[serde(default = "default_windows")]
    pub windows: String,
    #[serde(default)]
    pub perf: Perf,
    /// Extra Wine DLL overrides, merged with the ones exe-metal always sets.
    ///
    /// Separate from `env` because `WINEDLLOVERRIDES` is a single variable
    /// carrying a list: setting it through `env` replaces the Mono/Gecko
    /// suppression exe-metal depends on, which silently reintroduces the
    /// invisible installer dialog that makes a launch look like a hang.
    ///
    /// An empty value disables a DLL: `gameoverlayrenderer64 = ""`.
    #[serde(default)]
    pub dll_overrides: BTreeMap<String, String>,
    /// Extra environment variables, applied last so they win.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_windows() -> String {
    "win10".into()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Perf {
    /// Eventfd-based synchronisation. Large win on thread-heavy games.
    #[serde(default = "yes")]
    pub esync: bool,
    /// Mach-semaphore synchronisation. Newer than esync; mutually exclusive.
    #[serde(default)]
    pub msync: bool,
    /// Let Rosetta advertise AVX/AVX2. Modern titles take fast paths on it.
    #[serde(default = "yes")]
    pub avx: bool,
    /// MetalFX upscaling — renders lower and upscales. Real framerate win.
    #[serde(default = "yes")]
    pub metalfx: bool,
    /// Ray tracing through D3DMetal. Costs performance; off by default.
    #[serde(default)]
    pub dxr: bool,
    /// Metal's built-in performance overlay.
    #[serde(default)]
    pub hud: bool,
    /// Let Steam's embedded browser render on the GPU.
    ///
    /// Steam's whole interface is a CEF web app, so `-cef-disable-gpu` does not
    /// "cost nothing visually" — it rasterises the entire UI in software, on
    /// the CPU, through Rosetta. Measured here: `steamwebhelper` pinned at ~89%
    /// of a core while merely downloading, with visibly sluggish scrolling.
    ///
    /// On by default because D3DMetal supplies the D3D11 that CEF's ANGLE
    /// backend wants. Turn it off if the client crashes on startup — that is
    /// the failure this flag originally existed to avoid.
    #[serde(default = "yes")]
    pub cef_gpu: bool,
}

fn yes() -> bool {
    true
}

impl Default for Perf {
    fn default() -> Self {
        Self {
            esync: true,
            msync: false,
            avx: true,
            metalfx: true,
            dxr: false,
            hud: false,
            cef_gpu: true,
        }
    }
}

impl Bottle {
    pub fn dir(&self, layout: &Layout) -> PathBuf {
        layout.bottle(&self.name)
    }
    pub fn prefix(&self, layout: &Layout) -> PathBuf {
        self.dir(layout).join("pfx")
    }
    pub fn config_path(layout: &Layout, name: &str) -> PathBuf {
        layout.bottle(name).join("exe-metal.toml")
    }

    pub fn load(layout: &Layout, name: &str) -> Result<Self> {
        let p = Self::config_path(layout, name);
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("no bottle '{name}' (looked for {})", p.display()))?;
        Ok(toml::from_str(&text)?)
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        let dir = self.dir(layout);
        std::fs::create_dir_all(&dir)?;
        let text = toml::to_string_pretty(self)?;
        std::fs::write(dir.join("exe-metal.toml"), text)?;
        Ok(())
    }

    /// Path to the composed engine this bottle runs on.
    pub fn engine_dir(&self, layout: &Layout) -> Result<PathBuf> {
        engine::ensure(layout, &self.runtime, &self.renderer)
    }

    pub fn wine(&self, layout: &Layout) -> Result<PathBuf> {
        let eng = self.engine_dir(layout)?;
        let bin = paths::find_wine_bin(&eng)?;
        Ok(paths::wine_exe(&bin))
    }
}

pub fn create(
    layout: &Layout,
    name: &str,
    runtime_id: &str,
    renderer_id: &str,
    force: bool,
) -> Result<Bottle> {
    if let Err(e) = catalog::check_pairing(runtime_id, renderer_id) {
        bail!(e);
    }
    let dir = layout.bottle(name);
    if dir.exists() && !force {
        bail!(
            "bottle '{name}' already exists at {}\nuse --force to recreate it (this deletes the prefix)",
            dir.display()
        );
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }

    // Prefer msync where the runtime has it: it maps Wine's sync primitives
    // onto Mach semaphores instead of Linux eventfd, which costs measurably
    // less CPU. The CrossOver build has no esync at all, so on that runtime
    // this is not a preference but the only working option.
    let mut perf = Perf::default();
    if let Some(rt) = catalog::runtime(runtime_id) {
        perf.msync = rt.msync;
        perf.esync = !rt.msync;
    }

    let bottle = Bottle {
        name: name.to_string(),
        runtime: runtime_id.to_string(),
        renderer: renderer_id.to_string(),
        windows: default_windows(),
        perf,
        dll_overrides: BTreeMap::new(),
        env: BTreeMap::new(),
    };
    bottle.save(layout)?;

    let prefix = bottle.prefix(layout);
    std::fs::create_dir_all(&prefix)?;
    std::fs::create_dir_all(layout.shader_cache(name))?;

    println!("initialising prefix (first boot takes a moment)");
    let status = crate::run::wineboot(layout, &bottle)?;
    if !status {
        bail!("wineboot failed for bottle '{name}'");
    }

    // Steam's client is 32-bit; a prefix without WoW64 cannot run it.
    let wow = prefix.join("drive_c/windows/syswow64").is_dir();
    let pfx86 = prefix.join("drive_c/Program Files (x86)").is_dir();
    if !(wow && pfx86) {
        eprintln!(
            "  warn    prefix has no WoW64 layer — 32-bit applications (including the \
             Steam client) will not run in this bottle"
        );
    }

    println!(
        "bottle '{name}' ready\n  runtime  {runtime_id}\n  renderer {renderer_id}\n  prefix   {}",
        prefix.display()
    );
    Ok(bottle)
}

pub fn list(layout: &Layout) -> Vec<Bottle> {
    util::dir_names(&layout.bottles())
        .into_iter()
        .filter_map(|n| Bottle::load(layout, &n).ok())
        .collect()
}

pub fn remove(layout: &Layout, name: &str) -> Result<()> {
    let dir = layout.bottle(name);
    if !dir.exists() {
        return Err(anyhow!("no bottle '{name}'"));
    }
    std::fs::remove_dir_all(&dir)?;
    let sc = layout.shader_cache(name);
    if sc.exists() {
        std::fs::remove_dir_all(sc)?;
    }
    println!("removed bottle '{name}'");
    Ok(())
}
