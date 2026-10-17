//! A bottle's Start Menu, published into the Mac, and its file associations.
//!
//! CrossOver splits this across three tools: `cxmenu` walks a bottle's Start
//! Menu and turns each `.lnk` into a Mac `.app`, while `cxassoc`/`cxwinassoc`
//! carry file-type associations between the bottle's registry and
//! LaunchServices. `shortcut.rs` already builds the `.app`; what was missing is
//! the half that works out *what* a bottle wants published, which means reading
//! Windows' own two records of it — Shell Links and the registry.
//!
//! Three things here are load-bearing, and all three come from the real
//! bottles on this machine rather than from the specification:
//!
//! * **`.lnk` is parsed in-process, never by shelling out.** Nothing on a Mac
//!   reads Shell Links, and a scan that spawns a process per shortcut is both
//!   slow and a new failure mode. The format is small — a 76-byte header, then
//!   optional blocks, each carrying its own size. Every field read here is
//!   bounds-checked and the whole parser returns `Option`, because one corrupt
//!   `.lnk` must cost exactly one entry, never the rest of the menu.
//!
//! * **A Windows path crosses into the host differently per backend.**
//!   exe-metal keeps `drive_c` under `pfx/`; CrossOver keeps it directly in the
//!   bottle. Drives beyond `C:` go through `dosdevices`, which is not pedantry:
//!   this machine has `d:` and `e:` pointing at external volumes holding games,
//!   and a shortcut into one resolves to nothing without reading those links.
//!
//! * **The same program appears in the menu more than once.** The Steam bottle
//!   carries `Steam.lnk` under `ProgramData` *and* under the user profile, both
//!   resolving to `C:\Program Files (x86)\Steam\steam.exe`. Publishing both
//!   would put two identical `.app` bundles in `~/Applications` fighting over
//!   one name, so entries are collapsed on their resolved target.
//!
//! Nothing in `main.rs` calls this yet — wiring up the subcommand is a separate
//! change — so the module allows dead code rather than burying a build in
//! warnings for functions that exist to be called next.
#![allow(dead_code)]

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::library::{Backend, Library};
use crate::{crossover, paths, scan, shortcut};

// ---------------------------------------------------------------------------
// Prefix geometry
// ---------------------------------------------------------------------------

/// The directory that actually holds `drive_c` and `dosdevices`.
///
/// Callers hand this module "a prefix" and mean one of two different things:
/// a CrossOver bottle (`.../Bottles/ow`, `drive_c` right inside it) or an
/// exe-metal bottle (`.../bottles/cosmos-steam`, `drive_c` one level down under
/// `pfx/`). Accepting both, plus an already-correct `.../pfx`, is cheaper than
/// making every call site remember which shape it holds.
pub fn wine_root(prefix: &Path) -> Option<PathBuf> {
    [prefix.to_path_buf(), prefix.join("pfx")]
        .into_iter()
        .find(|candidate| candidate.join("drive_c").is_dir())
}

/// `drive_c` for a prefix in either shape.
pub fn drive_c(prefix: &Path) -> Option<PathBuf> {
    wine_root(prefix).map(|r| r.join("drive_c"))
}

/// Where a bottle lives, given the engine that owns it.
pub fn bottle_prefix(backend: Backend, bottle: &str) -> Result<PathBuf> {
    match backend {
        Backend::CrossOver => crossover::prefix(bottle),
        Backend::ExeMetal => {
            let layout = paths::Layout::discover()?;
            let pfx = layout.bottle(bottle).join("pfx");
            if !pfx.join("drive_c").is_dir() {
                anyhow::bail!("no exe-metal bottle named '{bottle}'");
            }
            Ok(pfx)
        }
    }
}

/// Map a Windows path inside a bottle to its host path.
///
/// Returns `None` rather than a guess. A UNC share or a drive letter with no
/// `dosdevices` entry has no host path at all, and fabricating one reproduces
/// exactly the failure `crossover::windows_path` warns about in the other
/// direction: a path that looks right, points nowhere, and fails silently.
pub fn windows_to_host(prefix: &Path, win: &str) -> Option<PathBuf> {
    let win = win.trim();
    // UNC. Wine can reach these through `z:`, but not by any rule we can apply
    // here without guessing at a mount, so say so instead.
    if win.starts_with("\\\\") {
        return None;
    }
    let root = wine_root(prefix)?;
    let mut chars = win.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next() != Some(':') {
        return None;
    }
    let rest = win.get(2..).unwrap_or("").trim_start_matches(['\\', '/']);

    let base = if letter.eq_ignore_ascii_case(&'c') {
        root.join("drive_c")
    } else {
        // `dosdevices/e:` is a symlink to the volume. Read the link rather than
        // following it so a broken mount surfaces as "not found" here instead
        // of as a directory walk that wanders off into the host filesystem.
        let link = root
            .join("dosdevices")
            .join(format!("{}:", letter.to_ascii_lowercase()));
        let target = std::fs::read_link(&link).ok()?;
        if target.is_absolute() {
            target
        } else {
            root.join("dosdevices").join(target)
        }
    };

    let mut out = base;
    for part in rest.split(['\\', '/']) {
        // `.` and `..` in a link target would let a shortcut escape the bottle.
        match part {
            "" | "." => continue,
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Shell Link (.lnk) parsing
// ---------------------------------------------------------------------------

const HEADER_SIZE: usize = 0x4C;
/// `00021401-0000-0000-C000-000000000046`, in the little-endian field order a
/// GUID is written on disk. A file that does not carry it is not a Shell Link,
/// whatever its extension claims.
const LINK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];

const HAS_ID_LIST: u32 = 0x0000_0001;
const HAS_LINK_INFO: u32 = 0x0000_0002;
const HAS_NAME: u32 = 0x0000_0004;
const HAS_RELATIVE_PATH: u32 = 0x0000_0008;
const HAS_WORKING_DIR: u32 = 0x0000_0010;
const HAS_ARGUMENTS: u32 = 0x0000_0020;
const HAS_ICON_LOCATION: u32 = 0x0000_0040;
const IS_UNICODE: u32 = 0x0000_0080;

/// `EnvironmentVariableDataBlock` — the fallback target for links written
/// against a path that only exists once environment variables are expanded.
const SIG_ENVIRONMENT: u32 = 0xA000_0001;
const ENVIRONMENT_BLOCK_SIZE: usize = 0x0314;

/// A `.lnk`'s fields, still in Windows terms. Mapping to the host is a separate
/// step so the parser can be tested without a prefix on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lnk {
    /// Absolute Windows path to the target, when the link carries one.
    pub target: Option<String>,
    /// Path relative to the `.lnk` itself. Some installers write only this.
    pub relative_path: Option<String>,
    pub working_dir: Option<String>,
    pub arguments: Option<String>,
    /// Raw icon location, index suffix (`shell32.dll,-16`) still attached.
    pub icon: Option<String>,
    /// The link's own description, which is usually empty in practice.
    pub description: Option<String>,
}

/// Parse a Shell Link. `None` for anything that is not one, or is damaged.
pub fn parse_lnk(b: &[u8]) -> Option<Lnk> {
    if b.len() < HEADER_SIZE {
        return None;
    }
    if le_u32(b, 0)? as usize != HEADER_SIZE || b.get(4..20)? != LINK_CLSID {
        return None;
    }
    let flags = le_u32(b, 20)?;
    let unicode = flags & IS_UNICODE != 0;
    let mut p = HEADER_SIZE;

    // LinkTargetIDList. Its contents are a shell item chain that needs the
    // Windows shell namespace to interpret, so it is skipped wholesale by its
    // own size — which is exactly why that size is stored ahead of it.
    if flags & HAS_ID_LIST != 0 {
        let size = le_u16(b, p)? as usize;
        p = p.checked_add(2)?.checked_add(size)?;
        if p > b.len() {
            return None;
        }
    }

    let mut out = Lnk::default();

    if flags & HAS_LINK_INFO != 0 {
        let size = le_u32(b, p)? as usize;
        // A LinkInfo smaller than its own fixed header, or claiming to run past
        // the end of the file, means the rest of the file cannot be located
        // either — so give up rather than read at a guessed offset.
        if size < 0x1C || p.checked_add(size)? > b.len() {
            return None;
        }
        out.target = link_info_path(b, p);
        p += size;
    }

    // StringData, written back to back in the order the flags are defined in
    // and with no key of its own. Reading them out of that order does not fail,
    // it silently files the working directory as the icon location.
    if flags & HAS_NAME != 0 {
        out.description = non_empty(read_string(b, &mut p, unicode)?);
    }
    if flags & HAS_RELATIVE_PATH != 0 {
        out.relative_path = non_empty(read_string(b, &mut p, unicode)?);
    }
    if flags & HAS_WORKING_DIR != 0 {
        out.working_dir = non_empty(read_string(b, &mut p, unicode)?);
    }
    if flags & HAS_ARGUMENTS != 0 {
        out.arguments = non_empty(read_string(b, &mut p, unicode)?);
    }
    if flags & HAS_ICON_LOCATION != 0 {
        out.icon = non_empty(read_string(b, &mut p, unicode)?);
    }

    // ExtraData. Only the environment block matters: it is the one place a
    // target survives when LinkInfo is absent, which is how Wine's own
    // winemenubuilder writes some links.
    if out.target.is_none() {
        out.target = environment_target(b, p);
    }

    Some(out)
}

/// Read a `.lnk` from disk. Anything unreadable, empty or absurdly large is
/// `None`: a 40 MB file named `.lnk` is not a shortcut, and reading it into
/// memory during a menu scan is a denial of service on ourselves.
pub fn read_lnk(path: &Path) -> Option<Lnk> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() < HEADER_SIZE as u64 || meta.len() > 1 << 22 {
        return None;
    }
    parse_lnk(&std::fs::read(path).ok()?)
}

