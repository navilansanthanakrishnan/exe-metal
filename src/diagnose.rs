//! Turning failures into their causes.
//!
//! Every signature in this file cost someone hours once. The pattern is always
//! the same: the symptom points somewhere useless, and the real cause is three
//! layers down. Wine reports a font problem when FreeType is missing; Steam
//! reports a transport error when the Wine build is too old; a 32-bit installer
//! reports an exception dispatch failure when the actual answer is "extract it
//! on the macOS side instead".
//!
//! So this module does two things:
//!
//!   * **Reactive** — scan logs for known signatures and name the fix.
//!   * **Proactive** — check the things that fail *silently*, which no log will
//!     ever mention. A missing FreeType produces no error at all; the game just
//!     draws no text.

use crate::bottle::Bottle;
use crate::catalog;
use crate::hw::Machine;
use crate::install;
use crate::paths::{self, Layout};
use crate::steam;
use crate::util;
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Stops the program running at all.
    Fatal,
    /// It runs, but something is measurably wrong.
    Warn,
    /// Worth knowing, not worth acting on.
    Note,
}

impl Severity {
    fn tag(&self) -> &'static str {
        match self {
            Severity::Fatal => "FATAL",
            Severity::Warn => " WARN",
            Severity::Note => " NOTE",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Match {
    /// Any one of the needles is enough.
    Any,
    /// All needles must be present — for signatures whose individual parts are
    /// too common to mean anything alone.
    All,
}

struct Signature {
    id: &'static str,
    needles: &'static [&'static str],
    mode: Match,
    severity: Severity,
    problem: &'static str,
    fix: &'static str,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub problem: String,
    pub fix: String,
    /// Where it was seen — a log path, or a check name for proactive findings.
    pub source: String,
    /// The line that matched, when there is one.
    pub evidence: Option<String>,
}

/// Failure signatures, each one a thing that actually happened.
const SIGNATURES: &[Signature] = &[
    Signature {
        id: "steam-webhelper-transport",
        needles: &["0x3008", "Unexpected Transport Error"],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "Steam's CEF helper was rejected by the client — the Wine build is too old.",
        fix: "Recreate the bottle on sikarugir-10.0:\n  \
              exe-metal create <bottle> --runtime sikarugir-10.0 --force",
    },
    Signature {
        id: "dxmt-no-winemetal",
        needles: &[
            "no exported symbols needed by DXMT",
            "Failed to create metal view",
        ],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "DXMT cannot reach Metal: this runtime does not export winemetal.",
        fix: "Only sikarugir-10.0 exports it. Either switch runtime, or use \
              --renderer d3dmetal.",
    },
    Signature {
        id: "inno-seh-dispatch",
        needles: &["NtRaiseException", "unable to dispatch exception"],
        mode: Match::All,
        severity: Severity::Fatal,
        problem: "A 32-bit Inno Setup installer hit the new WoW64's SEH dispatch bug.",
        fix: "Extract it on the macOS side instead — `brew install innoextract` then \
              `innoextract setup.exe` — and point exe-metal at the extracted .exe. \
              Or use --runtime sikarugir-10.0, which does not have this bug.",
    },
    Signature {
        id: "steam-two-clients",
        // Deliberately narrow. An earlier version matched "NO CONNECTION",
        // which also matches "SteamServerConnectFailure_t No Connection
        // Reconnect" — routine reconnect noise that appears dozens of times in
        // a perfectly healthy session. Only Steam's own eviction result means
        // the account was actually taken over.
        needles: &[
            "LoggedInElsewhere",
            "Logged off due to another login",
            "k_EResultLoggedInElsewhere",
        ],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "The account was signed in elsewhere, so this client was dropped.",
        fix: "A Steam account holds one session. Quit the native macOS Steam client \
              (and any other machine) and launch again.",
    },
    Signature {
        id: "mono-gecko-prompt",
        needles: &["wine-mono", "wine-gecko", "Wine Mono Installer"],
        mode: Match::Any,
        severity: Severity::Warn,
        problem: "The prefix tried to show the Mono/Gecko installer dialog, which blocks \
                  an unattended launch and looks exactly like a hang.",
        fix: "exe-metal sets WINEDLLOVERRIDES=mscoree,mshtml= to suppress this. If you \
              see it, the bottle is being launched outside exe-metal.",
    },
    Signature {
        id: "missing-dll",
        needles: &["import_dll Library", "err:module:loader_init"],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "The program needs a DLL the prefix does not have.",
        fix: "Usually a redistributable. Install the game's own _CommonRedist \
              vcredist, or check the line below for the exact name.",
    },
    Signature {
        id: "codesign-rejected",
        needles: &[
            "code signature",
            "Library not loaded",
            "not valid for use in process",
        ],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "macOS refused to load a dylib — its signature is invalid or it is quarantined.",
        fix: "Rebuild the engine so it is re-signed:\n  \
              exe-metal engine remove <runtime> <renderer>",
    },
    Signature {
        id: "bad-cpu-type",
        needles: &["Bad CPU type in executable"],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "x86-64 code could not be executed — Rosetta 2 is missing.",
        fix: "softwareupdate --install-rosetta --agree-to-license",
    },
    Signature {
        id: "prefix-broken",
        needles: &["could not load kernel32.dll", "wineserver: could not exec"],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "The Wine prefix is damaged.",
        fix: "Recreate it: exe-metal create <bottle> --force (this deletes the prefix).",
    },
    Signature {
        id: "d3d12-unavailable",
        needles: &["Direct3D 12 is not available", "D3D12CreateDevice failed"],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "The game asked for DirectX 12 and the renderer does not provide it.",
        fix: "D3DMetal is the only DirectX 12 backend. Recreate the bottle with \
              --renderer d3dmetal (note: 64-bit games only).",
    },
    Signature {
        id: "out-of-disk",
        needles: &["No space left on device", "ENOSPC"],
        mode: Match::Any,
        severity: Severity::Fatal,
        problem: "The disk filled up.",
        fix: "Free space, then: exe-metal prune",
    },
];

pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        !self.findings.iter().any(|f| f.severity != Severity::Note)
    }

    pub fn print(&self) {
        if self.findings.is_empty() {
            println!("nothing wrong found");
            return;
        }
        for f in &self.findings {
            println!("\n{}  {}", f.severity.tag(), f.problem);
            if let Some(e) = &f.evidence {
                println!("       seen in {}:", f.source);
                println!("         {}", truncate(e, 140));
            } else {
                println!("       check: {}", f.source);
            }
            for (i, l) in f.fix.lines().enumerate() {
                println!(
                    "       {}{}",
                    if i == 0 { "fix: " } else { "     " },
                    l.trim()
                );
            }
        }
        println!();
        let fatal = self
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Fatal)
            .count();
        let warn = self
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Warn)
            .count();
        println!("{fatal} fatal, {warn} warning(s)");
    }
}

