//! Bluetooth state, read through BlueZ's `bluetoothctl`.
//!
//! Like the rest of the shell, querying and toggling shell out to the standard
//! CLI rather than talking D-Bus directly.

use std::process::Command;

/// The Bluetooth controller's power and connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bluetooth {
    /// Whether the controller is powered on.
    pub powered: bool,
    /// Number of currently connected devices.
    pub connected: u32,
}

/// Read the current Bluetooth state, or `None` when there is no controller (or
/// `bluetoothctl` is unavailable / the service isn't running).
pub fn read() -> Option<Bluetooth> {
    let out = Command::new("bluetoothctl").arg("show").output().ok()?;

    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);

    if !text.lines().any(|line| line.trim_start().starts_with("Controller ")) {
        return None;
    }

    let powered = text
        .lines()
        .any(|line| line.trim() == "Powered: yes");
    let connected = if powered { connected_count() } else { 0 };

    Some(Bluetooth { powered, connected })
}

/// Power the controller on or off.
pub fn set_powered(on: bool) {
    let _ = Command::new("bluetoothctl")
        .args(["power", if on { "on" } else { "off" }])
        .output();
}

/// How many devices `bluetoothctl` reports as connected.
fn connected_count() -> u32 {
    let Ok(out) = Command::new("bluetoothctl")
        .args(["devices", "Connected"])
        .output()
    else {
        return 0;
    };

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.trim_start().starts_with("Device "))
        .count() as u32
}
