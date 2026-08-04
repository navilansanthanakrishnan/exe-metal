//! `exe-metal status` — is this bottle working *right now*?
//!
//! Everything else the tool reports is structural: what is installed, how a
//! bottle is configured. None of it answers the only question that matters
//! while something is running, and answering it by hand meant `ps`, two `du`
//! samples a second apart, and `lsof`.
//!
//! The two-sample part is the point. A single reading cannot distinguish a
//! download moving at 40 MB/s from one that stopped ten minutes ago, and it
//! cannot distinguish a game working hard from a process spinning on nothing.

use crate::bottle::Bottle;
use crate::paths::Layout;
use crate::util;
use anyhow::Result;
use std::path::Path;
use std::time::Duration;

pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub cpu: f32,
    pub rss_mb: u64,
    pub name: String,
}

impl Proc {
    /// Reparented to init and burning CPU: it has lost its wineserver and is
    /// spinning rather than working. A busy game never looks like this — it
    /// still has a live parent.
    pub fn is_spinning_orphan(&self) -> bool {
        self.ppid == 1 && self.cpu > 200.0
    }
}

pub fn processes(layout: &Layout, b: &Bottle) -> Vec<Proc> {
    let pfx = b.prefix(layout).to_string_lossy().to_string();
    let Ok(out) = util::capture("ps", &["-axo", "pid=,ppid=,%cpu=,rss=,command="]) else {
        return Vec::new();
    };

    // Wine processes report their *Windows* path — `C:\Program Files
    // (x86)\Steam\...\Celeste.exe` — so matching on the unix prefix finds the
    // loader and the services and misses every actual game. This is the same
    // blind spot that makes orphans hard to spot with a plain `ps` grep.
    //
    // So: match the prefix where it appears, and otherwise claim `C:\`
    // processes when this bottle is the only one with a live wineserver.
    // Ambiguous if two bottles run at once, which is rare and worth the far
    // more common case of seeing the game at all.
    // pgrep on the prefix, not a scan for "wineserver": the wineserver reports
    // the *engine* path on its command line, which is shared between bottles
    // and so identifies nothing.
    let live: Vec<String> = crate::bottle::list(layout)
        .into_iter()
        .filter(|other| {
            std::process::Command::new("pgrep")
                .args(["-f", &other.prefix(layout).to_string_lossy()])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        })
        .map(|o| o.name)
        .collect();
    let owns_windows_procs = live.len() == 1 && live[0] == b.name;

    let mut v = Vec::new();
    for line in out.lines() {
        let is_ours = line.contains(&pfx)
            || (owns_windows_procs && line.contains("C:\\") && line.contains(".exe"));
        if !is_ours {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(cpu), Some(rss)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(cpu), Ok(rss)) = (
            pid.parse::<u32>(),
            ppid.parse::<u32>(),
            cpu.parse::<f32>(),
            rss.parse::<u64>(),
        ) else {
            continue;
        };
        let name = line
            .split_whitespace()
            .find(|t| t.ends_with(".exe"))
            .map(|t| t.rsplit(['\\', '/']).next().unwrap_or(t).to_string())
            .or_else(|| {
                line.contains("wineserver")
                    .then(|| "wineserver".to_string())
            })
            .unwrap_or_else(|| "wine".into());
        v.push(Proc {
            pid,
            ppid,
            cpu,
            rss_mb: rss / 1024,
            name,
        });
    }
    v.sort_by(|a, b| {
        b.cpu
            .partial_cmp(&a.cpu)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    v
}

fn dir_mb(p: &Path) -> Option<u64> {
    util::capture("du", &["-sm", &p.to_string_lossy()])
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

pub fn run(layout: &Layout, b: &Bottle) -> Result<()> {
    println!(
        "{}   {} + {}   {}",
        b.name,
        b.runtime,
        b.renderer,
        if b.perf.msync {
            "msync"
        } else if b.perf.esync {
            "esync"
        } else {
            "no sync"
        }
    );
    println!("  window   {}", crate::window::describe(&b.window));

    let procs = processes(layout, b);
    if procs.is_empty() {
        println!("  running  nothing");
        return Ok(());
    }

    let mut warned = false;
    for p in procs.iter().take(8) {
        println!(
            "  {:<22} pid {:<7} {:>6.1}%  {:>6} MB{}",
            p.name,
            p.pid,
            p.cpu,
            p.rss_mb,
            if p.is_spinning_orphan() {
                "   <-- ORPHANED AND SPINNING"
            } else {
                ""
            }
        );
        warned |= p.is_spinning_orphan();
    }

    // Throughput, sampled rather than guessed.
    let steamapps = crate::steam::logs_dir(layout, b)
        .parent()
        .map(|p| p.join("steamapps"));
    if let Some(sa) = steamapps.filter(|p| p.is_dir()) {
        if let Some(a) = dir_mb(&sa) {
            std::thread::sleep(Duration::from_secs(2));
            if let Some(bb) = dir_mb(&sa) {
                let delta = bb.saturating_sub(a);
                if delta > 0 {
                    println!("  library  {bb} MB  (+{} MB/s)", delta / 2);
                } else {
                    println!("  library  {bb} MB  (idle)");
                }
            }
        }
    }

    println!(
        "  load     {}",
        util::capture("uptime", &[])
            .unwrap_or_default()
            .split("load averages:")
            .nth(1)
            .unwrap_or("")
            .trim()
    );

    if warned {
        println!("\n  a spinning orphan has lost its wineserver — it will not recover.");
        println!("  stop it with: exe-metal kill {}", b.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(ppid: u32, cpu: f32) -> Proc {
        Proc {
            pid: 100,
            ppid,
            cpu,
            rss_mb: 10,
            name: "x.exe".into(),
        }
    }

    #[test]
    fn a_busy_game_is_not_an_orphan() {
        // High CPU with a live parent is a game working, not a fault.
        assert!(!p(4242, 380.0).is_spinning_orphan());
    }

    #[test]
    fn an_idle_orphan_is_not_reported_either() {
        // Reparented but quiet: not the failure we are looking for.
        assert!(!p(1, 3.0).is_spinning_orphan());
    }

    #[test]
    fn both_conditions_together_are_the_failure() {
        // This is exactly what steam.exe looked like at 512%.
        assert!(p(1, 512.0).is_spinning_orphan());
    }
}