/// `LocalBasePath` + `CommonPathSuffix` out of a LinkInfo block.
fn link_info_path(b: &[u8], start: usize) -> Option<String> {
    let header_size = le_u32(b, start + 4)? as usize;
    let info_flags = le_u32(b, start + 8)?;
    // Bit 0 is VolumeIDAndLocalBasePath. Without it the target is a network
    // share described by CommonNetworkRelativeLink, which has no host path.
    if info_flags & 0x1 == 0 {
        return None;
    }
    // A header of 0x24 or more carries the Unicode offsets appended after the
    // ANSI ones. Wine writes the short 0x1C header, so both paths are exercised
    // only against links written by real Windows installers.
    let (base_off, suffix_off) = if header_size >= 0x24 {
        (
            le_u32(b, start + 28)? as usize,
            le_u32(b, start + 32)? as usize,
        )
    } else {
        (
            le_u32(b, start + 16)? as usize,
            le_u32(b, start + 24)? as usize,
        )
    };
    let unicode = header_size >= 0x24;

    let base = read_at(b, start.checked_add(base_off)?, unicode)?;
    if base.is_empty() {
        return None;
    }
    let suffix = start
        .checked_add(suffix_off)
        .and_then(|o| read_at(b, o, unicode))
        .unwrap_or_default();
    if suffix.is_empty() {
        Some(base)
    } else {
        Some(format!(
            "{}\\{}",
            base.trim_end_matches('\\'),
            suffix.trim_start_matches('\\')
        ))
    }
}

/// The target hidden in an `EnvironmentVariableDataBlock`, if present.
fn environment_target(b: &[u8], mut p: usize) -> Option<String> {
    loop {
        let size = le_u32(b, p)? as usize;
        // A block smaller than its own header is the documented terminal
        // marker, not corruption.
        if size < 8 {
            return None;
        }
        let end = p.checked_add(size)?;
        if end > b.len() {
            return None;
        }
        if le_u32(b, p + 4)? == SIG_ENVIRONMENT && size >= ENVIRONMENT_BLOCK_SIZE {
            // Unicode copy first: the ANSI field truncates at MAX_PATH in a
            // codepage that may not round-trip.
            let wide = read_at(b, p + 8 + 260, true).unwrap_or_default();
            if !wide.is_empty() {
                return Some(wide);
            }
            let ansi = read_at(b, p + 8, false).unwrap_or_default();
            if !ansi.is_empty() {
                return Some(ansi);
            }
        }
        p = end;
    }
}

/// One StringData item: a `u16` character count, then that many characters.
///
/// Wine counts its terminating NUL in the character count and Windows does not,
/// so the trailing NULs are trimmed rather than trusted either way.
fn read_string(b: &[u8], p: &mut usize, unicode: bool) -> Option<String> {
    let count = le_u16(b, *p)? as usize;
    let bytes = count.checked_mul(if unicode { 2 } else { 1 })?;
    let start = p.checked_add(2)?;
    let end = start.checked_add(bytes)?;
    let slice = b.get(start..end)?;
    *p = end;
    Some(decode(slice, unicode).trim_end_matches('\0').to_string())
}

/// A NUL-terminated string at a fixed offset, as LinkInfo and ExtraData use.
fn read_at(b: &[u8], off: usize, unicode: bool) -> Option<String> {
    let rest = b.get(off..)?;
    if unicode {
        let mut units = Vec::new();
        for pair in rest.chunks_exact(2) {
            let u = u16::from_le_bytes([pair[0], pair[1]]);
            if u == 0 {
                break;
            }
            units.push(u);
        }
        Some(String::from_utf16_lossy(&units))
    } else {
        let len = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        Some(decode(&rest[..len], false))
    }
}