/// Scan one bottle: its own logs, Steam's logs if present, plus the checks that
/// no log would ever reveal.
pub fn bottle(layout: &Layout, b: &Bottle) -> Result<Report> {
    let mut findings = Vec::new();

    for log in log_candidates(layout, b) {
        if let Ok(text) = std::fs::read_to_string(&log) {
            findings.extend(scan_text(&text, &log.display().to_string()));
        }
    }

    findings.extend(proactive(layout, b));
    dedupe(&mut findings);
    Ok(Report { findings })
}

/// Scan arbitrary text — used by `run` to explain a non-zero exit immediately.
pub fn scan_text(text: &str, source: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for sig in SIGNATURES {
        let hit = match sig.mode {
            Match::Any => sig.needles.iter().any(|n| contains_ci(text, n)),
            Match::All => sig.needles.iter().all(|n| contains_ci(text, n)),
        };
        if !hit {
            continue;
        }
        let evidence = text
            .lines()
            .rev()
            .find(|l| sig.needles.iter().any(|n| contains_ci(l, n)))
            .map(|l| l.trim().to_string());
        out.push(Finding {
            id: sig.id.to_string(),
            severity: sig.severity,
            problem: sig.problem.to_string(),
            fix: sig.fix.to_string(),
            source: source.to_string(),
            evidence,
        });
    }
    out
}

