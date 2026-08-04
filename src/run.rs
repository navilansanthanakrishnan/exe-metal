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
