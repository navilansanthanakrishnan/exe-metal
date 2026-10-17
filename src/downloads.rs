//! What you just downloaded, and where it should go.
//!
//! The gap between "I downloaded a game" and "it is installed" is two
//! questions nobody should have to answer by hand: *which file is the one*, and
//! *where does it go*. Both are answerable from the file itself.
//!
//! Which one: the newest `.exe` in Downloads is almost always it, and the PE
//! header says whether it is an installer (and which framework built it) or a
//! program that runs directly.
//!
//! Where: a bottle is a Wine prefix, and prefixes are not free — each one is a
//! fresh Windows with its own registry and redistributables. Reusing a bottle
//! whose runtime and renderer already match what this binary needs is the right
//! default; a separate bottle is the right answer when the plans disagree or
//! when isolation matters.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::bottle::{self, Bottle};
use crate::paths::Layout;
use crate::probe::{self, Arch, ExeInfo};

/// A `.exe` sitting in one of the folders downloads land in.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    /// Seconds since it was last modified — how "fresh" the download is.
    pub age_secs: u64,
    /// None when the file could not be parsed as a PE at all.
    pub info: Option<ExeInfo>,
}

impl Found {
    /// One line a person can act on: what this file actually is.
    pub fn describe(&self) -> String {
        match &self.info {
            None => "not a Windows executable".to_string(),
            Some(i) => {
                let bits = match i.arch {
                    Arch::X86 => "32-bit",
                    Arch::X64 => "64-bit",
                    Arch::Arm64 => "ARM64",
                    Arch::Unknown => "unknown architecture",
                };
                match (i.installer, i.highest_api()) {
                    (Some(framework), _) => format!("{framework} installer, {bits}"),
                    (None, Some(dx)) => format!("program, {bits}, {dx:?}"),
                    (None, None) => format!("program, {bits}"),
                }
            }
        }
    }

    pub fn is_installer(&self) -> bool {
        self.info.as_ref().and_then(|i| i.installer).is_some()
    }

    /// Rough "this is probably the one you want" score.
    fn score(&self) -> i64 {
        let mut s = 0i64;
        // Freshness dominates: you install what you just downloaded.
        s += match self.age_secs {
            0..=3_600 => 400,
            3_601..=86_400 => 250,
            86_401..=604_800 => 100,
            _ => 0,
        };
        if self.is_installer() {
            s += 120;
        }
        if self.info.is_none() {
            s -= 500;
        }
        // A real installer is rarely under a megabyte.
        s += ((self.size / (1024 * 1024)) as i64).min(60);
        s
    }
}

fn age_of(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX)
}

/// Folders a download realistically lands in.
pub fn search_dirs() -> Vec<PathBuf> {
    let Ok(home) = std::env::var("HOME") else {
        return Vec::new();
    };
    let home = PathBuf::from(home);
    ["Downloads", "Desktop", "Documents"]
        .iter()
        .map(|d| home.join(d))
        .filter(|d| d.is_dir())
        .collect()
}

/// Every `.exe` in the download folders, most likely candidate first.
///
/// Only the top level of each folder is scanned: a download is a loose file,
/// and walking Documents recursively would be slow and would surface every
/// game already installed elsewhere.
pub fn scan() -> Vec<Found> {
    let mut out = Vec::new();
    for dir in search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            if path
                .extension()
                .map(|x| !x.eq_ignore_ascii_case("exe"))
                .unwrap_or(true)
            {
                continue;
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            // A partial download (.crdownload renamed, or 0 bytes) is noise.
            if size == 0 {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push(Found {
                age_secs: age_of(&path),
                info: probe::inspect(&path).ok(),
                path,
                name,
                size,
            });
        }
    }
    out.sort_by(|a, b| b.score().cmp(&a.score()).then(a.age_secs.cmp(&b.age_secs)));
    out
}

/// Where an executable should be installed.
#[derive(Debug, Clone, PartialEq)]
pub enum Destination {
    /// Put it in a bottle that already exists.
    Existing(String),
    /// Make a new bottle with this name, runtime and renderer.
    New {
        name: String,
        runtime: String,
        renderer: String,
    },
}

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
impl Destination {
    pub fn bottle_name(&self) -> &str {
        match self {
            Destination::Existing(n) => n,
            Destination::New { name, .. } => name,
        }
    }
}

/// A destination option with the reason it is being offered, so the UI can
/// explain itself rather than presenting a bare list.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct Option_ {
    pub destination: Destination,
    pub label: String,
    pub reason: String,
    /// True for the one that should be selected by default.
    pub recommended: bool,
}

/// Turn a bottle name suggestion into something safe for a directory.
pub fn slug_for(exe: &Path) -> String {
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "game".into());
    let mut out = String::new();
    let mut dash = false;
    for c in stem.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    // Installers are usually called "setup" or "install", which makes a
    // terrible bottle name — fall back to the folder they came from.
    let generic = ["setup", "install", "installer", "setup-x64", "game"];
    if s.is_empty() || generic.contains(&s.as_str()) {
        exe.parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_lowercase().replace(' ', "-"))
            .unwrap_or_else(|| "new-bottle".into())
    } else {
        s
    }
}

