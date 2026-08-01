//! Cleaning up after Wine.
//!
//! When a bottle's `wineserver` dies without shutting down properly, its
//! service processes — `rpcss.exe`, `services.exe`, `plugplay.exe`,
//! `explorer.exe` — are reparented to init and keep spinning. Observed here:
//! five orphans at 50-115% CPU each, load average 65, and every subsequent
//! benchmark reading ruined.
//!
//! They are invisible to the obvious cleanup because `ps` reports their
//! Windows command line (`C:\windows\system32\rpcss.exe`), so matching on the
//! macOS install path finds nothing.

use crate::bottle;
use crate::paths::Layout;
use anyhow::Result;
use std::process::Command;

/// Windows-side processes that never legitimately outlive their prefix.
///
/// Steam's own helpers belong here too: `steamservice.exe /RunAsService`
/// survives a client shutdown and, once reparented to init, is unreachable by
/// `wineserver -k`.
const WINE_SERVICES: &[&str] = &[
    r"C:\windows\system32\rpcss.exe",
    r"C:\windows\system32\services.exe",
    r"C:\windows\system32\plugplay.exe",
    r"C:\windows\system32\winedevice.exe",
    r"C:\windows\system32\explorer.exe",
    r"C:\Program Files (x86)\Common Files\Steam\steamservice.exe",
    r"C:\Program Files (x86)\Steam\steam.exe",
    r"C:\Program Files (x86)\Steam\bin\cef",
];

struct Orphan {
    pid: i32,
    cpu: f32,
    cmd: String,
}

pub fn run(layout: &Layout, force: bool) -> Result<()> {
    // Shut down every known bottle the polite way first.
    for b in bottle::list(layout) {
        let _ = crate::run::kill(layout, &b);
    }

    let orphans = find_orphans();
    if orphans.is_empty() {
        println!("nothing to prune");
        return Ok(());
    }

    println!("orphaned Wine processes (no live wineserver):");
    for o in &orphans {
        println!("  pid {:<7} {:>6.1}% cpu  {}", o.pid, o.cpu, o.cmd);
    }

    if !force {
        println!("\nre-run with --force to terminate them");
        return Ok(());
    }

    for o in &orphans {
        let _ = Command::new("kill").arg("-TERM").arg(o.pid.to_string()).status();
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
    for o in find_orphans() {
        let _ = Command::new("kill").arg("-KILL").arg(o.pid.to_string()).status();
    }
    println!("\npruned {} process(es)", orphans.len());
    Ok(())
}

fn find_orphans() -> Vec<Orphan> {
    let Ok(out) = Command::new("ps").args(["-Ao", "pid,ppid,pcpu,args"]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v = Vec::new();

    for line in text.lines().skip(1) {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(cpu)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let args = line
            .splitn(4, |c: char| c.is_whitespace())
            .nth(3)
            .unwrap_or("")
            .trim();

        let is_wine_service = WINE_SERVICES.iter().any(|s| args.starts_with(s));
        if !is_wine_service {
            continue;
        }
        // Reparented to init means the wineserver that owned it is gone.
        if ppid != "1" {
            continue;
        }
        v.push(Orphan {
            pid: pid.parse().unwrap_or(0),
            cpu: cpu.parse().unwrap_or(0.0),
            cmd: args.chars().take(70).collect(),
        });
    }
    v
}
