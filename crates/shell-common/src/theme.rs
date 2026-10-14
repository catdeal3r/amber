//! The fuji palette: a warm brown accent over near-black, warm-gray neutrals.
//!
//! Ported from garado/cozy's `mountain/fuji` colorscheme so the ramp values
//! match the reference faithfully.

use iced_core::Color;

/// Semantic tokens. Named after role, not raw ramp slot, so swapping
/// the accent (or going wallust-driven later) only touches one place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// deepest neutral — bar, overlays, lock background
    pub bg: Color,
    /// cards, list rows, notification background
    pub surface: Color,
    /// hover fill
    pub hover: Color,
    /// search field, dimmed "empty" text, progress track
    pub field: Color,
    /// secondary / muted text
    pub muted: Color,
    /// primary text, "occupied" workspace, active clock
    pub text: Color,
    /// accent for focused/active text, cursors, focus rings
    pub accent: Color,
    /// accent fill for selected rows / pressed states
    pub accent_fill: Color,
    /// status red — battery low, critical notifications
    pub red: Color,
    /// status green
    pub green: Color,
    /// status yellow — attention
    pub yellow: Color,
}

/// Typeface pairing. Both are resolved through the system font stack, so
/// changing them (or the `amber` config file) restyles the whole shell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fonts {
    pub text: iced_core::Font,
    pub icon: iced_core::Font,
}

/// Spacing and sizing shared across components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// bar height in logical pixels
    pub bar_height: f32,
    /// window gaps feel (niri gaps), used as baseline spacing
    pub gap: f32,
    /// horizontal gap between bar clusters
    pub bar_cluster_gap: f32,
    /// overlay/launcher width
    pub overlay_width: f32,
}

impl Palette {
    pub const fn neutral900() -> Self {
        Self {
            bg: Color::from_rgb(15.0 / 255.0, 15.0 / 255.0, 15.0 / 255.0),
            surface: Color::from_rgb(19.0 / 255.0, 19.0 / 255.0, 19.0 / 255.0),
            hover: Color::from_rgb(24.0 / 255.0, 24.0 / 255.0, 24.0 / 255.0),
            field: Color::from_rgb(29.0 / 255.0, 29.0 / 255.0, 29.0 / 255.0),
            muted: Color::from_rgb(81.0 / 255.0, 81.0 / 255.0, 81.0 / 255.0),
            text: Color::from_rgb(222.0 / 255.0, 222.0 / 255.0, 222.0 / 255.0),
            accent: Color::from_rgb(146.0 / 255.0, 130.0 / 255.0, 107.0 / 255.0),
            accent_fill: Color::from_rgb(85.0 / 255.0, 72.0 / 255.0, 54.0 / 255.0),
            red: Color::from_rgb(196.0 / 255.0, 158.0 / 255.0, 160.0 / 255.0),
            green: Color::from_rgb(137.0 / 255.0, 171.0 / 255.0, 138.0 / 255.0),
            yellow: Color::from_rgb(196.0 / 255.0, 193.0 / 255.0, 158.0 / 255.0),
        }
    }
}

/// Everything one component needs to draw itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub palette: Palette,
    pub fonts: Fonts,
    pub metrics: Metrics,
}

/// The pinned default: cozy fuji warm-brown on near-black.
pub const FUJI: Theme = Theme {
    palette: Palette::neutral900(),
    fonts: Fonts {
        text: iced_core::Font::with_name("monospace"),
        icon: iced_core::Font::with_name("Material Symbols Sharp"),
    },
    metrics: Metrics {
        bar_height: 42.0,
        gap: 7.0,
        bar_cluster_gap: 15.0,
        overlay_width: 500.0,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_inverts_text_on_bg() {
        let p = FUJI.palette;
        assert!(luma(p.text) > luma(p.bg));
    }

    fn luma(c: Color) -> f32 {
        0.299 * c.r + 0.587 * c.g + 0.114 * c.b
    }
}
