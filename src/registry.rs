//! The Wine registry, for both backends — the `regedit` / `regsvr32` /
//! `cxgetsysinfo` surface CrossOver exposes and this tool did not have.
//!
//! Reads and writes are deliberately asymmetric, and that asymmetry is the
//! entire reason this module exists rather than a pair of `Command` calls.
//!
//! **Reads parse the `.reg` text directly.** Wine's hives are plain UTF-8:
//! `user.reg` is HKEY_CURRENT_USER, `system.reg` is HKEY_LOCAL_MACHINE,
//! `userdef.reg` is HKEY_USERS\.Default. Parsing them costs no process launch,
//! answers in microseconds instead of the second-and-change a `wine reg.exe
//! query` takes, and still works when the engine is missing, half-downloaded
//! or built for the other backend. A diagnostic that only works on a healthy
//! bottle is worthless precisely when it is wanted.
//!
//! **Writes never touch that text.** wineserver keeps the whole registry in
//! memory for as long as any process in the prefix is alive and rewrites all
//! three files from memory when the last one exits. Hand-editing `user.reg`
//! while a bottle is running therefore *appears* to work — the bytes on disk
//! are exactly right — and is silently thrown away when the game quits, twenty
//! minutes later, with no error anywhere. So every mutation here is handed to
//! `reg.exe` inside the prefix and it is wineserver that writes.
//!
//! Layouts differ between the backends and nothing else does: exe-metal keeps
//! the prefix at `bottles/<name>/pfx/`, CrossOver puts `drive_c` and the three
//! hives straight in `Bottles/<name>/`. `prefix_of` is the only place that
//! knows.

// Every entry point here is a capability the CLI has not been wired to yet.
// Marking them individually would be six identical attributes, and making them
// `pub(crate)` would not help — an unused `pub` item in a binary crate is dead
// either way.
#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bottle::Bottle;
use crate::crossover;
use crate::library::Backend;
use crate::paths::Layout;

// ---------------------------------------------------------------------------
// Hives
// ---------------------------------------------------------------------------

/// Which of the three files on disk a key lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    /// `user.reg` — HKEY_CURRENT_USER.
    CurrentUser,
    /// `system.reg` — HKEY_LOCAL_MACHINE.
    LocalMachine,
    /// `userdef.reg` — HKEY_USERS\.Default.
    Users,
}

impl Hive {
    pub fn file(self) -> &'static str {
        match self {
            Hive::CurrentUser => "user.reg",
            Hive::LocalMachine => "system.reg",
            Hive::Users => "userdef.reg",
        }
    }

    /// The abbreviation `reg.exe` accepts and this module prints.
    pub fn short(self) -> &'static str {
        match self {
            Hive::CurrentUser => "HKCU",
            Hive::LocalMachine => "HKLM",
            Hive::Users => "HKU",
        }
    }
}

/// Search order when a key is given without a hive prefix.
///
/// HKCU first because everything a user or this tool tunes — the Mac driver,
/// Direct3D, DLL overrides, the virtual desktop — lives there, and a
/// same-named key under HKLM is almost always the stale installer copy.
const SEARCH_ORDER: [Hive; 3] = [Hive::CurrentUser, Hive::LocalMachine, Hive::Users];

/// Split `HKLM\Software\...` into the hive and the path below it.
///
/// An unprefixed key yields `None`, which means "try each hive in
/// `SEARCH_ORDER`".
fn split_hive(key: &str) -> (Option<Hive>, String) {
    let key = key.trim().trim_matches('\\');
    let (head, rest) = match key.find('\\') {
        Some(i) => (&key[..i], &key[i + 1..]),
        None => (key, ""),
    };
    match head.to_ascii_uppercase().as_str() {
        "HKEY_CURRENT_USER" | "HKCU" => (Some(Hive::CurrentUser), rest.to_string()),
        "HKEY_LOCAL_MACHINE" | "HKLM" => (Some(Hive::LocalMachine), rest.to_string()),
        "HKEY_USERS" | "HKU" => (Some(Hive::Users), rest.to_string()),
        // HKCR is not a hive, it is a view onto HKLM\Software\Classes, and Wine
        // stores it there and nowhere else. Rewriting is the only way a caller
        // spelling it the Windows way finds anything.
        "HKEY_CLASSES_ROOT" | "HKCR" => (
            Some(Hive::LocalMachine),
            format!("Software\\Classes\\{rest}")
                .trim_end_matches('\\')
                .to_string(),
        ),
        _ => (None, key.to_string()),
    }
}

