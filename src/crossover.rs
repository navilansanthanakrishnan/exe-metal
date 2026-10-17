//! CrossOver as a second backend, alongside exe-metal's own bottles.
//!
//! Why this exists: CrossOver ships proprietary Wine patches that are not in
//! the LGPL tree exe-metal builds from, and some titles only run there —
//! Overwatch is the worked example. A launcher that can only see its own
//! bottles will confidently show you a copy of a game that cannot start, while
//! the copy that *does* start sits in a bottle it refuses to look at. That is
//! worse than not listing it at all.
//!
//! So a title carries the backend it belongs to, and launching dispatches on
//! it. CrossOver is optional: none of this is required for exe-metal bottles,
//! and everything degrades to "no CrossOver bottles found" when it is absent.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where CrossOver installs, and where it keeps bottles.
const APP: &str = "/Applications/CrossOver.app";
const SUPPORT: &str = "Contents/SharedSupport/CrossOver";

pub fn app_path() -> PathBuf {
    PathBuf::from(APP)
}

pub fn is_installed() -> bool {
    cxstart().is_file()
}

fn support_bin(name: &str) -> PathBuf {
    app_path().join(SUPPORT).join("bin").join(name)
}

pub fn cxstart() -> PathBuf {
    support_bin("cxstart")
}

/// CrossOver's version string, for the doctor output.
pub fn version() -> Option<String> {
    let out = Command::new(support_bin("wine"))
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.contains("Public Version"))
        .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
}

pub fn bottles_root() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("CrossOver")
        .join("Bottles"))
}

/// Names of every CrossOver bottle, or empty when CrossOver is not installed.
pub fn list_bottles() -> Vec<String> {
    let Ok(root) = bottles_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("drive_c").is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    out
}

/// A CrossOver bottle's prefix — the directory holding `drive_c`.
///
/// CrossOver puts `drive_c` directly in the bottle directory, where exe-metal
/// nests it under `pfx/`. Everything else about the layout matches, so the
/// scanner works unchanged once it is handed the right root.
pub fn prefix(bottle: &str) -> Result<PathBuf> {
    let p = bottles_root()?.join(bottle);
    if !p.join("drive_c").is_dir() {
        anyhow::bail!("no CrossOver bottle named '{bottle}'");
    }
    Ok(p)
}

/// Turn a host path inside a bottle into the `C:\...` form cxstart expects.
///
/// cxstart takes Windows paths; handing it a POSIX path silently starts
/// nothing, which is a miserable failure to debug.
pub fn windows_path(bottle: &str, exe: &Path) -> Result<String> {
    let pfx = prefix(bottle)?;
    let drive_c = pfx.join("drive_c");
    let rel = exe
        .strip_prefix(&drive_c)
        .with_context(|| format!("{} is not inside bottle '{bottle}'", exe.display()))?;
    let win = rel.to_string_lossy().replace('/', "\\");
    Ok(format!("C:\\{win}"))
}

/// Launch an executable in a CrossOver bottle.
///
/// `extra_env` carries the renderer tuning (MetalFX, DXR) so the caller decides
/// policy rather than this module hard-coding it.
pub fn run(
    bottle: &str,
    exe: &Path,
    args: &[String],
    extra_env: &[(String, String)],
    log: Option<&Path>,
) -> Result<u32> {
    if !is_installed() {
        anyhow::bail!("CrossOver is not installed at {APP}");
    }
    let win = windows_path(bottle, exe)?;
    let mut cmd = Command::new(cxstart());
    cmd.arg("--bottle")
        .arg(bottle)
        .arg("--")
        .arg(&win)
        .args(args);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    if let Some(p) = log {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(f) = std::fs::File::create(p) {
            let f2 = f.try_clone()?;
            cmd.stdout(Stdio::from(f)).stderr(Stdio::from(f2));
        }
    } else {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    cmd.stdin(Stdio::null());
    let child = cmd.spawn().context("launching through cxstart")?;
    Ok(child.id())
}

/// Once `image` is running in the bottle, start `exe` there with `arg`.
///
/// For clients that take commands from a second copy of themselves: the
/// second process hands its arguments to the first and exits. Sent before the
/// first copy is up, it becomes that copy instead and the argument is lost,
/// so this waits. It runs detached, because a client can take a minute to
/// appear and nobody should wait on it.
pub fn run_when_running(bottle: &str, image: &str, exe: &Path, arg: &str) -> Result<()> {
    let win = windows_path(bottle, exe)?;
    // The names travel in the environment, not in the script: `pgrep -f`
    // matches whole command lines, and a script that spelled the image name
    // out would find itself.
    Command::new("/bin/sh")
        .arg("-c")
        .arg(
            r#"i=0
while [ $i -lt 120 ] && ! pgrep -f -- "$EM_IMAGE" >/dev/null; do sleep 1; i=$((i+1)); done
[ $i -lt 120 ] || exit 1
sleep 10
exec "$EM_CXSTART" --bottle "$EM_BOTTLE" -- "$EM_EXE" "$EM_ARG""#,
        )
        .env("EM_IMAGE", image.replace('.', "\\."))
        .env("EM_CXSTART", cxstart())
        .env("EM_BOTTLE", bottle)
        .env("EM_EXE", win)
        .env("EM_ARG", arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("scheduling the hand-off")?;
    Ok(())
}

/// Ask a running program to close **gracefully**.
///
/// This matters more than it looks. Many Windows games only write their
/// settings on a clean shutdown: Overwatch discards every graphics change if
/// the process is killed. `taskkill` without `/F` posts WM_CLOSE, which is a
/// real quit; SIGKILL from the host is not, and silently loses the user's
/// configuration every time.
pub fn quit(bottle: &str, image_name: &str) -> Result<()> {
    if !is_installed() {
        anyhow::bail!("CrossOver is not installed at {APP}");
    }
    let status = Command::new(cxstart())
        .arg("--bottle")
        .arg(bottle)
        .arg("--")
        .arg("taskkill.exe")
        .arg("/IM")
        .arg(image_name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("sending WM_CLOSE through taskkill")?;
    if !status.success() {
        anyhow::bail!("taskkill reported failure for '{image_name}'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_path_rejects_a_path_outside_the_bottle() {
        // No bottle by this name exists, so this must error rather than
        // fabricate a C:\ path that cxstart would silently ignore.
        let r = windows_path("definitely-not-a-bottle", Path::new("/tmp/x.exe"));
        assert!(r.is_err());
    }

    #[test]
    fn list_bottles_is_empty_and_quiet_without_crossover() {
        // Never panics, whatever is or is not installed.
        let _ = list_bottles();
        let _ = is_installed();
    }

    #[test]
    fn bottles_root_ends_where_crossover_keeps_them() {
        let r = bottles_root().unwrap();
        assert!(r.ends_with("CrossOver/Bottles"), "got {}", r.display());
    }
}
