//! The list of things you have actually launched.
//!
//! Kept next to everything else under the exe-metal root so `rm -rf` still
//! uninstalls cleanly. Deliberately keyed on the executable path rather than
//! the bottle: people think in games, not prefixes.

use crate::paths::Layout;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Enough to be useful as a menu, few enough to stay readable.
const MAX_ENTRIES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub bottle: String,
    /// Unix seconds. Zero means "never recorded", which sorts last.
    #[serde(default)]
    pub last_run: u64,
    #[serde(default)]
    pub runs: u32,
}

impl Entry {
    /// What to show in a list: the file's own name, or its parent directory
    /// when the name is a generic launcher.
    pub fn label(&self) -> String {
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "?".into());
        let generic = matches!(
            stem.to_ascii_lowercase().as_str(),
            "game" | "launcher" | "start" | "play" | "bin" | "main"
        );
        if generic {
            if let Some(parent) = self.path.parent().and_then(|p| p.file_name()) {
                return format!("{} ({stem})", parent.to_string_lossy());
            }
        }
        stem
    }

    /// True when the file is still where we left it.
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Recent {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

impl Recent {
    fn path(layout: &Layout) -> PathBuf {
        layout.root.join("recent.toml")
    }

    /// A corrupt or missing file is not an error worth stopping a launch for —
    /// the recent list is a convenience, and losing it costs nothing.
    pub fn load(layout: &Layout) -> Self {
        std::fs::read_to_string(Self::path(layout))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        std::fs::create_dir_all(&layout.root)?;
        std::fs::write(Self::path(layout), toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Move an executable to the front, merging with any existing entry so the
    /// run count survives.
    pub fn record(&mut self, path: &Path, bottle: &str) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let previous_runs = self
            .entries
            .iter()
            .find(|e| e.path == canonical)
            .map(|e| e.runs)
            .unwrap_or(0);
        self.entries.retain(|e| e.path != canonical);
        self.entries.insert(
            0,
            Entry {
                path: canonical,
                bottle: bottle.to_string(),
                last_run: now(),
                runs: previous_runs + 1,
            },
        );
        self.entries.truncate(MAX_ENTRIES);
    }

    /// Most recent first.
    pub fn list(&self) -> Vec<Entry> {
        let mut v = self.entries.clone();
        v.sort_by(|a, b| b.last_run.cmp(&a.last_run));
        v
    }

    /// Drop entries whose file has been deleted or moved.
    pub fn forget_missing(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.exists());
        before - self.entries.len()
    }

    pub fn forget(&mut self, path: &Path) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.entries.retain(|e| e.path != canonical);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "3 minutes ago" style, for the UI and `exe-metal recent`.
pub fn ago(then: u64) -> String {
    if then == 0 {
        return "never".into();
    }
    let now = now();
    if now <= then {
        return "just now".into();
    }
    let secs = now - then;
    let (n, unit) = match secs {
        0..=59 => return "just now".into(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86_399 => (secs / 3600, "hour"),
        86_400..=2_591_999 => (secs / 86_400, "day"),
        _ => (secs / 2_592_000, "month"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_moves_an_entry_to_the_front_and_counts_runs() {
        let mut r = Recent::default();
        r.record(Path::new("/tmp/a.exe"), "one");
        r.record(Path::new("/tmp/b.exe"), "two");
        assert_eq!(r.entries[0].path, PathBuf::from("/tmp/b.exe"));

        r.record(Path::new("/tmp/a.exe"), "one");
        assert_eq!(r.entries[0].path, PathBuf::from("/tmp/a.exe"));
        assert_eq!(r.entries.len(), 2, "re-recording must not duplicate");
        assert_eq!(r.entries[0].runs, 2, "run count should survive the move");
    }

    #[test]
    fn the_list_is_capped() {
        let mut r = Recent::default();
        for i in 0..(MAX_ENTRIES + 10) {
            r.record(Path::new(&format!("/tmp/game{i}.exe")), "b");
        }
        assert_eq!(r.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn generic_launcher_names_borrow_the_folder_name() {
        let e = Entry {
            path: PathBuf::from("/games/Hollow Knight/game.exe"),
            bottle: "b".into(),
            last_run: 0,
            runs: 1,
        };
        assert_eq!(e.label(), "Hollow Knight (game)");

        let e2 = Entry {
            path: PathBuf::from("/games/Undertale/UNDERTALE.exe"),
            bottle: "b".into(),
            last_run: 0,
            runs: 1,
        };
        assert_eq!(e2.label(), "UNDERTALE");
    }

    #[test]
    fn missing_files_are_forgotten() {
        let mut r = Recent::default();
        r.record(Path::new("/definitely/not/here.exe"), "b");
        assert_eq!(r.forget_missing(), 1);
        assert!(r.entries.is_empty());
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(0), "never");
        assert_eq!(ago(now()), "just now");
        assert_eq!(ago(now() - 60), "1 minute ago");
        assert_eq!(ago(now() - 7200), "2 hours ago");
        assert_eq!(ago(now() - 86_400 * 3), "3 days ago");
    }
}
