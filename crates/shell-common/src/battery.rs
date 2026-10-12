//! Battery state, read from sysfs.

use std::fs;

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

            let Ok(capacity) = fs::read_to_string(path.join("capacity")) else {
                continue;
            };
            let Ok(percentage) = capacity.trim().parse() else {
                continue;
            };
            let status = fs::read_to_string(path.join("status"))
                .map(|s| Status::parse(&s))
                .unwrap_or(Status::Unknown);

            return Some(Self { percentage, status });
        }

        None
    }
}