/// Offer destinations for this executable, best first.
///
/// The plan comes from the binary, so a 32-bit program never gets offered a
/// D3DMetal bottle (D3DMetal is 64-bit only) and a DirectX 12 title never gets
/// offered DXVK, which has no D3D12.
pub fn options(layout: &Layout, exe: &Path) -> Vec<Option_> {
    let info = probe::inspect(exe).ok();
    let plan = info.as_ref().map(probe::plan);

    let mut out = Vec::new();

    if let Some(p) = &plan {
        // An existing bottle whose engine already matches is the cheapest
        // answer: no download, no prefix boot, and shared installed runtimes.
        for b in bottle::list(layout) {
            if b.runtime == p.runtime && b.renderer == p.renderer {
                out.push(Option_ {
                    label: format!("{} (existing)", b.name),
                    reason: format!(
                        "already {} + {}, which is what this needs",
                        b.runtime, b.renderer
                    ),
                    destination: Destination::Existing(b.name.clone()),
                    recommended: false,
                });
            }
        }
    }

    // A fresh bottle is always available, and is the right default for a game:
    // a broken prefix can be deleted without taking anything else with it.
    if let Some(p) = &plan {
        let name = unique_bottle_name(layout, &slug_for(exe));
        out.push(Option_ {
            label: format!("{name} (new)"),
            reason: format!(
                "a clean prefix with {} + {}, chosen from the binary",
                p.runtime, p.renderer
            ),
            destination: Destination::New {
                name,
                runtime: p.runtime.to_string(),
                renderer: p.renderer.to_string(),
            },
            recommended: true,
        });
    }

    // Bottles that do not match the plan still work for plenty of things, so
    // offer them last rather than hiding them.
    if let Some(p) = &plan {
        for b in bottle::list(layout) {
            if b.runtime != p.runtime || b.renderer != p.renderer {
                out.push(Option_ {
                    label: format!("{} (existing)", b.name),
                    reason: format!("{} + {} — does not match the plan", b.runtime, b.renderer),
                    destination: Destination::Existing(b.name.clone()),
                    recommended: false,
                });
            }
        }
    } else {
        for b in bottle::list(layout) {
            out.push(Option_ {
                label: format!("{} (existing)", b.name),
                reason: "could not read the binary, so no plan".into(),
                destination: Destination::Existing(b.name.clone()),
                recommended: false,
            });
        }
    }

    // Exactly one recommendation, and prefer a matching existing bottle over a
    // new one when there is a match — reusing avoids a 1.4 GB engine compose.
    if let Some(i) = out
        .iter()
        .position(|o| o.reason.contains("which is what this needs"))
    {
        for o in out.iter_mut() {
            o.recommended = false;
        }
        out[i].recommended = true;
        let first = out.remove(i);
        out.insert(0, first);
    }
    out
}

fn unique_bottle_name(layout: &Layout, base: &str) -> String {
    let taken: Vec<String> = bottle::list(layout).into_iter().map(|b| b.name).collect();
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    for n in 2..100 {
        let c = format!("{base}-{n}");
        if !taken.contains(&c) {
            return c;
        }
    }
    format!("{base}-new")
}

/// Create the destination if it does not exist, and return the bottle.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn realise(layout: &Layout, dest: &Destination) -> anyhow::Result<Bottle> {
    match dest {
        Destination::Existing(name) => Bottle::load(layout, name),
        Destination::New {
            name,
            runtime,
            renderer,
        } => match Bottle::load(layout, name) {
            Ok(b) => Ok(b),
            Err(_) => bottle::create(layout, name, runtime, renderer, false),
        },
    }
}

/// Free space on the volume holding the bottles, in bytes.
pub fn free_space(layout: &Layout) -> u64 {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    let Ok(path) = CString::new(layout.root.to_string_lossy().as_bytes()) else {
        return 0;
    };
    unsafe {
        let mut st = MaybeUninit::<libc::statfs>::uninit();
        if libc::statfs(path.as_ptr(), st.as_mut_ptr()) != 0 {
            return 0;
        }
        let st = st.assume_init();
        st.f_bavail * u64::from(st.f_bsize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottle_names_avoid_useless_generics() {
        assert_eq!(
            slug_for(Path::new("/x/Hollow Knight/HollowKnight.exe")),
            "hollowknight"
        );
        // "setup" tells you nothing, so the folder name wins
        assert_eq!(slug_for(Path::new("/x/Celeste/setup.exe")), "celeste");
        assert_eq!(slug_for(Path::new("/x/Celeste/install.exe")), "celeste");
    }

    #[test]
    fn slug_strips_punctuation_and_case() {
        assert_eq!(
            slug_for(Path::new("/x/Battle.net-Setup.exe")),
            "battle-net-setup"
        );
    }

    #[test]
    fn freshness_outranks_size() {
        let fresh = Found {
            path: "/a/new.exe".into(),
            name: "new.exe".into(),
            size: 1_000_000,
            age_secs: 60,
            info: None,
        };
        let old_big = Found {
            path: "/a/old.exe".into(),
            name: "old.exe".into(),
            size: 900_000_000,
            age_secs: 60 * 86_400,
            info: None,
        };
        assert!(fresh.score() > old_big.score());
    }

    #[test]
    fn describe_is_honest_about_unreadable_files() {
        let f = Found {
            path: "/a/x.exe".into(),
            name: "x.exe".into(),
            size: 10,
            age_secs: 1,
            info: None,
        };
        assert_eq!(f.describe(), "not a Windows executable");
        assert!(!f.is_installer());
    }

    #[test]
    fn destination_exposes_its_bottle_name() {
        let d = Destination::New {
            name: "celeste".into(),
            runtime: "sikarugir-10.0".into(),
            renderer: "dxvk".into(),
        };
        assert_eq!(d.bottle_name(), "celeste");
        assert_eq!(Destination::Existing("ow".into()).bottle_name(), "ow");
    }
}
