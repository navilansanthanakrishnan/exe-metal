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
use std::path::PathBuf;

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
    let code = run::exec(
        layout,
        bottle,
        &installer,
        &["/S".to_string()],
        false,
        None,
    )?;
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
        if attempt == 1 {
            println!("starting Steam (a first run downloads ~1.5 GB, several minutes)");
        } else {
            println!("bootstrapper handed off; restarting Steam (attempt {attempt})");
        }
        run::exec(layout, bottle, &exe, &args, true, Some(&log))?;

        if wait_until_ready(layout, bottle, 300) {
            println!("Steam is up. Log in in the Steam window.");
            return Ok(());
        }
        if still_running(layout, bottle) {
            println!("Steam is still working — watch it with: exe-metal steam logs {}", bottle.name);
            return Ok(());
        }
    }
    eprintln!(
        "Steam did not finish starting. Inspect: exe-metal steam logs {}",
        bottle.name
    );
    Ok(())
}

/// The client has genuinely reached the UI once it opens a connection log.
fn wait_until_ready(layout: &Layout, bottle: &Bottle, secs: u64) -> bool {
    let marker = logs_dir(layout, bottle).join("connection_log.txt");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if marker.is_file() {
            return true;
        }
        if !still_running(layout, bottle) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    marker.is_file()
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
    let running = std::process::Command::new("pgrep")
        .args(["-f", "Steam.AppBundle"])
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

/// Steam's own logs, which say far more than anything Wine prints.
pub fn logs_dir(layout: &Layout, bottle: &Bottle) -> PathBuf {
    bottle
        .prefix(layout)
        .join("drive_c/Program Files (x86)/Steam/logs")
}
