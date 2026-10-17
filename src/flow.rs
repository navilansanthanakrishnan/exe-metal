//! The two journeys the app exists for: *open an installer* and *play a thing
//! that was installed*.
//!
//! Everything here is deliberately callable from both the CLI and the GUI, and
//! returns data rather than printing, so the home page can show progress
//! without scraping stdout.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::auto;
use crate::bottle::{self, Bottle};
use crate::crossover;
use crate::library::{Backend, Library, TitleKind};
use crate::paths::Layout;
use crate::scan;

/// What running an installer produced.
#[derive(Debug)]
pub struct InstallOutcome {
    pub bottle: String,
    /// Bottle was created as part of this run.
    pub created_bottle: bool,
    /// Exit status of the installer itself.
    pub status: i32,
    /// Ids of titles registered because this installer created them.
    pub registered: Vec<String>,
    /// Executables that appeared but were classified as plumbing.
    pub support_found: usize,
}

/// Open an `.exe`. This is what a double-click in Finder ends up calling.
///
/// An installer is run and then diffed, so whatever it drops into the prefix
/// becomes library entries. A program that is already a known title is simply
/// launched. The distinction is made from the file, not from the user.
pub fn open_exe(
    layout: &Layout,
    exe: &Path,
    bottle_override: Option<&str>,
    detach: bool,
) -> Result<InstallOutcome> {
    let exe =
        std::fs::canonicalize(exe).with_context(|| format!("no such file: {}", exe.display()))?;

    // If this exact executable is already a registered title, this is a launch,
    // not an install. Re-running an installer is still allowed — it just will
    // not have been registered as a title in the first place.
    {
        let lib = Library::load()?;
        if let Some(t) = lib.by_exe(&exe) {
            let id = t.id.clone();
            let bottle_name = t.bottle.clone();
            let status = launch_title(layout, &id, detach)?;
            return Ok(InstallOutcome {
                bottle: bottle_name,
                created_bottle: false,
                status,
                registered: vec![],
                support_found: 0,
            });
        }
    }

    let decision = auto::decide(layout, &exe, bottle_override)?;
    let bottle_name = decision.bottle.clone();
    let created = decision.created;

    // Snapshot before, so the diff afterwards is exactly this installer's work.
    // A bottle that does not exist yet trivially has nothing in it.
    let before = if created {
        Default::default()
    } else {
        let b = Bottle::load(layout, &bottle_name)?;
        scan::inventory(&b.prefix(layout))
    };

    let req = auto::Request {
        exe: exe.clone(),
        args: vec![],
        detach,
        hud: false,
        bottle: bottle_override.map(|s| s.to_string()),
    };
    let status = auto::play(layout, &req)?;

    // A detached run has not finished installing anything yet, so there is
    // nothing to diff. The caller rescans when the user says they are done.
    if detach {
        return Ok(InstallOutcome {
            bottle: bottle_name,
            created_bottle: created,
            status,
            registered: vec![],
            support_found: 0,
        });
    }

    let b = Bottle::load(layout, &bottle_name)?;
    let fresh = scan::newly_installed(&b.prefix(layout), &before);

    let mut lib = Library::load()?;
    let mut registered = Vec::new();
    let mut support = 0;
    for c in &fresh {
        match c.kind {
            scan::Kind::Support => support += 1,
            _ => {
                let id = lib.add(&bottle_name, c, Some(exe.clone()));
                if !registered.contains(&id) {
                    registered.push(id);
                }
            }
        }
    }
    lib.save()?;

    Ok(InstallOutcome {
        bottle: bottle_name,
        created_bottle: created,
        status,
        registered,
        support_found: support,
    })
}

/// Does this title belong to a launcher that must start it?
///
/// Overwatch launched by path reports "unable to authenticate login": the game
/// has no session, because Battle.net is what mints one and passes it in. The
/// same shape as a Steam game launched by path, which exe-metal already hands
/// to the Steam client (see `steam::detect_app`). So: find the owning client in
/// the same bottle and start that instead, rather than starting a binary that
/// cannot possibly authenticate.
fn owning_launcher(title: &crate::library::Title) -> Option<(&'static str, std::path::PathBuf)> {
    let p = title.exe.to_string_lossy();
    // Blizzard titles live beside a Battle.net install in the same drive_c.
    if p.contains("/Overwatch/") || p.contains("\\Overwatch\\") || p.contains("Blizzard") {
        // drive_c is 3 levels up from Program Files (x86)/<Game>/<...>
        let mut dir = title.exe.as_path();
        while let Some(parent) = dir.parent() {
            let candidate = parent.join("Battle.net").join("Battle.net Launcher.exe");
            if candidate.is_file() {
                return Some(("Battle.net", candidate));
            }
            dir = parent;
            if parent.ends_with("drive_c") {
                break;
            }
        }
    }
    None
}