/// The failures that produce no log line at all.
fn proactive(layout: &Layout, b: &Bottle) -> Vec<Finding> {
    let mut out = Vec::new();
    let machine = Machine::detect();

    for (problem, fix) in machine.blocking_issues() {
        out.push(Finding {
            id: "system".into(),
            severity: Severity::Fatal,
            problem,
            fix,
            source: "system check".into(),
            evidence: None,
        });
    }

    // A pairing that cannot work should be caught before anything is launched.
    if let Err(e) = catalog::check_pairing(&b.runtime, &b.renderer) {
        out.push(Finding {
            id: "bad-pairing".into(),
            severity: Severity::Fatal,
            problem: e,
            fix: "Recreate the bottle with a compatible pair — see: exe-metal catalog".into(),
            source: "bottle config".into(),
            evidence: None,
        });
    }

    // The silent one. Wine dlopens libfreetype by bare soname; if the engine
    // does not carry it, everything starts normally and no text ever renders.
    if install::needs_frameworks(&b.runtime) {
        let engine = layout.engine(&b.runtime, &b.renderer);
        if engine.exists() {
            if let Ok(lib) = paths::find_wine_lib(&engine) {
                if !has_freetype(&lib) {
                    out.push(Finding {
                        id: "missing-freetype".into(),
                        severity: Severity::Warn,
                        problem: "This engine has no libfreetype. Nothing will error — the \
                                  game will simply render no text anywhere."
                            .into(),
                        fix: format!(
                            "Rebuild the engine:\n  exe-metal engine remove {} {}",
                            b.runtime, b.renderer
                        ),
                        source: lib.display().to_string(),
                        evidence: None,
                    });
                }
            }
        }
    }

    // 32-bit needs WoW64, and the Steam client is 32-bit.
    let pfx = b.prefix(layout);
    if pfx.exists() && !pfx.join("drive_c/windows/syswow64").is_dir() {
        out.push(Finding {
            id: "no-wow64".into(),
            severity: Severity::Warn,
            problem: "This prefix has no WoW64 layer, so no 32-bit program — including the \
                      Steam client — can run in it."
                .into(),
            fix: format!("exe-metal create {} --force", b.name),
            source: pfx.display().to_string(),
            evidence: None,
        });
    }

    for (pid, cpu, name) in spinning_orphans(layout, b) {
        out.push(Finding {
            id: format!("spinning-orphan-{pid}"),
            severity: Severity::Warn,
            problem: format!(
                "{name} (pid {pid}) is reparented to init and burning {cpu:.0}% CPU. It has \
                 lost its wineserver and is spinning rather than working — the window is \
                 usually already gone."
            ),
            fix: format!("exe-metal kill {}", b.name),
            source: "process table".into(),
            evidence: None,
        });
    }

    // Orphans are invisible to a normal `ps` grep because they report their
    // Windows path, not the prefix they came from.
    let orphans = orphan_count(layout);
    if orphans > 0 {
        out.push(Finding {
            id: "orphans".into(),
            severity: Severity::Warn,
            problem: format!(
                "{orphans} orphaned Wine service process(es) are running. These spin \
                 forever and will ruin both framerate and any measurement."
            ),
            fix: "exe-metal prune --force".into(),
            source: "process table".into(),
            evidence: None,
        });
    }

    if let Some(gb) = free_gb() {
        if gb < 10 {
            out.push(Finding {
                id: "low-disk".into(),
                severity: Severity::Warn,
                problem: format!("Only {gb} GB free. Engines and prefixes need room to compose."),
                fix: "Free space, or: exe-metal prune".into(),
                source: "disk".into(),
                evidence: None,
            });
        }
    }

    // .NET. Silent and total: the game exits before drawing anything.
    let engine = layout.engine(&b.runtime, &b.renderer);
    if engine.exists() && install::bundles_mono(&engine) && !crate::env::mono_installed(layout, b) {
        out.push(Finding {
            id: "no-dotnet".into(),
            severity: Severity::Fatal,
            problem: "wine-mono is not installed in this prefix, so no .NET game can run \
                      — Celeste, Stardew Valley, Terraria and anything XNA/FNA/MonoGame \
                      will exit before drawing a frame."
                .into(),
            fix: format!(
                "The installer is bundled with the runtime; this needs no download:\n  \
                 exe-metal repair {}",
                b.name
            ),
            source: b
                .prefix(layout)
                .join("drive_c/windows/mono")
                .display()
                .to_string(),
            evidence: None,
        });
    }

    // Settings frozen at creation time drift behind the catalog. This is what
    // left a bottle on esync burning four times the CPU it needed to.
    if let Some(rt) = catalog::runtime(&b.runtime) {
        if rt.msync && !b.perf.msync {
            out.push(Finding {
                id: "sync-drift".into(),
                severity: Severity::Warn,
                problem: format!(
                    "This bottle uses {}, but {} supports msync, which costs measurably \
                     less CPU. Measured here: 540-580% on esync versus 141-188% on msync, \
                     for the same client.",
                    if b.perf.esync {
                        "esync"
                    } else {
                        "no explicit sync"
                    },
                    b.runtime
                ),
                fix: format!("exe-metal repair {}", b.name),
                source: "bottle config".into(),
                evidence: None,
            });
        }
    }

    // The window complaints all trace back to this one switch.
    if b.window.capture_displays {
        out.push(Finding {
            id: "display-capture".into(),
            severity: Severity::Warn,
            problem: "This bottle lets Wine capture the display for fullscreen. That traps \
                      the pointer inside the game, stops the window being dragged to another \
                      monitor, and breaks cmd-tab."
                .into(),
            fix: format!("exe-metal window {} --no-capture", b.name),
            source: "bottle config".into(),
            evidence: None,
        });
    }

    // Asking for ray tracing on silicon that has no RT units is a pure loss.
    if b.perf.dxr && !machine.has_hardware_raytracing() {
        out.push(Finding {
            id: "dxr-without-hardware".into(),
            severity: Severity::Warn,
            problem: format!(
                "DXR is enabled, but {} has no hardware ray tracing (M3 introduced it). \
                 This costs performance and returns nothing.",
                machine.silicon
            ),
            fix: "Set dxr = false under [perf] in the bottle's exe-metal.toml".into(),
            source: "bottle config".into(),
            evidence: None,
        });
    }

    out
}

