//! Amber — a minimal, cozy Wayland desktop shell for niri.
//!
//! Shared design tokens and theming for every component (panel, launcher,
//! notifications, lock screen).

pub mod assets;
pub mod battery;
pub mod bluetooth;
pub mod config;
pub mod media;
pub mod niri;
pub mod theme;
pub mod tray;
pub mod wifi;

pub use theme::{FUJI, Fonts, Metrics, Palette, Theme};
