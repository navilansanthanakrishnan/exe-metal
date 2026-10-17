//! Finding the executables inside a bottle, and working out which of them a
//! person actually wants to click.
//!
//! The naive version of this — list every `.exe`, drop the ones called
//! `unins000` — produces 161 entries on a real Steam bottle, most of them
//! `bin`, `nuget`, `sgen-grep-binprot` and eleven copies of `Steam`. Useless.
//!
//! So the unit here is not the executable, it is the **install root**: a
//! directory that holds one program. `Program Files (x86)/Battle.net`,
//! `steamapps/common/Celeste`. Each root contributes exactly one tile, named
//! after the root, and the executable is chosen from inside it. Everything else
//! in that folder is still reachable — see [`siblings`] — it just does not get
//! its own tile.
//!
//! Detection after an install is a set difference: inventory before, inventory
//! after, and whatever appeared is what that installer produced. That beats
//! reading registry keys, which installers write inconsistently under Wine.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Directories under `drive_c` that never hold anything worth launching.
const SKIP_DIRS: &[&str] = &[
    "windows",
    "Program Files/Common Files",
    "Program Files (x86)/Common Files",
    "Program Files/Internet Explorer",
    "Program Files (x86)/Internet Explorer",
    "Program Files/Windows Media Player",
    "Program Files (x86)/Windows Media Player",
    "Program Files/Windows NT",
    "Program Files (x86)/Windows NT",
    "Program Files/Windows Photo Viewer",
    "Program Files (x86)/Windows Photo Viewer",
    "ProgramData/Package Cache",
    "ProgramData/Microsoft",
];

/// Folder names that hold bundled runtimes and plumbing rather than the
/// program itself. Matched case-insensitively against any path component
/// *below* an install root.
///
/// `binaries` is deliberately absent: Unreal ships the real game at
/// `Game/Binaries/Win64/Game-Win64-Shipping.exe`.
const SUB_NOISE: &[&str] = &[
    "_commonredist",
    "commonredist",
    "redist",
    "redistributable",
    "redistributables",
    "directx",
    "dotnet",
    "dotnetfx",
    "vcredist",
    "easyanticheat",
    "easyanticheat_eos",
    "battleye",
    "anticheat",
    "jre",
    "jdk",
    "mono",
    "python",
    "node_modules",
    "bin",
    "package",
    "ubuntu12_32",
    "ubuntu12_64",
    "linux32",
    "linux64",
    "legacycompat",
    "crashreporter",
    "crashhandler",
    "installscript",
    "dxsetup",
    "drivers",
    "sdk",
    "tools",
];

/// Filename stems that are never the thing you want to launch.
const NOISE: &[&str] = &[
    "unins",
    "uninstall",
    "redist",
    "vcredist",
    "vc_redist",
    "dxsetup",
    "dxwebsetup",
    "directx",
    "dotnetfx",
    "ndp4",
    "oalinst",
    "wmfdist",
    "crashpad",
    "crashreport",
    "crashhandler",
    "crashsender",
    "werfault",
    "bsdiff",
    "7za",
    "7zr",
    "ffmpeg",
    "pythonw",
    "python",
    "vcruntime",
    "dotnet",
    "easyanticheat",
    "battleye",
    "nuget",
    "ironruby",
    "sgen-",
    "mono-",
    "msbuild",
    "regasm",
    "ilasm",
    "gacutil",
    "steamerrorreporter",
    "steamsysinfo",
    "steamservice",
    "gameoverlayui",
    "wow_helper",
    "streaming_client",
    "html5app",
];

/// Stems that carry no information — `bin.exe`, `run.exe`, `start.exe`.
const GENERIC: &[&str] = &[
    "bin", "run", "main", "start", "launch", "app", "game", "play", "exec", "test", "tests",
    "tool", "cli", "setup2", "x64", "x86", "win32", "win64",
];

/// Softer signals: launchable, but ranked below the program itself.
const SUPPORT: &[&str] = &[
    "helper",
    "service",
    "daemon",
    "updater",
    "update",
    "patcher",
    "repair",
    "cleanup",
    "report",
    "config",
    "settings",
    "editor",
    "server",
    "console",
    "debug",
    "benchmark",
    "diagnostic",
];

/// What a given executable looks like it is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The main thing: worth a tile on the home page.
    App,
    /// Plumbing — a launcher helper, updater, settings editor.
    Support,
    /// An installer. Kept so it can be re-run, never auto-launched.
    Installer,
}

/// One executable found in a bottle, with enough context to rank it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    /// Filename without the `.exe`.
    pub stem: String,
    /// The install root this belongs to, as a display name.
    pub folder: String,
    /// Absolute path of the install root.
    pub root: PathBuf,
    pub size: u64,
    pub kind: Kind,
    pub score: i32,
}

