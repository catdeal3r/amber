//! Amber — a minimal, cozy Wayland desktop shell for niri.
//!
//! Shared design tokens and theming for every component (panel, launcher,
//! notifications, lock screen).

pub mod assets;
pub mod battery;
pub mod bluetooth;
pub mod config;
pub mod theme;
pub mod wifi;

pub use theme::{Fonts, Metrics, Palette, Theme, FUJI};