use crate::bottle::Bottle;
use crate::env;
use crate::paths::Layout;
use anyhow::{bail, Context, Result};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Initialise or update a prefix.
pub fn wineboot(layout: &Layout, bottle: &Bottle) -> Result<bool> {
    let wine = bottle.wine(layout)?;
    let vars = env::build(layout, bottle);
    let status = Command::new(&wine)
        .arg("wineboot")
        .arg("-u")
        .envs(&vars)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("running wineboot")?;
    Ok(status.success())
}

/// Launch a Windows executable inside a bottle.
///
/// The working directory is set to the executable's own directory — most games
/// resolve their data relative to it and simply exit otherwise.
pub fn exec(
    layout: &Layout,
    bottle: &Bottle,
    exe: &Path,
    args: &[String],
    detach: bool,
    log: Option<&Path>,
) -> Result<i32> {
    let wine = bottle.wine(layout)?;
    let vars = env::build(layout, bottle);

    let exe_abs = resolve_exe(layout, bottle, exe)?;
    let workdir = exe_abs
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| bottle.prefix(layout));

    let mut cmd = Command::new(&wine);
    cmd.arg(&exe_abs).args(args).envs(&vars).current_dir(&workdir);

    if let Some(logpath) = log {
        if let Some(parent) = logpath.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let f = std::fs::File::create(logpath)?;
        let f2 = f.try_clone()?;
        cmd.stdout(Stdio::from(f)).stderr(Stdio::from(f2));
    }

    if detach {
        // A game must outlive the shell that started it. Without a new session
        // the child stays in the caller's process group, so closing the
        // terminal — or any group-wide signal — takes the game down with it.
        cmd.stdin(Stdio::null());
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    // Already a group leader is fine; anything else is not.
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::EPERM) {
                        return Err(err);
                    }
                }
                Ok(())
            });
        }
        let child = cmd.spawn().context("launching executable")?;
        println!("launched pid {} (detached)", child.id());
        if let Some(l) = log {
            println!("log: {}", l.display());
        }
        return Ok(0);
    }

    let status = cmd.status().context("launching executable")?;
    Ok(status.code().unwrap_or(-1))
}

/// Accept a host path, a path inside the prefix's drive_c, or a bare
/// `C:\...` style Windows path.
fn resolve_exe(layout: &Layout, bottle: &Bottle, exe: &Path) -> Result<PathBuf> {
    let s = exe.to_string_lossy();

    if s.len() > 2 && s.as_bytes()[1] == b':' {
        // C:\Foo\bar.exe -> <prefix>/drive_c/Foo/bar.exe
        let rest = s[3..].replace('\\', "/");
        let p = bottle.prefix(layout).join("drive_c").join(rest);
        if p.exists() {
            return Ok(p);
        }
        bail!("no such executable in bottle: {s}");
    }

    if exe.is_absolute() && exe.exists() {
        return Ok(exe.to_path_buf());
    }

    // Relative to drive_c, which is how most things inside a bottle are named.
    let in_c = bottle.prefix(layout).join("drive_c").join(exe);
    if in_c.exists() {
        return Ok(in_c);
    }

    let cwd_rel = std::env::current_dir()?.join(exe);
    if cwd_rel.exists() {
        return Ok(cwd_rel);
    }

    bail!("could not find executable: {}", exe.display())
}