/// Battle.net's product code for a title, for `--exec="launch <code>"`.
///
/// Only codes seen working here are listed. A wrong code makes Battle.net open
/// on its home page and do nothing, which is a worse failure than not asking.
fn blizzard_code(exe: &Path) -> Option<&'static str> {
    const CODES: &[(&str, &str)] = &[("Overwatch", "Pro")];
    exe.ancestors().find_map(|dir| {
        let name = dir.file_name()?.to_str()?;
        CODES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
    })
}

/// Launch a registered title and record that it ran.
pub fn launch_title(layout: &Layout, id: &str, detach: bool) -> Result<i32> {
    let mut lib = Library::load()?;
    let title = lib
        .get(id)
        .cloned()
        .with_context(|| format!("no title '{id}' in the library"))?;

    if !title.exists() {
        anyhow::bail!(
            "'{}' is registered but its executable is gone:\n  {}\nrun `exe-metal rescan` to tidy the library",
            title.name,
            title.exe.display()
        );
    }

    // A title whose launcher owns authentication cannot be started by path.
    let launcher = owning_launcher(&title);

    let status = match title.backend {
        Backend::CrossOver => {
            // Renderer tuning has to be passed explicitly: CrossOver builds its
            // own environment and does not read exe-metal's bottle config.
            let env = vec![
                ("D3DM_ENABLE_METALFX".to_string(), "1".to_string()),
                // Overwatch and friends have no ray tracing, and D3DMetal
                // defaults DXR on for M3 and later — pure wasted GPU time.
                ("D3DM_SUPPORT_DXR".to_string(), "0".to_string()),
            ];
            let log = crossover::prefix(&title.bottle)
                .map(|p| p.join("exe-metal-run.log"))
                .ok();
            let target = match &launcher {
                Some((_, path)) => path.clone(),
                None => title.exe.clone(),
            };
            crossover::run(&title.bottle, &target, &[], &env, log.as_deref())?;
            // The launcher only opens the client, which then waits on Play.
            // Battle.net takes `--exec="launch <code>"` from a second copy of
            // itself and launches the game, so ask it once it is up.
            if let (Some(_), Some(code)) = (&launcher, blizzard_code(&title.exe)) {
                let client = target.with_file_name("Battle.net.exe");
                crossover::run_when_running(
                    &title.bottle,
                    "Battle.net.exe",
                    &client,
                    &format!("--exec=launch {code}"),
                )?;
            }
            0
        }
        // Through `play`, not straight to Wine: that is what hands a Steam
        // game to its client, refuses a bottle that cannot run it, and names
        // the cause when it exits non-zero. Calling `run::exec` here skipped
        // all three, so a library launch could fail where `play` would not.
        Backend::ExeMetal => {
            Bottle::load(layout, &title.bottle)
                .with_context(|| format!("bottle '{}' could not be loaded", title.bottle))?;
            auto::play(
                layout,
                &auto::Request {
                    exe: title.exe.clone(),
                    args: vec![],
                    detach,
                    hud: false,
                    bottle: Some(title.bottle.clone()),
                },
            )?
        }
    };
    if status != 0 {
        return Ok(status);
    }

    lib.mark_played(id);
    lib.save()?;
    Ok(status)
}

/// Close a running title **gracefully**, so it saves its settings.
///
/// Killing a Windows game is not the same as quitting it: many only write
/// their configuration on a clean shutdown. Overwatch discards every graphics
/// change when the process is killed, which looks exactly like "settings do not
/// save" and sent me chasing the wrong bug for hours. `taskkill` without `/F`
/// posts WM_CLOSE, which the game handles as a real quit.
pub fn quit_title(layout: &Layout, id: &str) -> Result<()> {
    let lib = Library::load()?;
    let title = lib
        .get(id)
        .with_context(|| format!("no title '{id}' in the library"))?;
    let image = title
        .exe
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("title has no executable name"))?;

    match title.backend {
        Backend::CrossOver => crossover::quit(&title.bottle, &image),
        Backend::ExeMetal => {
            let b = Bottle::load(layout, &title.bottle)?;
            // Named by its Windows path: `run` resolves a bare name against
            // drive_c and the working directory, and finds it in neither.
            let taskkill = PathBuf::from(r"C:\windows\system32\taskkill.exe");
            crate::run::exec(
                layout,
                &b,
                &taskkill,
                &["/IM".to_string(), image],
                false,
                None,
            )
            .map(|_| ())
        }
    }
}