impl Candidate {
    /// What to show a human. The install root's name is almost always the real
    /// title — `Poppy Playtime - Multiplayer`, not
    /// `Playtime_Multiplayer-Win64-Shipping`.
    pub fn display_name(&self) -> String {
        if self.folder.is_empty() {
            self.stem.clone()
        } else {
            self.folder.clone()
        }
    }
}

fn normalise(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_noise(stem_lower: &str) -> bool {
    NOISE.iter().any(|n| stem_lower.contains(n))
}

fn is_generic(stem_lower: &str) -> bool {
    GENERIC.contains(&stem_lower) || stem_lower.len() <= 2
}

fn is_support(stem_lower: &str) -> bool {
    SUPPORT.iter().any(|n| stem_lower.contains(n))
}

fn looks_like_installer(stem_lower: &str) -> bool {
    stem_lower.contains("setup") || stem_lower.contains("install")
}

/// Every `.exe` under a prefix's `drive_c`, Wine's own tree excluded.
///
/// Returns a sorted set so two inventories can be differenced directly.
pub fn inventory(prefix: &Path) -> BTreeSet<PathBuf> {
    let root = prefix.join("drive_c");
    let mut out = BTreeSet::new();
    walk(&root, &root, 0, &mut out);
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut BTreeSet<PathBuf>) {
    if depth > 12 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        // Never follow symlinks: `z:` points at `/` and would walk the disk.
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if let Ok(rel) = path.strip_prefix(root) {
                let rel_s = rel.to_string_lossy().replace('\\', "/");
                if SKIP_DIRS
                    .iter()
                    .any(|s| rel_s == *s || rel_s.starts_with(&format!("{s}/")))
                {
                    continue;
                }
                if rel_s.contains("/Temp") || rel_s.ends_with("/Temp") {
                    continue;
                }
            }
            walk(root, &path, depth + 1, out);
        } else if ft.is_file()
            && path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("exe"))
                .unwrap_or(false)
        {
            out.insert(path);
        }
    }
}

/// The install roots in a prefix: one directory per installed program.
///
/// Two shapes cover essentially everything: a top-level folder under
/// `Program Files`, and a Steam library entry under `steamapps/common`.
pub fn roots(prefix: &Path) -> Vec<PathBuf> {
    let c = prefix.join("drive_c");
    let mut out = Vec::new();

    for pf in ["Program Files", "Program Files (x86)"] {
        let dir = c.join(pf);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let rel = format!("{pf}/{name}");
            if SKIP_DIRS.iter().any(|s| *s == rel) {
                continue;
            }
            out.push(e.path());
        }
    }

    // Steam libraries can sit anywhere; find them by name rather than assuming
    // the default location.
    let mut steamapps = Vec::new();
    find_dirs_named(&c, "steamapps", 0, &mut steamapps);
    for sa in steamapps {
        let common = sa.join("common");
        let Ok(entries) = std::fs::read_dir(&common) else {
            continue;
        };
        for e in entries.flatten() {
            if e.path().is_dir() {
                out.push(e.path());
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

fn find_dirs_named(dir: &Path, want: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        if e.file_name().to_string_lossy().eq_ignore_ascii_case(want) {
            out.push(e.path());
            continue;
        }
        find_dirs_named(&e.path(), want, depth + 1, out);
    }
}

/// Does any path component below `root` look like bundled plumbing?
fn under_sub_noise(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let comps: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    // The filename itself is the last component; only judge directories.
    comps
        .iter()
        .take(comps.len().saturating_sub(1))
        .any(|c| SUB_NOISE.iter().any(|n| c == n))
}

fn describe(root: &Path, path: PathBuf) -> Option<Candidate> {
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let lower = stem.to_lowercase();
    if is_noise(&lower) {
        return None;
    }
    if under_sub_noise(root, &path) {
        return None;
    }

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let folder = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let kind = if looks_like_installer(&lower) {
        Kind::Installer
    } else if is_support(&lower) {
        Kind::Support
    } else {
        Kind::App
    };

    let depth = path
        .strip_prefix(root)
        .map(|r| r.components().count())
        .unwrap_or(9) as i32;
    // Deep inside an install root is a bundled tool, not the program.
    if depth > 4 {
        return None;
    }

    let mut score = 0;
    score += ((size / (1024 * 1024)) as i32).min(120);

    let f = normalise(&folder);
    let s = normalise(&stem);
    if !f.is_empty() && (s == f || s.contains(&f) || f.contains(&s)) {
        score += 70;
    }
    // Unreal's shipping build is the real entry point.
    if lower.ends_with("-shipping") || lower.contains("win64-shipping") {
        score += 45;
    }
    if lower.contains("launcher") {
        score += 20;
    }
    if is_generic(&lower) {
        score -= 70;
    }
    score -= (depth - 1).max(0) * 15;
    match kind {
        Kind::App => score += 30,
        Kind::Support => score -= 30,
        Kind::Installer => score -= 20,
    }

    Some(Candidate {
        path,
        stem,
        folder,
        root: root.to_path_buf(),
        size,
        kind,
        score,
    })
}

/// Rank a set of paths that all belong to one install root.
pub fn rank_in_root(root: &Path, paths: impl IntoIterator<Item = PathBuf>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = paths
        .into_iter()
        .filter_map(|p| describe(root, p))
        .collect();
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.stem.cmp(&b.stem)));
    out
}

