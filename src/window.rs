//! Window, input and display behaviour.
//!
//! Wine's defaults on macOS optimise for "act like a Windows machine", which
//! produces four complaints, all of them the same root cause — Wine taking
//! ownership of the display and the input devices:
//!
//!   * the pointer cannot leave the game window,
//!   * the window cannot be dragged to a second monitor,
//!   * cmd-tab / alt-tab does not switch away cleanly,
//!   * resizing fights back or corrupts the backing store.
//!
//! `CaptureDisplaysForFullscreen` is the big one. When Wine captures the
//! display it takes the whole screen away from the window server, so macOS can
//! no longer composite other windows over it, move it, or hand the pointer to
//! anything else. Turning it off costs nothing on a Metal backend, which does
//! not need an exclusive display to present.
//!
//! Virtual desktop mode is the belt-and-braces version: the game renders into
//! an ordinary resizable macOS window, so every one of the above becomes the
//! window server's problem rather than Wine's.

use crate::bottle::Bottle;
use crate::env;
use crate::paths::Layout;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowCfg {
    /// Let Wine seize the whole display for fullscreen. Off by default: this
    /// is what traps the pointer and blocks a second monitor.
    #[serde(default)]
    pub capture_displays: bool,
    /// Render into an ordinary macOS window instead of driving the screen.
    ///
    /// On by default, because it is what makes a Windows application behave
    /// like a Mac one: it gets real traffic lights, it resizes, it moves
    /// between monitors, it goes full screen with the green button, and the
    /// pointer is never trapped. The application keeps a fixed backbuffer and
    /// never learns the window changed, which is also why resizing stops
    /// crashing games that only ever expected one resolution.
    ///
    /// It additionally gives the guest a display-mode list. Without one,
    /// GoldSrc titles find no modes to choose from and fall back to 640x480 —
    /// which is exactly why Cry of Fear opened as a tiny black square.
    #[serde(default = "yes")]
    pub virtual_desktop: bool,
    /// Size of that window.
    #[serde(default = "default_desktop_size")]
    pub desktop_size: String,
    /// Report the real backing scale so text is not blurry on Retina.
    ///
    /// On its own this makes everything *smaller*, not sharper: Wine starts
    /// reporting the full pixel resolution while still laying out at 96 DPI,
    /// so a 3024x1964 panel gets a Steam window sized for 1512x982. `dpi`
    /// below is the other half of the pair and must move with it.
    #[serde(default = "yes")]
    pub retina: bool,
    /// Windows DPI, as `HKCU\Control Panel\Desktop\LogPixels`.
    ///
    /// 96 is 100%, 144 is 150%, 192 is 200% — the same scale factors the
    /// Windows display settings offer, and the mechanism every Windows
    /// application already understands. This is what makes Steam and games
    /// render at a sensible size rather than half of one, and it is real
    /// layout scaling: windows reflow instead of being pixel-stretched.
    #[serde(default = "default_dpi")]
    pub dpi: u32,
    /// Use the macOS cursor rather than Wine drawing its own, which is what
    /// makes the pointer feel like it is lagging behind the real one.
    #[serde(default = "yes")]
    pub system_cursor: bool,
    /// Let Wine take cmd-tab and the like. Off, so macOS keeps its shortcuts.
    #[serde(default)]
    pub grab_hotkeys: bool,
}

fn default_desktop_size() -> String {
    "1920x1080".into()
}

/// The logical size of the main display, which is what a window should match
/// to feel native rather than arbitrary.
///
/// Uses the "UI Looks like" figure rather than the panel's pixel count: on a
/// Retina display those differ by the scale factor, and the pixel count would
/// produce a guest desktop twice the size of the screen it has to fit on.
pub fn detect_display_size() -> Option<String> {
    let out = crate::util::capture("system_profiler", &["SPDisplaysDataType"]).ok()?;
    let line = out.lines().find(|l| l.contains("UI Looks like"))?;
    let mut it = line.split_whitespace().skip_while(|t| *t != "like");
    it.next()?;
    let w: u32 = it.next()?.parse().ok()?;
    it.next()?; // "x"
    let h: u32 = it.next()?.trim_end_matches('@').parse().ok()?;
    (w > 0 && h > 0).then(|| format!("{w}x{h}"))
}

/// 192 = 200%, which pairs with RetinaMode on a 2x display. Chosen rather than
/// detected because the alternative — 96 — is what produced a half-size Steam.
fn default_dpi() -> u32 {
    192
}

/// Windows accepts arbitrary values but applications are only well behaved at
/// the standard steps, so snap to them.
pub fn snap_dpi(v: u32) -> u32 {
    const STEPS: [u32; 7] = [96, 120, 144, 168, 192, 240, 288];
    *STEPS.iter().min_by_key(|s| s.abs_diff(v)).unwrap_or(&192)
}

/// What a DPI value means as a percentage, for display.
pub fn dpi_percent(dpi: u32) -> u32 {
    (dpi as f32 / 96.0 * 100.0).round() as u32
}
fn yes() -> bool {
    true
}

impl Default for WindowCfg {
    fn default() -> Self {
        Self {
            capture_displays: false,
            virtual_desktop: true,
            desktop_size: detect_display_size().unwrap_or_else(default_desktop_size),
            retina: true,
            dpi: default_dpi(),
            system_cursor: true,
            grab_hotkeys: false,
        }
    }
}

impl WindowCfg {
    /// Arguments that turn a bare exe launch into a windowed one.
    ///
    /// Done at launch time rather than through the registry because the
    /// registry form is global to the prefix, and a bottle may hold both a
    /// launcher that wants a window and a game that does not.
    pub fn desktop_args(&self) -> Option<Vec<String>> {
        if !self.virtual_desktop {
            return None;
        }
        let size = if self.desktop_size.contains('x') {
            self.desktop_size.clone()
        } else {
            default_desktop_size()
        };
        Some(vec![
            "explorer".into(),
            format!("/desktop=exe-metal,{size}"),
        ])
    }
}

