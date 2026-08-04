//! What machine is this, and what can it actually do?
//!
//! Everything downstream of here is a capability gate rather than a guess.
//! Three of them matter enough to change behaviour:
//!
//!   * **Hardware ray tracing arrived with the M3.** On M1 and M2 the silicon
//!     has no RT units, so `D3DM_SUPPORT_DXR` buys a slideshow rather than
//!     reflections. Turning it on there is strictly worse than leaving it off.
//!   * **Rosetta only advertises AVX on macOS 15 and newer.** Setting the
//!     variable on 14 is harmless but does nothing, so promising it there is a
//!     lie the user cannot check.
//!   * **Memory is the real ceiling.** An 8 GB M1 shares that between the game,
//!     Wine, and the GPU. It runs; it just cannot run everything.

use crate::util;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Silicon {
    /// Apple Silicon, with the generation number and the variant suffix.
    /// `Apple M5 Pro` becomes `{ generation: 5, variant: Some("Pro") }`.
    Apple {
        generation: u32,
        variant: Option<String>,
    },
    Intel,
    Unknown,
}

impl fmt::Display for Silicon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Silicon::Apple {
                generation,
                variant,
            } => match variant {
                Some(v) => write!(f, "M{generation} {v}"),
                None => write!(f, "M{generation}"),
            },
            Silicon::Intel => write!(f, "Intel"),
            Silicon::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Machine {
    pub arch: String,
    /// The raw `machdep.cpu.brand_string`, e.g. "Apple M5 Pro".
    pub cpu_brand: String,
    pub silicon: Silicon,
    /// Board identifier, e.g. "Mac17,9".
    pub model: String,
    pub memory_gb: u64,
    pub perf_cores: u32,
    pub eff_cores: u32,
    pub total_cores: u32,
    pub macos: (u32, u32),
    pub macos_raw: String,
    pub rosetta: bool,
}

impl Machine {
    pub fn detect() -> Self {
        let arch = util::capture("uname", &["-m"]).unwrap_or_default();
        let cpu_brand = sysctl_str("machdep.cpu.brand_string");
        let silicon = parse_silicon(&cpu_brand);
        let model = sysctl_str("hw.model");

        let memory_gb = sysctl_u64("hw.memsize").unwrap_or(0) / 1024 / 1024 / 1024;
        // perflevel0 is the performance cluster, perflevel1 the efficiency one.
        // Both are absent on Intel, where ncpu is the only meaningful number.
        let perf_cores = sysctl_u64("hw.perflevel0.physicalcpu").unwrap_or(0) as u32;
        let eff_cores = sysctl_u64("hw.perflevel1.physicalcpu").unwrap_or(0) as u32;
        let total_cores = sysctl_u64("hw.ncpu").unwrap_or(0) as u32;

        let macos_raw = util::capture("sw_vers", &["-productVersion"]).unwrap_or_default();
        let macos = parse_version(&macos_raw);

        Self {
            arch,
            cpu_brand,
            silicon,
            model,
            memory_gb,
            perf_cores,
            eff_cores,
            total_cores,
            macos,
            macos_raw,
            rosetta: util::rosetta_present(),
        }
    }

    pub fn is_apple_silicon(&self) -> bool {
        matches!(self.silicon, Silicon::Apple { .. })
    }

    pub fn generation(&self) -> u32 {
        match self.silicon {
            Silicon::Apple { generation, .. } => generation,
            _ => 0,
        }
    }

    /// Hardware ray tracing exists on M3 and later. Enabling DXR without it
    /// falls back to a software path that is far slower than not asking.
    pub fn has_hardware_raytracing(&self) -> bool {
        self.is_apple_silicon() && self.generation() >= 3
    }

    /// Rosetta gained AVX/AVX2 advertisement in macOS 15. On 14 the variable is
    /// accepted and ignored.
    pub fn rosetta_advertises_avx(&self) -> bool {
        self.macos.0 >= 15
    }

    /// MetalFX upscaling needs macOS 13 and Apple Silicon. Every machine this
    /// tool supports clears that bar, but the check is cheap and states why.
    pub fn has_metalfx(&self) -> bool {
        self.is_apple_silicon() && self.macos.0 >= 13
    }

    /// macOS 14 is the floor: the Wine builds exe-metal ships link against
    /// system libraries that are not present on 13.
    pub fn macos_supported(&self) -> bool {
        self.macos.0 >= 14
    }

    /// Below this, big modern titles will swap rather than stutter. Not a
    /// failure — an 8 GB M1 runs plenty — but it changes what to recommend.
    pub fn memory_is_tight(&self) -> bool {
        self.memory_gb > 0 && self.memory_gb < 16
    }

    /// A one-line summary for logs and the UI title bar.
    pub fn summary(&self) -> String {
        format!(
            "{} · {} GB · macOS {}",
            self.silicon, self.memory_gb, self.macos_raw
        )
    }

