//! Wi-Fi state, read through NetworkManager's `nmcli`.
//!
//! Querying and toggling are both plain subprocess calls, matching the panel's
//! existing `niri msg` approach. The list query passes `--rescan no` so polling
//! never triggers an access-point scan.

use std::process::Command;

/// The Wi-Fi radio and, when connected, its active network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wifi {
    /// Whether the Wi-Fi radio is switched on.
    pub enabled: bool,
    /// SSID of the connected network, if any.
    pub ssid: Option<String>,
    /// Signal strength (0–100) of the connected network, if any.
    pub signal: Option<u8>,
}

/// Read the current Wi-Fi state, or `None` when the machine has no Wi-Fi
/// hardware (or `nmcli` is unavailable).
pub fn read() -> Option<Wifi> {
    let (wifi, hardware) = general()?;

    // `WIFI-HW` is `missing` when there is no radio at all.
    if hardware == "missing" {
        return None;
    }

    let enabled = wifi == "enabled";
    let (ssid, signal) = if enabled {
        active_network().unwrap_or((None, None))
    } else {
        (None, None)
    };

    Some(Wifi {
        enabled,
        ssid,
        signal,
    })
}

/// Switch the radio on or off.
pub fn set_enabled(on: bool) {
    let _ = Command::new("nmcli")
        .args(["radio", "wifi", if on { "on" } else { "off" }])
        .output();
}

/// The `(WIFI, WIFI-HW)` pair from `nmcli general`.
fn general() -> Option<(String, String)> {
    let out = Command::new("nmcli")
        .args(["-t", "-f", "WIFI,WIFI-HW", "general"])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = split_terse(text.trim());
    let hardware = fields.pop()?;
    let wifi = fields.pop()?;

    Some((wifi, hardware))
}

/// The `(ssid, signal)` of the currently active access point.
fn active_network() -> Option<(Option<String>, Option<u8>)> {
    let out = Command::new("nmcli")
        .args([
            "-t",
            "-f",
            "ACTIVE,SSID,SIGNAL",
            "device",
            "wifi",
            "list",
            "--rescan",
            "no",
        ])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);

    for line in text.lines() {
        let fields = split_terse(line);
        if fields.first().map(String::as_str) != Some("yes") {
            continue;
        }

        let ssid = fields.get(1).filter(|ssid| !ssid.is_empty()).cloned();
        let signal = fields.get(2).and_then(|signal| signal.parse().ok());

        return Some((ssid, signal));
    }

    None
}

/// Split a `nmcli -t` line on unescaped `:` and undo `\:` / `\\` escaping.
fn split_terse(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut escaped = false;

    for ch in line.chars() {
        match ch {
            '\\' if !escaped => escaped = true,
            ':' if !escaped => fields.push(std::mem::take(&mut field)),
            _ => {
                field.push(ch);
                escaped = false;
            }
        }
    }

    fields.push(field);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_plain_fields() {
        assert_eq!(split_terse("yes:SPARK:51"), ["yes", "SPARK", "51"]);
    }

    #[test]
    fn keeps_escaped_colons() {
        assert_eq!(split_terse(r"yes:My\:Net:40"), ["yes", "My:Net", "40"]);
    }

    #[test]
    fn keeps_escaped_backslashes() {
        assert_eq!(split_terse(r"a:b\\c"), ["a", r"b\c"]);
    }

    #[test]
    fn empty_ssid_stays_empty() {
        assert_eq!(split_terse("yes::51"), ["yes", "", "51"]);
    }
}