/// Decode a string field. The ANSI case is treated as Latin-1 on purpose: the
/// real encoding is whatever codepage the installer ran under, which the file
/// does not record, and Latin-1 is the one mapping that never fails and never
/// mangles the ASCII that a path is made of.
fn decode(b: &[u8], unicode: bool) -> String {
    if unicode {
        let units: Vec<u16> = b
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        b.iter().map(|&c| c as char).collect()
    }
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

fn le_u16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn le_u32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

// ---------------------------------------------------------------------------
// Scanning the Start Menu
// ---------------------------------------------------------------------------

/// Which Start Menu an entry came from. Kept because "every user" and "this
/// user" are different publishing decisions, exactly as they are in `cxmenu`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    AllUsers,
    User(String),
}

/// One resolved Start Menu shortcut.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The shortcut's own name — `Steam`, not `steam.exe`. This is the name a
    /// person recognises, and the one worth putting on the bundle.
    pub name: String,
    /// Folder path within the menu, e.g. `Programs/Steam`.
    pub menu_path: String,
    /// The `.lnk` itself, on the host.
    pub lnk: PathBuf,
    /// Resolved target, on the host.
    pub target: PathBuf,
    /// Resolved target, still in Windows terms — what `cxstart` wants.
    pub target_windows: String,
    pub working_dir: Option<PathBuf>,
    /// Icon file on the host, only when one actually exists there.
    pub icon: Option<PathBuf>,
    pub arguments: Option<String>,
    pub scope: Scope,
}

impl Entry {
    pub fn exists(&self) -> bool {
        self.target.is_file()
    }

    pub fn is_executable(&self) -> bool {
        self.target
            .extension()
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false)
    }
}

/// The two Start Menu roots Windows defines, for a prefix in either shape.
fn start_menu_roots(prefix: &Path) -> Vec<(PathBuf, Scope)> {
    let Some(c) = drive_c(prefix) else {
        return Vec::new();
    };
    let mut roots = Vec::new();

    let all = c.join("ProgramData/Microsoft/Windows/Start Menu");
    if all.is_dir() {
        roots.push((all, Scope::AllUsers));
    }

    // `users/*` rather than a fixed name: exe-metal names the profile after the
    // host user, CrossOver always calls it `crossover`.
    if let Ok(users) = std::fs::read_dir(c.join("users")) {
        let mut found: Vec<(PathBuf, Scope)> = users
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let menu = e
                    .path()
                    .join("AppData/Roaming/Microsoft/Windows/Start Menu");
                menu.is_dir().then_some((menu, Scope::User(name)))
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        roots.extend(found);
    }
    roots
}

/// Every shortcut in a bottle's Start Menu, resolved to host paths.
///
/// All-users entries are listed before per-user ones and duplicates are
/// dropped, so a program installed for everyone and then pinned by the user
/// yields one entry rather than two identical bundles.
pub fn scan_start_menu(prefix: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut seen: Vec<(String, PathBuf)> = Vec::new();

    for (root, scope) in start_menu_roots(prefix) {
        let mut links = Vec::new();
        collect_lnks(&root, 0, &mut links);
        links.sort();
        for lnk in links {
            let Some(entry) = resolve(prefix, &root, &lnk, &scope) else {
                continue;
            };
            let key = (entry.name.to_lowercase(), entry.target.clone());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            out.push(entry);
        }
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

/// Gather `.lnk` files under a menu root.
///
/// Symlinks are never followed, for the same reason `scan::walk` refuses to:
/// a prefix links `z:` at `/` and a profile's Desktop at the host's, so one
/// followed link turns a menu scan into a walk of the whole disk.
fn collect_lnks(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let p = e.path();
        if ft.is_dir() {
            collect_lnks(&p, depth + 1, out);
        } else if ft.is_file()
            && p.extension()
                .map(|x| x.eq_ignore_ascii_case("lnk"))
                .unwrap_or(false)
        {
            out.push(p);
        }
    }
}

fn resolve(prefix: &Path, root: &Path, lnk: &Path, scope: &Scope) -> Option<Entry> {
    let parsed = read_lnk(lnk)?;

    // Target, in preference order. `relative_path` is last because it is
    // resolved against the `.lnk`'s own directory on the host, which is only
    // correct when the link has not been moved since it was written.
    let (target, target_windows) = if let Some(w) = parsed.target.as_deref() {
        (windows_to_host(prefix, w)?, w.to_string())
    } else if let Some(rel) = parsed.relative_path.as_deref() {
        let mut host = lnk.parent()?.to_path_buf();
        for part in rel.split(['\\', '/']) {
            match part {
                "" | "." => continue,
                ".." => {
                    host.pop();
                }
                p => host.push(p),
            }
        }
        let win = host_to_windows(prefix, &host).unwrap_or_default();
        (host, win)
    } else {
        return None;
    };

    let name = lnk.file_stem()?.to_string_lossy().to_string();
    let menu_path = lnk
        .parent()
        .and_then(|p| p.strip_prefix(root).ok())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let working_dir = parsed
        .working_dir
        .as_deref()
        .and_then(|w| windows_to_host(prefix, w))
        .filter(|p| p.is_dir());

    Some(Entry {
        name,
        menu_path,
        lnk: lnk.to_path_buf(),
        icon: resolve_icon(prefix, parsed.icon.as_deref(), &target),
        target,
        target_windows,
        working_dir,
        arguments: parsed.arguments,
        scope: scope.clone(),
    })
}

/// An icon location is `path` or `path,index`, and the path is often bare
/// (`steam.exe`) meaning "next to the target". Only a file that is really there
/// is returned — a path that does not exist is worse than no icon at all,
/// because the caller cannot tell the difference until the bundle looks broken.
fn resolve_icon(prefix: &Path, icon: Option<&str>, target: &Path) -> Option<PathBuf> {
    let raw = icon?.trim();
    // Split a trailing resource index, but only when what follows is a number:
    // a path may legitimately contain a comma.
    let path = match raw.rsplit_once(',') {
        Some((head, idx)) if idx.trim().parse::<i32>().is_ok() => head,
        _ => raw,
    };
    if path.is_empty() {
        return None;
    }
    let candidate = if path.len() > 1 && path.as_bytes()[1] == b':' {
        windows_to_host(prefix, path)?
    } else {
        target.parent()?.join(path.trim_start_matches(['\\', '/']))
    };
    candidate.is_file().then_some(candidate)
}

/// Host path back to `C:\...`, for entries resolved without a Windows path.
fn host_to_windows(prefix: &Path, host: &Path) -> Option<String> {
    let c = drive_c(prefix)?;
    let rel = host.strip_prefix(&c).ok()?;
    Some(format!("C:\\{}", rel.to_string_lossy().replace('/', "\\")))
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Published {
    pub name: String,
    /// Library id the bundle launches. The bundle is a script calling
    /// `exe-metal launch <id>`, so the entry has to be in the library first.
    pub id: String,
    pub bundle: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Skipped {
    pub name: String,
    pub why: String,
}

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub published: Vec<Published>,
    pub skipped: Vec<Skipped>,
}

/// Decide what is worth a bundle, and say why the rest is not.
///
/// Split out from [`publish`] so the rule is testable without touching
/// `~/Applications` or the real library.
fn triage(entries: &[Entry]) -> (Vec<&Entry>, Vec<Skipped>) {
    let mut keep = Vec::new();
    let mut skipped = Vec::new();
    for e in entries {
        if !e.is_executable() {
            // Start Menus are full of `.chm` help files and `.url` links to a
            // vendor's website. They are real shortcuts; they are not programs.
            skipped.push(Skipped {
                name: e.name.clone(),
                why: format!("target is not an executable: {}", e.target.display()),
            });
        } else if !e.exists() {
            skipped.push(Skipped {
                name: e.name.clone(),
                why: format!("target is missing: {}", e.target.display()),
            });
        } else {
            keep.push(e);
        }
    }
    (keep, skipped)
}

/// Publish Start Menu entries as native `.app` bundles.
///
/// The bundle itself is `shortcut::create`'s job and is not duplicated here;
/// this adds the step that makes it possible — registering each entry in the
/// library, because the generated script launches by library id. Registration
/// is idempotent, so republishing a menu after an update changes nothing.
///
/// One entry failing never aborts the rest: a Start Menu is published as a
/// batch, and losing nine bundles because the tenth had a bad path is the kind
/// of all-or-nothing failure that makes a person stop using the feature.
pub fn publish(
    bottle: &str,
    backend: Backend,
    entries: &[Entry],
    also_desktop: bool,
) -> Result<Report> {
    let (keep, mut skipped) = triage(entries);

    let mut lib = Library::load().context("loading the library")?;
    let mut ids: Vec<(String, String)> = Vec::new();
    for e in &keep {
        let id = lib.add_with(bottle, &candidate_for(e), None, backend);
        ids.push((id, e.name.clone()));
    }
    lib.save().context("saving the library")?;

    let mut published = Vec::new();
    for (id, name) in ids {
        let Some(title) = lib.get(&id) else {
            skipped.push(Skipped {
                name,
                why: "not registered in the library".into(),
            });
            continue;
        };
        match shortcut::create(title, also_desktop) {
            Ok(bundle) => published.push(Published {
                name: title.name.clone(),
                id,
                bundle,
            }),
            Err(err) => skipped.push(Skipped {
                name,
                why: format!("{err}"),
            }),
        }
    }
    Ok(Report { published, skipped })
}

/// Turn an entry into the shape the library indexes.
///
/// `folder` carries the shortcut's name rather than the containing directory's,
/// because `Candidate::display_name` prefers it and the Start Menu name is the
/// better one: `Battle.net`, not `Battle.net Launcher`.
fn candidate_for(e: &Entry) -> scan::Candidate {
    scan::Candidate {
        stem: e
            .target
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| e.name.clone()),
        folder: e.name.clone(),
        root: e
            .target
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| e.target.clone()),
        size: std::fs::metadata(&e.target).map(|m| m.len()).unwrap_or(0),
        path: e.target.clone(),
        kind: scan::Kind::App,
        score: 0,
    }
}

