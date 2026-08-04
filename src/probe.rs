//! Look inside an executable and decide how to run it.
//!
//! The user picking a file does not know that D3DMetal is 64-bit only, or that
//! DXMT needs a Wine that exports `winemetal`. They should not have to. Two
//! facts read straight out of the file settle the whole question:
//!
//!   * the COFF machine field, which gives the bitness exactly, and
//!   * which DirectX DLLs the binary names, which gives the API level.
//!
//! The second one is deliberately a string scan rather than an import-table
//! walk. Games routinely `LoadLibrary("d3d11.dll")` at runtime instead of
//! importing it, and those never appear in the import directory — the scan
//! catches both, and it cannot be thrown off by a packed or unusual section
//! layout.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Reading more than this is pointless: the DLL name tables live in the headers
/// and the import/rdata sections, all near the front. A 4 GB game blob would
/// otherwise be read in full to learn nothing new.
const SCAN_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86,
    X64,
    Arm64,
    Unknown,
}

impl Arch {
    pub fn label(&self) -> &'static str {
        match self {
            Arch::X86 => "32-bit x86",
            Arch::X64 => "64-bit x86-64",
            Arch::Arm64 => "ARM64",
            Arch::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dx {
    DirectDraw,
    D3D9,
    D3D10,
    D3D11,
    D3D12,
    Vulkan,
    OpenGL,
}

impl Dx {
    pub fn label(&self) -> &'static str {
        match self {
            Dx::DirectDraw => "DirectDraw",
            Dx::D3D9 => "DirectX 9",
            Dx::D3D10 => "DirectX 10",
            Dx::D3D11 => "DirectX 11",
            Dx::D3D12 => "DirectX 12",
            Dx::Vulkan => "Vulkan",
            Dx::OpenGL => "OpenGL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExeInfo {
    pub path: PathBuf,
    pub arch: Arch,
    /// Graphics APIs named anywhere in the binary, lowest to highest.
    pub apis: Vec<Dx>,
    /// Which installer framework built this, if it is an installer at all.
    pub installer: Option<&'static str>,
}

impl ExeInfo {
    pub fn highest_api(&self) -> Option<Dx> {
        self.apis.iter().copied().max()
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub runtime: &'static str,
    pub renderer: &'static str,
    /// Why this pairing, in one sentence, for the UI and `probe` output.
    pub why: String,
    /// Things that will bite but are not fatal.
    pub warnings: Vec<String>,
}

pub fn inspect(path: &Path) -> Result<ExeInfo> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    if meta.len() == 0 {
        bail!(
            "{} is empty (0 bytes) — the download or extraction did not complete",
            path.display()
        );
    }

    let bytes = read_capped(path, SCAN_LIMIT)?;
    // Distinguish "not a Windows binary" from "a Windows binary that is
    // damaged". Both used to pass through as Unknown, which meant a truncated
    // download got a bottle created for it and then failed inside Wine, where
    // the cause is far harder to see.
    let arch = match pe_status(&bytes) {
        PeStatus::NotPe => bail!(
            "{} is not a Windows executable (no MZ header)",
            path.display()
        ),
        PeStatus::Damaged => bail!(
            "{} has an MZ header but no valid PE header — the file is truncated or \
             corrupt. If it came from an archive, the extraction did not complete.",
            path.display()
        ),
        PeStatus::Ok(a) => a,
    };

    Ok(ExeInfo {
        path: path.to_path_buf(),
        arch,
        apis: detect_apis(&bytes),
        installer: detect_installer(&bytes),
    })
}

/// Choose a runtime and renderer for an inspected executable.
///
/// The ordering matters. Bitness is checked before API level because it can
/// veto a renderer outright: D3DMetal has no 32-bit build, so a 32-bit DirectX
/// 11 game cannot use the otherwise-obvious backend.
pub fn plan(info: &ExeInfo) -> Plan {
    let mut warnings = Vec::new();
    // sikarugir-10.0 is the only runtime exporting winemetal, and the fastest
    // measured, so nothing below has a reason to pick a different one.
    let runtime = "sikarugir-10.0";
    let api = info.highest_api();

    if let Some(kind) = info.installer {
        warnings.push(format!(
            "this looks like {kind} installer, not a game. Run it, then point exe-metal \
             at the executable it installs."
        ));
    }

    let (renderer, why) = match (info.arch, api) {
        (Arch::Arm64, _) => {
            warnings.push(
                "an ARM64 Windows binary cannot run here: Wine on macOS translates x86, and \
                 Rosetta does not translate ARM64 Windows code."
                    .into(),
            );
            ("d3dmetal", "ARM64 binary — this will not run".to_string())
        }

        // 32-bit vetoes D3DMetal regardless of API level.
        (Arch::X86, Some(Dx::DirectDraw)) => (
            "cnc-ddraw",
            "32-bit DirectDraw title, so the legacy 2D backend".to_string(),
        ),
        (Arch::X86, Some(Dx::D3D9)) => (
            "d9vk",
            "32-bit DirectX 9, which d9vk handles and D3DMetal cannot (64-bit only)".to_string(),
        ),
        (Arch::X86, Some(Dx::D3D12)) => {
            // No such thing in practice, but say so rather than silently
            // choosing a backend that cannot work.
            warnings.push(
                "a 32-bit binary naming DirectX 12 is unusual; if it fails, try --renderer dxvk."
                    .into(),
            );
            (
                "dxmt",
                "32-bit, so D3DMetal is unavailable — DXMT is the 32-bit capable Metal backend"
                    .to_string(),
            )
        }
        (Arch::X86, _) => (
            "dxmt",
            "32-bit, so D3DMetal is out (64-bit only) — DXMT covers DirectX 10/11 at 32-bit"
                .to_string(),
        ),

        // 64-bit: D3DMetal unless the API is older than it covers.
        (Arch::X64, Some(Dx::D3D12)) => (
            "d3dmetal",
            "DirectX 12, and D3DMetal is the only backend that implements it".to_string(),
        ),
        (Arch::X64, Some(Dx::D3D11)) | (Arch::X64, Some(Dx::D3D10)) => (
            "d3dmetal",
            "64-bit DirectX 11, which D3DMetal translates directly to Metal".to_string(),
        ),
        (Arch::X64, Some(Dx::D3D9)) => (
            "d9vk",
            "DirectX 9, which needs the d9vk backend".to_string(),
        ),
        (Arch::X64, Some(Dx::DirectDraw)) => (
            "cnc-ddraw",
            "DirectDraw title, so the legacy 2D backend".to_string(),
        ),
        (Arch::X64, Some(Dx::Vulkan)) => {
            // Verified: a Windows Vulkan binary run on sikarugir-10.0 loaded
            // MoltenVK 1.4.1 and created a VkDevice on the Apple GPU. The
            // DirectX renderer is not in the path at all for these.
            (
                "d3dmetal",
                "Vulkan title — Wine's own MoltenVK path handles it; the DirectX renderer \
                 is not used"
                    .to_string(),
            )
        }
        (Arch::X64, Some(Dx::OpenGL)) => (
            "d3dmetal",
            "OpenGL title — Wine's own GL path handles it; the renderer is not used".to_string(),
        ),
        (Arch::X64, None) => (
            "d3dmetal",
            "no graphics API named in the binary — using the default; it may load one at runtime"
                .to_string(),
        ),
        (Arch::Unknown, _) => (
            "d3dmetal",
            "could not read the machine type — using defaults".to_string(),
        ),
    };

    if info.arch == Arch::X86 {
        warnings.push(
            "32-bit needs a prefix with a WoW64 layer; exe-metal creates one by default.".into(),
        );
    }

    Plan {
        runtime,
        renderer,
        why,
        warnings,
    }
}

fn read_capped(path: &Path, cap: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(cap as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn starts_with_mz(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == b'M' && b[1] == b'Z'
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeStatus {
    /// No MZ header — not a Windows binary at all.
    NotPe,
    /// MZ present, but the PE header is missing, out of bounds, or unreadable.
    Damaged,
    Ok(Arch),
}

/// Walk MZ -> e_lfanew -> PE\0\0 -> COFF machine field.
///
/// A machine value we do not recognise is still a structurally valid PE, so it
/// reports `Ok(Unknown)` rather than Damaged — the file is fine, we just have
/// no opinion about its architecture.
fn pe_status(b: &[u8]) -> PeStatus {
    if !starts_with_mz(b) {
        return PeStatus::NotPe;
    }
    if b.len() < 0x40 {
        return PeStatus::Damaged;
    }
    let e_lfanew = u32::from_le_bytes([b[0x3c], b[0x3d], b[0x3e], b[0x3f]]) as usize;
    // Need the 4-byte signature plus the 2-byte machine field that follows it.
    if e_lfanew + 6 > b.len() {
        return PeStatus::Damaged;
    }
    if &b[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
        return PeStatus::Damaged;
    }
    let machine = u16::from_le_bytes([b[e_lfanew + 4], b[e_lfanew + 5]]);
    PeStatus::Ok(match machine {
        0x014c => Arch::X86,
        0x8664 => Arch::X64,
        0xaa64 => Arch::Arm64,
        _ => Arch::Unknown,
    })
}

#[cfg(test)]
fn read_arch(b: &[u8]) -> Arch {
    match pe_status(b) {
        PeStatus::Ok(a) => a,
        _ => Arch::Unknown,
    }
}

fn detect_apis(b: &[u8]) -> Vec<Dx> {
    // Order matters only in that we dedupe; `highest_api` does the ranking.
    let probes: &[(&[u8], Dx)] = &[
        (b"d3d12.dll", Dx::D3D12),
        (b"d3d12core.dll", Dx::D3D12),
        (b"d3d11.dll", Dx::D3D11),
        (b"d3d10.dll", Dx::D3D10),
        (b"d3d10core.dll", Dx::D3D10),
        (b"d3d9.dll", Dx::D3D9),
        (b"ddraw.dll", Dx::DirectDraw),
        (b"vulkan-1.dll", Dx::Vulkan),
        (b"opengl32.dll", Dx::OpenGL),
    ];
    let mut found: Vec<Dx> = Vec::new();
    for (needle, api) in probes {
        if contains_ci(b, needle) && !found.contains(api) {
            found.push(*api);
        }
    }
    found.sort();
    found
}

fn detect_installer(b: &[u8]) -> Option<&'static str> {
    // Inno matters specifically: its 32-bit installers crash on the new WoW64
    // in Wine 11.14, which is the kind of thing worth naming up front.
    if contains_ci(b, b"Inno Setup") || contains_ci(b, b"JR.Inno.Setup") {
        return Some("an Inno Setup");
    }
    if contains_ci(b, b"Nullsoft.NSIS") || contains_ci(b, b"NullsoftInst") {
        return Some("an NSIS");
    }
    if contains_ci(b, b"InstallShield") {
        return Some("an InstallShield");
    }
    None
}

/// Case-insensitive substring search over bytes.
///
/// Done in place rather than by lowercasing a copy: these buffers reach 64 MB
/// and duplicating one to find a nine-byte string is wasteful.
fn contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    let first = needle[0].to_ascii_lowercase();
    for i in 0..=(haystack.len() - needle.len()) {
        if haystack[i].to_ascii_lowercase() != first {
            continue;
        }
        if haystack[i..i + needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but structurally valid PE far enough to read `machine`.
    fn fake_pe(machine: u16, tail: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 0x80];
        b[0] = b'M';
        b[1] = b'Z';
        let e_lfanew: u32 = 0x40;
        b[0x3c..0x40].copy_from_slice(&e_lfanew.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        b[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
        b.extend_from_slice(tail);
        b
    }

    #[test]
    fn reads_bitness_from_the_coff_header() {
        assert_eq!(read_arch(&fake_pe(0x8664, b"")), Arch::X64);
        assert_eq!(read_arch(&fake_pe(0x014c, b"")), Arch::X86);
        assert_eq!(read_arch(&fake_pe(0xaa64, b"")), Arch::Arm64);
    }

    #[test]
    fn rejects_non_pe_input_instead_of_guessing() {
        assert_eq!(read_arch(b"not an exe at all"), Arch::Unknown);
        assert_eq!(read_arch(&[]), Arch::Unknown);
        // MZ present but e_lfanew points past the end.
        let mut truncated = vec![0u8; 0x40];
        truncated[0] = b'M';
        truncated[1] = b'Z';
        truncated[0x3c..0x40].copy_from_slice(&0xffff_u32.to_le_bytes());
        assert_eq!(read_arch(&truncated), Arch::Unknown);
    }

    /// A half-extracted download is the common real case — an MZ header and
    /// nothing behind it. That must be named as damaged, not silently accepted
    /// and turned into a bottle that fails obscurely inside Wine.
    #[test]
    fn damaged_pe_is_distinguished_from_not_a_pe() {
        assert_eq!(pe_status(b"plain text file"), PeStatus::NotPe);
        assert_eq!(pe_status(&[]), PeStatus::NotPe);

        // MZ, then nothing.
        assert_eq!(pe_status(b"MZ"), PeStatus::Damaged);

        // MZ and a full DOS header, but e_lfanew points past the end.
        let mut t = vec![0u8; 0x40];
        t[0] = b'M';
        t[1] = b'Z';
        t[0x3c..0x40].copy_from_slice(&0xffff_u32.to_le_bytes());
        assert_eq!(pe_status(&t), PeStatus::Damaged);

        // e_lfanew in range, but the signature is not "PE\0\0".
        let mut s = vec![0u8; 0x80];
        s[0] = b'M';
        s[1] = b'Z';
        s[0x3c..0x40].copy_from_slice(&0x40_u32.to_le_bytes());
        s[0x40..0x44].copy_from_slice(b"XXXX");
        assert_eq!(pe_status(&s), PeStatus::Damaged);

        // A valid PE with a machine value we do not know is not damaged.
        assert_eq!(
            pe_status(&fake_pe(0x1234, b"")),
            PeStatus::Ok(Arch::Unknown)
        );
    }

    #[test]
    fn case_insensitive_search_matches_mixed_case() {
        assert!(contains_ci(b"xxD3D11.DLLxx", b"d3d11.dll"));
        assert!(contains_ci(b"d3d11.dll", b"D3D11.DLL"));
        assert!(!contains_ci(b"d3d9.dll", b"d3d11.dll"));
        assert!(!contains_ci(b"", b"a"));
    }

    #[test]
    fn highest_api_wins_when_several_are_named() {
        let b = fake_pe(0x8664, b"d3d9.dll\0d3d11.dll\0d3d12.dll\0");
        let apis = detect_apis(&b);
        assert!(apis.contains(&Dx::D3D9));
        assert!(apis.contains(&Dx::D3D12));
        let info = ExeInfo {
            path: PathBuf::from("x.exe"),
            arch: Arch::X64,
            apis,
            installer: None,
        };
        assert_eq!(info.highest_api(), Some(Dx::D3D12));
        assert_eq!(plan(&info).renderer, "d3dmetal");
    }

    #[test]
    fn thirty_two_bit_never_gets_d3dmetal() {
        // D3DMetal has no 32-bit build, so no combination of APIs may select it.
        for tail in [
            &b"d3d11.dll"[..],
            &b"d3d12.dll"[..],
            &b"d3d10.dll"[..],
            &b""[..],
        ] {
            let info = ExeInfo {
                path: PathBuf::from("x.exe"),
                arch: Arch::X86,
                apis: detect_apis(&fake_pe(0x014c, tail)),
                installer: None,
            };
            let p = plan(&info);
            assert_ne!(
                p.renderer, "d3dmetal",
                "32-bit with tail {tail:?} chose a 64-bit-only renderer"
            );
        }
    }

    #[test]
    fn thirty_two_bit_directx_nine_prefers_d9vk_over_dxmt() {
        let info = ExeInfo {
            path: PathBuf::from("x.exe"),
            arch: Arch::X86,
            apis: detect_apis(&fake_pe(0x014c, b"d3d9.dll\0")),
            installer: None,
        };
        assert_eq!(plan(&info).renderer, "d9vk");
    }

    #[test]
    fn every_plan_pairs_dxmt_with_a_winemetal_runtime() {
        // check_pairing rejects dxmt on a runtime without winemetal, so a plan
        // that produced one would fail at bottle creation.
        let info = ExeInfo {
            path: PathBuf::from("x.exe"),
            arch: Arch::X86,
            apis: vec![Dx::D3D11],
            installer: None,
        };
        let p = plan(&info);
        assert_eq!(p.renderer, "dxmt");
        assert!(crate::catalog::check_pairing(p.runtime, p.renderer).is_ok());
    }

    #[test]
    fn installers_are_called_out() {
        let b = fake_pe(0x014c, b"This installation was built with Inno Setup.");
        assert_eq!(detect_installer(&b), Some("an Inno Setup"));
    }
}