/// One entry per installed program: the best executable from each install root.
///
/// This is what the home page shows.
pub fn titles(prefix: &Path) -> Vec<Candidate> {
    let inv = inventory(prefix);
    let mut by_root: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let roots = roots(prefix);

    for path in inv {
        // Longest matching root wins, so a Steam game under a Steam install
        // is attributed to the game rather than to Steam.
        if let Some(root) = roots
            .iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
        {
            by_root.entry(root.clone()).or_default().push(path);
        }
    }

    let mut out: Vec<Candidate> = by_root
        .into_iter()
        .filter_map(|(root, paths)| rank_in_root(&root, paths).into_iter().next())
        // A root whose best candidate is still plumbing is not a program.
        .filter(|c| c.kind == Kind::App && c.score > 0)
        .collect();
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.folder.cmp(&b.folder)));
    out
}

/// Backwards-compatible alias used by the diff path.
pub fn candidates(prefix: &Path) -> Vec<Candidate> {
    titles(prefix)
}

/// What an installer produced: install roots that gained executables.
pub fn newly_installed(prefix: &Path, before: &BTreeSet<PathBuf>) -> Vec<Candidate> {
    let after = inventory(prefix);
    let fresh: BTreeSet<PathBuf> = after.difference(before).cloned().collect();
    if fresh.is_empty() {
        return Vec::new();
    }
    // Re-rank whole roots, but only keep roots something new landed in.
    titles(prefix)
        .into_iter()
        .filter(|c| fresh.iter().any(|f| f.starts_with(&c.root)))
        .collect()
}

/// An executable kept outside every bottle — a game in `~/Games` started
/// with `play` — judged against its own folder, which is its install root.
///
/// A folder that is just where things land (`~/Downloads`) names nothing, so
/// the executable's own name is used instead.
pub fn external(exe: &Path) -> Option<Candidate> {
    let root = exe.parent()?;
    let mut c = describe(root, exe.to_path_buf())?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if home.as_deref() == root.parent() || home.as_deref() == Some(root) {
        c.folder.clear();
    }
    Some(c)
}