fn log_candidates(layout: &Layout, b: &Bottle) -> Vec<PathBuf> {
    let mut v = vec![
        b.dir(layout).join("logs/run.log"),
        b.dir(layout).join("logs/steam.log"),
    ];
    let sd = steam::logs_dir(layout, b);
    for f in [
        "bootstrap_log.txt",
        "connection_log.txt",
        "webhelper.txt",
        "content_log.txt",
    ] {
        v.push(sd.join(f));
    }
    v.into_iter().filter(|p| p.is_file()).collect()
}

fn has_freetype(lib: &Path) -> bool {
    std::fs::read_dir(lib)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("libfreetype"))
        })
        .unwrap_or(false)
}

/// A process inside a bottle that has been reparented to init and is burning
/// CPU. Named by behaviour rather than by executable, because the case that
/// actually happened was `steam.exe` — which `prune` does not look for, and
/// which no log line ever mentioned.
///
/// Both conditions are required. A busy game is not orphaned: it has a live
/// wineserver as its parent, so `ppid` is never 1. That is what keeps this from
/// firing on something working hard and legitimately.
fn spinning_orphans(layout: &Layout, b: &Bottle) -> Vec<(u32, f32, String)> {
    let pfx = b.prefix(layout).to_string_lossy().to_string();
    let Ok(out) = util::capture("ps", &["-axo", "pid=,ppid=,%cpu=,command="]) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for line in out.lines() {
        if !line.contains(&pfx) && !line.contains("\\Steam\\") {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(cpu)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(cpu)) =
            (pid.parse::<u32>(), ppid.parse::<u32>(), cpu.parse::<f32>())
        else {
            continue;
        };
        // 200% is well clear of anything idle, and far below what a real game
        // under load would sit at while correctly parented.
        if ppid == 1 && cpu > 200.0 {
            let name = line
                .split_whitespace()
                .find(|t| t.ends_with(".exe"))
                .map(|t| t.rsplit(['\\', '/']).next().unwrap_or(t).to_string())
                .unwrap_or_else(|| "wine process".into());
            hits.push((pid, cpu, name));
        }
    }
    hits
}

/// Count Wine service processes whose prefix no longer has a live wineserver.
fn orphan_count(layout: &Layout) -> usize {
    let out = match util::capture("ps", &["-axo", "pid=,command="]) {
        Ok(o) => o,
        Err(_) => return 0,
    };
    let root = layout.root.to_string_lossy().to_string();
    out.lines()
        .filter(|l| {
            // Orphans report a Windows path, so match on the service names and
            // confirm the line belongs to us via the engines directory.
            (l.contains("services.exe") || l.contains("rpcss.exe") || l.contains("plugplay.exe"))
                && l.contains(&root)
        })
        .count()
}

