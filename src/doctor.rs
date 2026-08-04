use crate::bottle;
use crate::catalog;
use crate::hw::Machine;
use crate::paths::Layout;
use crate::util;
use anyhow::Result;

/// Check the things that actually stop this from working, in the order they
/// bite: architecture, Rosetta, disk, then what is installed.
pub fn run(layout: &Layout) -> Result<()> {
    let mut problems = 0;

    let m = Machine::detect();

    println!("system");
    line(m.is_apple_silicon(), &format!("architecture: {}", m.arch));
    if !m.is_apple_silicon() {
        println!("        exe-metal targets Apple Silicon.");
        problems += 1;
    }

    line(m.macos_supported(), &format!("macOS: {}", m.macos_raw));
    if !m.macos_supported() {
        println!("        macOS 14 (Sonoma) or newer is required.");
        problems += 1;
    }

    line(m.rosetta, "Rosetta 2");
    if !m.rosetta {
        println!("        install with: softwareupdate --install-rosetta --agree-to-license");
        problems += 1;
    }

    println!("  ---   chip: {} ({}) on {}", m.silicon, m.cpu_brand, m.model);
    println!(
        "  ---   cores: {} total — {} performance, {} efficiency",
        m.total_cores, m.perf_cores, m.eff_cores
    );

    let ok_mem = !m.memory_is_tight();
    line(ok_mem, &format!("memory: {} GB", m.memory_gb));
    if !ok_mem {
        println!("        16 GB is the practical floor for heavy modern titles.");
    }

    // Capability gates, so nobody enables a feature the silicon cannot honour.
    println!("\ncapabilities");
    line(m.has_metalfx(), "MetalFX upscaling");
    line(m.has_hardware_raytracing(), "hardware ray tracing");
    if !m.has_hardware_raytracing() && m.is_apple_silicon() {
        println!(
            "        {} has no RT units — hardware ray tracing arrived with the M3.",
            m.silicon
        );
        println!("        Leave dxr = false; enabling it costs performance and returns nothing.");
    }
    line(m.rosetta_advertises_avx(), "Rosetta AVX advertisement");
    if !m.rosetta_advertises_avx() {
        println!("        macOS 15 added this. ROSETTA_ADVERTISE_AVX is inert on macOS 14.");
    }

    let free = util::capture("df", &["-h", "/System/Volumes/Data"]).unwrap_or_default();
    if let Some(l) = free.lines().nth(1) {
        let cols: Vec<&str> = l.split_whitespace().collect();
        if cols.len() > 3 {
            println!("  ---   disk free: {}", cols[3]);
        }
    }

    println!("\ninstalled");
    let runtimes = util::dir_names(&layout.runtimes());
    if runtimes.is_empty() {
        line(false, "runtimes: none");
        println!("        run: exe-metal install");
        problems += 1;
    } else {
        for r in &runtimes {
            let known = catalog::runtime(r);
            let ver = known.map(|k| k.wine_version).unwrap_or("unknown");
            line(true, &format!("runtime {r} (wine {ver})"));
        }
    }

    let renderers = util::dir_names(&layout.renderers());
    if renderers.is_empty() {
        line(false, "renderers: none");
        println!("        run: exe-metal install");
        problems += 1;
    } else {
        line(true, &format!("renderers: {}", renderers.join(", ")));
    }

    let fw = layout.frameworks().join("libinotify.0.dylib").exists();
    line(fw, "shared frameworks");
    if !fw {
        println!("        run: exe-metal install");
        problems += 1;
    }

    println!("\nbottles");
    let bottles = bottle::list(layout);
    if bottles.is_empty() {
        println!("  ---   none yet — create one with: exe-metal create <name>");
    } else {
        for b in &bottles {
            let pfx = b.prefix(layout);
            let wow = pfx.join("drive_c/windows/syswow64").is_dir();
            println!(
                "  ---   {} [{} + {}] {}{}",
                b.name,
                b.runtime,
                b.renderer,
                util::human_size(&pfx),
                if wow { "" } else { "  (no WoW64: 32-bit apps will not run)" }
            );
        }
    }

    println!();
    if problems == 0 {
        println!("no problems found");
    } else {
        println!("{problems} problem(s) found");
    }
    Ok(())
}

fn line(ok: bool, msg: &str) {
    println!("  {}   {msg}", if ok { " ok " } else { "FAIL" });
}