fn b(v: bool) -> &'static str {
    if v {
        "Y"
    } else {
        "N"
    }
}

/// Write the window/input settings into a prefix.
///
/// Idempotent, and safe to run on an existing bottle — it only touches the
/// specific values it owns.
pub fn apply(layout: &Layout, bottle: &Bottle) -> Result<()> {
    let wine = bottle.wine(layout)?;
    let vars = env::build(layout, bottle);
    let w = &bottle.window;

    let settings: &[(&str, &str, &str)] = &[
        // The one that causes most of the trouble.
        (
            r"HKCU\Software\Wine\Mac Driver",
            "CaptureDisplaysForFullscreen",
            b(w.capture_displays),
        ),
        (r"HKCU\Software\Wine\Mac Driver", "RetinaMode", b(w.retina)),
        (
            r"HKCU\Software\Wine\Mac Driver",
            "UseSystemCursor",
            b(w.system_cursor),
        ),
        // Wine reads this generically for hotkey grabbing, Mac driver included.
        (
            r"HKCU\Software\Wine\Mac Driver",
            "UseSystemKeyboardShortcuts",
            b(!w.grab_hotkeys),
        ),
        // Belongs to the X11 key by name but is honoured as a general
        // "may I confine the pointer" switch.
        (
            r"HKCU\Software\Wine\X11 Driver",
            "GrabFullscreen",
            b(w.capture_displays),
        ),
        (
            r"HKCU\Software\Wine\X11 Driver",
            "GrabPointer",
            b(w.capture_displays),
        ),
    ];

    // DPI is a DWORD under Control Panel\Desktop rather than a Y/N string, so
    // it cannot ride along in the table above.
    let dpi = snap_dpi(w.dpi);
    let status = Command::new(&wine)
        .args([
            "reg",
            "add",
            r"HKCU\Control Panel\Desktop",
            "/v",
            "LogPixels",
            "/t",
            "REG_DWORD",
            "/d",
            &dpi.to_string(),
            "/f",
        ])
        .envs(&vars)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("setting LogPixels")?;
    if !status.success() {
        eprintln!("  warn    could not set the Windows DPI");
    }

    for (key, name, value) in settings {
        let status = Command::new(&wine)
            .args(["reg", "add", key, "/v", name, "/d", value, "/f"])
            .envs(&vars)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("setting {name}"))?;
        if !status.success() {
            eprintln!("  warn    could not set {name}");
        }
    }
    Ok(())
}

/// One-line description for `show` and the UI.
pub fn describe(w: &WindowCfg) -> String {
    let mut parts = Vec::new();
    parts.push(if w.virtual_desktop {
        format!("virtual desktop {}", w.desktop_size)
    } else {
        "native windows".to_string()
    });
    if !w.capture_displays {
        parts.push("display not captured".into());
    } else {
        parts.push("CAPTURES DISPLAY (pointer will be trapped)".into());
    }
    if w.retina {
        parts.push("retina".into());
    }
    parts.push(format!("{}% dpi", dpi_percent(snap_dpi(w.dpi))));
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_apps_get_a_native_macos_window_by_default() {
        // Traffic lights, resizing, green-button full screen and a free
        // pointer all come from this one setting.
        let w = WindowCfg::default();
        assert!(w.virtual_desktop);
        assert!(w.desktop_args().is_some());
    }

    #[test]
    fn defaults_do_not_trap_the_pointer() {
        let w = WindowCfg::default();
        assert!(
            !w.capture_displays,
            "capturing the display is what traps the pointer and blocks a second monitor"
        );
        assert!(!w.grab_hotkeys, "macOS should keep cmd-tab");
        assert!(w.system_cursor);
    }

    #[test]
    fn desktop_args_are_produced_by_default_and_suppressible() {
        let mut w = WindowCfg::default();
        let a = w.desktop_args().expect("native windowing is the default");
        assert_eq!(a[0], "explorer");
        assert!(a[1].starts_with("/desktop=exe-metal,"));

        // Opting out has to remain possible: a game that drives the display
        // itself does not want a desktop wrapped around it.
        w.virtual_desktop = false;
        assert!(w.desktop_args().is_none());
    }

    #[test]
    fn a_malformed_size_falls_back_rather_than_producing_a_broken_flag() {
        let w = WindowCfg {
            virtual_desktop: true,
            desktop_size: "enormous".into(),
            ..Default::default()
        };
        assert!(w.desktop_args().unwrap()[1].contains(&default_desktop_size()));
    }

    #[test]
    fn dpi_snaps_to_the_steps_windows_apps_expect() {
        assert_eq!(snap_dpi(190), 192);
        assert_eq!(snap_dpi(100), 96);
        assert_eq!(snap_dpi(150), 144);
        assert_eq!(snap_dpi(0), 96);
        assert_eq!(snap_dpi(9999), 288);
    }

    #[test]
    fn the_default_is_two_hundred_percent() {
        // Paired with retina. 96 with RetinaMode on is what produced a
        // half-size Steam window, so the default must not be 96.
        let w = WindowCfg::default();
        assert!(w.retina);
        assert_eq!(dpi_percent(w.dpi), 200);
        assert_ne!(w.dpi, 96, "96 with retina renders everything half size");
    }

    #[test]
    fn describe_calls_out_the_dangerous_setting() {
        let w = WindowCfg {
            capture_displays: true,
            ..Default::default()
        };
        assert!(describe(&w).contains("CAPTURES DISPLAY"));
    }
}
