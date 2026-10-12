//! Battery state, read from sysfs.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Charging state as reported by the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Charging,
    Discharging,
    Full,
    NotCharging,
    Unknown,
}

impl Status {
    fn parse(raw: &str) -> Self {
        match raw.trim() {
            "Charging" => Self::Charging,
            "Discharging" => Self::Discharging,
            "Full" => Self::Full,
            "Not charging" => Self::NotCharging,
            _ => Self::Unknown,
        }
    }
}

/// The first battery found under `/sys/class/power_supply`.
#[derive(Debug, Clone, Copy)]
pub struct Battery {
    pub percentage: u8,
    pub status: Status,
    /// Health: the full-charge capacity as a percentage of the design capacity.
    pub health: Option<u8>,
    /// Number of charge cycles the battery has been through.
    pub cycles: Option<u32>,
}

impl Battery {
    /// Reads the first battery, or `None` on a machine without one.
    pub fn read() -> Option<Self> {
        let entries = fs::read_dir("/sys/class/power_supply").ok()?;

        for entry in entries.flatten() {
            let path = entry.path();

            if fs::read_to_string(path.join("type"))
                .map(|ty| ty.trim() != "Battery")
                .unwrap_or(true)
            {
                continue;
            }

            let Some(percentage) =
                read_number(&path, "capacity").and_then(|n| u8::try_from(n).ok())
            else {
                continue;
            };
            let status = fs::read_to_string(path.join("status"))
                .map(|s| Status::parse(&s))
                .unwrap_or(Status::Unknown);

            return Some(Self {
                percentage,
                status,
                health: health(&path),
                cycles: read_number(&path, "cycle_count").and_then(|n| u32::try_from(n).ok()),
            });
        }

        None
    }
}

/// Read a sysfs file as an unsigned number, ignoring a missing/malformed one.
fn read_number(path: &Path, name: &str) -> Option<u64> {
    fs::read_to_string(path.join(name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Full-charge capacity as a percentage of the design capacity, preferring
/// energy over charge and clamping to 100 (fresh cells can read slightly over).
fn health(path: &Path) -> Option<u8> {
    let full = read_number(path, "energy_full");
    let design = read_number(path, "energy_full_design");
    let (full, design) = match (full, design) {
        (Some(full), Some(design)) => (full, design),
        _ => (
            read_number(path, "charge_full")?,
            read_number(path, "charge_full_design")?,
        ),
    };

    if design == 0 {
        return None;
    }

    Some((full * 100 / design).min(100) as u8)
}

/// The active system power profile as reported by power-profiles-daemon, e.g.
/// `"power-saver"`, `"balanced"` or `"performance"`. `None` when unavailable
/// (no daemon, or not installed).
pub fn power_profile() -> Option<String> {
    let out = Command::new("powerprofilesctl").arg("get").output().ok()?;

    if !out.status.success() {
        return None;
    }

    let profile = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!profile.is_empty()).then_some(profile)
}
