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
    /// Render into a resizable macOS window instead of driving the screen.
    /// The most reliable fix for pointer capture and multi-monitor dragging.
    #[serde(default)]
    pub virtual_desktop: bool,
    /// Size of that window.
    #[serde(default = "default_desktop_size")]
    pub desktop_size: String,
    /// Report the real backing scale so text is not blurry on Retina.
    #[serde(default = "yes")]
    pub retina: bool,
    /// Use the macOS cursor rather than Wine drawing its own, which is what
    /// makes the pointer feel like it is lagging behind the real one.
    #[serde(default = "yes")]
    pub system_cursor: bool,
    /// Let Wine take cmd-tab and the like. Off, so macOS keeps its shortcuts.
    #[serde(default)]
    pub grab_hotkeys: bool,
}

fn default_desktop_size() -> String {
    "1600x900".into()
}
fn yes() -> bool {
    true
}

impl Default for WindowCfg {
    fn default() -> Self {
        Self {
            capture_displays: false,
            virtual_desktop: false,
            desktop_size: default_desktop_size(),
            retina: true,
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
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn desktop_args_only_when_asked() {
        let mut w = WindowCfg::default();
        assert!(w.desktop_args().is_none());
        w.virtual_desktop = true;
        let a = w.desktop_args().unwrap();
        assert_eq!(a[0], "explorer");
        assert!(a[1].contains("1600x900"));
    }

    #[test]
    fn a_malformed_size_falls_back_rather_than_producing_a_broken_flag() {
        let w = WindowCfg {
            virtual_desktop: true,
            desktop_size: "enormous".into(),
            ..Default::default()
        };
        assert!(w.desktop_args().unwrap()[1].contains("1600x900"));
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