/// The registry is case-insensitive; the file is not written in any one case.
fn key_eq(a: &str, b: &str) -> bool {
    a.trim_matches('\\')
        .eq_ignore_ascii_case(b.trim_matches('\\'))
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A decoded registry value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Sz(String),
    /// `str(2):` — REG_EXPAND_SZ, still holding `%USERPROFILE%` unexpanded.
    ExpandSz(String),
    /// `str(7):` — REG_MULTI_SZ, NUL-separated in the file.
    MultiSz(Vec<String>),
    Dword(u32),
    /// `hex(b):` — REG_QWORD, little-endian in the file.
    Qword(u64),
    /// Anything else, with the `hex(N)` type number kept so a caller can tell
    /// REG_BINARY from a type this module chose not to decode.
    Binary {
        kind: u32,
        bytes: Vec<u8>,
    },
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Sz(_) => "REG_SZ",
            Value::ExpandSz(_) => "REG_EXPAND_SZ",
            Value::MultiSz(_) => "REG_MULTI_SZ",
            Value::Dword(_) => "REG_DWORD",
            Value::Qword(_) => "REG_QWORD",
            Value::Binary { .. } => "REG_BINARY",
        }
    }

    /// One-line rendering. Numbers come out in decimal rather than `reg
    /// query`'s `0x0`, because every caller of `get` is comparing against a
    /// number and not against a formatting convention.
    pub fn display(&self) -> String {
        match self {
            Value::Sz(s) | Value::ExpandSz(s) => s.clone(),
            Value::MultiSz(v) => v.join("; "),
            Value::Dword(n) => n.to_string(),
            Value::Qword(n) => n.to_string(),
            Value::Binary { bytes, .. } => {
                // A DigitalProductId is 164 bytes of zeroes. Printing it in
                // full turns a diagnostic dump into a wall.
                let head: Vec<String> = bytes.iter().take(16).map(|b| format!("{b:02x}")).collect();
                if bytes.len() > 16 {
                    format!("{} … ({} bytes)", head.join(" "), bytes.len())
                } else {
                    head.join(" ")
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Undo the escaping Wine applies when it writes a quoted string.
///
/// The doubled backslash is the one that matters: `"C:\\windows"` in the file
/// is the four characters `C:\w…`, and a parser that skips this step hands
/// back paths that no Windows program will ever match.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('f') => out.push('\x0c'),
            Some('v') => out.push('\x0b'),
            // Wine writes any non-ASCII character as \xNNNN. Fewer than four
            // digits is malformed rather than fatal: take what is there.
            Some('x') => {
                let mut n: u32 = 0;
                let mut got = 0;
                while got < 4 {
                    match it.peek().and_then(|c| c.to_digit(16)) {
                        Some(d) => {
                            n = n * 16 + d;
                            it.next();
                            got += 1;
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(n).unwrap_or('\u{fffd}'));
            }
            Some(other) => out.push(other),
            // A line ending in a lone backslash. Keep it rather than dropping a
            // character the file genuinely contains.
            None => out.push('\\'),
        }
    }
    out
}

/// Read a quoted, escaped string starting at byte 0, returning it and the rest
/// of the line. `None` when the quote is never closed — a truncated hive, which
/// must be skipped rather than panicked on.
fn quoted(s: &str) -> Option<(String, &str)> {
    let b = s.as_bytes();
    if b.first() != Some(&b'"') {
        return None;
    }
    let mut esc = false;
    for (i, &c) in b.iter().enumerate().skip(1) {
        match c {
            b'\\' if !esc => esc = true,
            _ if esc => esc = false,
            b'"' => return Some((unescape(&s[1..i]), &s[i + 1..])),
            _ => {}
        }
    }
    None
}

/// `[Software\\Wine\\Mac Driver] 1789708696` -> `Software\Wine\Mac Driver`.
///
/// The trailing number is the key's modification time and is not part of the
/// name. `rfind` rather than `find` because a key name may itself contain `]`.
fn section_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix('[')?;
    let end = rest.rfind(']')?;
    Some(unescape(&rest[..end]))
}

/// Split an entry line into its value name and the undecoded right-hand side.
/// `@=...` is the key's default value and is reported under the name `@`.
fn entry_parts(line: &str) -> Option<(String, &str)> {
    if let Some(rest) = line.strip_prefix("@=") {
        return Some(("@".to_string(), rest));
    }
    let (name, rest) = quoted(line)?;
    Some((name, rest.strip_prefix('=')?))
}

fn hex_bytes(s: &str) -> Vec<u8> {
    s.split(',')
        .filter_map(|b| u8::from_str_radix(b.trim(), 16).ok())
        .collect()
}

fn utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

/// Decode the right-hand side of an entry. `None` for anything unrecognised or
/// truncated, which the caller drops.
fn parse_value(raw: &str) -> Option<Value> {
    let raw = raw.trim();
    if raw.starts_with('"') {
        return quoted(raw).map(|(s, _)| Value::Sz(s));
    }
    if let Some(rest) = raw.strip_prefix("dword:") {
        return u32::from_str_radix(rest.trim(), 16).ok().map(Value::Dword);
    }
    if let Some(rest) = raw.strip_prefix("str(") {
        let (n, tail) = rest.split_once("):")?;
        let kind: u32 = n.parse().ok()?;
        let (s, _) = quoted(tail.trim())?;
        return Some(match kind {
            // REG_MULTI_SZ arrives as one string with embedded NULs.
            7 => Value::MultiSz(
                s.split('\0')
                    .filter(|p| !p.is_empty())
                    .map(String::from)
                    .collect(),
            ),
            _ => Value::ExpandSz(s),
        });
    }
    if let Some(rest) = raw.strip_prefix("hex:") {
        return Some(Value::Binary {
            kind: 3,
            bytes: hex_bytes(rest),
        });
    }
    if let Some(rest) = raw.strip_prefix("hex(") {
        let (n, tail) = rest.split_once("):")?;
        let kind = u32::from_str_radix(n.trim(), 16).ok()?;
        let bytes = hex_bytes(tail);
        return Some(match kind {
            2 => Value::ExpandSz(utf16le(&bytes)),
            4 if bytes.len() >= 4 => {
                Value::Dword(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            7 => Value::MultiSz(
                utf16le(&bytes)
                    .split('\0')
                    .filter(|p| !p.is_empty())
                    .map(String::from)
                    .collect(),
            ),
            0xb if bytes.len() >= 8 => {
                let mut a = [0u8; 8];
                a.copy_from_slice(&bytes[..8]);
                Value::Qword(u64::from_le_bytes(a))
            }
            _ => Value::Binary { kind, bytes },
        });
    }
    None
}

/// Every value in one key of one hive file, in file order.
///
/// Nothing in here can fail loudly: a hive is a file the user can corrupt, and
/// half a diagnostic beats a panic in the middle of one.
fn read_section(path: &Path, key: &str) -> Vec<(String, Value)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<(String, Value)> = Vec::new();
    let mut inside = false;
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        i += 1;

        if line.starts_with('[') {
            match section_name(line) {
                Some(name) => {
                    // Keys are unique in a hive file, so the section after ours
                    // ends the search — no need to read 4 MB of system.reg to
                    // answer a question about one key near the top.
                    if inside {
                        break;
                    }
                    inside = key_eq(&name, key);
                }
                // A `[` with no `]`: a truncated hive. Treat it as the end of
                // whatever we were in rather than carrying values across.
                None => inside = false,
            }
            continue;
        }
        if !inside || line.is_empty() {
            continue;
        }
        // `#time=`, `#arch=`, `;;` header comments. Never entries.
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        let Some((name, raw)) = entry_parts(line) else {
            continue;
        };

        // Only `hex` values wrap, and they wrap with a trailing backslash on
        // every line but the last. A REG_SZ can also end in `\` — `"SystemRoot"
        // ="C:\\windows\\"` does — so joining on the backslash alone would
        // swallow the following entry. Gate on the hex prefix.
        let mut raw = raw.trim().to_string();
        if raw.starts_with("hex") {
            while raw.ends_with('\\') && i < lines.len() {
                raw.pop();
                raw.push_str(lines[i].trim());
                i += 1;
            }
        }

        if let Some(v) = parse_value(&raw) {
            out.push((name, v));
        }
    }
    out
}

/// Which hive files to consult for a key, in order.
fn hive_files(prefix: &Path, key: &str) -> (Vec<PathBuf>, String) {
    let (hive, path) = split_hive(key);
    match hive {
        Some(h) => (vec![prefix.join(h.file())], path),
        None => (
            SEARCH_ORDER.iter().map(|h| prefix.join(h.file())).collect(),
            path,
        ),
    }
}

/// Read one value. `key` may carry a hive prefix (`HKLM\Software\...`); without
/// one, HKCU then HKLM then HKU are tried and the first hit wins.
///
/// Numbers come back in decimal, so a `dword:00000000` and a `REG_SZ` of `"0"`
/// both render as `0`. Use `read` when the distinction matters.
pub fn get(prefix: &Path, key: &str, value: &str) -> Option<String> {
    read(prefix, key, value).map(|v| v.display())
}

/// As `get`, but keeping the type.
pub fn read(prefix: &Path, key: &str, value: &str) -> Option<Value> {
    let (files, path) = hive_files(prefix, key);
    for f in files {
        for (n, v) in read_section(&f, &path) {
            if n.eq_ignore_ascii_case(value) {
                return Some(v);
            }
        }
    }
    None
}

/// Every value in a key, as `(name, rendered)`. The key's default value is
/// named `@`. Empty when the key, the hive or the prefix is absent.
pub fn list(prefix: &Path, key: &str) -> Vec<(String, String)> {
    let (files, path) = hive_files(prefix, key);
    for f in files {
        let vals = read_section(&f, &path);
        if !vals.is_empty() {
            return vals.into_iter().map(|(n, v)| (n, v.display())).collect();
        }
    }
    Vec::new()
}

/// Names of the immediate subkeys of a key — what `reg query` prints above the
/// values. Needed because a Wine hive has no tree structure: every key is a
/// flat section, and "children" means "sections one component longer".
pub fn subkeys(prefix: &Path, key: &str) -> Vec<String> {
    let (files, path) = hive_files(prefix, key);
    let want = path.trim_matches('\\').to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        for line in text.lines().filter(|l| l.starts_with('[')) {
            let Some(name) = section_name(line) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            let Some(rest) = lower.strip_prefix(&want) else {
                continue;
            };
            let rest = rest.trim_start_matches('\\');
            if rest.is_empty() || (!want.is_empty() && lower.len() == want.len()) {
                continue;
            }
            let child = rest.split('\\').next().unwrap_or("");
            // Report the original casing, not the lowercased probe.
            let child = &name[name.len() - rest.len()..][..child.len()];
            if !child.is_empty() && !out.iter().any(|c| c.eq_ignore_ascii_case(child)) {
                out.push(child.to_string());
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Prefixes
// ---------------------------------------------------------------------------

/// The directory holding `drive_c` and the three hives.
///
/// This is the only function that knows the two layouts differ: exe-metal nests
/// the prefix under `pfx/`, CrossOver does not. Getting this wrong produces an
/// empty parse rather than an error, which is why it is checked here once.
pub fn prefix_of(layout: &Layout, backend: Backend, bottle: &str) -> Result<PathBuf> {
    let p = match backend {
        Backend::CrossOver => crossover::prefix(bottle)?,
        Backend::ExeMetal => layout.bottle(bottle).join("pfx"),
    };
    if !p.join("drive_c").is_dir() {
        bail!(
            "no {} bottle '{bottle}' (looked for {})",
            backend.label(),
            p.join("drive_c").display()
        );
    }
    Ok(p)
}

// ---------------------------------------------------------------------------
// Writing, through the prefix
// ---------------------------------------------------------------------------

/// What to write, in the shapes `reg.exe` understands.
#[derive(Debug, Clone)]
pub enum Data<'a> {
    Sz(&'a str),
    ExpandSz(&'a str),
    Dword(u32),
    Binary(&'a [u8]),
}

impl Data<'_> {
    fn reg_type(&self) -> &'static str {
        match self {
            Data::Sz(_) => "REG_SZ",
            Data::ExpandSz(_) => "REG_EXPAND_SZ",
            Data::Dword(_) => "REG_DWORD",
            Data::Binary(_) => "REG_BINARY",
        }
    }

    fn reg_data(&self) -> String {
        match self {
            Data::Sz(s) | Data::ExpandSz(s) => (*s).to_string(),
            Data::Dword(n) => n.to_string(),
            Data::Binary(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
        }
    }
}

/// Give a key an explicit hive for a write. A write has to pick one — there is
/// no "search order" for creating a key — and HKCU is where everything this
/// tool tunes belongs.
fn qualify(key: &str) -> String {
    let (hive, path) = split_hive(key);
    format!("{}\\{}", hive.unwrap_or(Hive::CurrentUser).short(), path)
}

/// Run a Windows program from `system32` inside a bottle and require success.
///
/// Both backends go through their own launcher: CrossOver's Wine and
/// exe-metal's are different builds against different prefixes, and crossing
/// them over deadlocks the prefix rather than failing.
fn run_in_bottle(
    layout: &Layout,
    backend: Backend,
    bottle: &str,
    exe: &str,
    args: &[String],
) -> Result<()> {
    match backend {
        Backend::CrossOver => {
            if !crossover::is_installed() {
                bail!("CrossOver is not installed");
            }
            // Same shape as `crossover::run`, but by image name rather than
            // host path: reg.exe lives in the bottle's own system32 and has no
            // meaningful path on this side.
            let out = Command::new(crossover::cxstart())
                .arg("--bottle")
                .arg(bottle)
                .arg("--")
                .arg(exe)
                .args(args)
                .output()
                .with_context(|| format!("running {exe} through cxstart"))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                let tail: Vec<&str> = err.lines().rev().take(4).collect();
                bail!(
                    "{exe} failed in CrossOver bottle '{bottle}': {}",
                    tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
                );
            }
            Ok(())
        }
        Backend::ExeMetal => {
            let mut b = Bottle::load(layout, bottle)
                .with_context(|| format!("bottle '{bottle}' could not be loaded"))?;
            // The virtual desktop has to come off for this one call. `run::exec`
            // wraps the command in `explorer /desktop=…` when it is on, which
            // means the status we get back is explorer's and not reg.exe's — a
            // rejected write would report success — and a desktop window flashes
            // up for what should be an invisible operation.
            b.window.virtual_desktop = false;

            let logs = layout.bottle(bottle).join("logs");
            let _ = std::fs::create_dir_all(&logs);
            let log = logs.join("registry.log");

            let win = PathBuf::from(format!("C:\\windows\\system32\\{exe}"));
            let code = crate::run::exec(layout, &b, &win, args, false, Some(&log))?;
            if code != 0 {
                bail!(
                    "{exe} exited {code} in bottle '{bottle}' — see {}",
                    log.display()
                );
            }
            Ok(())
        }
    }
}

/// Write a registry value.
///
/// Never edits the hive files. See the module comment: wineserver owns them
/// while the bottle is live and will discard anything written behind its back.
pub fn set(
    layout: &Layout,
    backend: Backend,
    bottle: &str,
    key: &str,
    value: &str,
    data: Data<'_>,
) -> Result<()> {
    let mut args = vec!["add".to_string(), qualify(key)];
    if value == "@" || value.is_empty() {
        args.push("/ve".into());
    } else {
        args.push("/v".into());
        args.push(value.to_string());
    }
    args.push("/t".into());
    args.push(data.reg_type().into());
    args.push("/d".into());
    args.push(data.reg_data());
    args.push("/f".into());
    run_in_bottle(layout, backend, bottle, "reg.exe", &args)
}

/// Delete one value, or the whole key when `value` is `None`.
pub fn delete(
    layout: &Layout,
    backend: Backend,
    bottle: &str,
    key: &str,
    value: Option<&str>,
) -> Result<()> {
    let mut args = vec!["delete".to_string(), qualify(key)];
    match value {
        Some("@") | Some("") => args.push("/ve".into()),
        Some(v) => {
            args.push("/v".into());
            args.push(v.to_string());
        }
        None => {}
    }
    args.push("/f".into());
    run_in_bottle(layout, backend, bottle, "reg.exe", &args)
}

/// Apply several values at once through `reg.exe import`.
///
/// One wine start instead of one per value. On CrossOver a `reg add` costs a
/// cxstart round trip of about a second, so setting a Windows version the naive
/// way is eight seconds of nothing visible happening. The `.reg` file goes into
/// the bottle's own `windows\temp` because reg.exe takes a Windows path.
///
/// `entries` is `(fully-qualified key, value name, data)`.
fn import(
    layout: &Layout,
    backend: Backend,
    bottle: &str,
    entries: &[(String, String, Data<'_>)],
) -> Result<()> {
    let prefix = prefix_of(layout, backend, bottle)?;
    let temp = prefix.join("drive_c/windows/temp");
    std::fs::create_dir_all(&temp).ok();
    let file = temp.join("exe-metal-import.reg");

    // ANSI form on purpose: no BOM, pure ASCII. Wine's reg.exe sniffs the BOM
    // to decide UTF-16 vs ANSI, and a UTF-8 BOM it does not recognise makes the
    // first key name unparsable.
    let mut text = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
    let mut current = String::new();
    for (key, name, data) in entries {
        if !key.eq_ignore_ascii_case(&current) {
            let _ = write!(text, "\r\n[{}]\r\n", expand_hive(key));
            current = key.clone();
        }
        let rhs = match data {
            Data::Sz(s) => format!("\"{}\"", escape_reg(s)),
            Data::ExpandSz(s) => format!("hex(2):{}", utf16_hex(s)),
            Data::Dword(n) => format!("dword:{n:08x}"),
            Data::Binary(b) => format!(
                "hex:{}",
                b.iter()
                    .map(|x| format!("{x:02x}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        };
        let lhs = if name == "@" || name.is_empty() {
            "@".to_string()
        } else {
            format!("\"{}\"", escape_reg(name))
        };
        let _ = write!(text, "{lhs}={rhs}\r\n");
    }
    std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;

    let r = run_in_bottle(
        layout,
        backend,
        bottle,
        "reg.exe",
        &[
            "import".to_string(),
            "C:\\windows\\temp\\exe-metal-import.reg".to_string(),
        ],
    );
    let _ = std::fs::remove_file(&file);

    // Older reg.exe builds have no `import` verb and exit non-zero without
    // writing anything. Falling back keeps this working rather than leaving the
    // bottle half-configured, which is the worst of the three outcomes.
    if r.is_err() {
        for (key, name, data) in entries {
            set(layout, backend, bottle, key, name, data.clone())?;
        }
    }
    Ok(())
}

fn escape_reg(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn utf16_hex(s: &str) -> String {
    let mut units: Vec<u16> = s.encode_utf16().collect();
    units.push(0);
    units
        .iter()
        .flat_map(|u| u.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn expand_hive(key: &str) -> String {
    let (hive, path) = split_hive(key);
    let full = match hive.unwrap_or(Hive::CurrentUser) {
        Hive::CurrentUser => "HKEY_CURRENT_USER",
        Hive::LocalMachine => "HKEY_LOCAL_MACHINE",
        Hive::Users => "HKEY_USERS",
    };
    format!("{full}\\{path}")
}

// ---------------------------------------------------------------------------
// Windows version
// ---------------------------------------------------------------------------

/// One entry of the version table Wine and CrossOver both key off.
pub struct Template {
    /// What `HKCU\Software\Wine` `Version` accepts, and winecfg's name for it.
    pub id: &'static str,
    pub label: &'static str,
    /// CrossOver spells the same thing with a bitness suffix in
    /// `cxbottle.conf`: `win10_64`, `win7_64`, `winxp`.
    pub crossover: &'static str,
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    /// What the NT `CurrentVersion` string reports. Windows 10 and 11 both
    /// still say `6.3` there — real Windows does, so Wine does — which is
    /// exactly why the *build* number is what identifies a version and this
    /// string must never be used for detection.
    pub current_version: &'static str,
    pub csd: &'static str,
    pub product: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "winxp",
        label: "Windows XP",
        crossover: "winxp",
        major: 5,
        minor: 1,
        build: 2600,
        current_version: "5.1",
        csd: "Service Pack 3",
        product: "Microsoft Windows XP",
    },
    Template {
        id: "win2k3",
        label: "Windows Server 2003",
        crossover: "win2k3",
        major: 5,
        minor: 2,
        build: 3790,
        current_version: "5.2",
        csd: "Service Pack 2",
        product: "Microsoft Windows Server 2003",
    },
    Template {
        id: "vista",
        label: "Windows Vista",
        crossover: "winvista",
        major: 6,
        minor: 0,
        build: 6002,
        current_version: "6.0",
        csd: "Service Pack 2",
        product: "Windows Vista",
    },
    Template {
        id: "win7",
        label: "Windows 7",
        crossover: "win7_64",
        major: 6,
        minor: 1,
        build: 7601,
        current_version: "6.1",
        csd: "Service Pack 1",
        product: "Windows 7 Professional",
    },
    Template {
        id: "win8",
        label: "Windows 8",
        crossover: "win8_64",
        major: 6,
        minor: 2,
        build: 9200,
        current_version: "6.2",
        csd: "",
        product: "Windows 8",
    },
    Template {
        id: "win81",
        label: "Windows 8.1",
        crossover: "win81_64",
        major: 6,
        minor: 3,
        build: 9600,
        current_version: "6.3",
        csd: "",
        product: "Windows 8.1",
    },
    Template {
        id: "win10",
        label: "Windows 10",
        crossover: "win10_64",
        major: 10,
        minor: 0,
        build: 19043,
        current_version: "6.3",
        csd: "",
        product: "Windows 10 Pro",
    },
    Template {
        id: "win11",
        label: "Windows 11",
        crossover: "win11_64",
        major: 10,
        minor: 0,
        build: 22000,
        current_version: "6.3",
        // Wine keeps the Windows 10 product string for win11; installers key off
        // the build number, and changing this breaks nothing but confuses every
        // "what am I running" dialog.
        csd: "",
        product: "Windows 10 Pro",
    },
];

pub fn template(id: &str) -> Option<&'static Template> {
    let id = id.trim().to_ascii_lowercase();
    // Accept CrossOver's spelling too, so a cxbottle.conf Template can be fed
    // straight back in.
    let bare = id
        .trim_end_matches("_64")
        .trim_end_matches("_32")
        .to_string();
    TEMPLATES
        .iter()
        .find(|t| t.id == id || t.crossover == id || t.id == bare)
}

/// Nearest template for a build number. Ranges, not equality: CrossOver ships
/// build 19045 where Wine ships 19043, and both are Windows 10.
fn template_for_build(build: u32) -> Option<&'static Template> {
    let id = match build {
        b if b >= 22000 => "win11",
        b if b >= 10240 => "win10",
        b if b >= 9600 => "win81",
        b if b >= 9200 => "win8",
        b if b >= 7600 => "win7",
        b if b >= 6000 => "vista",
        b if b >= 3790 => "win2k3",
        b if b >= 2600 => "winxp",
        _ => return None,
    };
    template(id)
}

/// The Windows version a bottle reports, as a template id (`win10`, `win7`…).
///
/// Two sources, in the order Wine itself consults them. `HKCU\Software\Wine`
/// `Version` is the explicit override winecfg writes and ntdll honours above
/// everything else; it is absent on a default bottle, which is why falling back
/// to the NT `CurrentBuildNumber` is required and not a nicety.
pub fn windows_version(prefix: &Path) -> Option<String> {
    if let Some(v) = get(prefix, "HKCU\\Software\\Wine", "Version") {
        if let Some(t) = template(&v) {
            return Some(t.id.to_string());
        }
    }
    let build = get(
        prefix,
        "HKLM\\Software\\Microsoft\\Windows NT\\CurrentVersion",
        "CurrentBuildNumber",
    )?;
    template_for_build(build.trim().parse().ok()?).map(|t| t.id.to_string())
}

/// Version line for a diagnostic: `win10 (Windows 10 Pro, build 19045)`.
pub fn windows_version_detail(prefix: &Path) -> String {
    let nt = "HKLM\\Software\\Microsoft\\Windows NT\\CurrentVersion";
    let id = windows_version(prefix).unwrap_or_else(|| "unknown".into());
    let product = get(prefix, nt, "ProductName").unwrap_or_else(|| "?".into());
    let build = get(prefix, nt, "CurrentBuildNumber").unwrap_or_else(|| "?".into());
    let csd = get(prefix, nt, "CSDVersion").unwrap_or_default();
    let sp = if csd.is_empty() {
        String::new()
    } else {
        format!(", {csd}")
    };
    format!("{id} ({product}, build {build}{sp})")
}

/// Set a bottle's reported Windows version.
///
/// Writes both halves on purpose. `HKCU\Software\Wine` `Version` is what ntdll
/// reads, so it alone changes what `GetVersionEx` returns — but installers read
/// the NT `CurrentVersion` key straight out of the registry and would then
/// disagree with the API about what they are installing onto. Setting only one
/// produces a bottle that passes its own version check and fails the
/// installer's.
pub fn set_windows_version(
    layout: &Layout,
    backend: Backend,
    bottle: &str,
    version: &str,
) -> Result<()> {
    let t = template(version).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown Windows version '{version}' — one of: {}",
            TEMPLATES
                .iter()
                .map(|t| t.id)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let wine = "HKCU\\Software\\Wine".to_string();
    let nt = "HKLM\\Software\\Microsoft\\Windows NT\\CurrentVersion".to_string();
    let build = t.build.to_string();

    let entries: Vec<(String, String, Data<'_>)> = vec![
        (wine, "Version".into(), Data::Sz(t.id)),
        (
            nt.clone(),
            "CurrentVersion".into(),
            Data::Sz(t.current_version),
        ),
        (nt.clone(), "CurrentBuild".into(), Data::Sz(&build)),
        (nt.clone(), "CurrentBuildNumber".into(), Data::Sz(&build)),
        (
            nt.clone(),
            "CurrentMajorVersionNumber".into(),
            Data::Dword(t.major),
        ),
        (
            nt.clone(),
            "CurrentMinorVersionNumber".into(),
            Data::Dword(t.minor),
        ),
        (nt.clone(), "ProductName".into(), Data::Sz(t.product)),
        (nt, "CSDVersion".into(), Data::Sz(t.csd)),
    ];
    import(layout, backend, bottle, &entries)
}

// ---------------------------------------------------------------------------
// COM registration
// ---------------------------------------------------------------------------

/// `regsvr32` for a bottle. `dll` is a Windows path or a bare name resolved on
/// the prefix's DLL search path.
///
/// Registering is only half of what people mean by "install this DLL".
/// regsvr32 writes the COM class registration and nothing else, so a native
/// DLL dropped into `system32` and registered still loads Wine's builtin
/// unless `Software\Wine\DllOverrides` says otherwise — the registration
/// succeeds, the behaviour does not change, and there is no error to find.
/// `set_dll_override` is the other half.
pub fn register_dll(
    layout: &Layout,
    backend: Backend,
    bottle: &str,
    dll: &str,
    unregister: bool,
) -> Result<()> {
    let mut args = vec!["/s".to_string()];
    if unregister {
        args.push("/u".into());
    }
    args.push(dll.to_string());
    run_in_bottle(layout, backend, bottle, "regsvr32.exe", &args)
}

/// Point a DLL at the native or builtin implementation.
///
/// `dll` is the base name without `.dll`; `mode` is winecfg's spelling —
/// `native`, `builtin`, `native,builtin`, `builtin,native`, or `""` to disable.
pub fn set_dll_override(
    layout: &Layout,
    backend: Backend,
    bottle: &str,
    dll: &str,
    mode: &str,
) -> Result<()> {
    let name = dll.trim_end_matches(".dll");
    set(
        layout,
        backend,
        bottle,
        "HKCU\\Software\\Wine\\DllOverrides",
        name,
        Data::Sz(mode),
    )
}

// ---------------------------------------------------------------------------
// sysinfo
// ---------------------------------------------------------------------------

/// The keys that change how a bottle behaves, and so the ones a bug report
/// needs. Everything else in a hive is either Windows' own furniture or a
/// per-application setting nobody can act on.
const INTERESTING: &[(&str, &str)] = &[
    ("Mac Driver", "HKCU\\Software\\Wine\\Mac Driver"),
    ("Direct3D", "HKCU\\Software\\Wine\\Direct3D"),
    ("Explorer", "HKCU\\Software\\Wine\\Explorer"),
    // Labelled "Desktops" and not by its full path: the column is 14 wide and
    // "Explorer\Desktops" pushes every value on that one line out of alignment.
    ("Desktops", "HKCU\\Software\\Wine\\Explorer\\Desktops"),
    ("Drivers", "HKCU\\Software\\Wine\\Drivers"),
    ("WineDbg", "HKCU\\Software\\Wine\\WineDbg"),
];

/// How many DLL overrides to spell out before summarising. A stock CrossOver
/// bottle has about sixty; printing them all buries the two the user changed.
const OVERRIDE_CAP: usize = 24;

/// A `cxgetsysinfo`-style dump: machine, backends, every bottle either one can
/// see, and the registry settings that decide how each behaves.
///
/// This is the text a user pastes into a bug report, so it states facts and
/// never advice, and it degrades instead of failing — a missing CrossOver, an
/// unreadable bottle or a corrupt hive each cost one line, not the report.
pub fn sysinfo(layout: &Layout) -> String {
    let mut s = String::new();
    let m = crate::hw::Machine::detect();

    let _ = writeln!(s, "exe-metal sysinfo");
    let _ = writeln!(s);
    let _ = writeln!(s, "system");
    let build = crate::util::capture("sw_vers", &["-buildVersion"]).unwrap_or_default();
    let _ = writeln!(s, "  macOS          {} ({build})", m.macos_raw);
    let _ = writeln!(
        s,
        "  chip           {} — {} ({})",
        m.silicon, m.cpu_brand, m.model
    );
    let _ = writeln!(
        s,
        "  cores          {} total, {}P + {}E",
        m.total_cores, m.perf_cores, m.eff_cores
    );
    let _ = writeln!(s, "  memory         {} GB", m.memory_gb);
    let _ = writeln!(s, "  architecture   {}", m.arch);
    let _ = writeln!(
        s,
        "  Rosetta 2      {}",
        if m.rosetta { "present" } else { "ABSENT" }
    );
    if let Ok(df) = crate::util::capture("df", &["-h", "/System/Volumes/Data"]) {
        if let Some(cols) = df.lines().nth(1).map(|l| {
            l.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<String>>()
        }) {
            if cols.len() > 3 {
                let _ = writeln!(s, "  disk free      {}", cols[3]);
            }
        }
    }

    let _ = writeln!(s);
    let _ = writeln!(s, "backends");
    if crossover::is_installed() {
        let _ = writeln!(
            s,
            "  CrossOver      {} at {}",
            crossover::version().unwrap_or_else(|| "version unreadable".into()),
            crossover::app_path().display()
        );
    } else {
        let _ = writeln!(s, "  CrossOver      not installed");
    }
    let runtimes = crate::util::dir_names(&layout.runtimes());
    let renderers = crate::util::dir_names(&layout.renderers());
    let _ = writeln!(
        s,
        "  exe-metal      runtimes: {} | renderers: {}",
        if runtimes.is_empty() {
            "none".into()
        } else {
            runtimes.join(", ")
        },
        if renderers.is_empty() {
            "none".into()
        } else {
            renderers.join(", ")
        }
    );

    let titles = crate::library::Library::load()
        .map(|l| l.titles)
        .unwrap_or_default();

    let _ = writeln!(s);
    let _ = writeln!(s, "bottles — exe-metal ({})", layout.bottles().display());
    let names = crate::util::dir_names(&layout.bottles());
    if names.is_empty() {
        let _ = writeln!(s, "  none");
    }
    for name in &names {
        let head = match Bottle::load(layout, name) {
            Ok(b) => format!(
                "{} + {}, windows={}, esync={} msync={} metalfx={} dxr={}",
                b.runtime,
                b.renderer,
                b.windows,
                b.perf.esync,
                b.perf.msync,
                b.perf.metalfx,
                b.perf.dxr
            ),
            // A bottle directory with no exe-metal.toml is exactly what a
            // half-finished create leaves behind, and is worth reporting rather
            // than hiding.
            Err(_) => "no exe-metal.toml".to_string(),
        };
        bottle_block(
            &mut s,
            name,
            &layout.bottle(name).join("pfx"),
            &head,
            &titles,
            Backend::ExeMetal,
        );
    }

    let _ = writeln!(s);
    let cx_root = crossover::bottles_root()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let _ = writeln!(s, "bottles — CrossOver ({cx_root})");
    let cx = crossover::list_bottles();
    if cx.is_empty() {
        let _ = writeln!(s, "  none");
    }
    for name in &cx {
        let Ok(prefix) = crossover::prefix(name) else {
            continue;
        };
        let conf = cxbottle_conf(&prefix);
        let head = format!(
            "CrossOver {}, template {}",
            conf.get("Version").cloned().unwrap_or_else(|| "?".into()),
            conf.get("Template").cloned().unwrap_or_else(|| "?".into())
        );
        bottle_block(&mut s, name, &prefix, &head, &titles, Backend::CrossOver);
    }
    s
}

/// One bottle's stanza, identical for both backends because only the prefix
/// path differs.
fn bottle_block(
    s: &mut String,
    name: &str,
    prefix: &Path,
    head: &str,
    titles: &[crate::library::Title],
    backend: Backend,
) {
    let size = crate::util::human_size(prefix);
    let _ = writeln!(s, "  {name}  [{size}]  {head}");

    if !prefix.join("drive_c").is_dir() {
        let _ = writeln!(s, "      prefix         MISSING at {}", prefix.display());
        return;
    }
    let _ = writeln!(s, "      windows        {}", windows_version_detail(prefix));
    let _ = writeln!(
        s,
        "      arch           {}   WoW64 {}",
        hive_arch(prefix).unwrap_or_else(|| "?".into()),
        if prefix.join("drive_c/windows/syswow64").is_dir() {
            "yes"
        } else {
            "no (32-bit applications will not run)"
        }
    );

    for (label, key) in INTERESTING {
        let vals = list(prefix, key);
        if vals.is_empty() {
            continue;
        }
        let rendered: Vec<String> = vals.iter().map(|(n, v)| format!("{n}={v}")).collect();
        let _ = writeln!(s, "      {label:<14} {}", rendered.join("  "));
    }

    let ov = list(prefix, "HKCU\\Software\\Wine\\DllOverrides");
    if !ov.is_empty() {
        let shown: Vec<String> = ov
            .iter()
            .take(OVERRIDE_CAP)
            .map(|(n, v)| format!("{n}={v}"))
            .collect();
        let more = ov.len().saturating_sub(shown.len());
        let _ = writeln!(
            s,
            "      DllOverrides   {} entries: {}{}",
            ov.len(),
            shown.join("  "),
            if more > 0 {
                format!("  … and {more} more")
            } else {
                String::new()
            }
        );
    }

    let mine: Vec<&str> = titles
        .iter()
        .filter(|t| t.bottle == name && t.backend == backend)
        .map(|t| t.id.as_str())
        .collect();
    if !mine.is_empty() {
        let _ = writeln!(s, "      titles         {}", mine.join(", "));
    }
}

/// `#arch=win64` from the top of a hive. Tells you whether the prefix is 64-bit
/// without launching anything.
fn hive_arch(prefix: &Path) -> Option<String> {
    let text = std::fs::read_to_string(prefix.join("user.reg")).ok()?;
    text.lines()
        .take(10)
        .find_map(|l| l.strip_prefix("#arch=").map(|a| a.trim().to_string()))
}

/// The handful of `"Key" = "value"` lines in `cxbottle.conf` worth reporting.
/// CrossOver owns that file; this only reads it.
fn cxbottle_conf(prefix: &Path) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(prefix.join("cxbottle.conf")) else {
        return out;
    };
    for line in text.lines() {
        let line = line.trim();
        // `;;"WindowsVersion" = ""` is a commented-out default, not a setting.
        if line.starts_with(';') || !line.starts_with('"') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().trim_matches('"');
        let v = v.trim().trim_matches('"');
        if !k.is_empty() && !v.is_empty() {
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway prefix. No `tempfile` dependency on purpose — adding a crate
    /// for a dozen tests is not worth the build time.
    ///
    /// The counter is not decoration. `cargo test` runs these in parallel, so a
    /// name built from the pid and a human tag alone collides between two tests
    /// that want the same fixture, and `remove_dir_all` in one deletes the hive
    /// the other is mid-read on. That failed six tests non-deterministically.
    struct Tmp(PathBuf);

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    impl Tmp {
        fn new(tag: &str) -> Self {
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "exe-metal-registry-{}-{tag}-{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn write(&self, name: &str, body: &str) -> &Path {
            std::fs::write(self.0.join(name), body).unwrap();
            &self.0
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Shaped exactly like a real `user.reg`, down to the `#time` lines and the
    /// modification stamp after the section name.
    const USER: &str = r#"WINE REGISTRY Version 2
;; All keys relative to REGISTRY\\User\\S-1-5-21-0-0-0-1000

#arch=win64

[Software\\Wine\\Mac Driver] 1789708696
#time=1dd472d1c4db088
"AllowSetGamma"=dword:00000000
"RetinaMode"="n"
"CaptureDisplaysForFullscreen"="N"

[Software\\Wine\\Explorer\\Desktops] 1789787756
#time=1dd47e52fe8167a
"ow"="1280x720"

[Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders] 1789707287
#time=1dd4729d4acf94e
@="default value"
"Desktop"="C:\\users\\crossover\\Desktop"
"Personal"=str(2):"%USERPROFILE%\\Documents"
"Paths"=str(7):"C:\\one\0C:\\two\0"
"Blob"=hex:00,01,02,03,\
  04,05,06,07,\
  08,09
"Next"="after the continuation"
"Trailing"="C:\\windows\\"
"AfterTrailing"="still parsed"
"#;

    fn prefix_with_user() -> Tmp {
        let t = Tmp::new("user");
        t.write("user.reg", USER);
        t
    }

    #[test]
    fn reads_reg_sz_and_dword_from_the_same_key() {
        let t = prefix_with_user();
        let key = "Software\\Wine\\Mac Driver";
        assert_eq!(get(t.path(), key, "RetinaMode").as_deref(), Some("n"));
        // dword comes back in decimal, not as the file's eight hex digits.
        assert_eq!(get(t.path(), key, "AllowSetGamma").as_deref(), Some("0"));
        assert_eq!(
            read(t.path(), key, "AllowSetGamma"),
            Some(Value::Dword(0)),
            "the type has to survive, not just the rendering"
        );
    }

    #[test]
    fn doubled_backslashes_are_decoded_to_one() {
        let t = prefix_with_user();
        let key = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders";
        assert_eq!(
            get(t.path(), key, "Desktop").as_deref(),
            Some("C:\\users\\crossover\\Desktop"),
            "a path with doubled separators is not a path any Windows program will match"
        );
    }

    #[test]
    fn value_names_and_keys_are_case_insensitive() {
        let t = prefix_with_user();
        assert_eq!(
            get(t.path(), "software\\wine\\MAC DRIVER", "retinamode").as_deref(),
            Some("n")
        );
    }

    #[test]
    fn a_reg_sz_ending_in_a_backslash_does_not_swallow_the_next_entry() {
        // This is the bug a naive "join lines ending in \" parser has:
        // "C:\\windows\\" ends in a backslash in the raw text.
        let t = prefix_with_user();
        let key = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders";
        assert_eq!(
            get(t.path(), key, "Trailing").as_deref(),
            Some("C:\\windows\\")
        );
        assert_eq!(
            get(t.path(), key, "AfterTrailing").as_deref(),
            Some("still parsed")
        );
    }

    #[test]
    fn hex_continuation_lines_are_joined_and_not_read_as_entries() {
        let t = prefix_with_user();
        let key = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders";
        assert_eq!(
            read(t.path(), key, "Blob"),
            Some(Value::Binary {
                kind: 3,
                bytes: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            })
        );
        assert_eq!(
            get(t.path(), key, "Next").as_deref(),
            Some("after the continuation"),
            "the entry after a wrapped hex value must still be seen"
        );
    }

    #[test]
    fn expand_sz_multi_sz_and_the_default_value() {
        let t = prefix_with_user();
        let key = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders";
        assert_eq!(
            read(t.path(), key, "Personal"),
            Some(Value::ExpandSz("%USERPROFILE%\\Documents".into())),
            "expansion is the caller's job; storing the expanded form loses information"
        );
        assert_eq!(
            read(t.path(), key, "Paths"),
            Some(Value::MultiSz(vec!["C:\\one".into(), "C:\\two".into()]))
        );
        assert_eq!(get(t.path(), key, "@").as_deref(), Some("default value"));
    }

    #[test]
    fn list_returns_one_key_and_stops_at_the_next_section() {
        let t = prefix_with_user();
        let vals = list(t.path(), "Software\\Wine\\Mac Driver");
        assert_eq!(vals.len(), 3, "got {vals:?}");
        assert!(vals.iter().any(|(n, v)| n == "RetinaMode" && v == "n"));
        assert!(
            !vals.iter().any(|(n, _)| n == "ow"),
            "values from the following section leaked in"
        );
    }

    #[test]
    fn malformed_lines_are_skipped_and_never_panic() {
        let t = Tmp::new("broken");
        t.write(
            "user.reg",
            concat!(
                "WINE REGISTRY Version 2\n",
                "[Software\\\\Wine\\\\Broken] 1\n",
                "#time=1\n",
                "\"Unterminated=\"oops\n", // name's quote never closes
                "=\"no name at all\"\n",   // no name
                "\"NoEquals\"\n",          // no assignment
                "\"BadDword\"=dword:zzzzzzzz\n", // not hex
                "\"Empty\"=\n",            // no value at all
                "\"HalfType\"=str(:\"x\"\n", // malformed type marker
                "\"Lone\"=\"ends with a lone backslash \\\"\n", // escaped quote, unterminated
                "[Unclosed section 2\n",
                "\"Survivor\"=\"yes\"\n",
                "\n",
                "[Software\\\\Wine\\\\Good] 2\n",
                "\"Fine\"=\"value\"\n",
            ),
        );
        // The point is that none of the above aborts the parse or the process.
        let broken = list(t.path(), "Software\\Wine\\Broken");
        assert!(
            broken.iter().all(|(n, _)| n != "BadDword"),
            "an undecodable dword must be dropped, not guessed at"
        );
        assert_eq!(
            get(t.path(), "Software\\Wine\\Good", "Fine").as_deref(),
            Some("value"),
            "a good key after malformed input must still be readable"
        );
    }

    #[test]
    fn a_missing_prefix_or_key_is_none_not_an_error() {
        let t = Tmp::new("empty");
        assert!(get(t.path(), "Software\\Wine", "Version").is_none());
        assert!(list(t.path(), "Software\\Wine").is_empty());
        assert!(get(Path::new("/definitely/not/here"), "Software", "x").is_none());
    }

    #[test]
    fn the_hive_prefix_picks_the_file() {
        let t = Tmp::new("hives");
        t.write(
            "user.reg",
            "WINE REGISTRY Version 2\n\n[Software\\\\Wine] 1\n\"Version\"=\"win7\"\n",
        );
        t.write(
            "system.reg",
            "WINE REGISTRY Version 2\n\n[Software\\\\Wine] 1\n\"Version\"=\"win10\"\n",
        );
        assert_eq!(
            get(t.path(), "HKCU\\Software\\Wine", "Version").as_deref(),
            Some("win7")
        );
        assert_eq!(
            get(t.path(), "HKLM\\Software\\Wine", "Version").as_deref(),
            Some("win10")
        );
        // No prefix: HKCU wins, because that is where a user override lives and
        // the HKLM copy is usually the installer's stale one.
        assert_eq!(
            get(t.path(), "Software\\Wine", "Version").as_deref(),
            Some("win7")
        );
    }

    #[test]
    fn hkcr_is_rewritten_onto_hklm_software_classes() {
        let (hive, path) = split_hive("HKCR\\.txt");
        assert_eq!(hive, Some(Hive::LocalMachine));
        assert_eq!(path, "Software\\Classes\\.txt");
    }

    #[test]
    fn x_escapes_decode() {
        assert_eq!(unescape(r"caf\x00e9"), "café");
        assert_eq!(unescape(r#"a\tb\\c\"d"#), "a\tb\\c\"d");
        // A line ending in a lone backslash keeps it rather than losing a char.
        assert_eq!(unescape(r"trailing\"), "trailing\\");
    }

    #[test]
    fn subkeys_lists_children_one_level_down() {
        let t = Tmp::new("subkeys");
        t.write(
            "user.reg",
            concat!(
                "WINE REGISTRY Version 2\n\n",
                "[Software\\\\Wine\\\\Mac Driver] 1\n\"a\"=\"1\"\n\n",
                "[Software\\\\Wine\\\\Explorer] 1\n\"b\"=\"2\"\n\n",
                "[Software\\\\Wine\\\\Explorer\\\\Desktops] 1\n\"c\"=\"3\"\n",
            ),
        );
        let kids = subkeys(t.path(), "HKCU\\Software\\Wine");
        assert_eq!(kids, vec!["Explorer", "Mac Driver"]);
        assert_eq!(
            subkeys(t.path(), "HKCU\\Software\\Wine\\Explorer"),
            vec!["Desktops"]
        );
    }

    #[test]
    fn windows_version_falls_back_to_the_build_number() {
        let t = Tmp::new("winver");
        t.write(
            "system.reg",
            concat!(
                "WINE REGISTRY Version 2\n\n",
                "[Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion] 1\n",
                // Real CrossOver values: CurrentVersion says 6.3 even though
                // this is Windows 10, which is why detection uses the build.
                "\"CurrentVersion\"=\"6.3\"\n",
                "\"CurrentBuildNumber\"=\"19045\"\n",
                "\"ProductName\"=\"Windows 10 Pro\"\n",
                "\"CSDVersion\"=\"\"\n",
            ),
        );
        assert_eq!(windows_version(t.path()).as_deref(), Some("win10"));
        assert_eq!(
            windows_version_detail(t.path()),
            "win10 (Windows 10 Pro, build 19045)"
        );

        // The explicit override outranks it, exactly as ntdll does.
        t.write(
            "user.reg",
            "WINE REGISTRY Version 2\n\n[Software\\\\Wine] 1\n\"Version\"=\"win7\"\n",
        );
        assert_eq!(windows_version(t.path()).as_deref(), Some("win7"));
    }

    #[test]
    fn templates_accept_both_spellings() {
        assert_eq!(template("win10").unwrap().crossover, "win10_64");
        assert_eq!(template("win10_64").unwrap().id, "win10");
        assert_eq!(template("WIN7_64").unwrap().id, "win7");
        assert!(template("windows95").is_none());
        // Build 22000 is the only thing separating 11 from 10 in the registry.
        assert_eq!(template_for_build(22000).unwrap().id, "win11");
        assert_eq!(template_for_build(19045).unwrap().id, "win10");
        assert!(template_for_build(1).is_none());
    }

    #[test]
    fn a_write_qualifies_an_unprefixed_key_to_hkcu() {
        assert_eq!(
            qualify("Software\\Wine\\Mac Driver"),
            "HKCU\\Software\\Wine\\Mac Driver"
        );
        assert_eq!(
            qualify("HKLM\\System\\CurrentControlSet"),
            "HKLM\\System\\CurrentControlSet"
        );
        assert_eq!(
            expand_hive("HKLM\\Software\\X"),
            "HKEY_LOCAL_MACHINE\\Software\\X"
        );
    }

    #[test]
    fn import_text_escapes_backslashes_the_way_regedit_expects() {
        assert_eq!(escape_reg("C:\\windows"), "C:\\\\windows");
        assert_eq!(escape_reg("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(Data::Dword(10).reg_data(), "10");
        assert_eq!(Data::Binary(&[0xde, 0xad]).reg_data(), "dead");
    }

    #[test]
    fn cxbottle_conf_ignores_commented_defaults() {
        let t = Tmp::new("cxconf");
        t.write(
            "cxbottle.conf",
            concat!(
                ";; comment\n",
                "\"Version\" = \"26.3.0.39832\"\n",
                "\"Template\" = \"win10_64\"\n",
                ";;\"WindowsVersion\" = \"\"\n",
            ),
        );
        let c = cxbottle_conf(t.path());
        assert_eq!(c.get("Template").map(String::as_str), Some("win10_64"));
        assert!(
            !c.contains_key("WindowsVersion"),
            "a ;; line is a documented default, not a setting"
        );
    }

    #[test]
    fn prefix_of_rejects_a_bottle_that_is_not_there() {
        let layout = Layout::discover().unwrap();
        assert!(prefix_of(&layout, Backend::ExeMetal, "definitely-not-a-bottle").is_err());
        assert!(prefix_of(&layout, Backend::CrossOver, "definitely-not-a-bottle").is_err());
    }
}