/// The other executables in a title's install root — "what EXEs this
/// application is made of".
pub fn siblings(prefix: &Path, exe: &Path) -> Vec<Candidate> {
    if !exe.starts_with(prefix.join("drive_c")) {
        let Some(root) = exe.parent() else {
            return Vec::new();
        };
        let mut inv = BTreeSet::new();
        walk(root, root, 0, &mut inv);
        inv.remove(exe);
        return rank_in_root(root, inv);
    }
    let root = roots(prefix)
        .into_iter()
        .filter(|r| exe.starts_with(r))
        .max_by_key(|r| r.components().count())
        .or_else(|| exe.parent().map(|p| p.to_path_buf()));
    let Some(root) = root else {
        return Vec::new();
    };
    let inv: Vec<PathBuf> = inventory(prefix)
        .into_iter()
        .filter(|p| p.starts_with(&root) && p != exe)
        .collect();
    rank_in_root(&root, inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, bytes: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; bytes]).unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("em-scan-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_game_outside_every_bottle_is_named_for_its_folder() {
        let c = external(Path::new(
            "/x/Games/Hollow Knight Silksong/Hollow Knight Silksong.exe",
        ))
        .unwrap();
        assert_eq!(c.display_name(), "Hollow Knight Silksong");
        assert_eq!(c.kind, Kind::App);
        let setup = external(Path::new("/x/Downloads/Battle.net-Setup.exe")).unwrap();
        assert_eq!(setup.kind, Kind::Installer);
    }

    #[test]
    fn siblings_of_a_game_outside_the_prefix_come_from_its_own_folder() {
        let d = scratch("external");
        let pfx = d.join("pfx");
        std::fs::create_dir_all(pfx.join("drive_c")).unwrap();
        let game = d.join("Games/Silksong/Silksong.exe");
        touch(&game, 4096);
        touch(&d.join("Games/Silksong/UnityCrashHandler64.exe"), 1024);
        touch(&d.join("Games/Silksong/Settings.exe"), 1024);
        let parts = siblings(&pfx, &game);
        assert_eq!(parts.len(), 1, "{parts:?}");
        assert!(
            parts[0].path.ends_with("Settings.exe"),
            "crash handlers are noise"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn skips_wine_own_tree() {
        let pfx = scratch("wine");
        touch(&pfx.join("drive_c/windows/system32/notepad.exe"), 10);
        touch(&pfx.join("drive_c/Program Files/Thing/thing.exe"), 10);
        let inv = inventory(&pfx);
        assert_eq!(inv.len(), 1);
    }

    #[test]
    fn one_tile_per_install_root() {
        let pfx = scratch("root");
        let g = pfx.join("drive_c/Program Files/Celeste");
        touch(&g.join("Celeste.exe"), 8_000_000);
        touch(&g.join("CrashHandler.exe"), 50);
        touch(&g.join("tools/config.exe"), 50);
        let t = titles(&pfx);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].display_name(), "Celeste");
    }

    #[test]
    fn steam_games_beat_the_steam_client_that_contains_them() {
        let pfx = scratch("steam");
        let steam = pfx.join("drive_c/Program Files (x86)/Steam");
        touch(&steam.join("steam.exe"), 3_000_000);
        touch(&steam.join("bin/bin.exe"), 900_000);
        touch(&steam.join("package/nuget.exe"), 900_000);
        let game = steam.join("steamapps/common/Celeste");
        touch(&game.join("Celeste.exe"), 8_000_000);

        let t = titles(&pfx);
        let names: Vec<String> = t.iter().map(|c| c.display_name()).collect();
        assert!(names.contains(&"Celeste".to_string()), "got {names:?}");
        assert!(names.contains(&"Steam".to_string()), "got {names:?}");
        // The junk inside Steam must not become tiles of its own.
        assert_eq!(t.len(), 2, "got {names:?}");
    }

    #[test]
    fn unreal_shipping_binary_is_found_but_named_after_the_game() {
        let pfx = scratch("unreal");
        let g = pfx.join("drive_c/Program Files/Poppy Playtime - Multiplayer");
        touch(&g.join("Playtime.exe"), 200_000);
        touch(
            &g.join("Playtime/Binaries/Win64/Playtime-Win64-Shipping.exe"),
            90_000_000,
        );
        let t = titles(&pfx);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].display_name(), "Poppy Playtime - Multiplayer");
        assert!(t[0].path.to_string_lossy().contains("Shipping"));
    }

    #[test]
    fn bundled_runtimes_and_anticheat_are_ignored() {
        let pfx = scratch("bundled");
        let g = pfx.join("drive_c/Program Files/Game");
        touch(&g.join("Game.exe"), 5_000_000);
        touch(&g.join("_CommonRedist/vcredist_x64.exe"), 9_000_000);
        touch(&g.join("EasyAntiCheat/EasyAntiCheat_EOS.exe"), 9_000_000);
        touch(&g.join("jre/bin/java.exe"), 9_000_000);
        let t = titles(&pfx);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].stem, "Game");
    }

    #[test]
    fn generic_stems_never_win_a_root() {
        let pfx = scratch("generic");
        let g = pfx.join("drive_c/Program Files/Thing");
        touch(&g.join("bin.exe"), 40_000_000);
        touch(&g.join("Thing.exe"), 2_000_000);
        let t = titles(&pfx);
        assert_eq!(t[0].stem, "Thing");
    }

    #[test]
    fn diff_reports_only_the_root_the_installer_filled() {
        let pfx = scratch("diff");
        touch(&pfx.join("drive_c/Program Files/Old/Old.exe"), 2_000_000);
        let before = inventory(&pfx);
        touch(&pfx.join("drive_c/Program Files/New/New.exe"), 5_000_000);
        let fresh = newly_installed(&pfx, &before);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].stem, "New");
    }

    #[test]
    fn siblings_lists_the_rest_of_the_install_root() {
        let pfx = scratch("sib");
        let g = pfx.join("drive_c/Program Files/Game");
        touch(&g.join("Game.exe"), 5_000_000);
        touch(&g.join("GameSettings.exe"), 100_000);
        let s = siblings(&pfx, &g.join("Game.exe"));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].stem, "GameSettings");
    }

    #[test]
    fn symlinks_are_not_followed() {
        let pfx = scratch("link");
        touch(&pfx.join("drive_c/Program Files/Real/real.exe"), 10);
        let _ = std::os::unix::fs::symlink("/", pfx.join("drive_c/loop"));
        assert_eq!(inventory(&pfx).len(), 1);
    }
}
