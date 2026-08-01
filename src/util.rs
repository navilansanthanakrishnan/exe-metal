use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// Run a command, streaming its output to the terminal, and fail if it does.
pub fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to spawn {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

/// Run a command and capture stdout, ignoring a non-zero exit.
pub fn capture(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to spawn {program}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Download to `dest`, resuming a partial file and skipping work if the file is
/// already complete. Uses curl because it ships with macOS and handles the
/// redirect chains GitHub release assets rely on.
pub fn download(url: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        println!("  cached  {}", dest.file_name().unwrap().to_string_lossy());
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = dest.with_extension("partial");
    println!("  fetch   {url}");
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--retry",
            "3",
            "--continue-at",
            "-",
            "--progress-bar",
            "--output",
        ])
        .arg(&partial)
        .arg(url)
        .status()
        .context("curl failed to start")?;
    if !status.success() {
        bail!("download failed: {url}");
    }
    std::fs::rename(&partial, dest)?;
    Ok(())
}

/// Extract a tarball into `dest`, autodetecting compression.
pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    println!(
        "  extract {}",
        archive.file_name().unwrap().to_string_lossy()
    );
    run(
        "tar",
        &[
            "-xf",
            &archive.to_string_lossy(),
            "-C",
            &dest.to_string_lossy(),
        ],
    )
}

/// Copy a directory tree using APFS clonefile when possible.
///
/// This is what makes composing engines cheap: a full Wine bundle is ~700 MB,
/// but cloning it costs a few milliseconds and no additional disk until the
/// renderer overlay rewrites a handful of DLLs.
pub fn clone_tree(src: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cow = Command::new("cp")
        .args(["-c", "-R"])
        .arg(src)
        .arg(dest)
        .status();
    match cow {
        Ok(s) if s.success() => Ok(()),
        // Non-APFS or cross-volume: fall back to a plain recursive copy.
        _ => run("cp", &["-R", &src.to_string_lossy(), &dest.to_string_lossy()]),
    }
}

/// Overlay the *contents* of `src` onto `dest`, preserving symlinks.
///
/// Renderer overlays rely on symlinks: the unix-side `d3d11.so` is a link to
/// `../../external/libd3dshared.dylib`, so dereferencing would silently produce
/// a broken engine.
pub fn overlay(src: &Path, dest: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dest)?;
    let from = format!("{}/.", src.to_string_lossy());
    run("cp", &["-R", &from, &dest.to_string_lossy()])
}

/// Remove the quarantine flag from a tree.
///
/// Downloads that arrive through a browser carry it, and Gatekeeper will refuse
/// to load a quarantined dylib into a signed process.
pub fn unquarantine(path: &Path) -> Result<()> {
    let _ = Command::new("/usr/bin/xattr")
        .args(["-drs", "com.apple.quarantine"])
        .arg(path)
        .status();
    Ok(())
}

/// Apply an ad-hoc signature to a bundle.
///
/// Only meaningful for `.app` and `.framework` directories and for bare Mach-O
/// files — codesign rejects an arbitrary directory with "bundle format
/// unrecognized". The Wine builds arrive already ad-hoc signed, so this exists
/// for the pieces we rewrite: replacing a DLL inside a signed framework
/// invalidates its signature and macOS then refuses to load it.
pub fn sign_adhoc(path: &Path) -> Result<()> {
    unquarantine(path)?;
    if !is_signable(path) {
        return Ok(());
    }
    let status = Command::new("/usr/bin/codesign")
        .args(["--force", "--deep", "-s", "-"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("codesign failed to start")?;
    if !status.success() {
        eprintln!("  warn    could not re-sign {}", path.display());
    }
    Ok(())
}

fn is_signable(path: &Path) -> bool {
    match path.extension().and_then(|s| s.to_str()) {
        Some("app") | Some("framework") => true,
        _ => path.is_file(),
    }
}

/// Sign every bundle inside a tree that needs it, rather than the tree itself.
pub fn sign_tree(root: &Path) -> Result<()> {
    unquarantine(root)?;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            // Do not follow symlinks: renderer shims point back into the tree.
            let Ok(md) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if md.file_type().is_symlink() || !md.is_dir() {
                continue;
            }
            match p.extension().and_then(|s| s.to_str()) {
                Some("app") | Some("framework") => {
                    sign_adhoc(&p)?;
                }
                _ => stack.push(p),
            }
        }
    }
    Ok(())
}

pub fn require_apple_silicon() -> Result<()> {
    let arch = capture("uname", &["-m"])?;
    if arch != "arm64" {
        bail!("exe-metal targets Apple Silicon; this machine reports {arch}");
    }
    Ok(())
}

/// Rosetta 2 is what actually executes the game's x86-64 instructions.
/// Without it nothing here runs.
pub fn rosetta_present() -> bool {
    Path::new("/Library/Apple/usr/libexec/oah/libRosettaRuntime").exists()
        || Command::new("arch")
            .args(["-x86_64", "/usr/bin/true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

pub fn dir_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn human_size(path: &Path) -> String {
    capture("du", &["-sh", &path.to_string_lossy()])
        .ok()
        .and_then(|s| s.split_whitespace().next().map(|x| x.to_string()))
        .unwrap_or_else(|| "?".into())
}

pub fn ensure_exists(p: &Path, what: &str) -> Result<()> {
    if !p.exists() {
        return Err(anyhow!("{what} not found at {}", p.display()));
    }
    Ok(())
}
