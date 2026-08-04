//! `exe-metal play <exe>` — the whole flow, decided from the file itself.
//!
//! Inspect the binary, pick a runtime and renderer that can actually run it,
//! reuse or create a bottle, launch, remember it, and if it fails, say why.
//! The user supplies a path and nothing else.

use crate::bottle::{self, Bottle};
use crate::diagnose;
use crate::paths::Layout;
use crate::probe::{self, ExeInfo, Plan};
use crate::recent::Recent;
use crate::run;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct Request {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub detach: bool,
    pub hud: bool,
    /// Skip bottle selection and use this one.
    pub bottle: Option<String>,
}

pub struct Decision {
    pub info: ExeInfo,
    pub plan: Plan,
    pub bottle: String,
    /// True when the bottle had to be created.
    pub created: bool,
}

/// Work out what would happen, without doing it. `probe` prints this; `play`
/// acts on it.
pub fn decide(layout: &Layout, exe: &Path, override_bottle: Option<&str>) -> Result<Decision> {
    let info = probe::inspect(exe)?;
    let plan = probe::plan(&info);

    if let Some(name) = override_bottle {
        let created = Bottle::load(layout, name).is_err();
        return Ok(Decision {
            info,
            plan,
            bottle: name.to_string(),
            created,
        });
    }

    // Prefer a bottle already built for this game — the user may have tuned it,
    // and its shader cache is warm.
    let preferred = bottle_name_for(exe);
    if let Ok(existing) = Bottle::load(layout, &preferred) {
        if existing.renderer != plan.renderer {
            println!(
                "  note    bottle '{preferred}' uses {} but this binary suggests {} — \
                 keeping the existing bottle",
                existing.renderer, plan.renderer
            );
        }
        return Ok(Decision {
            info,
            plan,
            bottle: preferred,
            created: false,
        });
    }

    Ok(Decision {
        info,
        plan,
        bottle: preferred,
        created: true,
    })
}

pub fn play(layout: &Layout, req: &Request) -> Result<i32> {
    let exe = std::fs::canonicalize(&req.exe)
        .with_context(|| format!("no such file: {}", req.exe.display()))?;

    let decision = decide(layout, &exe, req.bottle.as_deref())?;
    print_decision(&decision);

    let mut b = if decision.created {
        println!("\ncreating bottle '{}'", decision.bottle);
        bottle::create(
            layout,
            &decision.bottle,
            decision.plan.runtime,
            decision.plan.renderer,
            false,
        )?
    } else {
        Bottle::load(layout, &decision.bottle)?
    };

    if req.hud {
        b.perf.hud = true;
    }

    // Check before launching, not after. A bad pairing or a missing WoW64 layer
    // produces a failure that is much harder to read once a game has started.
    let pre = diagnose::bottle(layout, &b)?;
    let fatal: Vec<_> = pre
        .findings
        .iter()
        .filter(|f| f.severity == diagnose::Severity::Fatal)
        .collect();
    if !fatal.is_empty() {
        println!("\nthis will not run yet:");
        pre.print();
        return Ok(1);
    }

    let log = b.dir(layout).join("logs/run.log");
    let sink = if req.detach { Some(log.as_path()) } else { None };

    println!("\nlaunching {}", exe.display());
    let code = run::exec(layout, &b, &exe, &req.args, req.detach, sink)?;

    // Record only what actually started. An exe that failed to launch is not
    // something to offer back in a recent list.
    if code == 0 {
        let mut recent = Recent::load(layout);
        recent.record(&exe, &b.name);
        let _ = recent.save(layout);
    }

    if code != 0 {
        println!("\nexited with {code} — looking for a cause");
        explain_failure(layout, &b, &log);
    }
    Ok(code)
}

/// After a non-zero exit, scan whatever was captured and name the cause.
pub fn explain_failure(layout: &Layout, b: &Bottle, log: &Path) {
    let mut findings = Vec::new();
    if let Ok(text) = std::fs::read_to_string(log) {
        findings.extend(diagnose::scan_text(&text, &log.display().to_string()));
    }
    if let Ok(report) = diagnose::bottle(layout, b) {
        findings.extend(report.findings);
    }
    let report = diagnose::Report { findings };
    if report.findings.is_empty() {
        println!("  no known failure signature matched.");
        println!("  the full log is at: {}", log.display());
        println!("  for more detail, re-run with Wine's channels on:");
        println!("    WINEDEBUG=+err,+warn exe-metal run {} <exe>", b.name);
    } else {
        report.print();
    }
}

pub fn print_decision(d: &Decision) {
    println!("{}", d.info.path.display());
    println!("  binary    {}", d.info.arch.label());
    let apis = if d.info.apis.is_empty() {
        "none named in the binary".to_string()
    } else {
        d.info
            .apis
            .iter()
            .map(|a| a.label())
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("  graphics  {apis}");
    println!("  runtime   {}", d.plan.runtime);
    println!("  renderer  {}  — {}", d.plan.renderer, d.plan.why);
    println!(
        "  bottle    {}{}",
        d.bottle,
        if d.created { "  (will be created)" } else { "" }
    );
    for w in &d.plan.warnings {
        println!("  warn      {w}");
    }
}

/// Name a bottle after the game rather than the executable.
///
/// Game directories are named for the game; executables are frequently named
/// `game.exe`, `launcher.exe` or the engine's own binary. Preferring the
/// directory means the bottle list stays readable.
fn bottle_name_for(exe: &Path) -> String {
    let from_dir = exe
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !is_generic_dir(s));
    let raw = from_dir.unwrap_or_else(|| {
        exe.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "game".into())
    });
    sanitize(&raw)
}

fn is_generic_dir(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "bin" | "bin64" | "win64" | "win32" | "x64" | "x86" | "game" | "release" | "build"
            | "desktop" | "downloads" | "documents"
    )
}

fn sanitize(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "game".into()
    } else {
        trimmed.chars().take(32).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottles_are_named_after_the_game_directory() {
        assert_eq!(
            bottle_name_for(Path::new("/games/Hollow Knight/hollow_knight.exe")),
            "hollow-knight"
        );
        assert_eq!(
            bottle_name_for(Path::new("/games/UNDERTALE/UNDERTALE.exe")),
            "undertale"
        );
    }

    #[test]
    fn generic_directories_fall_back_to_the_executable_name() {
        // ".../Celeste/bin/Celeste.exe" must not produce a bottle called "bin".
        assert_eq!(
            bottle_name_for(Path::new("/games/Celeste/bin/Celeste.exe")),
            "celeste"
        );
        assert_eq!(
            bottle_name_for(Path::new("/x/win64/Portal2.exe")),
            "portal2"
        );
    }

    #[test]
    fn sanitize_produces_usable_directory_names() {
        assert_eq!(sanitize("Baldur's Gate 3"), "baldur-s-gate-3");
        assert_eq!(sanitize("!!!"), "game");
        assert_eq!(sanitize(""), "game");
        assert_eq!(sanitize("  spaced  out  "), "spaced-out");
        assert!(!sanitize("a".repeat(100).as_str()).contains('/'));
        assert!(sanitize("a".repeat(100).as_str()).len() <= 32);
    }

    #[test]
    fn names_never_contain_path_separators() {
        // A bottle name becomes a directory under bottles/, so a stray slash
        // would escape the layout entirely.
        for s in ["../../etc", "a/b/c", "x\\y"] {
            let n = sanitize(s);
            assert!(!n.contains('/'), "{s} -> {n}");
            assert!(!n.contains('\\'), "{s} -> {n}");
            assert!(!n.contains(".."), "{s} -> {n}");
        }
    }
}
