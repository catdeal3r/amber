//! Optional `amber` configuration overrides.
//!
//! Looks for `$XDG_CONFIG_HOME/amber/config.toml` (default
//! `~/.config/amber/config.toml`). Anything omitted falls back to the
//! pinned [`Theme`] defaults — so changing the typeface or metrics is
//! a one-file change, per the "make it easy to change" requirement.

use iced_core::{ContentFit, Font};

use crate::theme::{Fonts, Theme};

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RawConfig {
    font: Option<RawFonts>,
    #[allow(dead_code)]
    metrics: Option<serde::de::IgnoredAny>,
    wallpaper: Option<RawWallpaper>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RawFonts {
    text: Option<String>,
    icon: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RawWallpaper {
    /// Absolute path to the image. `None` keeps the solid `bg`.
    path: Option<String>,
    /// `cover` (default), `contain`, `fill` or `none`.
    fit: Option<String>,
}

/// Wallpaper settings from `[wallpaper]`.
#[derive(Debug, Clone)]
pub struct WallpaperConfig {
    /// Image to paint; `None` renders the solid `bg` colour instead.
    pub path: Option<std::path::PathBuf>,
    /// How the image fills the output.
    pub fit: ContentFit,
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            path: None,
            fit: ContentFit::Cover,
        }
    }
}

/// Load the `[wallpaper]` section, falling back to the solid background.
pub fn wallpaper() -> WallpaperConfig {
    let Some(cfg) = read() else {
        return WallpaperConfig::default();
    };
    let raw = cfg.wallpaper.unwrap_or_default();

    WallpaperConfig {
        path: raw.path.map(std::path::PathBuf::from),
        fit: match raw.fit.as_deref() {
            Some("contain") => ContentFit::Contain,
            Some("fill") => ContentFit::Fill,
            Some("none") => ContentFit::None,
            _ => ContentFit::Cover,
        },
    }
}

/// Load fonts from the config file, falling back to `base` values.
pub fn fonts(base: Fonts) -> Fonts {
    match read() {
        Some(cfg) => Fonts {
            text: cfg
                .font
                .as_ref()
                .and_then(|f| f.text.clone())
                .map_or(base.text, |name| Font::with_name(leak(name))),
            icon: cfg
                .font
                .as_ref()
                .and_then(|f| f.icon.clone())
                .map_or(base.icon, |name| Font::with_name(leak(name))),
        },
        None => base,
    }
}

/// `Font::with_name` wants a `&'static str`; the config is read once at
/// startup, so leaking the parsed names is harmless.
fn leak(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// Build the effective [`Theme`]: pinned defaults, overridden by config.
pub fn theme() -> Theme {
    let mut theme = crate::FUJI;
    theme.fonts = fonts(theme.fonts);
    theme
}

fn read() -> Option<RawConfig> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))?;

    let path = dir.join("amber").join("config.toml");
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}