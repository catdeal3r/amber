//! Minimal niri IPC helpers, read through the `niri` CLI.
//!
//! niri has no D-Bus interface; the supported way to observe it is parsing
//! `niri msg` output. These helpers shell out to it, so they are only suitable
//! for low-frequency polling.

use std::process::Command;

use serde::Deserialize;

/// The focused window, reduced to the geometry we compare.
#[derive(Debug, Deserialize)]
struct FocusedWindow {
    layout: Layout,
}

#[derive(Debug, Deserialize)]
struct Layout {
    window_size: [i32; 2],
}

/// The focused output, reduced to its logical size.
#[derive(Debug, Deserialize)]
struct FocusedOutput {
    logical: Logical,
}

#[derive(Debug, Deserialize)]
struct Logical {
    width: i32,
    height: i32,
}

/// Whether the focused window currently fills its output — i.e. is fullscreen.
///
/// niri's IPC exposes no fullscreen flag, so this compares the focused window's
/// size against the focused output's logical size: a fullscreen window fills the
/// output exactly, whereas tiled and floating windows are inset by the layout
/// gaps.
pub fn is_fullscreen() -> bool {
    let (Some(window), Some(output)) = (focused_window(), focused_output()) else {
        return false;
    };
    covers_output(
        window.layout.window_size,
        [output.logical.width, output.logical.height],
    )
}

/// Whether a window of `window_size` fills an output of `output_size`.
fn covers_output(window_size: [i32; 2], output_size: [i32; 2]) -> bool {
    window_size[0] >= output_size[0] && window_size[1] >= output_size[1]
}

fn focused_window() -> Option<FocusedWindow> {
    let out = Command::new("niri")
        .args(["msg", "--json", "focused-window"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn focused_output() -> Option<FocusedOutput> {
    let out = Command::new("niri")
        .args(["msg", "--json", "focused-output"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_filling_the_output_is_fullscreen() {
        assert!(covers_output([1920, 1080], [1920, 1080]));
    }

    #[test]
    fn a_gapped_tile_is_not_fullscreen() {
        assert!(!covers_output([1760, 878], [1920, 1080]));
        assert!(!covers_output([1920, 878], [1920, 1080]));
    }
}