// ---------------------------------------------------------------------------
// File associations
// ---------------------------------------------------------------------------

/// An extension the bottle knows how to open, and what opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assoc {
    /// Lowercased, with the dot: `.exe`.
    pub extension: String,
    /// The ProgID the extension points at: `exefile`.
    pub prog_id: String,
    /// The ProgID's friendly name: `Application`.
    pub description: Option<String>,
    /// MIME type, when the bottle recorded one.
    pub content_type: Option<String>,
    /// The open verb's command line, `%1` placeholders intact.
    pub command: String,
    /// The program out of that command line, mapped to the host — `None` when
    /// the command opens the document itself (`"%1" %*`, which is what `.exe`
    /// resolves to) or names a program that is not in this bottle.
    pub program: Option<PathBuf>,
    /// Raw `DefaultIcon` value, index suffix intact.
    pub icon: Option<String>,
}

/// Read a bottle's file associations out of its registry.
///
/// `user.reg` and `system.reg` are plain UTF-8 text, so no registry API is
/// needed — but they are two hives describing one namespace: both hold
/// `Software\Classes`, and `HKCU` wins, which is why they are merged in that
/// order rather than concatenated.
///
/// Only extensions that reach an actual open command are returned. A bottle
/// lists roughly a hundred extensions and most carry nothing but a MIME type,
/// which names no program and so cannot be associated with anything.
pub fn file_associations(prefix: &Path) -> Vec<Assoc> {
    let Some(root) = wine_root(prefix) else {
        return Vec::new();
    };
    let mut reg = Registry::default();
    // system.reg first so user.reg overwrites it.
    reg.merge_file(&root.join("system.reg"));
    reg.merge_file(&root.join("user.reg"));

    let mut out = Vec::new();
    for (key, values) in &reg.keys {
        // `software\classes\.ext` and nothing deeper.
        let Some(ext) = key.strip_prefix("software\\classes\\") else {
            continue;
        };
        if !ext.starts_with('.') || ext.contains('\\') {
            continue;
        }
        let Some(prog_id) = values.get("@").filter(|v| !v.is_empty()) else {
            continue;
        };
        // Case varies between installers — this bottle has both
        // `shell\open\command` and `Shell\Open\Command` — so keys are matched
        // lowercased throughout.
        let cmd_key = format!(
            "software\\classes\\{}\\shell\\open\\command",
            prog_id.to_lowercase()
        );
        let Some(command) = reg.default_value(&cmd_key).filter(|c| !c.is_empty()) else {
            continue;
        };
        let prog_key = format!("software\\classes\\{}", prog_id.to_lowercase());
        out.push(Assoc {
            extension: ext.to_string(),
            prog_id: prog_id.clone(),
            description: reg.default_value(&prog_key).filter(|s| !s.is_empty()),
            content_type: values.get("content type").cloned(),
            program: command_program(&command).and_then(|p| windows_to_host(prefix, &p)),
            icon: reg.default_value(&format!("{prog_key}\\defaulticon")),
            command,
        });
    }
    out.sort_by(|a, b| a.extension.cmp(&b.extension));
    out
}

/// The executable out of a command line, still in Windows terms.
///
/// `None` when the first token is a placeholder: `.exe` maps to `"%1" %*`,
/// meaning "run the document", and treating `%1` as a path produces a host path
/// that cannot exist.
fn command_program(cmd: &str) -> Option<String> {
    let cmd = cmd.trim();
    let first = if let Some(rest) = cmd.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        cmd.split_whitespace().next()?
    };
    if first.is_empty() || first.contains('%') {
        return None;
    }
    Some(first.to_string())
}

/// The slice of a Wine `.reg` file this module cares about.
///
/// Keys and value names are lowercased on the way in; the default value is
/// stored under `@`, as the file writes it.
#[derive(Debug, Default)]
struct Registry {
    keys: BTreeMap<String, BTreeMap<String, String>>,
}

impl Registry {
    fn default_value(&self, key: &str) -> Option<String> {
        self.keys.get(key)?.get("@").cloned()
    }

    fn merge_file(&mut self, path: &Path) {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        // Lossy on purpose: one mangled byte somewhere in a 64 000-line hive
        // must not cost the whole association table.
        self.merge(&String::from_utf8_lossy(&bytes));
    }

