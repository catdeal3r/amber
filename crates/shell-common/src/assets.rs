//! Bundled artwork.
//!
//! The PNGs are decoded to raw RGBA up front and handed to iced as
//! `image::Handle::from_rgba`, so the renderer never has to decode encoded
//! bytes asynchronously (which can leave a first frame blank). Regenerate the
//! source PNGs with:
//!
//! ```sh
//! curl -sL -o snowflake.svg \
//!   https://raw.githubusercontent.com/NixOS/nixos-artwork/master/logo/nix-snowflake-colours.svg
//! magick snowflake.svg -background none -trim +repage \
//!   -fuzz 0% -transparent white -fill white -colorize 100 snow.png
//! magick snow.png -resize 18x18 -bordercolor none -border 3 -type TrueColorAlpha PNG32:nix-bar.png
//! magick snow.png -resize 48x48 -type TrueColorAlpha PNG32:nix-lock.png
//! ```

use std::sync::LazyLock;

/// Decoded RGBA8 pixels plus dimensions.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

fn decode(png: &'static [u8]) -> Rgba {
    let image = image::load_from_memory(png)
        .expect("bundled PNG is valid")
        .into_rgba8();
    let (width, height) = image.dimensions();

    Rgba {
        width,
        height,
        pixels: image.into_raw(),
    }
}

/// The Nix snowflake, ~18px with a transparent margin, for the bar.
pub fn nix_bar() -> &'static Rgba {
    static BAR: LazyLock<Rgba> = LazyLock::new(|| decode(include_bytes!("../assets/nix-bar.png")));
    &BAR
}

/// The Nix snowflake at 48px, for the lock screen.
pub fn nix_lock() -> &'static Rgba {
    static LOCK: LazyLock<Rgba> = LazyLock::new(|| decode(include_bytes!("../assets/nix-lock.png")));
    &LOCK
}

/// Bundled CommitMono Regular (SIL Open Font License 1.1).
///
/// iced's font database only sees fonts surfaced by the host fontconfig
/// configuration. On some setups (e.g. NixOS with a stale fontconfig cache)
/// that list omits `CommitMono`, so the face is shipped here and registered
/// with the renderer at startup via `iced_layershell`'s `.font()`.
pub const COMMIT_MONO: &[u8] = include_bytes!("../assets/CommitMono-400-Regular.ttf");
