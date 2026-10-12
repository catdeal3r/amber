//! Configuration manager.
//!
//! Loads `~/.config/amber/config.toml` once (honouring `$XDG_CONFIG_HOME`)
//! and hands every component the same typed settings. Anything omitted
//! falls back to the pinned [`Theme`] defaults, so changing the typeface,
//! the wallpaper or future tokens is a one-file change.
//!
//! ```
//! let cfg = shell_common::config::get();
//! println!("{:?}", cfg.font.text);
//! ```
//!
//! Call [`reload`] to re-read the file from disk (e.g. from a "reload"
//! keybinding); [`theme`] and [`wallpaper`] are thin conveniences over
//! the current snapshot.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use iced_core::{ContentFit, Font};

use crate::theme::{Fonts, Theme};

static CONFIG: LazyLock<RwLock<Config>> = LazyLock::new(|| RwLock::new(Config::load()));

/// Fully-resolved settings for the shell.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Typefaces, as family names.
    pub font: FontConfig,
    /// Desktop background.
    pub wallpaper: WallpaperConfig,
}

/// `[font]` section.
#[derive(Debug, Clone, Default)]
pub struct FontConfig {
    /// Text/monospace family.
    pub text: Option<String>,
    /// Icon family (e.g. `Material Symbols Sharp`).
    pub icon: Option<String>,
}

/// `[wallpaper]` section.
#[derive(Debug, Clone)]
pub struct WallpaperConfig {
    /// Image to paint; `None` renders the solid `bg` colour instead.
    pub path: Option<PathBuf>,
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

impl Config {
    /// Read the config file from disk, applying defaults for anything absent.
    fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|content| Self::parse(&content))
            .unwrap_or_default()
    }

    /// Parse a TOML document into a [`Config`].
    fn parse(content: &str) -> Option<Self> {
        let raw: RawConfig = toml::from_str(content).ok()?;
        Some(Self::from_raw(raw))
    }

    fn from_raw(raw: RawConfig) -> Self {
        let font = raw.font.unwrap_or_default();
        let wallpaper = raw.wallpaper.unwrap_or_default();

        Config {
            font: FontConfig {
                text: font.text,
                icon: font.icon,
            },
            wallpaper: WallpaperConfig {
                path: wallpaper.path.map(PathBuf::from),
                fit: match wallpaper.fit.as_deref() {
                    Some("contain") => ContentFit::Contain,
                    Some("fill") => ContentFit::Fill,
                    Some("none") => ContentFit::None,
                    _ => ContentFit::Cover,
                },
            },
        }
    }
}

impl FontConfig {
    /// Resolve to concrete [`Fonts`], keeping `base` for anything unset.
    ///
    /// `Font::with_name` wants a `&'static str`; the config is read once at
    /// startup, so leaking the parsed names is harmless.
    pub fn resolve(&self, base: Fonts) -> Fonts {
        Fonts {
            text: self
                .text
                .clone()
                .map_or(base.text, |name| Font::with_name(leak(name))),
            icon: self
                .icon
                .clone()
                .map_or(base.icon, |name| Font::with_name(leak(name))),
        }
    }
}

/// The current configuration snapshot.
pub fn get() -> Config {
    CONFIG.read().expect("config lock poisoned").clone()
}

/// Re-read the config file from disk and return the fresh snapshot.
pub fn reload() -> Config {
    let fresh = Config::load();
    *CONFIG.write().expect("config lock poisoned") = fresh.clone();
    fresh
}

/// Path of the config file, whether or not it exists yet.
pub fn path() -> PathBuf {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_default();

    dir.join("amber").join("config.toml")
}

/// Build the effective [`Theme`]: pinned defaults, overridden by config.
pub fn theme() -> Theme {
    let mut theme = crate::FUJI;
    theme.fonts = get().font.resolve(theme.fonts);
    theme
}

/// Convenience for the current [`WallpaperConfig`].
pub fn wallpaper() -> WallpaperConfig {
    get().wallpaper
}

fn leak(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_font_and_wallpaper() {
        let cfg = Config::parse(
            r#"
            [font]
            text = "CommitMono"
            icon = "Material Symbols Sharp"

            [wallpaper]
            path = "/tmp/wall.png"
            fit = "contain"
            "#,
        )
        .expect("valid toml");

        assert_eq!(cfg.font.text.as_deref(), Some("CommitMono"));
        assert_eq!(cfg.font.icon.as_deref(), Some("Material Symbols Sharp"));
        assert_eq!(
            cfg.wallpaper.path.as_deref(),
            Some(Path::new("/tmp/wall.png"))
        );
        assert!(matches!(cfg.wallpaper.fit, ContentFit::Contain));
    }

    #[test]
    fn unknown_fit_falls_back_to_cover() {
        let cfg = Config::parse("[wallpaper]\npath = \"/x.png\"\nfit = \"stretch\"\n").unwrap();
        assert!(matches!(cfg.wallpaper.fit, ContentFit::Cover));
    }

    #[test]
    fn empty_config_is_all_defaults() {
        let cfg = Config::parse("").expect("empty toml is valid");

        assert!(cfg.font.text.is_none());
        assert!(cfg.font.icon.is_none());
        assert!(cfg.wallpaper.path.is_none());
        assert!(matches!(cfg.wallpaper.fit, ContentFit::Cover));
    }
}