    fn merge(&mut self, text: &str) {
        let mut key: Option<String> = None;
        let mut lines = text.lines().peekable();
        while let Some(raw) = lines.next() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let name = rest.rsplit_once(']').map(|(k, _)| k).unwrap_or(rest);
                let unescaped = unescape(name);
                // Only `Software\Classes` is ever read here, and skipping the
                // rest keeps a 4 MB system.reg from becoming a 4 MB map.
                key = unescaped
                    .to_lowercase()
                    .starts_with("software\\classes\\")
                    .then(|| unescaped.to_lowercase());
                continue;
            }
            let Some(k) = key.clone() else { continue };

            // A trailing backslash continues a long value onto the next line.
            let mut joined = line.to_string();
            while joined.ends_with('\\') {
                joined.pop();
                match lines.next() {
                    Some(next) => joined.push_str(next.trim()),
                    None => break,
                }
            }
            if let Some((name, value)) = parse_value_line(&joined) {
                self.keys
                    .entry(k)
                    .or_default()
                    .insert(name.to_lowercase(), value);
            }
        }
    }
}

/// `"name"="value"` or `@="value"`, with the right-hand side decoded.
fn parse_value_line(line: &str) -> Option<(String, String)> {
    let (name, rhs) = if let Some(rest) = line.strip_prefix("@=") {
        ("@".to_string(), rest)
    } else {
        let (name, used) = read_quoted(line.strip_prefix('"')?)?;
        (name, line.get(1 + used..)?.strip_prefix('=')?)
    };
    Some((name, parse_reg_value(rhs)?))
}

/// Decode a registry value. `None` for the types that are not text — a `dword`
/// or a raw `hex:` blob is not a command line, and turning one into a string
/// would put nonsense in the association table.
fn parse_reg_value(rhs: &str) -> Option<String> {
    let rhs = rhs.trim();
    if let Some(rest) = rhs.strip_prefix('"') {
        return read_quoted(rest).map(|(s, _)| s);
    }
    // `str(2):"..."` is REG_EXPAND_SZ; the variables are left unexpanded
    // because expanding them needs the bottle's own environment block.
    for p in ["str(2):", "str(1):", "str:"] {
        if let Some(rest) = rhs.strip_prefix(p) {
            return read_quoted(rest.strip_prefix('"')?).map(|(s, _)| s);
        }
    }
    // `hex(1)`/`hex(2)` are the same two types written as UTF-16LE bytes, which
    // is how Wine stores a value containing characters it will not quote. The
    // empty form (`"URL Protocol"=hex(1):`) is a real, valid, empty string.
    for p in ["hex(1):", "hex(2):"] {
        if let Some(rest) = rhs.strip_prefix(p) {
            let mut units = Vec::new();
            let bytes: Vec<u8> = rest
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .filter_map(|s| u8::from_str_radix(s.trim(), 16).ok())
                .collect();
            for pair in bytes.chunks_exact(2) {
                units.push(u16::from_le_bytes([pair[0], pair[1]]));
            }
            let s = String::from_utf16_lossy(&units);
            return Some(s.trim_end_matches('\0').to_string());
        }
    }
    None
}

/// Read a quoted string starting just after its opening quote. Returns the
/// unescaped contents and how many bytes were consumed, closing quote included.
fn read_quoted(s: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '\\' => match it.next() {
                Some((_, esc)) => out.push(esc),
                None => return None,
            },
            '"' => return Some((out, i + 1)),
            _ => out.push(c),
        }
    }
    None
}

