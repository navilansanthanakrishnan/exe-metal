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
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The client's own install directory inside the bottle. Everything else here
/// hangs off it: the executable, the logs, `steamapps`, the login config.
pub fn steam_dir(layout: &Layout, bottle: &Bottle) -> PathBuf {
    bottle
        .prefix(layout)
        .join("drive_c/Program Files (x86)/Steam")
}

pub fn steam_exe(layout: &Layout, bottle: &Bottle) -> PathBuf {
    steam_dir(layout, bottle).join("steam.exe")
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
    //
    // Scoped to the client processes, not to the prefix: a `wineserver` hangs
    // around after the client it served has gone, and refusing on that would
    // make the bottle unlaunchable until it happened to time out.
    if client_running(layout, bottle) {
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

    // Steam's embedded browser is the most common crash source under Wine, and
    // taking it off the GPU is the usual way to stabilise it — but that is not
    // free. The client's entire interface is CEF, so software rasterising it
    // means compositing every scroll on the CPU under Rosetta.
    let mut args = Vec::new();
    if !bottle.perf.cef_gpu {
        args.push("-cef-disable-gpu".to_string());
    }

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
///
/// Liveness is judged on the bootstrap log still growing, not on a process
/// being visible. The bootstrapper re-execs itself when it swaps the 32-bit
/// client for the 64-bit one, and a process check taken during that handoff
/// reports nothing running — which made the launcher abandon a download that
/// was progressing perfectly well.
fn wait_until_ready(layout: &Layout, bottle: &Bottle, secs: u64) -> bool {
    let marker = logs_dir(layout, bottle).join("connection_log.txt");
    let boot = logs_dir(layout, bottle).join("bootstrap_log.txt");

    // How long the bootstrap log may sit untouched, with nothing alive in the
    // prefix, before this is called a failure rather than a slow download.
    const STALL: std::time::Duration = std::time::Duration::from_secs(45);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut last_len = 0u64;
    let mut last_change = std::time::Instant::now();

    while std::time::Instant::now() < deadline {
        if marker.is_file() {
            return true;
        }
        let len = std::fs::metadata(&boot).map(|m| m.len()).unwrap_or(0);
        if len != last_len {
            last_len = len;
            last_change = std::time::Instant::now();
        }
        if !still_running(layout, bottle) && last_change.elapsed() > STALL {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    marker.is_file()
}

/// Is anything still alive in this bottle's prefix?
fn still_running(layout: &Layout, bottle: &Bottle) -> bool {
    run::alive(layout, bottle)
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
    steam_dir(layout, bottle).join("logs")
}

// ---------------------------------------------------------------------------
// Apps
// ---------------------------------------------------------------------------
//
// Launching a game is not the same problem as launching an executable. Steam
// games expect the client to be live, to have authenticated, and to have
// handed them their app id — running the .exe directly gets you an immediate
// exit with no message. So everything below drives the client rather than the
// game.

/// Where the client keeps app manifests and the games themselves.
pub fn steamapps(layout: &Layout, bottle: &Bottle) -> PathBuf {
    steam_dir(layout, bottle).join("steamapps")
}

/// `StateFlags` bit 2. It is the only value that means "ready to run"; a
/// manifest exists from the moment a download is queued, so its presence
/// proves nothing on its own.
const STATE_FULLY_INSTALLED: u64 = 4;

/// One app, as the bottled client's `.acf` manifest describes it.
#[derive(Debug, Clone)]
pub struct App {
    pub id: String,
    pub name: String,
    pub installdir: String,
    pub state: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

impl App {
    pub fn installed(&self) -> bool {
        self.state & STATE_FULLY_INSTALLED != 0
    }

    /// Host path to the game's own directory.
    pub fn dir(&self, layout: &Layout, bottle: &Bottle) -> PathBuf {
        steamapps(layout, bottle)
            .join("common")
            .join(&self.installdir)
    }

    pub fn percent(&self) -> f64 {
        if self.bytes_total == 0 {
            return if self.installed() { 100.0 } else { 0.0 };
        }
        (self.bytes_done as f64 / self.bytes_total as f64 * 100.0).min(100.0)
    }
}

/// Pull the flat `"key" "value"` pairs out of a KeyValues file.
///
/// Valve's format nests, but every field an app manifest exposes that matters
/// here sits at the top level of `AppState`; the nested blocks below it are
/// depot tables keyed by numeric id, which cannot collide with the names read
/// out. First occurrence wins, so a nested `"name"` cannot shadow the real one.
fn kv_pairs(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let toks: Vec<&str> = line.split('"').collect();
        // "key"<ws>"value" splits to ["", key, ws, value, ""].
        if toks.len() >= 5 && !toks[1].is_empty() {
            map.entry(toks[1].to_string())
                .or_insert_with(|| toks[3].to_string());
        }
    }
    map
}

/// Every app the bottled client has a manifest for.
pub fn apps(layout: &Layout, bottle: &Bottle) -> Vec<App> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(steamapps(layout, bottle)) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("acf") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let kv = kv_pairs(&text);
        let Some(id) = kv.get("appid") else { continue };
        out.push(App {
            id: id.clone(),
            name: kv.get("name").cloned().unwrap_or_default(),
            installdir: kv.get("installdir").cloned().unwrap_or_default(),
            state: kv.get("StateFlags").and_then(|s| s.parse().ok()).unwrap_or(0),
            bytes_done: kv
                .get("BytesDownloaded")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            bytes_total: kv
                .get("BytesToDownload")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn app(layout: &Layout, bottle: &Bottle, appid: &str) -> Option<App> {
    apps(layout, bottle).into_iter().find(|a| a.id == appid)
}

/// Has anyone signed in to this bottle's client yet?
///
/// Everything app-related fails in a confusing way when nobody has: the client
/// silently drops `steam://` requests rather than reporting anything.
pub fn logged_in(layout: &Layout, bottle: &Bottle) -> bool {
    std::fs::read_to_string(steam_dir(layout, bottle).join("config/loginusers.vdf"))
        .map(|t| t.contains("AccountName"))
        .unwrap_or(false)
}

/// Is the Steam client itself up in this bottle?
///
/// Two signals, because neither is sufficient alone. `run::alive` is exact
/// about *which bottle* is running but says nothing about what is in it — a
/// bare `wineserver` outlives the client it served. The process scan knows
/// Steam when it sees it but cannot say which prefix it belongs to, since Wine
/// leaves only the Windows path on the command line.
///
/// Together they are right whenever one client is running, which is the only
/// case Steam itself supports.
pub fn client_running(layout: &Layout, bottle: &Bottle) -> bool {
    if !run::alive(layout, bottle) {
        return false;
    }
    std::process::Command::new("ps")
        .args(["-Ao", "command="])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).lines().any(|l| {
                let l = l.to_ascii_lowercase();
                (l.contains("steam.exe") || l.contains("steamwebhelper")) && !l.contains("appbundle")
            })
        })
        .unwrap_or(false)
}

/// Start the client if it is not already up.
///
/// Idempotent, which is the difference from `launch`: driving an install or a
/// game launch needs a live client, so refusing because one already exists is
/// exactly backwards there.
pub fn ensure_running(layout: &Layout, bottle: &Bottle) -> Result<()> {
    if client_running(layout, bottle) {
        return Ok(());
    }
    println!("client is not running; starting it first");
    launch(layout, bottle, true)
}

/// Hand a `steam://` request to the client.
///
/// A second `steam.exe` is the documented way to do this: it finds the live
/// client, forwards the URL and exits immediately. It is not a second client.
fn steam_url(layout: &Layout, bottle: &Bottle, url: &str) -> Result<()> {
    let exe = steam_exe(layout, bottle);
    let log = bottle.dir(layout).join("logs/steam-url.log");
    run::exec(layout, bottle, &exe, &[url.to_string()], true, Some(&log))?;
    Ok(())
}

/// Queue an app for install. Free-to-play titles are added to the account by
/// the same request, so this covers "I do not own it yet" as well.
///
/// **This is not headless.** The client answers a `steam://install` request by
/// opening its Install dialog, and a title with a EULA opens that too; nothing
/// downloads until both are dismissed in the Steam window. There is no command
/// line that skips them. The manifest does not appear until they are answered,
/// which is why `wait_for_install` tolerates a long silence at the start.
pub fn install_app(layout: &Layout, bottle: &Bottle, appid: &str) -> Result<()> {
    if !logged_in(layout, bottle) {
        bail!(
            "nobody is signed in to the client in bottle '{}'.\n\
             Start it with 'exe-metal steam launch {}' and log in first — until then\n\
             the client discards steam:// requests without reporting anything.",
            bottle.name,
            bottle.name
        );
    }
    ensure_running(layout, bottle)?;
    println!("requesting install of app {appid}");
    steam_url(layout, bottle, &format!("steam://install/{appid}"))
}

/// Poll the manifest until the download completes, reporting progress.
///
/// The client gives no exit code for a download, so the manifest is the only
/// thing to watch. Returns false on timeout rather than failing: a slow
/// download is not an error, and the caller may want to keep waiting.
pub fn wait_for_install(layout: &Layout, bottle: &Bottle, appid: &str, secs: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        if let Some(a) = app(layout, bottle, appid) {
            if a.installed() {
                println!("  {} is installed", a.name);
                return true;
            }
            let line = format!("  {:>6.1}%  {}", a.percent(), a.name);
            if line != last {
                println!("{line}");
                last = line;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    false
}

/// Is this app's own executable running?
///
/// Wine leaves the *Windows* path on the command line, and that path contains
/// the app's install directory — the one part of it specific to this game. Not
/// bottle-scoped for the same reason `client_running` is not, but the same
/// reasoning applies: the same game running in two bottles at once is not a
/// case worth handling.
pub fn app_running(a: &App) -> bool {
    if a.installdir.is_empty() {
        return false;
    }
    let needle = format!(r"\steamapps\common\{}\", a.installdir).to_ascii_lowercase();
    std::process::Command::new("ps")
        .args(["-Ao", "command="])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.to_ascii_lowercase().contains(&needle))
        })
        .unwrap_or(false)
}

/// Run the client's prerequisite chain for an app without starting it.
///
/// Titles with prerequisites need `-applaunch` **twice**. The first request
/// makes the client run the installers and then stop — for SCP:SL that is
/// `mono.msi`, VC++ 2022 x86 and x64, DirectX Jun2010 and .NET 4.8, plus the
/// game's own `-monosetup_first` pass, which exits by design. Nothing is left
/// running and nothing says so.
pub fn prepare_app(layout: &Layout, bottle: &Bottle, appid: &str) -> Result<()> {
    ensure_running(layout, bottle)?;
    let exe = steam_exe(layout, bottle);
    let log = bottle.dir(layout).join("logs/prereq.log");
    run::exec(
        layout,
        bottle,
        &exe,
        &["-applaunch".to_string(), appid.to_string()],
        true,
        Some(&log),
    )?;
    Ok(())
}

/// Launch a game through the client, by app id.
///
/// `-applaunch` is what the client's own library buttons use: it sets up the
/// app environment and ownership ticket before starting the executable, which
/// is why running the .exe directly does not work. SCP:SL dies outright that
/// way — "SCPSL.exe has encountered a serious problem" — even with
/// `SteamAppId` set and the client live.
pub fn run_app(layout: &Layout, bottle: &Bottle, appid: &str, args: &[String]) -> Result<()> {
    let a = match app(layout, bottle, appid) {
        Some(a) if a.installed() => a,
        Some(a) => bail!(
            "app {appid} ({}) is only {:.1}% downloaded in bottle '{}'",
            a.name,
            a.percent(),
            bottle.name
        ),
        None => bail!(
            "app {appid} is not installed in bottle '{}' — run:\n  exe-metal steam install-app {} {appid}",
            bottle.name,
            bottle.name
        ),
    };
    if !logged_in(layout, bottle) {
        bail!(
            "nobody is signed in to the client in bottle '{}'; the game would start\n\
             unauthenticated and be kicked by any online server.",
            bottle.name
        );
    }
    ensure_running(layout, bottle)?;

    println!("launching {} ({appid})", a.name);
    let mut argv = vec!["-applaunch".to_string(), appid.to_string()];
    argv.extend_from_slice(args);
    let exe = steam_exe(layout, bottle);
    let log = bottle.dir(layout).join("logs/game.log");
    run::exec(layout, bottle, &exe, &argv, true, Some(&log))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while std::time::Instant::now() < deadline {
        if app_running(&a) {
            println!("{} is running", a.name);
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    eprintln!(
        "{} did not start. Its own log is the place to look:\n  {}",
        a.name,
        player_log_hint(layout, bottle)
    );
    Ok(())
}

/// Where a Unity title keeps the log that actually explains a failed start.
fn player_log_hint(layout: &Layout, bottle: &Bottle) -> String {
    bottle
        .prefix(layout)
        .join("drive_c/users/*/AppData/LocalLow/*/*/Player.log")
        .display()
        .to_string()
}
