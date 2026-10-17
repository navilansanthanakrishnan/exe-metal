//! Bottle lifecycle: uninstalling a program, rebooting a prefix, and moving a
//! whole bottle around — clone, archive, restore, measure.
//!
//! This is the CrossOver `cxuninstall` / `cxreboot` / `cxbottle --copy --tar
//! --restore` surface, for both backends at once.
//!
//! Two things here are not obvious and cost real time to learn:
//!
//! * **Cloning is free, and copying is not.** `cp -Rc` asks APFS for
//!   copy-on-write clones, so duplicating a 75 GB game bottle takes a fraction
//!   of a second and adds no disk at all until one of the copies is written to.
//!   A plain `cp -R` of the same bottle takes minutes and costs another 75 GB.
//!   Everything in this module that duplicates a tree goes through `cp -Rc`
//!   first and only falls back — loudly — when the filesystem refuses.
//!
//! * **Nothing here may follow a symlink.** Every prefix has
//!   `dosdevices/z: -> /`, so one dereferenced link turns "archive a bottle"
//!   into "archive the entire machine". `cp -R`, `tar` and `du` all preserve
//!   links by default; that default is load-bearing, so never add `-L`, `-H`
//!   or tar's `-h` to any command below.
//!
//! Deleting a user's game is never implicit. [`uninstall`] runs the program's
//! own uninstaller or reports that there is none; throwing the files away is
//! [`remove_files`], a separate call the caller has to make on purpose.

// Nothing inside the crate calls these yet — the CLI wires them in. Without
// this the whole module reads as dead code the moment it is declared.
#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::bottle::Bottle;
use crate::crossover;
use crate::library::{Backend, Library};
use crate::paths::Layout;
use crate::scan;

// --- locating a bottle, whichever backend owns it ------------------------

/// A bottle on disk, backend-independent.
///
/// `prefix` is the directory that *contains* `drive_c`. That is the one piece
/// of layout the two backends disagree on: exe-metal nests the prefix under
/// `pfx/`, CrossOver puts `drive_c` straight in the bottle directory. Resolve
/// it once here and the rest of the module never has to care.
#[derive(Debug, Clone)]
pub struct BottleRef {
    pub backend: Backend,
    pub name: String,
    /// The directory a clone or an archive covers.
    pub dir: PathBuf,
    /// The directory holding `drive_c`.
    pub prefix: PathBuf,
}

impl BottleRef {
    pub fn drive_c(&self) -> PathBuf {
        self.prefix.join("drive_c")
    }
}

pub fn locate(layout: &Layout, backend: Backend, name: &str) -> Result<BottleRef> {
    match backend {
        Backend::ExeMetal => {
            let dir = layout.bottle(name);
            if !dir.is_dir() {
                bail!("no bottle '{name}' at {}", dir.display());
            }
            Ok(BottleRef {
                backend,
                name: name.to_string(),
                prefix: dir.join("pfx"),
                dir,
            })
        }
        Backend::CrossOver => {
            // `crossover::prefix` is also the existence check: it fails unless
            // drive_c is really there.
            let dir = crossover::prefix(name)?;
            Ok(BottleRef {
                backend,
                name: name.to_string(),
                prefix: dir.clone(),
                dir,
            })
        }
    }
}

/// Where a new bottle of this backend would go. Used by `restore`, which has
/// to name a directory that does not exist yet.
fn bottles_root(layout: &Layout, backend: Backend) -> Result<PathBuf> {
    match backend {
        Backend::ExeMetal => Ok(layout.bottles()),
        Backend::CrossOver => crossover::bottles_root(),
    }
}

// --- size ----------------------------------------------------------------

/// How much disk a bottle occupies, in bytes.
///
/// `du` counts the blocks each file actually owns, so two APFS clones each
/// report the full size while the volume only holds one copy. That is the
/// honest per-bottle number, but it means the sum over bottles will exceed
/// what `df` says is used — say so wherever this is displayed rather than
/// letting someone conclude the disk is lying.
pub fn disk_usage(layout: &Layout, backend: Backend, name: &str) -> Result<u64> {
    let b = locate(layout, backend, name)?;
    dir_bytes(&b.dir)
}

