//! The installed-title index behind the home page.
//!
//! One JSON file, written atomically. It is a *cache of intent*, not a source
//! of truth: the prefix on disk is authoritative, so every read reconciles
//! against it and drops entries whose executable has gone. That way removing a
//! bottle by hand does not leave dead tiles behind.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::scan::{self, Kind};

/// Which engine actually runs this title.
///
/// A title in a CrossOver bottle cannot be launched by exe-metal's Wine and
/// vice versa, so this is not cosmetic — dispatching on it is the difference
/// between a game that starts and one that deadlocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// exe-metal's own bottles and composed engines.
    #[default]
    ExeMetal,
    /// A CrossOver bottle, launched through cxstart.
    CrossOver,
}

impl Backend {
    pub fn label(&self) -> &'static str {
        match self {
            Backend::ExeMetal => "exe-metal",
            Backend::CrossOver => "CrossOver",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleKind {
    App,
    Support,
    Installer,
}

impl From<Kind> for TitleKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::App => TitleKind::App,
            Kind::Support => TitleKind::Support,
            Kind::Installer => TitleKind::Installer,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Title {
    /// Stable slug, unique within the library.
    pub id: String,
    pub name: String,
    /// Bottle this lives in.
    pub bottle: String,
    /// Absolute host path to the executable.
    pub exe: PathBuf,
    pub kind: TitleKind,
    /// Unix seconds.
    pub added: u64,
    #[serde(default)]
    pub last_played: Option<u64>,
    #[serde(default)]
    pub play_count: u32,
    /// The installer this came from, when it was detected by an install diff.
    #[serde(default)]
    pub installer: Option<PathBuf>,
    /// Set by hand from the UI; overrides the scanned name.
    #[serde(default)]
    pub renamed: bool,
    /// Which engine runs it. Defaults to exe-metal so older library files
    /// keep working without migration.
    #[serde(default)]
    pub backend: Backend,
}

impl Title {
    pub fn exists(&self) -> bool {
        self.exe.is_file()
    }

    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    pub fn size(&self) -> u64 {
        std::fs::metadata(&self.exe).map(|m| m.len()).unwrap_or(0)
    }

    /// Directory the title occupies — what to reveal in Finder.
    #[allow(dead_code)]
    pub fn folder(&self) -> Option<&Path> {
        self.exe.parent()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub titles: Vec<Title>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where the library lives: beside the bottles it indexes.
pub fn library_root() -> Result<PathBuf> {
    Ok(support_dir()?.join("exe-metal"))
}

pub fn library_file() -> Result<PathBuf> {
    Ok(library_root()?.join("library.json"))
}

fn support_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support"))
}

/// The index exe-metal-2 kept before the two tools merged. Read once, when
/// this library has no file of its own, so its titles and play counts carry
/// over. It is copied, never moved: an exe-metal-2 install keeps working.
fn legacy_library_file() -> Result<PathBuf> {
    Ok(support_dir()?.join("exe-metal-2").join("library.json"))
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

impl Library {
    pub fn load() -> Result<Self> {
        let mut path = library_file()?;
        if !path.exists() {
            path = legacy_library_file()?;
            if !path.exists() {
                return Ok(Self::default());
            }
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        // A corrupt index must never be fatal — the prefix still has the games,
        // and a rescan rebuilds this. Start empty rather than refusing to open.
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    pub fn save(&self) -> Result<()> {
        let path = library_file()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        // Write-then-rename: a crash mid-write must not truncate the index.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Drop entries whose executable no longer exists. Returns how many went.
    pub fn prune_missing(&mut self) -> usize {
        let before = self.titles.len();
        self.titles.retain(|t| t.exists());
        before - self.titles.len()
    }

    pub fn get(&self, id: &str) -> Option<&Title> {
        self.titles.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Title> {
        self.titles.iter_mut().find(|t| t.id == id)
    }

    pub fn by_exe(&self, exe: &Path) -> Option<&Title> {
        self.titles.iter().find(|t| t.exe == exe)
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.titles.len();
        self.titles.retain(|t| t.id != id);
        before != self.titles.len()
    }

    fn unique_id(&self, base: &str) -> String {
        let base = if base.is_empty() { "title" } else { base };
        if self.get(base).is_none() {
            return base.to_string();
        }
        for n in 2..1000 {
            let candidate = format!("{base}-{n}");
            if self.get(&candidate).is_none() {
                return candidate;
            }
        }
        format!("{base}-{}", now())
    }

    /// Register a scanned candidate. Returns the id, or the existing id if this
    /// executable is already known — adding twice is a no-op, which matters
    /// because rescans run often.
    pub fn add(&mut self, bottle: &str, c: &scan::Candidate, installer: Option<PathBuf>) -> String {
        self.add_with(bottle, c, installer, Backend::ExeMetal)
    }

    /// Register a candidate against a specific backend.
    pub fn add_with(
        &mut self,
        bottle: &str,
        c: &scan::Candidate,
        installer: Option<PathBuf>,
        backend: Backend,
    ) -> String {
        if let Some(existing) = self.by_exe(&c.path) {
            return existing.id.clone();
        }
        let name = c.display_name();
        let id = self.unique_id(&slugify(&name));
        self.titles.push(Title {
            id: id.clone(),
            name,
            bottle: bottle.to_string(),
            exe: c.path.clone(),
            kind: c.kind.into(),
            added: now(),
            last_played: None,
            play_count: 0,
            installer,
            renamed: false,
            backend,
        });
        id
    }

    pub fn mark_played(&mut self, id: &str) {
        if let Some(t) = self.get_mut(id) {
            t.last_played = Some(now());
            t.play_count += 1;
        }
    }

    /// Titles worth a tile, most recently played first, then newest.
    pub fn shelf(&self) -> Vec<&Title> {
        let mut v: Vec<&Title> = self
            .titles
            .iter()
            .filter(|t| t.kind != TitleKind::Support)
            .collect();
        v.sort_by(|a, b| {
            b.last_played
                .unwrap_or(0)
                .cmp(&a.last_played.unwrap_or(0))
                .then(b.added.cmp(&a.added))
        });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Candidate;

    fn cand(path: &str, stem: &str, folder: &str) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            stem: stem.into(),
            folder: folder.into(),
            root: PathBuf::from(path).parent().unwrap().to_path_buf(),
            size: 1,
            kind: Kind::App,
            score: 1,
        }
    }

    #[test]
    fn slugs_are_stable_and_clean() {
        assert_eq!(slugify("Battle.net Launcher"), "battle-net-launcher");
        assert_eq!(slugify("  Celeste!! "), "celeste");
        assert_eq!(slugify("???"), "");
    }

    #[test]
    fn adding_the_same_exe_twice_is_one_entry() {
        let mut lib = Library::default();
        let c = cand("/x/Game/game.exe", "game", "Game");
        let a = lib.add("b", &c, None);
        let b = lib.add("b", &c, None);
        assert_eq!(a, b);
        assert_eq!(lib.titles.len(), 1);
    }

    #[test]
    fn colliding_names_get_distinct_ids() {
        let mut lib = Library::default();
        lib.add("b", &cand("/x/A/game.exe", "game", "A"), None);
        lib.add("b", &cand("/y/B/game.exe", "game", "B"), None);
        assert_eq!(lib.titles.len(), 2);
        assert_ne!(lib.titles[0].id, lib.titles[1].id);
    }

    #[test]
    fn prune_drops_entries_whose_exe_vanished() {
        let mut lib = Library::default();
        lib.add("b", &cand("/definitely/not/here.exe", "here", "not"), None);
        assert_eq!(lib.prune_missing(), 1);
        assert!(lib.titles.is_empty());
    }

    #[test]
    fn shelf_hides_support_binaries_and_orders_by_recency() {
        let mut lib = Library::default();
        let id_a = lib.add("b", &cand("/x/A/a.exe", "a", "A"), None);
        lib.add("b", &cand("/x/B/b.exe", "b", "B"), None);
        let mut helper = cand("/x/C/c.exe", "c", "C");
        helper.kind = Kind::Support;
        lib.add("b", &helper, None);

        lib.mark_played(&id_a);
        let shelf = lib.shelf();
        assert_eq!(shelf.len(), 2, "support binary should be hidden");
        assert_eq!(shelf[0].id, id_a, "most recently played comes first");
    }

    #[test]
    fn play_count_increments() {
        let mut lib = Library::default();
        let id = lib.add("b", &cand("/x/A/a.exe", "a", "A"), None);
        lib.mark_played(&id);
        lib.mark_played(&id);
        assert_eq!(lib.get(&id).unwrap().play_count, 2);
    }
}