fn free_gb() -> Option<u64> {
    let out = util::capture("df", &["-g", "/System/Volumes/Data"]).ok()?;
    let line = out.lines().nth(1)?;
    line.split_whitespace().nth(3)?.parse().ok()
}

fn dedupe(v: &mut Vec<Finding>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|f| seen.insert(f.id.clone()));
    // Fatal first — that is the thing to act on.
    v.sort_by_key(|f| match f.severity {
        Severity::Fatal => 0,
        Severity::Warn => 1,
        Severity::Note => 2,
    });
}

fn contains_ci(h: &str, n: &str) -> bool {
    h.to_ascii_lowercase().contains(&n.to_ascii_lowercase())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_steam_transport_error() {
        let log = "some noise\n[2026-01-01] Unexpected Transport Error (0x3008)\nmore noise";
        let f = scan_text(log, "test");
        assert!(f.iter().any(|x| x.id == "steam-webhelper-transport"));
        assert_eq!(f[0].severity, Severity::Fatal);
        assert!(f[0].evidence.as_ref().unwrap().contains("0x3008"));
    }

    #[test]
    fn all_mode_needs_every_needle() {
        // NtRaiseException alone is common and means nothing on its own.
        let partial = "err:seh:NtRaiseException something happened";
        assert!(!scan_text(partial, "t")
            .iter()
            .any(|f| f.id == "inno-seh-dispatch"));

        let full = "err:seh:NtRaiseException unable to dispatch exception to user mode";
        assert!(scan_text(full, "t")
            .iter()
            .any(|f| f.id == "inno-seh-dispatch"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let log = "FAILED TO CREATE METAL VIEW";
        assert!(scan_text(log, "t")
            .iter()
            .any(|f| f.id == "dxmt-no-winemetal"));
    }

    #[test]
    fn a_clean_log_produces_nothing() {
        let log = "Steam started\nConnectionCompleted() ok\nall good";
        assert!(scan_text(log, "t").is_empty());
    }

    /// Verbatim from a healthy connection_log.txt. Routine reconnect noise must
    /// not be reported as the account being taken over — this exact line was a
    /// false positive once and is the reason the signature is narrow.
    #[test]
    fn routine_reconnect_noise_is_not_an_elsewhere_login() {
        let log = "\
[2026-08-01 00:15:37] [Logged Off, 0, 0] [U:1:0] Sending SteamServerConnectFailure_t No Connection Reconnect
[2026-08-03 18:55:16] [Logging On, 4, 15] RecvMsgClientLogOnResponse() : 'OK'
[2026-08-03 18:55:16] [Logged On, 4, 15] RecvMsgClientLogOnResponse() : processing complete";
        let f = scan_text(log, "connection_log.txt");
        assert!(
            !f.iter().any(|x| x.id == "steam-two-clients"),
            "a successful login was reported as an eviction: {f:?}"
        );
        assert!(f.is_empty(), "healthy log produced findings: {f:?}");
    }

    #[test]
    fn a_genuine_eviction_is_still_caught() {
        let log = "[..] Logged off, result k_EResultLoggedInElsewhere";
        assert!(scan_text(log, "t")
            .iter()
            .any(|f| f.id == "steam-two-clients"));
    }

    #[test]
    fn every_signature_carries_an_actionable_fix() {
        for s in SIGNATURES {
            assert!(!s.fix.trim().is_empty(), "{} has no fix", s.id);
            assert!(!s.needles.is_empty(), "{} has no needles", s.id);
            assert!(!s.problem.trim().is_empty(), "{} has no problem", s.id);
        }
    }

    #[test]
    fn dedupe_keeps_fatal_first() {
        let mk = |id: &str, sev| Finding {
            id: id.into(),
            severity: sev,
            problem: String::new(),
            fix: String::new(),
            source: String::new(),
            evidence: None,
        };
        let mut v = vec![
            mk("a", Severity::Warn),
            mk("b", Severity::Fatal),
            mk("a", Severity::Warn),
        ];
        dedupe(&mut v);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].severity, Severity::Fatal);
    }
}
