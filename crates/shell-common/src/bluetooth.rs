//! Bluetooth state, read through BlueZ's `bluetoothctl`.
//!
//! Like the rest of the shell, querying and toggling shell out to the standard
//! CLI rather than talking D-Bus directly.

use std::process::Command;

/// The Bluetooth controller's power and connection state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bluetooth {
    /// Whether the controller is powered on.
    pub powered: bool,
    /// Names of the currently connected devices.
    pub devices: Vec<String>,
}

/// Read the current Bluetooth state, or `None` when there is no controller (or
/// `bluetoothctl` is unavailable / the service isn't running).
pub fn read() -> Option<Bluetooth> {
    let out = Command::new("bluetoothctl").arg("show").output().ok()?;

    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);

    if !text
        .lines()
        .any(|line| line.trim_start().starts_with("Controller "))
    {
        return None;
    }

    let powered = text.lines().any(|line| line.trim() == "Powered: yes");
    let devices = if powered {
        connected_devices()
    } else {
        Vec::new()
    };

    Some(Bluetooth { powered, devices })
}

/// Power the controller on or off.
pub fn set_powered(on: bool) {
    let _ = Command::new("bluetoothctl")
        .args(["power", if on { "on" } else { "off" }])
        .output();
}

/// The names of devices `bluetoothctl` reports as connected.
fn connected_devices() -> Vec<String> {
    let Ok(out) = Command::new("bluetoothctl")
        .args(["devices", "Connected"])
        .output()
    else {
        return Vec::new();
    };

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(device_name)
        .collect()
}

/// Pull the name from a `Device AA:BB:CC:DD:EE:FF Name` line, falling back to
/// the address when the name is empty.
fn device_name(line: &str) -> Option<String> {
    let mut fields = line.split_whitespace();
    if fields.next()? != "Device" {
        return None;
    }
    let address = fields.next()?;
    let name = fields.collect::<Vec<_>>().join(" ");
    Some(if name.is_empty() {
        address.to_string()
    } else {
        name
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_device_line_name() {
        assert_eq!(
            device_name("Device AA:BB:CC:DD:EE:FF Sony WH-1000XM5"),
            Some("Sony WH-1000XM5".to_string())
        );
    }

    #[test]
    fn keeps_spaces_in_a_name() {
        assert_eq!(
            device_name("Device 11:22:33:44:55:66 My Fancy Keyboard"),
            Some("My Fancy Keyboard".to_string())
        );
    }

    #[test]
    fn falls_back_to_the_address() {
        assert_eq!(
            device_name("Device AA:BB:CC:DD:EE:FF"),
            Some("AA:BB:CC:DD:EE:FF".to_string())
        );
    }

    #[test]
    fn ignores_non_device_lines() {
        assert_eq!(device_name("not a device line"), None);
    }
}
