//! `exe-metal setup` — one command from a bare machine to a playable one.
//!
//! Everything here is idempotent. Running it twice is not an error and does not
//! re-download anything, because the most common reason to run it a second time
//! is that the first run was interrupted.
//!
//! The one step that cannot be automated is Rosetta: installing it needs root,
//! and silently escalating on someone's behalf is not a thing this tool should
//! do. So it is deferred rather than skipped — the ~1.5 GB of downloading
//! happens regardless, and the single command needing a password is printed at
//! the end, where it will not be missed.

use crate::bottle;
use crate::catalog;
use crate::diagnose;
use crate::engine;
use crate::hw::Machine;
use crate::install;
use crate::paths::Layout;
use anyhow::{bail, Result};

pub struct Options {
    pub bottle: String,
    pub runtime: String,
    pub renderer: String,
    pub force: bool,
    pub dry_run: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bottle: "default".into(),
            runtime: catalog::DEFAULT_RUNTIME.into(),
            renderer: catalog::DEFAULT_RENDERER.into(),
            force: false,
            dry_run: false,
        }
    }
}

pub fn run(layout: &Layout, opts: &Options) -> Result<()> {
    let machine = Machine::detect();

    println!("exe-metal setup");
    println!("  machine   {}", machine.summary());
    if !machine.cpu_brand.is_empty() {
        println!(
            "  cpu       {} ({} cores: {}P + {}E)",
            machine.cpu_brand, machine.total_cores, machine.perf_cores, machine.eff_cores
        );
    }
    println!();

    // Hard stops first — no point downloading 1.5 GB onto a machine that cannot
    // use it. Rosetta is the exception and is handled separately below.
    if !machine.is_apple_silicon() {
        bail!(
            "this machine reports '{}', not Apple Silicon.\n\
             exe-metal translates DirectX to Metal on Apple GPUs; there is no Intel path.",
            machine.cpu_brand
        );
    }
    if !machine.macos_supported() {
        bail!(
            "macOS {} is too old — 14 (Sonoma) or newer is required.",
            machine.macos_raw
        );
    }

    if let Err(e) = catalog::check_pairing(&opts.runtime, &opts.renderer) {
        bail!(e);
    }

    // What this machine will and will not do, stated once, up front.
    report_capabilities(&machine);

    if opts.dry_run {
        println!("\nplan (nothing will be downloaded):");
        println!("  1. runtime   {}", opts.runtime);
        println!("  2. renderers {}", renderer_names());
        println!("  3. engine    {}+{}", opts.runtime, opts.renderer);
        println!("  4. bottle    {}", opts.bottle);
        if !machine.rosetta {
            println!(
                "  5. rosetta   needs: sudo softwareupdate --install-rosetta --agree-to-license"
            );
        }
        return Ok(());
    }

    println!("\n[1/4] runtime");
    install::runtime(layout, &opts.runtime)?;

    println!("\n[2/4] renderers and shared frameworks");
    install::renderers_and_frameworks(layout)?;

    // Composing now rather than on first launch means the first game start is
    // a game start, not a 700 MB clone plus a codesign pass.
    println!("\n[3/4] engine");
    engine::ensure(layout, &opts.runtime, &opts.renderer)?;

    println!("\n[4/4] bottle '{}'", opts.bottle);
    let exists = bottle::Bottle::load(layout, &opts.bottle).is_ok();
    if exists && !opts.force {
        println!("  already exists — keeping it (use --force to recreate)");
    } else {
        bottle::create(
            layout,
            &opts.bottle,
            &opts.runtime,
            &opts.renderer,
            opts.force,
        )?;
    }

    // Verify rather than assume. If any of the above quietly half-worked, this
    // is where it surfaces, not on the user's first launch.
    println!("\nverifying");
    let b = bottle::Bottle::load(layout, &opts.bottle)?;
    let report = diagnose::bottle(layout, &b)?;
    if report.is_clean() {
        println!("  ok    everything checks out");
    } else {
        report.print();
    }

    println!("\n{}", "-".repeat(60));
    if machine.rosetta {
        println!("ready.");
    } else {
        println!("one step left — Rosetta 2 executes the x86-64 code, and installing");
        println!("it needs admin rights:\n");
        println!("  sudo softwareupdate --install-rosetta --agree-to-license\n");
    }
    println!("run something:");
    println!("  exe-metal play /path/to/game.exe      # picks the renderer for you");
    println!("  exe-metal ui                          # same thing, with a window");
    Ok(())
}

/// State the capability gates that change what this machine can do, so nobody
/// has to discover them by watching a game run badly.
fn report_capabilities(m: &Machine) {
    println!("capabilities");
    ok(m.rosetta, "Rosetta 2", "needed to execute x86-64 code");
    ok(
        m.has_metalfx(),
        "MetalFX upscaling",
        "renders low, upscales — real framerate win",
    );
    ok(
        m.has_hardware_raytracing(),
        "hardware ray tracing",
        if m.has_hardware_raytracing() {
            "DXR is worth enabling"
        } else {
            "M3 introduced it — leave dxr off on this chip"
        },
    );
    ok(
        m.rosetta_advertises_avx(),
        "Rosetta AVX advertisement",
        if m.rosetta_advertises_avx() {
            "modern engines take faster code paths"
        } else {
            "macOS 15 added this; the variable is inert on 14"
        },
    );
    if m.memory_is_tight() {
        println!(
            "  note  {} GB of memory is below the 16 GB that heavy titles want. \
             Smaller games are unaffected.",
            m.memory_gb
        );
    }
}

fn ok(yes: bool, label: &str, why: &str) {
    println!("  {}  {label:<28} {why}", if yes { "yes" } else { " no" });
}

fn renderer_names() -> String {
    catalog::RENDERERS
        .iter()
        .map(|r| r.id)
        .collect::<Vec<_>>()
        .join(", ")
}
