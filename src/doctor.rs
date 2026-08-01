use crate::bottle;
use crate::catalog;
use crate::paths::Layout;
use crate::util;
use anyhow::Result;

/// Check the things that actually stop this from working, in the order they
/// bite: architecture, Rosetta, disk, then what is installed.
pub fn run(layout: &Layout) -> Result<()> {
    let mut problems = 0;

    println!("system");
    let arch = util::capture("uname", &["-m"]).unwrap_or_default();
    let ok_arch = arch == "arm64";
    line(ok_arch, &format!("architecture: {arch}"));
    if !ok_arch {
        println!("        exe-metal targets Apple Silicon.");
        problems += 1;
    }

    let osver = util::capture("sw_vers", &["-productVersion"]).unwrap_or_default();
    let major: u32 = osver.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ok_os = major >= 14;
    line(ok_os, &format!("macOS: {osver}"));
    if !ok_os {
        println!("        macOS 14 (Sonoma) or newer is required.");
        problems += 1;
    }

    let ros = util::rosetta_present();
    line(ros, "Rosetta 2");
    if !ros {
        println!("        install with: softwareupdate --install-rosetta --agree-to-license");
        problems += 1;
    }

    let cpu = util::capture("sysctl", &["-n", "machdep.cpu.brand_string"]).unwrap_or_default();
    println!("  ---   cpu: {cpu}");

    let mem = util::capture("sysctl", &["-n", "hw.memsize"])
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|b| b / 1024 / 1024 / 1024)
        .unwrap_or(0);
    let ok_mem = mem >= 16;
    line(ok_mem, &format!("memory: {mem} GB"));
    if !ok_mem {
        println!("        16 GB is the practical floor for modern titles.");
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