/// Walk every bottle and register anything launchable that is not known yet.
///
/// Also drops entries whose executable has gone. Returns (added, pruned).
pub fn rescan(layout: &Layout, only: Option<&str>) -> Result<(Vec<String>, usize)> {
    let mut lib = Library::load()?;
    let pruned = lib.prune_missing();

    let bottles: Vec<Bottle> = match only {
        Some(name) => vec![Bottle::load(layout, name)?],
        None => bottle::list(layout),
    };

    let mut added = Vec::new();

    // CrossOver bottles are scanned with the same logic; only the prefix
    // layout differs (drive_c sits directly in the bottle, not under pfx/).
    if only.is_none() {
        for name in crossover::list_bottles() {
            let Ok(prefix) = crossover::prefix(&name) else {
                continue;
            };
            for c in scan::candidates(&prefix) {
                if c.kind != scan::Kind::App || lib.by_exe(&c.path).is_some() {
                    continue;
                }
                added.push(lib.add_with(&name, &c, None, Backend::CrossOver));
            }
        }
    }

    for b in &bottles {
        let prefix = b.prefix(layout);
        if !prefix.exists() {
            continue;
        }
        for c in scan::candidates(&prefix) {
            // Only real applications get added automatically. Support binaries
            // and installers are still discoverable from a title's detail
            // panel, but they do not earn a tile on their own.
            if c.kind != scan::Kind::App {
                continue;
            }
            if lib.by_exe(&c.path).is_some() {
                continue;
            }
            added.push(lib.add(&b.name, &c, None));
        }
    }
    added.extend(adopt_played(layout, &mut lib, only));
    lib.save()?;
    Ok((added, pruned))
}

/// Register what `play` has run from outside every bottle.
///
/// A game kept in `~/Games` is invisible to a prefix scan, so without this the
/// library missed exactly the titles played most. The recent list already
/// knows them, along with how often and how lately they ran.
fn adopt_played(layout: &Layout, lib: &mut Library, only: Option<&str>) -> Vec<String> {
    let mut added = Vec::new();
    for e in crate::recent::Recent::load(layout).list() {
        if only.is_some_and(|o| o != e.bottle) || !e.exists() {
            continue;
        }
        let Ok(b) = Bottle::load(layout, &e.bottle) else {
            continue;
        };
        if e.path.starts_with(b.prefix(layout)) || lib.by_exe(&e.path).is_some() {
            continue;
        }
        let Some(c) = scan::external(&e.path) else {
            continue;
        };
        if c.kind != scan::Kind::App {
            continue;
        }
        let id = lib.add(&e.bottle, &c, None);
        if let Some(t) = lib.get_mut(&id) {
            t.last_played = Some(e.last_run);
            t.play_count = e.runs;
        }
        added.push(id);
    }
    added
}

/// After `play`, make sure what just ran has a tile.
pub fn remember_played(layout: &Layout) -> Result<()> {
    let mut lib = Library::load()?;
    if !adopt_played(layout, &mut lib, None).is_empty() {
        lib.save()?;
    }
    Ok(())
}

/// Create the bottle a fresh install should go to, if it is not there already.
#[allow(dead_code)]
pub fn ensure_bottle(layout: &Layout, name: &str, runtime: &str, renderer: &str) -> Result<Bottle> {
    if let Ok(b) = Bottle::load(layout, name) {
        return Ok(b);
    }
    bottle::create(layout, name, runtime, renderer, false)
}

/// The other executables shipped with a title — its settings tool, server
/// binary, launcher, and so on.
pub fn parts_of(layout: &Layout, id: &str) -> Result<Vec<scan::Candidate>> {
    let lib = Library::load()?;
    let title = lib
        .get(id)
        .with_context(|| format!("no title '{id}' in the library"))?;
    let prefix = crate::registry::prefix_of(layout, title.backend, &title.bottle)?;
    Ok(scan::siblings(&prefix, &title.exe))
}

/// Did launching this title hand off to its client rather than start it directly?
pub fn launcher_for(id: &str) -> Option<String> {
    let lib = Library::load().ok()?;
    let t = lib.get(id)?;
    owning_launcher(t).map(|(name, _)| name.to_string())
}

/// Human summary for the CLI and the GUI status line.
pub fn describe_outcome(o: &InstallOutcome) -> String {
    if !o.registered.is_empty() {
        let n = o.registered.len();
        format!(
            "installed {} {} into {} bottle '{}'",
            n,
            if n == 1 {
                "application"
            } else {
                "applications"
            },
            if o.created_bottle { "new" } else { "existing" },
            o.bottle
        )
    } else if o.status != 0 {
        format!("the installer exited with status {}", o.status)
    } else {
        format!(
            "nothing new appeared in bottle '{}' — if the installer is still \
             running, rescan when it finishes",
            o.bottle
        )
    }
}

/// Kind shown in the UI, as a short word.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn kind_label(k: TitleKind) -> &'static str {
    match k {
        TitleKind::App => "application",
        TitleKind::Support => "helper",
        TitleKind::Installer => "installer",
    }
}

/// Absolute path an entry should reveal in Finder.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn reveal_target(exe: &Path) -> PathBuf {
    exe.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| exe.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blizzard_codes_come_from_the_install_folder() {
        let ow = Path::new("/b/drive_c/Program Files (x86)/Overwatch/_retail_/Overwatch.exe");
        assert_eq!(blizzard_code(ow), Some("Pro"));
        let other =
            Path::new("/b/drive_c/Program Files (x86)/Steam/steamapps/common/Celeste/Celeste.exe");
        assert_eq!(blizzard_code(other), None);
    }
}