/// Launch through a generated `.app` bundle, so macOS treats the program as a
/// real foreground application.
///
/// This is not cosmetic, and skipping it costs you every input the game should
/// have received. Executing the `wine` binary directly leaves the resulting
/// process with activation policy **Prohibited**: it draws windows perfectly
/// well, but it can never become the active application. Clicking its window
/// does not focus it, it accepts no keyboard or mouse input, and it exposes
/// nothing to the accessibility APIs — which reads as "the game is frozen".
///
/// Measured on the same executable, from `NSRunningApplication`:
///
/// ```text
/// wine <exe>          policy=Prohibited(background-only)   active=False
/// open Game.app       policy=Regular(foreground)           active=True
/// ```
///
/// macOS only hands out a regular activation policy to something started
/// through LaunchServices with a bundle behind it, so exe-metal builds one. The
/// bundle is a throwaway: regenerated on every launch and named for the game
/// only so it reads sensibly in the Dock and the app switcher.
pub fn exec_bundled(
    layout: &Layout,
    bottle: &Bottle,
    exe: &Path,
    args: &[String],
    label: &str,
    extra_env: &[(String, String)],
) -> Result<PathBuf> {
    let wine = bottle.wine(layout)?;
    let exe_abs = resolve_exe(layout, bottle, exe)?;
    let workdir = exe_abs
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| bottle.prefix(layout));

    // Keep the name filesystem-safe; it becomes the bundle directory and the
    // name macOS shows for the running application.
    let safe: String = label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let app = bottle.dir(layout).join("apps").join(format!("{safe}.app"));
    let macos = app.join("Contents/MacOS");
    if app.exists() {
        std::fs::remove_dir_all(&app)?;
    }
    std::fs::create_dir_all(&macos)?;

    std::fs::write(
        app.join("Contents/Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>launch</string>
  <key>CFBundleIdentifier</key><string>org.exe-metal.{}.{safe}</string>
  <key>CFBundleName</key><string>{label}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#,
            bottle.name
        ),
    )?;

    let mut script = String::from("#!/bin/bash\n");
    let mut vars = env::build(layout, bottle);
    for (k, v) in extra_env {
        vars.insert(k.clone(), v.clone());
    }
    for (k, v) in &vars {
        script.push_str(&format!("export {k}={}\n", sh_quote(v)));
    }
    script.push_str(&format!("cd {}\n", sh_quote(&workdir.to_string_lossy())));
    let logpath = bottle.dir(layout).join("logs/game.log");
    script.push_str(&format!(
        "exec {} {} {} >{} 2>&1\n",
        sh_quote(&wine.to_string_lossy()),
        sh_quote(&exe_abs.to_string_lossy()),
        args.iter()
            .map(|a| sh_quote(a))
            .collect::<Vec<_>>()
            .join(" "),
        sh_quote(&logpath.to_string_lossy()),
    ));

    let launcher = macos.join("launch");
    std::fs::write(&launcher, script)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    std::fs::create_dir_all(logpath.parent().unwrap())?;

    let status = Command::new("open")
        .arg(&app)
        .status()
        .context("open(1) failed to launch the bundle")?;
    if !status.success() {
        bail!("could not launch {}", app.display());
    }
    Ok(app)
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Point the prefix's `explorer` at a virtual desktop, or turn it off.
///
/// Written to the registry rather than passed on a command line because the
/// process that matters is not one we launch: Steam starts the game itself, so
/// `wine explorer /desktop=...` around our own invocation would never reach it.
/// The registry setting applies to everything in the prefix, which is what a
/// game launched by a client needs.
pub fn set_virtual_desktop(layout: &Layout, bottle: &Bottle) -> Result<()> {
    let wine = bottle.wine(layout)?;
    let vars = env::build(layout, bottle);

    let reg = |args: &[&str]| -> Result<()> {
        Command::new(&wine)
            .arg("reg")
            .args(args)
            .envs(&vars)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("running wine reg")?;
        Ok(())
    };

    match bottle.virtual_desktop.as_deref() {
        Some(size) => {
            reg(&[
                "add",
                r"HKCU\Software\Wine\Explorer\Desktops",
                "/v",
                "Default",
                "/t",
                "REG_SZ",
                "/d",
                size,
                "/f",
            ])?;
            reg(&[
                "add",
                r"HKCU\Software\Wine\Explorer",
                "/v",
                "Desktop",
                "/t",
                "REG_SZ",
                "/d",
                "Default",
                "/f",
            ])?;
            println!("virtual desktop set to {size} for bottle '{}'", bottle.name);
        }
        None => {
            reg(&["delete", r"HKCU\Software\Wine\Explorer", "/v", "Desktop", "/f"])?;
            println!("virtual desktop disabled for bottle '{}'", bottle.name);
        }
    }
    Ok(())
}

/// The directory Wine keys to a prefix, named from the prefix's device and
/// inode. Its `lock` file is held open by that prefix's `wineserver` for as
/// long as anything in the prefix is alive.
pub fn server_dir(layout: &Layout, bottle: &Bottle) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(bottle.prefix(layout)).ok()?;
    let uid = unsafe { libc::getuid() };
    Some(PathBuf::from(format!(
        "/tmp/.wine-{uid}/server-{:x}-{:x}",
        md.dev(),
        md.ino()
    )))
}

/// Is anything at all alive in this bottle?
///
/// Matching processes by the prefix path does not work, and the failure is
/// silent. Wine re-execs into the Windows image, after which `ps` shows
/// `C:\Program Files (x86)\Steam\steam.exe` and the host path is gone from the
/// command line entirely — the same blind spot `prune` documents. A `pgrep` for
/// the prefix therefore matches only during the instant before the exec, so a
/// perfectly healthy client reads as dead.
///
/// The wineserver's lock file is the one handle that stays bound to a specific
/// prefix, because Wine names its server directory from that prefix's device
/// and inode.
pub fn alive(layout: &Layout, bottle: &Bottle) -> bool {
    let Some(dir) = server_dir(layout, bottle) else {
        return false;
    };
    let lock = dir.join("lock");
    if !lock.exists() {
        return false;
    }
    // The directory outlives an unclean shutdown; only an open file descriptor
    // proves a server is still there.
    Command::new("lsof")
        .arg("-t")
        .arg(&lock)
        .stderr(Stdio::null())
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Stop everything running in one bottle, and nothing else.
///
/// Scoped to the bottle's own prefix path so other bottles — and any unrelated
/// Wine on the machine — are untouched.
pub fn kill(layout: &Layout, bottle: &Bottle) -> Result<()> {
    let wine = bottle.wine(layout)?;
    let bin = wine.parent().unwrap();
    let wineserver = bin.join("wineserver");
    let vars = env::build(layout, bottle);

    if wineserver.is_file() {
        let _ = Command::new(&wineserver)
            .arg("-k")
            .envs(&vars)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        println!("stopped bottle '{}'", bottle.name);
        return Ok(());
    }
    bail!("no wineserver found for bottle '{}'", bottle.name);
}

/// Open Wine's own configuration UI for a bottle.
pub fn winecfg(layout: &Layout, bottle: &Bottle) -> Result<()> {
    let wine = bottle.wine(layout)?;
    let vars = env::build(layout, bottle);
    Command::new(&wine)
        .arg("winecfg")
        .envs(&vars)
        .status()
        .context("running winecfg")?;
    Ok(())
}