/// Registry key names are written with every backslash doubled.
fn unescape(s: &str) -> String {
    s.replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- fixtures ----------------------------------------------------------

    /// Build a Shell Link byte for byte, so the parser is tested against the
    /// format rather than against one file that happened to be on this disk.
    #[derive(Default)]
    struct Fixture {
        id_list: Option<Vec<u8>>,
        local_base: Option<String>,
        description: Option<String>,
        relative: Option<String>,
        working: Option<String>,
        arguments: Option<String>,
        icon: Option<String>,
        env_target: Option<String>,
    }

    impl Fixture {
        fn build(&self) -> Vec<u8> {
            let mut flags = IS_UNICODE;
            if self.id_list.is_some() {
                flags |= HAS_ID_LIST;
            }
            if self.local_base.is_some() {
                flags |= HAS_LINK_INFO;
            }
            for (f, s) in [
                (HAS_NAME, &self.description),
                (HAS_RELATIVE_PATH, &self.relative),
                (HAS_WORKING_DIR, &self.working),
                (HAS_ARGUMENTS, &self.arguments),
                (HAS_ICON_LOCATION, &self.icon),
            ] {
                if s.is_some() {
                    flags |= f;
                }
            }

            let mut b = Vec::new();
            b.extend((HEADER_SIZE as u32).to_le_bytes());
            b.extend(LINK_CLSID);
            b.extend(flags.to_le_bytes());
            b.resize(HEADER_SIZE, 0);

            if let Some(ids) = &self.id_list {
                b.extend((ids.len() as u16 + 2).to_le_bytes());
                b.extend(ids);
                b.extend([0u8, 0]); // TerminalID
            }
            if let Some(base) = &self.local_base {
                b.extend(link_info(base));
            }
            for s in [
                &self.description,
                &self.relative,
                &self.working,
                &self.arguments,
                &self.icon,
            ]
            .into_iter()
            .flatten()
            {
                // Wine's own convention: NUL-terminated and counted.
                let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
                b.extend((units.len() as u16).to_le_bytes());
                for u in units {
                    b.extend(u.to_le_bytes());
                }
            }
            if let Some(t) = &self.env_target {
                let mut block = vec![0u8; ENVIRONMENT_BLOCK_SIZE];
                block[0..4].copy_from_slice(&(ENVIRONMENT_BLOCK_SIZE as u32).to_le_bytes());
                block[4..8].copy_from_slice(&SIG_ENVIRONMENT.to_le_bytes());
                let mut off = 8 + 260;
                for u in t.encode_utf16() {
                    block[off..off + 2].copy_from_slice(&u.to_le_bytes());
                    off += 2;
                }
                b.extend(block);
            }
            b.extend(0u32.to_le_bytes()); // terminal block
            b
        }
    }

    /// A LinkInfo carrying only a LocalBasePath, which is the shape Wine and
    /// most installers write.
    fn link_info(local_base: &str) -> Vec<u8> {
        let header = 0x1Cu32;
        let base_off = header;
        let suffix_off = base_off + local_base.len() as u32 + 1;
        let total = suffix_off + 1;
        let mut v = Vec::new();
        v.extend(total.to_le_bytes());
        v.extend(header.to_le_bytes());
        v.extend(1u32.to_le_bytes()); // VolumeIDAndLocalBasePath
        v.extend(0u32.to_le_bytes()); // VolumeIDOffset
        v.extend(base_off.to_le_bytes());
        v.extend(0u32.to_le_bytes()); // CommonNetworkRelativeLinkOffset
        v.extend(suffix_off.to_le_bytes());
        v.extend(local_base.as_bytes());
        v.push(0);
        v.push(0); // empty CommonPathSuffix
        v
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("em-menu-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn write(p: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    // -- the parser --------------------------------------------------------

    #[test]
    fn link_info_gives_up_the_target_and_the_working_directory() {
        let f = Fixture {
            local_base: Some("C:\\Program Files (x86)\\Steam\\steam.exe".into()),
            working: Some("C:\\Program Files (x86)\\Steam".into()),
            ..Default::default()
        };
        let lnk = parse_lnk(&f.build()).expect("well-formed link must parse");
        assert_eq!(
            lnk.target.as_deref(),
            Some("C:\\Program Files (x86)\\Steam\\steam.exe")
        );
        assert_eq!(
            lnk.working_dir.as_deref(),
            Some("C:\\Program Files (x86)\\Steam")
        );
    }

    #[test]
    fn a_target_id_list_is_skipped_by_its_own_size() {
        // The real Steam.lnk carries 263 bytes of shell item chain before the
        // LinkInfo. Reading it needs the Windows shell namespace; stepping over
        // it needs only the size field, and getting that wrong lands the parser
        // mid-structure and yields a garbage path rather than an error.
        let f = Fixture {
            id_list: Some(vec![0xAB; 261]),
            local_base: Some("C:\\Games\\Celeste\\Celeste.exe".into()),
            arguments: Some("--windowed".into()),
            ..Default::default()
        };
        let lnk = parse_lnk(&f.build()).unwrap();
        assert_eq!(
            lnk.target.as_deref(),
            Some("C:\\Games\\Celeste\\Celeste.exe")
        );
        assert_eq!(lnk.arguments.as_deref(), Some("--windowed"));
    }

    #[test]
    fn strings_arrive_in_flag_order_not_in_the_order_they_are_wanted() {
        // Misordering these silently swaps the working directory for the icon.
        let f = Fixture {
            local_base: Some("C:\\App\\app.exe".into()),
            description: Some("the app".into()),
            relative: Some("..\\..\\App\\app.exe".into()),
            working: Some("C:\\App".into()),
            arguments: Some("-q".into()),
            icon: Some("C:\\App\\app.exe,0".into()),
            ..Default::default()
        };
        let lnk = parse_lnk(&f.build()).unwrap();
        assert_eq!(lnk.description.as_deref(), Some("the app"));
        assert_eq!(lnk.relative_path.as_deref(), Some("..\\..\\App\\app.exe"));
        assert_eq!(lnk.working_dir.as_deref(), Some("C:\\App"));
        assert_eq!(lnk.arguments.as_deref(), Some("-q"));
        assert_eq!(lnk.icon.as_deref(), Some("C:\\App\\app.exe,0"));
    }

    #[test]
    fn the_environment_block_is_the_fallback_when_there_is_no_link_info() {
        let f = Fixture {
            env_target: Some("C:\\Windows\\notepad.exe".into()),
            ..Default::default()
        };
        let lnk = parse_lnk(&f.build()).unwrap();
        assert_eq!(lnk.target.as_deref(), Some("C:\\Windows\\notepad.exe"));
    }

    #[test]
    fn malformed_links_return_none_and_never_panic() {
        let good = Fixture {
            local_base: Some("C:\\App\\app.exe".into()),
            working: Some("C:\\App".into()),
            ..Default::default()
        }
        .build();

        assert!(parse_lnk(&[]).is_none(), "empty file");
        assert!(parse_lnk(&[0u8; 75]).is_none(), "shorter than the header");
        assert!(parse_lnk(&[0u8; 200]).is_none(), "zeroed, wrong CLSID");

        let mut wrong_clsid = good.clone();
        wrong_clsid[4] = 0xFF;
        assert!(parse_lnk(&wrong_clsid).is_none(), "not a Shell Link");

        let mut wrong_header = good.clone();
        wrong_header[0] = 0x4D;
        assert!(
            parse_lnk(&wrong_header).is_none(),
            "header size must be 0x4C"
        );

        // An ID list that claims to run past the end of the file. This is the
        // one that matters: trusting it is an out-of-bounds read.
        let mut huge_ids = Fixture {
            id_list: Some(vec![0; 4]),
            local_base: Some("C:\\App\\app.exe".into()),
            ..Default::default()
        }
        .build();
        huge_ids[HEADER_SIZE..HEADER_SIZE + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        assert!(parse_lnk(&huge_ids).is_none(), "oversized LinkTargetIDList");

        // A LinkInfo whose size field is nonsense.
        let mut bad_info = good.clone();
        bad_info[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert!(parse_lnk(&bad_info).is_none(), "oversized LinkInfo");

        // Truncation at every length: none of these may panic.
        for n in 0..good.len() {
            let _ = parse_lnk(&good[..n]);
        }
        // Nor may arbitrary bytes behind a valid header.
        let mut noise = good.clone();
        for (i, b) in noise.iter_mut().enumerate().skip(HEADER_SIZE) {
            *b = (i as u8).wrapping_mul(37);
        }
        let _ = parse_lnk(&noise);
    }

    #[test]
    fn a_corrupt_lnk_costs_one_entry_not_the_scan() {
        let pfx = scratch("corrupt");
        let menu = pfx.join("drive_c/ProgramData/Microsoft/Windows/Start Menu/Programs");
        write(&pfx.join("drive_c/App/app.exe"), b"MZ");
        write(
            &menu.join("Good.lnk"),
            &Fixture {
                local_base: Some("C:\\App\\app.exe".into()),
                ..Default::default()
            }
            .build(),
        );
        write(&menu.join("Broken.lnk"), b"not a shell link at all, sorry");

        let found = scan_start_menu(&pfx);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].name, "Good");
    }

    // -- path mapping ------------------------------------------------------

    #[test]
    fn both_bottle_shapes_resolve_to_the_same_host_path() {
        let crossover = scratch("co");
        std::fs::create_dir_all(crossover.join("drive_c/App")).unwrap();
        assert_eq!(
            windows_to_host(&crossover, "C:\\App\\app.exe").unwrap(),
            crossover.join("drive_c/App/app.exe")
        );

        // exe-metal nests the prefix under `pfx/`, and the bottle directory is
        // what a caller usually has in hand.
        let exemetal = scratch("em");
        std::fs::create_dir_all(exemetal.join("pfx/drive_c/App")).unwrap();
        assert_eq!(
            windows_to_host(&exemetal, "C:\\App\\app.exe").unwrap(),
            exemetal.join("pfx/drive_c/App/app.exe")
        );
        assert_eq!(
            windows_to_host(&exemetal.join("pfx"), "C:\\App\\app.exe").unwrap(),
            exemetal.join("pfx/drive_c/App/app.exe")
        );
    }

    #[test]
    fn other_drives_go_through_dosdevices() {
        let pfx = scratch("drives");
        std::fs::create_dir_all(pfx.join("drive_c")).unwrap();
        std::fs::create_dir_all(pfx.join("dosdevices")).unwrap();
        // This machine really does keep games on an external `e:`; without the
        // dosdevices lookup those shortcuts resolve to nothing.
        std::os::unix::fs::symlink("/Volumes/Games", pfx.join("dosdevices/e:")).unwrap();
        assert_eq!(
            windows_to_host(&pfx, "E:\\Cyberpunk\\game.exe").unwrap(),
            PathBuf::from("/Volumes/Games/Cyberpunk/game.exe")
        );
        assert!(
            windows_to_host(&pfx, "Q:\\nope.exe").is_none(),
            "an unmapped drive has no host path"
        );
        assert!(
            windows_to_host(&pfx, "\\\\server\\share\\x.exe").is_none(),
            "a UNC share has no host path"
        );
        assert!(windows_to_host(&pfx, "not a path").is_none());
    }

    #[test]
    fn dot_dot_cannot_walk_out_of_the_bottle() {
        let pfx = scratch("escape");
        std::fs::create_dir_all(pfx.join("drive_c")).unwrap();
        let got = windows_to_host(&pfx, "C:\\..\\..\\..\\etc\\passwd").unwrap();
        assert!(
            !got.to_string_lossy().contains(".."),
            "must be normalised, got {}",
            got.display()
        );
    }

    // -- scanning ----------------------------------------------------------

    #[test]
    fn both_start_menu_roots_are_read_and_duplicates_collapse() {
        // The real Steam bottle has Steam.lnk in ProgramData *and* in the user
        // profile, both pointing at the same exe. Publishing both would put two
        // bundles of the same name in ~/Applications.
        let pfx = scratch("dupes");
        write(&pfx.join("drive_c/Steam/steam.exe"), b"MZ");
        let lnk = Fixture {
            local_base: Some("C:\\Steam\\steam.exe".into()),
            ..Default::default()
        }
        .build();
        write(
            &pfx.join("drive_c/ProgramData/Microsoft/Windows/Start Menu/Programs/Steam/Steam.lnk"),
            &lnk,
        );
        write(
            &pfx.join(
                "drive_c/users/navilan/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Steam/Steam.lnk",
            ),
            &lnk,
        );

        let found = scan_start_menu(&pfx);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].name, "Steam");
        assert_eq!(found[0].menu_path, "Programs/Steam");
        assert_eq!(found[0].scope, Scope::AllUsers, "all-users wins the tie");
        assert!(found[0].exists());
    }

    #[test]
    fn symlinks_inside_the_menu_are_not_followed() {
        // `z:` is `/`, and a profile's Desktop is the host's. One followed link
        // turns a menu scan into a walk of the whole disk.
        let pfx = scratch("menulink");
        let menu = pfx.join("drive_c/ProgramData/Microsoft/Windows/Start Menu/Programs");
        std::fs::create_dir_all(&menu).unwrap();
        std::os::unix::fs::symlink("/", menu.join("Everything")).unwrap();
        assert!(scan_start_menu(&pfx).is_empty());
    }

    #[test]
    fn an_icon_is_reported_only_when_the_file_is_really_there() {
        let pfx = scratch("icon");
        write(&pfx.join("drive_c/App/app.exe"), b"MZ");
        write(&pfx.join("drive_c/App/app.ico"), b"\0\0\x01\0");
        let menu = pfx.join("drive_c/ProgramData/Microsoft/Windows/Start Menu");
        write(
            &menu.join("Real.lnk"),
            &Fixture {
                local_base: Some("C:\\App\\app.exe".into()),
                icon: Some("app.ico,0".into()),
                ..Default::default()
            }
            .build(),
        );
        write(
            &menu.join("Ghost.lnk"),
            &Fixture {
                local_base: Some("C:\\App\\app.exe".into()),
                icon: Some("C:\\Windows\\system32\\shell32.dll,-16".into()),
                ..Default::default()
            }
            .build(),
        );
        let found = scan_start_menu(&pfx);
        let real = found.iter().find(|e| e.name == "Real").unwrap();
        assert_eq!(
            real.icon.as_deref(),
            Some(pfx.join("drive_c/App/app.ico").as_path())
        );
        let ghost = found.iter().find(|e| e.name == "Ghost").unwrap();
        assert!(ghost.icon.is_none(), "a missing icon must not be reported");
    }

    #[test]
    fn triage_separates_programs_from_help_files_and_dead_links() {
        let pfx = scratch("triage");
        write(&pfx.join("drive_c/App/app.exe"), b"MZ");
        write(&pfx.join("drive_c/App/help.chm"), b"ITSF");
        let menu = pfx.join("drive_c/ProgramData/Microsoft/Windows/Start Menu");
        for (name, target) in [
            ("App", "C:\\App\\app.exe"),
            ("Help", "C:\\App\\help.chm"),
            ("Gone", "C:\\App\\removed.exe"),
        ] {
            write(
                &menu.join(format!("{name}.lnk")),
                &Fixture {
                    local_base: Some(target.into()),
                    ..Default::default()
                }
                .build(),
            );
        }
        let entries = scan_start_menu(&pfx);
        assert_eq!(entries.len(), 3);
        let (keep, skipped) = triage(&entries);
        assert_eq!(keep.len(), 1);
        assert_eq!(keep[0].name, "App");
        assert_eq!(skipped.len(), 2);
        assert!(skipped.iter().any(|s| s.why.contains("not an executable")));
        assert!(skipped.iter().any(|s| s.why.contains("missing")));
    }

    #[test]
    fn a_prefix_with_no_start_menu_is_empty_rather_than_an_error() {
        assert!(scan_start_menu(Path::new("/definitely/not/a/bottle")).is_empty());
        assert!(file_associations(Path::new("/definitely/not/a/bottle")).is_empty());
    }

    // -- the registry ------------------------------------------------------

    fn assoc_prefix(tag: &str, system: &str, user: &str) -> PathBuf {
        let pfx = scratch(tag);
        std::fs::create_dir_all(pfx.join("drive_c")).unwrap();
        std::fs::write(pfx.join("system.reg"), system).unwrap();
        std::fs::write(pfx.join("user.reg"), user).unwrap();
        pfx
    }

    #[test]
    fn associations_join_an_extension_to_the_command_that_opens_it() {
        let system = r#"WINE REGISTRY Version 2
;; All keys relative to \\Machine

[Software\\Classes\\.exe] 1785813034
#time=1dd23bed1156f7a
@="exefile"
"Content Type"="application/x-msdownload"

[Software\\Classes\\exefile] 1785567901
@="Application"

[Software\\Classes\\exefile\\DefaultIcon] 1785567901
@="%1"

[Software\\Classes\\exefile\\shell\\open\\command] 1785567901
@="\"%1\" %*"

[Software\\Classes\\.txt] 1785567901
@="txtfile"

[Software\\Classes\\txtfile\\shell\\open\\command] 1785567901
@=str(2):"C:\\windows\\notepad.exe %1"

[Software\\Classes\\.gz] 1785813034
"Content Type"="application/x-gzip"
"#;
        let pfx = assoc_prefix("assoc", system, "WINE REGISTRY Version 2\n");
        std::fs::create_dir_all(pfx.join("drive_c/windows")).unwrap();
        std::fs::write(pfx.join("drive_c/windows/notepad.exe"), b"MZ").unwrap();

        let a = file_associations(&pfx);
        let exts: Vec<&str> = a.iter().map(|x| x.extension.as_str()).collect();
        // `.gz` names a MIME type and no program, so it is not an association.
        assert_eq!(exts, vec![".exe", ".txt"], "got {exts:?}");

        let exe = &a[0];
        assert_eq!(exe.prog_id, "exefile");
        assert_eq!(exe.description.as_deref(), Some("Application"));
        assert_eq!(
            exe.content_type.as_deref(),
            Some("application/x-msdownload")
        );
        assert_eq!(exe.command, "\"%1\" %*");
        assert!(exe.program.is_none(), "`%1` is the document, not a program");

        let txt = &a[1];
        assert_eq!(txt.command, "C:\\windows\\notepad.exe %1");
        assert_eq!(
            txt.program.as_deref(),
            Some(pfx.join("drive_c/windows/notepad.exe").as_path())
        );
    }

    #[test]
    fn the_user_hive_overrides_the_machine_hive() {
        // Both files describe `Software\Classes`; HKCU wins. Concatenating them
        // in the other order would hand back the stock Wine handler instead of
        // the one the installed program registered.
        let system = "WINE REGISTRY Version 2\n\n[Software\\\\Classes\\\\.log] 1\n@=\"txtfile\"\n\n[Software\\\\Classes\\\\txtfile\\\\shell\\\\open\\\\command] 1\n@=\"C:\\\\windows\\\\notepad.exe\"\n";
        let user = "WINE REGISTRY Version 2\n\n[Software\\\\Classes\\\\txtfile\\\\shell\\\\open\\\\command] 2\n@=\"C:\\\\Tools\\\\editor.exe\"\n";
        let pfx = assoc_prefix("hives", system, user);
        let a = file_associations(&pfx);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].command, "C:\\Tools\\editor.exe");
    }

    #[test]
    fn registry_key_case_and_value_encodings_are_all_handled() {
        // Every shape below is taken from a real bottle: Steam writes
        // `Shell\Open\Command` with capitals and `hex(1):` for an empty string,
        // while Wine writes `str(2):` for an expandable path.
        let system = r#"WINE REGISTRY Version 2

[Software\\Classes\\.url] 1
@="InternetShortcut"

[Software\\Classes\\InternetShortcut\\Shell\\Open\\Command] 1
@="\"C:\\Program Files (x86)\\Steam\\steam.exe\" -- \"%1\""

[Software\\Classes\\InternetShortcut] 1
@=hex(1):

[Software\\Classes\\.cfg] 1
@="cfgfile"

[Software\\Classes\\cfgfile\\shell\\open\\command] 1
@=hex(2):43,00,3a,00,5c,00,78,00,2e,00,65,00,78,00,65,00,00,00

[Software\\Classes\\.bin] 1
@="binfile"

[Software\\Classes\\binfile\\shell\\open\\command] 1
@=dword:00000001
"#;
        let pfx = assoc_prefix("encodings", system, "WINE REGISTRY Version 2\n");
        let a = file_associations(&pfx);
        let by_ext: BTreeMap<&str, &Assoc> = a.iter().map(|x| (x.extension.as_str(), x)).collect();

        let url = by_ext
            .get(".url")
            .expect("capitalised key must still match");
        assert_eq!(
            url.command,
            "\"C:\\Program Files (x86)\\Steam\\steam.exe\" -- \"%1\""
        );
        assert_eq!(
            url.program.as_deref(),
            windows_to_host(&pfx, "C:\\Program Files (x86)\\Steam\\steam.exe").as_deref()
        );
        // An empty hex(1) default is a real empty string, so the ProgID has no
        // description — but it must not blow away the association.
        assert!(url.description.is_none());

        assert_eq!(by_ext.get(".cfg").unwrap().command, "C:\\x.exe");
        // A dword is not a command line and must not be invented into one.
        assert!(!by_ext.contains_key(".bin"));
    }

    #[test]
    fn a_continued_hex_value_is_rejoined_before_it_is_decoded() {
        let system = "WINE REGISTRY Version 2\n\n[Software\\\\Classes\\\\.q] 1\n@=\"qfile\"\n\n[Software\\\\Classes\\\\qfile\\\\shell\\\\open\\\\command] 1\n@=hex(2):43,00,3a,00,5c,00,\\\n  71,00,2e,00,65,00,78,00,65,00,00,00\n";
        let pfx = assoc_prefix("continued", system, "WINE REGISTRY Version 2\n");
        let a = file_associations(&pfx);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].command, "C:\\q.exe");
    }

    #[test]
    fn command_program_ignores_placeholders_and_respects_quotes() {
        assert_eq!(
            command_program("\"C:\\Program Files\\A B\\a.exe\" -- \"%1\"").as_deref(),
            Some("C:\\Program Files\\A B\\a.exe")
        );
        assert_eq!(
            command_program("C:\\windows\\notepad.exe %1").as_deref(),
            Some("C:\\windows\\notepad.exe")
        );
        assert!(command_program("\"%1\" %*").is_none());
        assert!(command_program("").is_none());
    }

    // -- the bottles on this machine ---------------------------------------

    /// Runs against the real bottles when they are present and is silent when
    /// they are not, so it is a live check here and a no-op on CI. It asserts
    /// only what must hold for *any* bottle: nothing panics, and everything it
    /// claims to have resolved is really on disk.
    #[test]
    fn real_bottles_scan_without_surprises() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let support = PathBuf::from(home).join("Library/Application Support");
        for bottle in [
            support.join("CrossOver/Bottles/ow"),
            support.join("exe-metal/bottles/cosmos-steam/pfx"),
        ] {
            if !bottle.join("drive_c").is_dir() {
                continue;
            }
            for e in scan_start_menu(&bottle) {
                assert!(e.lnk.is_file(), "{} vanished", e.lnk.display());
                assert!(
                    e.target_windows.is_empty() || e.target_windows.contains(':'),
                    "{} resolved to a path with no drive: {:?}",
                    e.name,
                    e.target_windows
                );
                if let Some(w) = &e.working_dir {
                    assert!(w.is_dir(), "{} working dir is not a directory", e.name);
                }
                if let Some(i) = &e.icon {
                    assert!(i.is_file(), "{} icon is not a file", e.name);
                }
            }
            for a in file_associations(&bottle) {
                assert!(a.extension.starts_with('.'));
                assert!(!a.command.is_empty());
                assert!(!a.prog_id.is_empty());
            }
        }
    }
}