fn dir_bytes(dir: &Path) -> Result<u64> {
    // -k for a stable unit: BLOCKSIZE in the environment silently rescales
    // bare `du` output, which turns a size report into fiction.
    let out = Command::new("du")
        .arg("-sk")
        .arg(dir)
        .stderr(Stdio::null())
        .output()
        .context("running du")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let kb: u64 = text
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .with_context(|| format!("du printed nothing usable for {}", dir.display()))?;
    Ok(kb * 1024)
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

// --- reboot --------------------------------------------------------------

/// `wineboot -r` in a bottle: restart the Windows side without touching the
/// files.
///
/// This is the fix for a prefix whose services have wedged — the state that
/// makes an installer hang at 0% or a launcher sit forever on "starting". It
/// is not a repair: nothing is reinstalled and no configuration changes.
pub fn reboot(layout: &Layout, backend: Backend, name: &str) -> Result<()> {
    match backend {
        Backend::ExeMetal => {
            let b = Bottle::load(layout, name)?;
            let wine = b.wine(layout)?;
            let vars = crate::env::build(layout, &b);
            let status = Command::new(&wine)
                .arg("wineboot")
                .arg("-r")
                .envs(&vars)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("running wineboot -r")?;
            if !status.success() {
                bail!("wineboot -r failed in bottle '{name}'");
            }
            Ok(())
        }
        Backend::CrossOver => {
            let b = locate(layout, backend, name)?;
            // cxstart wants a path inside the bottle, so point at the prefix's
            // own wineboot rather than the host's.
            let exe = b.drive_c().join("windows/system32/wineboot.exe");
            if !exe.is_file() {
                bail!(
                    "bottle '{name}' has no wineboot at {} — is it a complete prefix?",
                    exe.display()
                );
            }
            let pid = crossover::run(name, &exe, &["-r".to_string()], &[], None)?;
            // cxstart returns immediately, so without this the caller reports
            // "rebooted" while the prefix is still coming up and the very next
            // command races it.
            await_pid(pid, Duration::from_secs(60));
            Ok(())
        }
    }
}

/// Wait for a child we spawned but did not keep a handle on.
///
/// Returns its exit code, or `None` on timeout or if it was already reaped.
/// `kill(pid, 0)` is not usable here: a zombie still answers it, so polling
/// that way never terminates.
fn await_pid(pid: u32, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut status: libc::c_int = 0;
        let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if r == pid as libc::pid_t {
            return if libc::WIFEXITED(status) {
                Some(libc::WEXITSTATUS(status))
            } else {
                Some(-1)
            };
        }
        if r < 0 {
            return None;
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// --- clone ---------------------------------------------------------------

#[derive(Debug)]
pub struct Cloned {
    pub bottle: BottleRef,
    pub took: Duration,
    /// False when the filesystem refused copy-on-write and a full byte copy
    /// was made instead — the clone is correct, it just cost real disk.
    pub cow: bool,
    /// Top-level entries under `drive_c`, compared against the source.
    pub entries: usize,
}

/// Duplicate a bottle, copy-on-write.
///
/// The whole point of this command. `cp -Rc` hands the work to APFS, which
/// shares the existing blocks instead of reading and rewriting them: a 75 GB
/// game bottle duplicates in a fraction of a second and adds nothing to disk
/// until one copy diverges. That makes "try this renderer on a copy" a
/// decision worth taking, where a byte copy makes it an overnight job.
pub fn clone(layout: &Layout, backend: Backend, src: &str, dest: &str) -> Result<Cloned> {
    let from = locate(layout, backend, src)?;
    let root = bottles_root(layout, backend)?;
    let to = root.join(dest);
    if dest.is_empty() || dest.contains('/') {
        bail!("'{dest}' is not a bottle name");
    }
    if to.exists() {
        bail!(
            "bottle '{dest}' already exists at {} — pick another name",
            to.display()
        );
    }
    std::fs::create_dir_all(&root)?;

    let started = Instant::now();
    let cow = copy_tree(&from.dir, &to)?;
    let took = started.elapsed();

    let cloned = locate(layout, backend, dest)?;

    // Verify before claiming success. A clone that lost its drive_c looks
    // exactly like a working bottle until the first launch fails deep inside
    // Wine, where the cause is invisible.
    let want = top_level(&from.drive_c());
    let got = top_level(&cloned.drive_c());
    if got.is_empty() || got != want {
        let _ = std::fs::remove_dir_all(&to);
        bail!(
            "clone of '{src}' did not come out intact ({} entries under drive_c, expected {}) — \
             removed the partial copy at {}",
            got.len(),
            want.len(),
            to.display()
        );
    }

    match backend {
        Backend::ExeMetal => {
            // The copied `exe-metal.toml` still carries the old name, and
            // `Bottle::dir` is derived from that field — leave it and every
            // path the clone computes points back at the bottle it came from,
            // so a launch from the clone writes into the original.
            let mut b = Bottle::load(layout, dest)
                .with_context(|| format!("clone at {} has no exe-metal.toml", to.display()))?;
            b.name = dest.to_string();
            b.save(layout)?;

            // Warm shader caches live outside the prefix. Carrying them over
            // is what stops the clone stuttering through its first hour.
            let sc_from = layout.shader_cache(src);
            let sc_to = layout.shader_cache(dest);
            if sc_from.is_dir() && !sc_to.exists() {
                let _ = copy_tree(&sc_from, &sc_to);
            }
        }
        Backend::CrossOver => {
            // BottleID is CrossOver's tag for menu entries and file
            // associations. Two bottles sharing one makes the clone's menu
            // items collide with the original's.
            refresh_bottle_id(&to.join("cxbottle.conf"));
        }
    }

    Ok(Cloned {
        bottle: cloned,
        took,
        cow,
        entries: got.len(),
    })
}

/// Copy a tree, preferring APFS clones. Returns whether copy-on-write was used.
///
/// `-R` without `-L`/`-H` copies symlinks as symlinks, which is mandatory: the
/// prefix's `z:` points at `/`.
fn copy_tree(src: &Path, dest: &Path) -> Result<bool> {
    let cow = Command::new("cp")
        .args(["-Rc"])
        .arg(src)
        .arg(dest)
        .status()
        .context("spawning cp")?;
    if cow.success() {
        return Ok(true);
    }
    // Non-APFS or cross-volume. A failed clone can leave a partial tree
    // behind, and `cp` onto an existing directory would merge into it rather
    // than replace it, so clear it first.
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    eprintln!(
        "  warn    {} does not support copy-on-write — falling back to a full \
         copy, which takes the time and the disk space a clone would not",
        dest.parent().unwrap_or(dest).display()
    );
    let plain = Command::new("cp")
        .arg("-R")
        .arg(src)
        .arg(dest)
        .status()
        .context("spawning cp")?;
    if !plain.success() {
        bail!("copying {} to {} failed", src.display(), dest.display());
    }
    Ok(false)
}

/// Names directly under a directory, sorted. Cheap enough to run on a 75 GB
/// bottle because it never descends.
fn top_level(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn refresh_bottle_id(conf: &Path) {
    let Ok(text) = std::fs::read_to_string(conf) else {
        return;
    };
    let Ok(out) = Command::new("/usr/bin/uuidgen").output() else {
        return;
    };
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if id.is_empty() {
        return;
    }
    let patched: String = text
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("\"BottleID\"") {
                format!("\"BottleID\" = \"{id}\"")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(conf, patched + "\n");
}

// --- archive and restore -------------------------------------------------

#[derive(Debug)]
pub struct Archived {
    pub file: PathBuf,
    pub bytes: u64,
    pub took: Duration,
}

/// Write a bottle to a `.tar.gz`.
///
/// Streamed: `tar` is given the output path and writes it itself, so a 75 GB
/// bottle never passes through this process's memory. Unlike [`clone`] this
/// really does read and compress every byte — it is for getting a bottle off
/// the machine, not for making a local copy.
pub fn archive(layout: &Layout, backend: Backend, name: &str, dest: &Path) -> Result<Archived> {
    let b = locate(layout, backend, name)?;
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let parent = b
        .dir
        .parent()
        .context("bottle directory has no parent")?
        .to_path_buf();

    let started = Instant::now();
    let (tar, mac_metadata) = tar_bin();
    let mut cmd = Command::new(tar);
    if mac_metadata {
        // Otherwise every file with an xattr — and a Wine prefix is full of
        // them — gains an `._name` AppleDouble twin in the archive, which
        // restores as junk beside the real file.
        cmd.arg("--no-mac-metadata");
    }
    // No `-h`. Dereferencing links here would follow `dosdevices/z: -> /` and
    // try to archive the whole disk into the bottle's tarball.
    cmd.arg("-czf")
        .arg(dest)
        .arg("-C")
        .arg(&parent)
        .arg(&b.name);
    let status = cmd.status().context("running tar")?;
    if !status.success() {
        let _ = std::fs::remove_file(dest);
        bail!("tar failed while archiving bottle '{name}'");
    }
    let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    Ok(Archived {
        file: dest.to_path_buf(),
        bytes,
        took: started.elapsed(),
    })
}

/// Restore a bottle from an archive written by [`archive`].
///
/// `as_name` renames on the way in. Extraction goes to a staging directory
/// beside the bottles and is moved into place only once `drive_c` is confirmed
/// present, so a truncated download cannot half-overwrite a live bottle.
pub fn restore(
    layout: &Layout,
    backend: Backend,
    file: &Path,
    as_name: Option<&str>,
    force: bool,
) -> Result<BottleRef> {
    if !file.is_file() {
        bail!("no archive at {}", file.display());
    }
    let root = bottles_root(layout, backend)?;
    std::fs::create_dir_all(&root)?;

    let staging = root.join(format!(".restore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    let (tar, mac_metadata) = tar_bin();
    let mut cmd = Command::new(tar);
    if mac_metadata {
        cmd.arg("--no-mac-metadata");
    }
    let status = cmd
        .arg("-xzf")
        .arg(file)
        .arg("-C")
        .arg(&staging)
        .status()
        .context("running tar")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("tar failed while restoring {}", file.display());
    }

    let extracted = top_level(&staging);
    let Some(inner) = extracted.first().map(|n| staging.join(n)) else {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("{} contained nothing", file.display());
    };
    let has_drive_c = inner.join("drive_c").is_dir() || inner.join("pfx/drive_c").is_dir();
    if extracted.len() != 1 || !has_drive_c {
        let _ = std::fs::remove_dir_all(&staging);
        bail!(
            "{} is not a bottle archive — expected one directory containing drive_c",
            file.display()
        );
    }

    let name = as_name
        .map(|s| s.to_string())
        .unwrap_or_else(|| extracted[0].clone());
    if name.is_empty() || name.contains('/') {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("'{name}' is not a bottle name");
    }
    let target = root.join(&name);
    if target.exists() {
        if !force {
            let _ = std::fs::remove_dir_all(&staging);
            bail!(
                "bottle '{name}' already exists at {} — restore under another name, \
                 or pass force to replace it",
                target.display()
            );
        }
        std::fs::remove_dir_all(&target)?;
    }
    std::fs::rename(&inner, &target)?;
    let _ = std::fs::remove_dir_all(&staging);

    if backend == Backend::ExeMetal {
        // Same trap as a clone: the config names the bottle it was archived
        // from, and every path the bottle computes comes from that name.
        if let Ok(mut b) = Bottle::load(layout, &name) {
            if b.name != name {
                b.name = name.clone();
                b.save(layout)?;
            }
        }
    }
    locate(layout, backend, &name)
}

/// macOS ships bsdtar as `/usr/bin/tar`, which understands
/// `--no-mac-metadata`; a Homebrew GNU tar earlier in PATH does not and would
/// abort on the flag. Prefer the system one, and only pass the flag to it.
fn tar_bin() -> (&'static str, bool) {
    if Path::new("/usr/bin/tar").is_file() {
        ("/usr/bin/tar", true)
    } else {
        ("tar", false)
    }
}

// --- uninstalling a program ----------------------------------------------

/// How an uninstaller was found — worth showing, because the two have very
/// different confidence levels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An `unins000.exe`-style binary sitting in the install root.
    File,
    /// A `CurrentVersion\Uninstall` registry entry, with its display name.
    Registry(String),
}

#[derive(Debug, Clone)]
pub struct Uninstaller {
    /// Host path to the executable to run.
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub source: Source,
}

#[derive(Debug)]
pub enum Outcome {
    /// The program's own uninstaller ran. Its exit code is its own.
    Ran {
        uninstaller: Uninstaller,
        status: i32,
        /// Library entries dropped because their executable is now gone.
        dropped: usize,
    },
    /// Nothing to run. The caller may follow up with [`remove_files`] — but
    /// that is a deletion, and it must be the user's decision, not a fallback
    /// this function takes on its own.
    NoUninstaller { install_root: PathBuf, bytes: u64 },
}

/// Uninstall a registered title by running the program's own uninstaller.
///
/// Quit the title first (`flow::quit_title`) — an uninstaller cannot delete
/// files that a running game still has open, and it will report success having
/// left most of them behind.
pub fn uninstall(layout: &Layout, id: &str) -> Result<Outcome> {
    let lib = Library::load()?;
    let title = lib
        .get(id)
        .cloned()
        .with_context(|| format!("no title '{id}' in the library"))?;
    let b = locate(layout, title.backend, &title.bottle)?;
    let root = install_root(&b.prefix, &title.exe);

    let Some(u) = find_uninstaller(&b.prefix, &root) else {
        return Ok(Outcome::NoUninstaller {
            bytes: dir_bytes(&root).unwrap_or(0),
            install_root: root,
        });
    };

    let status = match title.backend {
        Backend::CrossOver => {
            let log = b.prefix.join("exe-metal-uninstall.log");
            let pid = crossover::run(&title.bottle, &u.exe, &u.args, &[], Some(&log))?;
            // An uninstaller is interactive and can take minutes; the point of
            // waiting is that the library prune below sees the finished state
            // rather than a directory mid-deletion.
            await_pid(pid, Duration::from_secs(60 * 30)).unwrap_or(0)
        }
        Backend::ExeMetal => {
            let bottle = Bottle::load(layout, &title.bottle)?;
            let logs = layout.bottle(&bottle.name).join("logs");
            let _ = std::fs::create_dir_all(&logs);
            crate::run::exec(
                layout,
                &bottle,
                &u.exe,
                &u.args,
                false,
                Some(&logs.join("uninstall.log")),
            )?
        }
    };

    // Reconcile: the index is a cache, and whatever the uninstaller removed
    // has to stop showing up as a tile.
    let mut lib = Library::load()?;
    let dropped = lib.prune_missing();
    lib.save()?;

    Ok(Outcome::Ran {
        uninstaller: u,
        status,
        dropped,
    })
}

#[derive(Debug)]
pub struct Removed {
    pub install_root: PathBuf,
    pub bytes: u64,
    /// Library entries that pointed into the deleted directory.
    pub titles_dropped: usize,
}

/// Delete a title's install directory outright.
///
/// The blunt instrument, for programs that shipped no uninstaller. It is a
/// separate call from [`uninstall`] on purpose: this is unrecoverable, and it
/// must never happen as a silent fallback from "the uninstaller was missing".
/// The caller is expected to have shown the user the path and the size first.
pub fn remove_files(layout: &Layout, id: &str) -> Result<Removed> {
    let lib = Library::load()?;
    let title = lib
        .get(id)
        .cloned()
        .with_context(|| format!("no title '{id}' in the library"))?;
    let b = locate(layout, title.backend, &title.bottle)?;
    let root = install_root(&b.prefix, &title.exe);
    check_removable(&b.drive_c(), &root)?;

    let bytes = dir_bytes(&root).unwrap_or(0);
    std::fs::remove_dir_all(&root).with_context(|| format!("removing {}", root.display()))?;

    let mut lib = Library::load()?;
    let before = lib.titles.len();
    lib.titles.retain(|t| !t.exe.starts_with(&root));
    let titles_dropped = before - lib.titles.len();
    lib.save()?;

    Ok(Removed {
        install_root: root,
        bytes,
        titles_dropped,
    })
}

/// Refuse to delete anything that is not unmistakably one program's folder.
///
/// A bad `install_root` here is the difference between removing a game and
/// removing `drive_c/Program Files` or, through a symlinked `z:`, something
/// outside the bottle entirely. Both paths are resolved before comparing so a
/// `..` or a link cannot smuggle the target out of the prefix.
fn check_removable(drive_c: &Path, root: &Path) -> Result<()> {
    let real_c = drive_c
        .canonicalize()
        .with_context(|| format!("no drive_c at {}", drive_c.display()))?;
    let real_root = root
        .canonicalize()
        .with_context(|| format!("no directory at {}", root.display()))?;
    let rel = real_root
        .strip_prefix(&real_c)
        .map_err(|_| anyhow::anyhow!("{} is not inside {}", root.display(), drive_c.display()))?;

    let comps: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    if comps.len() < 2 {
        bail!(
            "{} is a top-level directory of the prefix, not one program's folder",
            root.display()
        );
    }
    if comps[0] == "windows" || comps[0] == "users" {
        bail!(
            "{} is part of Windows itself, not a program",
            root.display()
        );
    }
    Ok(())
}

/// The directory holding one program, given one of its executables.
///
/// Longest match wins: a Steam game is under both
/// `Program Files (x86)/Steam` and `.../steamapps/common/Celeste`, and only
/// the second one is the game. Taking the shorter would offer to uninstall
/// every Steam title at once.
pub fn install_root(prefix: &Path, exe: &Path) -> PathBuf {
    let mut best: Option<PathBuf> = None;
    for r in scan::roots(prefix) {
        if !exe.starts_with(&r) {
            continue;
        }
        let better = best
            .as_ref()
            .map(|b| r.as_os_str().len() > b.as_os_str().len())
            .unwrap_or(true);
        if better {
            best = Some(r);
        }
    }
    best.unwrap_or_else(|| {
        exe.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| prefix.to_path_buf())
    })
}

/// Find something that can uninstall the program in `install_root`.
///
/// Files first: an `unins000.exe` beside the program is unambiguous, needs no
/// parsing, and is what Inno Setup and NSIS both leave behind. The registry is
/// the fallback, and it is only trusted when the entry can be tied back to
/// *this* install root — a `CurrentVersion\Uninstall` key names no bottle and
/// no folder by itself, so matching loosely would happily hand you Steam's
/// uninstaller when you asked to remove one Steam game.
pub fn find_uninstaller(prefix: &Path, install_root: &Path) -> Option<Uninstaller> {
    if let Some(exe) = uninstaller_file(install_root) {
        return Some(Uninstaller {
            exe,
            args: Vec::new(),
            source: Source::File,
        });
    }

    for e in registry_entries(prefix) {
        let Some((cmd, args)) = split_command(&e.uninstall) else {
            continue;
        };
        let Some(exe) = windows_to_host(prefix, &cmd) else {
            continue;
        };
        // Either the uninstaller lives in the folder we are removing, or the
        // entry says outright that it installed there. The second case is not
        // an edge: a Steam game's UninstallString is
        // `"C:\...\Steam\steam.exe" steam://uninstall/<appid>`, which lives
        // nowhere near the game, and handing that URL to Steam is the only
        // correct way to uninstall it.
        let by_path = exe.starts_with(install_root);
        let by_location = windows_to_host(prefix, &e.location)
            .map(|l| l == install_root || l.starts_with(install_root))
            .unwrap_or(false);
        if !by_path && !by_location {
            continue;
        }
        if !exe.is_file() {
            continue;
        }
        return Some(Uninstaller {
            exe,
            args,
            source: Source::Registry(e.display),
        });
    }
    None
}

/// An `unins000.exe` / `uninstall.exe` / `Uninstall*.exe` in the install root.
///
/// Two levels deep at most, and never through a symlink: `z:` is `/`, and the
/// first thing a full-disk walk finds is somebody else's uninstaller.
/// Shallowest wins, so the program's own beats one belonging to a bundled
/// runtime in a subfolder.
fn uninstaller_file(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(usize, usize, PathBuf)> = None;
    let mut level = vec![root.to_path_buf()];
    for depth in 0..=2usize {
        let mut next = Vec::new();
        for dir in &level {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for e in entries.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                let p = e.path();
                if ft.is_dir() {
                    next.push(p);
                    continue;
                }
                if !p
                    .extension()
                    .map(|x| x.eq_ignore_ascii_case("exe"))
                    .unwrap_or(false)
                {
                    continue;
                }
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !(stem.starts_with("unins") || stem.starts_with("uninstall")) {
                    continue;
                }
                let key = (depth, stem.len(), p);
                if best.as_ref().map(|b| key < *b).unwrap_or(true) {
                    best = Some(key);
                }
            }
        }
        level = next;
        if level.is_empty() {
            break;
        }
    }
    best.map(|(_, _, p)| p)
}

// --- the registry --------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct RegEntry {
    display: String,
    uninstall: String,
    /// `InstallLocation`, which is how an entry is tied to a folder.
    location: String,
}

/// Every `CurrentVersion\Uninstall` entry in a prefix that names an
/// uninstaller.
///
/// Both registry files are read: Wine keeps HKLM in `system.reg` and HKCU in
/// `user.reg`, and per-user installers only appear in the second. The 32-bit
/// view (`Wow6432Node`) counts too — most game installers are still 32-bit.
fn registry_entries(prefix: &Path) -> Vec<RegEntry> {
    let mut out = Vec::new();
    for f in ["system.reg", "user.reg"] {
        let Ok(text) = std::fs::read_to_string(prefix.join(f)) else {
            continue;
        };
        let mut inside = false;
        let mut cur = RegEntry::default();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix('[') {
                if inside && !cur.uninstall.is_empty() {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur = RegEntry::default();
                }
                let key = rest.split(']').next().unwrap_or("").replace("\\\\", "\\");
                inside = key.to_lowercase().contains(r"currentversion\uninstall\");
                continue;
            }
            if !inside {
                continue;
            }
            let Some((name, value)) = reg_value(line) else {
                continue;
            };
            match name.as_str() {
                "DisplayName" => cur.display = value,
                "UninstallString" => cur.uninstall = value,
                "InstallLocation" => cur.location = value,
                _ => {}
            }
        }
        if inside && !cur.uninstall.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// Parse one `"Name"="value"` line out of a `.reg` file.
///
/// Typed values arrive as `"UninstallString"=str(2):"MsiExec.exe /I{...}"` —
/// that `str(2):` is REG_EXPAND_SZ, and a parser that does not strip it reads
/// the type tag as the start of the path and finds nothing. `dword:` values
/// are not strings and fall out here as `None`.
fn reg_value(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix('"')?;
    let (name, rest) = take_quoted(rest)?;
    let rest = rest.strip_prefix('=')?;
    let rest = if rest.starts_with("str(") {
        &rest[rest.find(':')? + 1..]
    } else {
        rest
    };
    let rest = rest.strip_prefix('"')?;
    let (value, _) = take_quoted(rest)?;
    Some((name, value))
}

/// Read a quoted, backslash-escaped string starting just after its opening
/// quote. Returns the unescaped content and what follows the closing quote.
fn take_quoted(s: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '\\' => out.push(it.next()?.1),
            '"' => return Some((out, &s[i + 1..])),
            _ => out.push(c),
        }
    }
    None
}

/// Split an `UninstallString` into a program and its arguments.
///
/// The hard case is the unquoted one. Real values include
/// `C:\Program Files (x86)\Steam\uninstall.exe` — a path with spaces and no
/// quotes — so cutting at the first space produces `C:\Program`. Cutting after
/// `.exe` is what actually works. A value with no `.exe` at all
/// (`iexplore -unregserver`) is a command, not a program we can locate, and is
/// rejected rather than guessed at.
fn split_command(cmd: &str) -> Option<(String, Vec<String>)> {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some((rest[..end].to_string(), split_args(&rest[end + 1..])));
    }
    let end = cmd.to_lowercase().find(".exe")? + 4;
    Some((cmd[..end].to_string(), split_args(&cmd[end..])))
}

fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Resolve a Windows path from the registry to a host path inside the bottle.
///
/// Only `C:` is accepted. `Z:` is the host filesystem root — an entry pointing
/// there is an installer that ran from the Mac side, and running it as "this
/// bottle's uninstaller" would execute something outside the prefix entirely.
/// A bare `MsiExec.exe` is resolved against the prefix's own system32, because
/// every MSI-installed program has that and nothing else.
fn windows_to_host(prefix: &Path, win: &str) -> Option<PathBuf> {
    let win = win.trim();
    if win.is_empty() {
        return None;
    }
    let b = win.as_bytes();
    if b.len() > 2 && b[1] == b':' {
        if !b[0].eq_ignore_ascii_case(&b'C') {
            return None;
        }
        let rel = win[3..].trim_start_matches(['\\', '/']).replace('\\', "/");
        // Lexical, because the path may not exist yet and `canonicalize`
        // cannot check what is not there. One `..` is enough to leave the
        // bottle.
        if rel.split('/').any(|c| c == "..") {
            return None;
        }
        return Some(prefix.join("drive_c").join(rel));
    }
    if !win.contains('\\') && win.to_lowercase().ends_with(".exe") {
        let p = prefix.join("drive_c/windows/system32").join(win);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn scratch(tag: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("em-life-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path, bytes: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; bytes]).unwrap();
    }

    /// A believable exe-metal bottle: config, prefix, a game, and the `z: -> /`
    /// symlink that makes naive recursion catastrophic.
    fn bottle(layout: &Layout, name: &str) -> PathBuf {
        let dir = layout.bottle(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("exe-metal.toml"),
            format!("name = \"{name}\"\nruntime = \"sikarugir-10.0\"\nrenderer = \"d3dmetal\"\n"),
        )
        .unwrap();
        let pfx = dir.join("pfx");
        touch(&pfx.join("drive_c/Program Files/Game/Game.exe"), 2048);
        touch(&pfx.join("drive_c/windows/system32/wineboot.exe"), 16);
        std::fs::create_dir_all(pfx.join("dosdevices")).unwrap();
        std::os::unix::fs::symlink("/", pfx.join("dosdevices/z:")).unwrap();
        dir
    }

    // --- registry ---------------------------------------------------------

    #[test]
    fn reads_uninstall_entries_from_both_registry_files() {
        let pfx = scratch("reg");
        std::fs::write(
            pfx.join("system.reg"),
            concat!(
                "WINE REGISTRY Version 2\n\n",
                "[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Uninstall\\\\Steam App 504230] 17\n",
                "#time=1dd23b9c38ad444\n",
                "\"DisplayName\"=\"Celeste\"\n",
                "\"InstallLocation\"=\"C:\\\\Program Files (x86)\\\\Steam\\\\steamapps\\\\common\\\\Celeste\"\n",
                "\"NoModify\"=dword:00000001\n",
                "\"UninstallString\"=\"\\\"C:\\\\Program Files (x86)\\\\Steam\\\\steam.exe\\\" steam://uninstall/504230\"\n",
                "\n",
                "[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\URL\\\\Prefixes] 17\n",
                "\"ftp\"=\"ftp://\"\n",
            ),
        )
        .unwrap();
        std::fs::write(
            pfx.join("user.reg"),
            concat!(
                "[Software\\\\Wow6432Node\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Uninstall\\\\{GUID}] 9\n",
                "\"DisplayName\"=\"Wine Mono\"\n",
                "\"UninstallString\"=str(2):\"MsiExec.exe /I{0591009C}\"\n",
            ),
        )
        .unwrap();

        let e = registry_entries(&pfx);
        assert_eq!(
            e.len(),
            2,
            "one per Uninstall key, nothing from URL\\Prefixes"
        );
        assert_eq!(e[0].display, "Celeste");
        assert!(e[0].uninstall.starts_with("\"C:\\Program Files (x86)"));
        assert!(e[0].location.ends_with("common\\Celeste"));
        // The str(2): type tag must not survive into the value.
        assert_eq!(e[1].uninstall, "MsiExec.exe /I{0591009C}");
    }

    #[test]
    fn dword_values_are_not_mistaken_for_strings() {
        assert_eq!(reg_value("\"NoRepair\"=dword:00000001"), None);
        assert_eq!(
            reg_value("\"DisplayName\"=\"Celeste\""),
            Some(("DisplayName".into(), "Celeste".into()))
        );
    }

    #[test]
    fn unquoted_paths_with_spaces_survive_splitting() {
        // Cutting at the first space would yield `C:\Program`.
        let (exe, args) = split_command(r"C:\Program Files (x86)\Steam\uninstall.exe").unwrap();
        assert_eq!(exe, r"C:\Program Files (x86)\Steam\uninstall.exe");
        assert!(args.is_empty());

        let (exe, args) =
            split_command("\"C:\\Games\\X\\unins000.exe\" /SILENT /NORESTART").unwrap();
        assert_eq!(exe, r"C:\Games\X\unins000.exe");
        assert_eq!(args, vec!["/SILENT", "/NORESTART"]);

        // A shell command, not a program: refuse rather than guess.
        assert!(split_command("iexplore -unregserver").is_none());
    }

    #[test]
    fn only_drive_c_resolves_and_dotdot_never_escapes() {
        let pfx = scratch("host");
        let c = windows_to_host(&pfx, r"C:\Games\X\unins000.exe").unwrap();
        assert_eq!(c, pfx.join("drive_c/Games/X/unins000.exe"));
        // Z: is the host root; running from there is outside the bottle.
        assert!(windows_to_host(&pfx, r"Z:\Applications\Thing.app\thing.exe").is_none());
        assert!(windows_to_host(&pfx, r"C:\Games\..\..\..\evil.exe").is_none());
        assert!(windows_to_host(&pfx, "").is_none());
    }

    // --- finding an uninstaller ------------------------------------------

    #[test]
    fn a_file_in_the_install_root_wins() {
        let pfx = scratch("unins");
        let root = pfx.join("drive_c/Program Files/Game");
        touch(&root.join("Game.exe"), 10);
        touch(&root.join("unins000.exe"), 10);
        touch(&root.join("_CommonRedist/uninstall.exe"), 10);

        let u = find_uninstaller(&pfx, &root).unwrap();
        assert_eq!(u.source, Source::File);
        assert_eq!(u.exe, root.join("unins000.exe"), "shallowest wins");
    }

    #[test]
    fn a_registry_entry_is_used_when_it_names_this_install_root() {
        let pfx = scratch("regmatch");
        let root = pfx.join("drive_c/Program Files (x86)/Steam/steamapps/common/Celeste");
        touch(&root.join("Celeste.exe"), 10);
        let steam = pfx.join("drive_c/Program Files (x86)/Steam/steam.exe");
        touch(&steam, 10);
        std::fs::write(
            pfx.join("system.reg"),
            concat!(
                "[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Uninstall\\\\Steam App 504230] 1\n",
                "\"DisplayName\"=\"Celeste\"\n",
                "\"InstallLocation\"=\"C:\\\\Program Files (x86)\\\\Steam\\\\steamapps\\\\common\\\\Celeste\"\n",
                "\"UninstallString\"=\"\\\"C:\\\\Program Files (x86)\\\\Steam\\\\steam.exe\\\" steam://uninstall/504230\"\n",
            ),
        )
        .unwrap();

        let u = find_uninstaller(&pfx, &root).unwrap();
        assert_eq!(u.exe, steam, "the Steam client is the uninstaller here");
        assert_eq!(u.args, vec!["steam://uninstall/504230"]);
        assert_eq!(u.source, Source::Registry("Celeste".into()));
    }

    #[test]
    fn a_registry_entry_for_another_program_is_ignored() {
        // The trap this guards: Steam's own entry would otherwise be offered
        // as the uninstaller for a game that happens to live under it, and
        // running it removes every Steam title at once.
        let pfx = scratch("regwrong");
        let root = pfx.join("drive_c/Program Files (x86)/Steam/steamapps/common/Celeste");
        touch(&root.join("Celeste.exe"), 10);
        touch(
            &pfx.join("drive_c/Program Files (x86)/Steam/uninstall.exe"),
            10,
        );
        std::fs::write(
            pfx.join("system.reg"),
            concat!(
                "[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Uninstall\\\\Steam] 1\n",
                "\"DisplayName\"=\"Steam\"\n",
                "\"InstallLocation\"=\"C:\\\\Program Files (x86)\\\\Steam\"\n",
                "\"UninstallString\"=\"C:\\\\Program Files (x86)\\\\Steam\\\\uninstall.exe\"\n",
            ),
        )
        .unwrap();

        assert!(find_uninstaller(&pfx, &root).is_none());
    }

    #[test]
    fn install_root_prefers_the_steam_game_over_the_steam_client() {
        let pfx = scratch("root");
        let game = pfx.join("drive_c/Program Files (x86)/Steam/steamapps/common/Celeste");
        touch(&game.join("Celeste.exe"), 10);
        touch(&pfx.join("drive_c/Program Files (x86)/Steam/steam.exe"), 10);
        assert_eq!(install_root(&pfx, &game.join("Celeste.exe")), game);
    }

    #[test]
    fn install_root_holds_for_an_unreal_style_nested_exe() {
        let pfx = scratch("unreal");
        let root = pfx.join("drive_c/Program Files/Poppy");
        let exe = root.join("Game/Binaries/Win64/Game-Win64-Shipping.exe");
        touch(&exe, 10);
        assert_eq!(install_root(&pfx, &exe), root);
    }

    // --- deletion guard ---------------------------------------------------

    #[test]
    fn deletion_refuses_anything_that_is_not_one_programs_folder() {
        let pfx = scratch("guard");
        let c = pfx.join("drive_c");
        let game = c.join("Program Files/Game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(c.join("windows/system32")).unwrap();
        std::fs::create_dir_all(pfx.join("outside")).unwrap();

        assert!(check_removable(&c, &game).is_ok());
        assert!(check_removable(&c, &c).is_err(), "drive_c itself");
        assert!(
            check_removable(&c, &c.join("Program Files")).is_err(),
            "every program at once"
        );
        assert!(check_removable(&c, &c.join("windows/system32")).is_err());
        assert!(check_removable(&c, &pfx.join("outside")).is_err());
    }

    #[test]
    fn deletion_refuses_a_symlink_that_leads_out_of_the_prefix() {
        let pfx = scratch("guardlink");
        let c = pfx.join("drive_c");
        std::fs::create_dir_all(c.join("Program Files")).unwrap();
        let elsewhere = pfx.join("elsewhere/Game");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, c.join("Program Files/Game")).unwrap();
        // Looks like `drive_c/Program Files/Game`; is not.
        assert!(check_removable(&c, &c.join("Program Files/Game")).is_err());
    }

    // --- clone ------------------------------------------------------------

    #[test]
    fn cloning_copies_the_prefix_and_renames_the_config() {
        let root = scratch("clone");
        let layout = Layout { root };
        bottle(&layout, "sk");

        let c = clone(&layout, Backend::ExeMetal, "sk", "sk-copy").unwrap();
        let game = c.bottle.drive_c().join("Program Files/Game/Game.exe");
        assert!(game.is_file());
        assert_eq!(c.entries, 2, "Program Files and windows");

        // The config names the bottle its paths are derived from. Left at the
        // old name, a launch from the clone writes into the original.
        let b = Bottle::load(&layout, "sk-copy").unwrap();
        assert_eq!(b.name, "sk-copy");
        assert_eq!(b.runtime, "sikarugir-10.0");
    }

    #[test]
    fn cloning_does_not_follow_the_z_drive_into_the_whole_disk() {
        let root = scratch("clonelink");
        let layout = Layout { root };
        bottle(&layout, "sk");

        let c = clone(&layout, Backend::ExeMetal, "sk", "sk-copy").unwrap();
        let z = c.bottle.prefix.join("dosdevices/z:");
        let md = std::fs::symlink_metadata(&z).unwrap();
        assert!(
            md.file_type().is_symlink(),
            "z: must stay a link, not become /"
        );
        // A bottle this small clones in milliseconds; anything that walked `/`
        // would not have finished.
        assert!(c.took < Duration::from_secs(30));
    }

    #[test]
    fn cloning_onto_an_existing_bottle_is_refused() {
        let root = scratch("clonedup");
        let layout = Layout { root };
        bottle(&layout, "sk");
        bottle(&layout, "other");
        let e = clone(&layout, Backend::ExeMetal, "sk", "other").unwrap_err();
        assert!(e.to_string().contains("already exists"), "{e}");
        // The existing bottle is untouched.
        assert!(layout.bottle("other").join("pfx/drive_c").is_dir());
    }

    #[test]
    fn cloning_an_unknown_bottle_fails_before_touching_anything() {
        let root = scratch("clonemissing");
        let layout = Layout { root: root.clone() };
        assert!(clone(&layout, Backend::ExeMetal, "nope", "copy").is_err());
        assert!(!root.join("bottles/copy").exists());
    }

    // --- archive and restore ---------------------------------------------

    #[test]
    fn archive_then_restore_round_trips_under_a_new_name() {
        let root = scratch("tar");
        let layout = Layout { root: root.clone() };
        bottle(&layout, "sk");

        let tarball = root.join("out/sk.tar.gz");
        let a = archive(&layout, Backend::ExeMetal, "sk", &tarball).unwrap();
        assert!(a.bytes > 0);

        let r = restore(
            &layout,
            Backend::ExeMetal,
            &tarball,
            Some("sk-restored"),
            false,
        )
        .unwrap();
        assert!(r.drive_c().join("Program Files/Game/Game.exe").is_file());
        assert_eq!(
            Bottle::load(&layout, "sk-restored").unwrap().name,
            "sk-restored",
            "the restored config must name the bottle it now is"
        );

        // The z: link has to come back as a link. Had tar dereferenced it, the
        // archive would have been the whole filesystem.
        let md = std::fs::symlink_metadata(r.prefix.join("dosdevices/z:")).unwrap();
        assert!(md.file_type().is_symlink());
    }

    #[test]
    fn restoring_over_a_live_bottle_needs_force() {
        let root = scratch("tarforce");
        let layout = Layout { root: root.clone() };
        bottle(&layout, "sk");
        let tarball = root.join("sk.tar.gz");
        archive(&layout, Backend::ExeMetal, "sk", &tarball).unwrap();

        let e = restore(&layout, Backend::ExeMetal, &tarball, None, false).unwrap_err();
        assert!(e.to_string().contains("already exists"), "{e}");
        // The bottle that was already there survived the refusal.
        assert!(layout.bottle("sk").join("pfx/drive_c").is_dir());

        restore(&layout, Backend::ExeMetal, &tarball, None, true).unwrap();
        assert!(layout
            .bottle("sk")
            .join("pfx/drive_c/Program Files/Game/Game.exe")
            .is_file());
    }

    #[test]
    fn archiving_refuses_to_overwrite_an_existing_file() {
        let root = scratch("tarover");
        let layout = Layout { root: root.clone() };
        bottle(&layout, "sk");
        let tarball = root.join("sk.tar.gz");
        std::fs::write(&tarball, b"not a bottle").unwrap();
        assert!(archive(&layout, Backend::ExeMetal, "sk", &tarball).is_err());
        assert_eq!(std::fs::read(&tarball).unwrap(), b"not a bottle");
    }

    #[test]
    fn a_tarball_that_is_not_a_bottle_is_rejected_and_leaves_no_staging() {
        let root = scratch("tarjunk");
        let layout = Layout { root: root.clone() };
        let junk = root.join("junk");
        touch(&junk.join("hello.txt"), 4);
        let tarball = root.join("junk.tar.gz");
        let st = Command::new("/usr/bin/tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&root)
            .arg("junk")
            .status()
            .unwrap();
        assert!(st.success());

        let e = restore(&layout, Backend::ExeMetal, &tarball, None, false).unwrap_err();
        assert!(e.to_string().contains("not a bottle archive"), "{e}");
        let leftovers: Vec<String> = top_level(&layout.bottles());
        assert!(
            leftovers.is_empty(),
            "staging must be cleaned up: {leftovers:?}"
        );
    }

    // --- size -------------------------------------------------------------

    #[test]
    fn disk_usage_counts_the_whole_bottle() {
        let root = scratch("du");
        let layout = Layout { root };
        let dir = bottle(&layout, "sk");
        touch(
            &dir.join("pfx/drive_c/Program Files/Game/data.bin"),
            400_000,
        );

        let n = disk_usage(&layout, Backend::ExeMetal, "sk").unwrap();
        assert!(n >= 400_000, "got {n}");
        assert!(disk_usage(&layout, Backend::ExeMetal, "nope").is_err());
    }

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.0 KB");
        assert_eq!(human(80_530_636_800), "75.0 GB");
    }
}