    /// Everything that would stop a game running, in the order it bites.
    /// Returns (ok, label, remedy) so callers can render it however they like.
    pub fn blocking_issues(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if !self.is_apple_silicon() {
            out.push((
                format!("this machine reports {}, not Apple Silicon", self.cpu_brand),
                "exe-metal translates DirectX to Metal on Apple GPUs; there is no Intel path."
                    .to_string(),
            ));
        }
        if !self.macos_supported() {
            out.push((
                format!("macOS {} is too old", self.macos_raw),
                "macOS 14 (Sonoma) or newer is required.".to_string(),
            ));
        }
        if !self.rosetta {
            out.push((
                "Rosetta 2 is not installed".to_string(),
                "softwareupdate --install-rosetta --agree-to-license".to_string(),
            ));
        }
        out
    }
}

/// Parse `Apple M5 Pro` / `Apple M1` / `Apple M2 Max` into a generation and
/// variant. Written to accept generations that do not exist yet: the number is
/// read rather than matched, so an M6 identifies correctly without a code
/// change and simply inherits the M3+ capability answers.
fn parse_silicon(brand: &str) -> Silicon {
    let b = brand.trim();
    if b.is_empty() {
        return Silicon::Unknown;
    }
    if b.contains("Intel") {
        return Silicon::Intel;
    }
    let Some(rest) = b.strip_prefix("Apple ") else {
        return Silicon::Unknown;
    };
    let mut parts = rest.split_whitespace();
    let Some(chip) = parts.next() else {
        return Silicon::Unknown;
    };
    let Some(digits) = chip.strip_prefix('M') else {
        return Silicon::Unknown;
    };
    // "M5" -> 5. Anything non-numeric here is a chip naming scheme we do not
    // know, and claiming a generation for it would be worse than admitting so.
    let Ok(generation) = digits.parse::<u32>() else {
        return Silicon::Unknown;
    };
    let variant = parts.next().map(|s| s.to_string());
    Silicon::Apple {
        generation,
        variant,
    }
}

fn parse_version(v: &str) -> (u32, u32) {
    let mut it = v.split('.');
    let major = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor)
}

fn sysctl_str(key: &str) -> String {
    util::capture("sysctl", &["-n", key]).unwrap_or_default()
}

fn sysctl_u64(key: &str) -> Option<u64> {
    util::capture("sysctl", &["-n", key])
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_shipping_apple_chip() {
        let cases = [
            ("Apple M1", 1, None),
            ("Apple M1 Pro", 1, Some("Pro")),
            ("Apple M1 Max", 1, Some("Max")),
            ("Apple M1 Ultra", 1, Some("Ultra")),
            ("Apple M2", 2, None),
            ("Apple M2 Pro", 2, Some("Pro")),
            ("Apple M3 Max", 3, Some("Max")),
            ("Apple M4", 4, None),
            ("Apple M4 Pro", 4, Some("Pro")),
            ("Apple M5 Pro", 5, Some("Pro")),
        ];
        for (brand, gen, variant) in cases {
            match parse_silicon(brand) {
                Silicon::Apple {
                    generation,
                    variant: v,
                } => {
                    assert_eq!(generation, gen, "generation for {brand}");
                    assert_eq!(v.as_deref(), variant, "variant for {brand}");
                }
                other => panic!("{brand} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_generations_parse_rather_than_panic() {
        // The point of reading the digit instead of matching a list.
        assert_eq!(
            parse_silicon("Apple M9 Ultra"),
            Silicon::Apple {
                generation: 9,
                variant: Some("Ultra".into())
            }
        );
    }

    #[test]
    fn non_apple_brands_are_not_claimed() {
        assert_eq!(
            parse_silicon("Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz"),
            Silicon::Intel
        );
        assert_eq!(parse_silicon(""), Silicon::Unknown);
        assert_eq!(parse_silicon("Apple Silicon"), Silicon::Unknown);
    }

    #[test]
    fn raytracing_gate_matches_the_hardware() {
        let m = |g: u32| Machine {
            arch: "arm64".into(),
            cpu_brand: format!("Apple M{g}"),
            silicon: Silicon::Apple {
                generation: g,
                variant: None,
            },
            model: String::new(),
            memory_gb: 16,
            perf_cores: 4,
            eff_cores: 4,
            total_cores: 8,
            macos: (26, 0),
            macos_raw: "26.0".into(),
            rosetta: true,
        };
        // M1 and M2 have no RT units; M3 introduced them.
        assert!(!m(1).has_hardware_raytracing());
        assert!(!m(2).has_hardware_raytracing());
        assert!(m(3).has_hardware_raytracing());
        assert!(m(5).has_hardware_raytracing());
    }

    #[test]
    fn version_parsing_survives_short_strings() {
        assert_eq!(parse_version("26.5"), (26, 5));
        assert_eq!(parse_version("15"), (15, 0));
        assert_eq!(parse_version(""), (0, 0));
    }
}
