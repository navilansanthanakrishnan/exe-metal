//! Steam integration.
//!
//! Steam is the reason most of this exists, and it is also the most fragile
//! part. Two things matter:
//!
//!   * The client is 32-bit, so the bottle needs WoW64.
//!   * The client rejects its own `steamwebhelper` on older Wine, failing with
//!     `Unexpected Transport Error (0x3008)`. Wine 10.0 is the oldest base
//!     verified working here.

use crate::bottle::Bottle;
use crate::install;
use crate::paths::Layout;
use crate::run;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub fn steam_exe(layout: &Layout, bottle: &Bottle) -> PathBuf {
    bottle
        .prefix(layout)
        .join("drive_c/Program Files (x86)/Steam/steam.exe")
}

pub fn is_installed(layout: &Layout, bottle: &Bottle) -> bool {
    steam_exe(layout, bottle).is_file()
}

pub fn install(layout: &Layout, bottle: &Bottle) -> Result<()> {
    if is_installed(layout, bottle) {
        println!("Steam is already installed in bottle '{}'", bottle.name);
        return Ok(());
    }

    if !bottle
        .prefix(layout)
        .join("drive_c/Program Files (x86)")
        .is_dir()
    {
        bail!(
            "bottle '{}' has no WoW64 layer, so the 32-bit Steam client cannot run.\n\
             Recreate it with a runtime that supports 32-bit.",
            bottle.name
        );
    }

    let installer = install::steam_installer(layout)?;
    println!("running the Steam installer");
    let code = run::exec(layout, bottle, &installer, &["/S".to_string()], false, None)?;
    if code != 0 {
        eprintln!("  warn    installer exited with {code}");
    }

    // The NSIS installer returns before the files land.
    for _ in 0..60 {
        if is_installed(layout, bottle) {
            println!("Steam installed into bottle '{}'", bottle.name);
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    bail!("Steam installer finished but steam.exe never appeared")
}

pub fn launch(layout: &Layout, bottle: &Bottle, detach: bool) -> Result<()> {
    if !is_installed(layout, bottle) {
        bail!(
            "Steam is not installed in bottle '{}' — run: exe-metal steam install {}",
            bottle.name,
            bottle.name
        );
    }

    // Refuse to stack a second client on top of a live one. Detached launches
    // survive the shell that started them, so without this check every repeat
    // invocation leaves another Steam window on screen and another set of
    // processes competing for the CPU.
    if still_running(layout, bottle) {
        bail!(
            "something is already running in bottle '{}'.\n\
             Stop it first:  exe-metal kill {}",
            bottle.name,
            bottle.name
        );
    }

    warn_about_native_steam();

    let exe = steam_exe(layout, bottle);
    let log = bottle.dir(layout).join("logs/steam.log");

    // Steam's embedded browser is the most common crash source under Wine;
    // keeping it off the GPU is far more stable and costs nothing visually.
    let args = vec!["-cef-disable-gpu".to_string()];

    if !detach {
        run::exec(layout, bottle, &exe, &args, false, Some(&log))?;
        return Ok(());
    }

    // A first run does not reach the UI in one go. The bootstrapper fetches the
    // 32-bit client, then discovers the client is now 64-bit, downloads that
    // set too, and exits with "Update complete, launching Steam..." — without
    // actually leaving a client running. One relaunch picks up where it left
    // off; more than that means something is wrong, and spinning here would
    // just pile up windows.
    for attempt in 1..=2 {
        // Never stack a second client on a live one. The bootstrapper handoff
        // is the only reason to relaunch, and that only happens once the first
        // process has exited — so if anything is still alive here, relaunching
        // would produce the duplicate Steam windows rather than fix anything.
        if attempt > 1 && still_running(layout, bottle) {
            println!("Steam is already running — not launching a second copy");
            return Ok(());
        }
        if attempt == 1 {
            println!("starting Steam (a first run downloads ~1.5 GB, several minutes)");
        } else {
            println!("bootstrapper handed off; restarting Steam (attempt {attempt})");
        }
        let launched_at = SystemTime::now();
        run::exec(layout, bottle, &exe, &args, true, Some(&log))?;

        if wait_until_ready(layout, bottle, 300, launched_at) {
            println!("Steam is up. Log in in the Steam window.");
            return Ok(());
        }
        if still_running(layout, bottle) {
            println!(
                "Steam is still working — watch it with: exe-metal steam logs {}",
                bottle.name
            );
            return Ok(());
        }
    }
    eprintln!(
        "Steam did not finish starting. Inspect: exe-metal steam logs {}",
        bottle.name
    );
    Ok(())
}

/// The client has genuinely reached the UI once it *writes* to its connection
/// log.
///
/// Existence alone is not enough, and trusting it was a bug: the file survives
/// from every previous session, so on any bottle that had run before this
/// returned true instantly and reported "Steam is up" while the client was
/// still starting — or had already died. Comparing against the launch time
/// makes the check mean what it says.
fn wait_until_ready(layout: &Layout, bottle: &Bottle, secs: u64, since: SystemTime) -> bool {
    let marker = logs_dir(layout, bottle).join("connection_log.txt");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if written_since(&marker, since) {
            return true;
        }
        if !still_running(layout, bottle) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    written_since(&marker, since)
}

fn written_since(p: &std::path::Path, since: SystemTime) -> bool {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .map(|m| m >= since)
        .unwrap_or(false)
}

/// Is anything still alive in this bottle's prefix?
fn still_running(layout: &Layout, bottle: &Bottle) -> bool {
    let pfx = bottle.prefix(layout);
    std::process::Command::new("pgrep")
        .args(["-f", &pfx.to_string_lossy()])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

/// A Steam account can hold exactly one session. If the native macOS client is
/// signed in, the bottled client logs in and is immediately dropped with
/// "NO CONNECTION", which looks like a networking bug and is not one.
fn warn_about_native_steam() {
    // Match the client itself, not the bundle path. `Steam.AppBundle` also
    // matches the `ipcserver` helper, which lingers long after the native
    // client quits — it fired on every launch during testing and was wrong
    // every time. A warning that cries wolf gets ignored when it matters.
    let running = std::process::Command::new("pgrep")
        .args(["-f", "Steam.AppBundle/Steam/Contents/MacOS/steam_osx"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if running {
        eprintln!(
            "  note    the native macOS Steam client is running. Two clients cannot share\n\
             \x20         one account: the bottled client will be disconnected right after\n\
             \x20         login. Quit the native client first."
        );
    }
}

/// A game that Steam owns, and the id Steam knows it by.
#[derive(Debug, Clone)]
pub struct SteamApp {
    pub appid: String,
    pub install_dir: String,
}

/// Is this executable a Steam game, and if so which one?
///
/// This matters more than it sounds. A game shipping `steam_api.dll` does not
/// simply run when launched directly — it re-invokes the client with
/// `steam.exe steam://run/<appid>` and exits. Launch it by path and you get no
/// game and an extra Steam client; do it a few times and you have several,
/// which is exactly the "duplicate Steam" everyone eventually hits.
///
/// Observed verbatim in a Wine log:
/// `Startup - Steam Client launched with: "…\steam.exe" steam://run/504230`
pub fn detect_app(exe: &Path) -> Option<SteamApp> {
    let dir = exe.parent()?;

    // The DRM shim sits next to the executable.
    let has_api = ["steam_api.dll", "steam_api64.dll"]
        .iter()
        .any(|n| dir.join(n).is_file());
    if !has_api {
        return None;
    }

    // .../steamapps/common/<install_dir>/... — walk up to find which.
    let mut cur = dir;
    let mut install_dir = None;
    while let Some(parent) = cur.parent() {
        if parent.file_name().is_some_and(|n| n == "common")
            && parent
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|n| n == "steamapps"))
        {
            install_dir = cur.file_name().map(|s| s.to_string_lossy().to_string());
            break;
        }
        cur = parent;
    }
    let install_dir = install_dir?;
    let steamapps = cur.parent()?.parent()?;

    // Match the manifest by installdir rather than guessing from the name:
    // Steam's folder name and the store name often differ (Project Playtime
    // installs as "Poppy Playtime - Multiplayer").
    for entry in std::fs::read_dir(steamapps).ok()?.flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "acf") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if vdf_value(&text, "installdir").is_none_or(|d| d != install_dir) {
            continue;
        }
        if let Some(appid) = vdf_value(&text, "appid") {
            return Some(SteamApp { appid, install_dir });
        }
    }
    None
}

/// Pull `"key"  "value"` out of Steam's VDF, which is not quite JSON and not
/// worth a dependency for two fields.
fn vdf_value(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    for line in text.lines() {
        let t = line.trim();
        if !t
            .to_ascii_lowercase()
            .starts_with(&needle.to_ascii_lowercase())
        {
            continue;
        }
        let mut parts = t.split('"').filter(|s| !s.trim().is_empty());
        let _k = parts.next()?;
        let v = parts.next()?;
        return Some(v.trim().to_string());
    }
    None
}

/// Launch a game the way Steam expects, through the running client.
pub fn run_app(layout: &Layout, bottle: &Bottle, appid: &str) -> Result<()> {
    if !is_installed(layout, bottle) {
        bail!(
            "bottle '{}' has no Steam client, so it cannot launch a Steam game",
            bottle.name
        );
    }
    let exe = steam_exe(layout, bottle);
    let url = format!("steam://run/{appid}");
    let log = bottle.dir(layout).join("logs/steam.log");

    if still_running(layout, bottle) {
        // Handing the URL to the live client is the whole point — it does not
        // start a second one.
        println!("asking the running Steam client to launch app {appid}");
    } else {
        println!("starting Steam and launching app {appid} (this takes a moment)");
    }
    run::exec(layout, bottle, &exe, &[url], true, Some(&log))?;
    Ok(())
}

/// Steam's own logs, which say far more than anything Wine prints.
pub fn logs_dir(layout: &Layout, bottle: &Bottle) -> PathBuf {
    bottle
        .prefix(layout)
        .join("drive_c/Program Files (x86)/Steam/logs")
}
