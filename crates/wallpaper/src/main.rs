//! `amber-wallpaper` — a background layer-shell surface that paints the
//! configured image across an output.
//!
//! The image is decoded up front and handed to iced as raw RGBA (the same
//! approach as the bundled assets), so any format the `image` crate can read
//! works without depending on iced's heavy codec features. With no `[wallpaper]`
//! `path` configured, it just fills the output with the theme `bg`.

use std::time::Duration;

use iced::widget::{Space, container, image as image_widget, text};
use iced::{ContentFit, Element, Length, Subscription, Task as Command};
use iced_layershell::application;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use shell_common::config::{self, WallpaperConfig};
use shell_common::theme::Theme;

/// iced uploads images through a texture atlas sized at most to this, and
/// larger images render as garbage; see `iced_wgpu`'s `image::atlas::MAX_SIZE`.
const ATLAS_MAX_SIDE: u32 = 2048;

/// Images at least this many bytes of RGBA are uploaded by iced on a background
/// thread, which then asks for a redraw once the upload finishes. That redraw
/// request can be lost for a static layer surface, leaving the theme background
/// (black) on screen until something else forces a frame. To avoid that we
/// request our own redraws for a short while after startup.
const SETTLE_TICKS: u32 = 40;

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    /// Re-read the wallpaper from disk.
    #[allow(dead_code)]
    Reload,
    /// Internal: keep forcing frames until the image upload has landed.
    Tick,
}

struct Background {
    theme: Theme,
    image: Option<image_widget::Handle>,
    fit: ContentFit,
    /// Set when the configured wallpaper couldn't be loaded; shown on screen
    /// and printed to stderr.
    warning: Option<String>,
    /// Redraws still owed while the asynchronous image upload settles.
    settling: u32,
}

impl Background {
    fn new(theme: Theme, cfg: &WallpaperConfig) -> Self {
        let mut background = Self {
            theme,
            image: None,
            fit: cfg.fit,
            warning: None,
            settling: SETTLE_TICKS,
        };
        background.apply(cfg);
        background
    }

    /// Resolve the configured image (or the solid fallback), reporting any
    /// problem both to the user and to stderr.
    fn apply(&mut self, cfg: &WallpaperConfig) {
        self.fit = cfg.fit;
        self.warning = None;

        let Some(path) = cfg.path.as_deref() else {
            self.image = None;
            return;
        };

        match load(path) {
            Ok(handle) => self.image = Some(handle),
            Err(message) => {
                eprintln!("warning: {message}");
                self.warning = Some(message);
                self.image = None;
            }
        }
    }
}

/// Decode an image file to RGBA, sniffing the format from the file's content
/// (so the extension doesn't have to match). Errors are human-readable.
fn load(path: &std::path::Path) -> Result<image_widget::Handle, String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(format!("wallpaper is not a file: {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("wallpaper does not exist: {}", path.display()));
        }
        Err(err) => return Err(format!("cannot access wallpaper {}: {err}", path.display())),
    }

    let mut image = image::ImageReader::open(path)
        .map_err(|err| format!("cannot open wallpaper {}: {err}", path.display()))?
        .with_guessed_format()
        .map_err(|err| format!("cannot read wallpaper {}: {err}", path.display()))?
        .decode()
        .map_err(|err| format!("wallpaper is corrupted: {}: {err}", path.display()))?;

    // iced uploads images through a texture atlas that is capped at
    // `ATLAS_MAX_SIDE`; anything larger is split into fragments that don't
    // render correctly, so downscale first. The result is still far more
    // pixels than any output shows.
    if image.width() > ATLAS_MAX_SIDE || image.height() > ATLAS_MAX_SIDE {
        image = image.resize(
            ATLAS_MAX_SIDE,
            ATLAS_MAX_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
    }

    let decoded = image.into_rgba8();
    let (width, height) = decoded.dimensions();
    Ok(image_widget::Handle::from_rgba(
        width,
        height,
        decoded.into_raw(),
    ))
}

fn main() -> Result<(), iced_layershell::Error> {
    let theme = config::theme();
    let wallpaper = config::wallpaper();

    // A trailing output name (e.g. from niri) pins the surface to that output;
    // otherwise it follows the active one.
    let start_mode = std::env::args()
        .nth(1)
        .map(StartMode::TargetScreen)
        .unwrap_or(StartMode::Active);

    application(
        move || Background::new(theme, &wallpaper),
        "amber-wallpaper",
        update,
        view,
    )
    .settings(Settings {
        default_font: theme.fonts.text,
        layer_settings: LayerShellSettings {
            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            layer: Layer::Background,
            exclusive_zone: -1,
            size: None,
            margin: (0, 0, 0, 0),
            keyboard_interactivity: KeyboardInteractivity::None,
            start_mode,
            events_transparent: true,
        },
        ..Settings::default()
    })
    .antialiasing(true)
    .subscription(subscription)
    .run()
}

/// While `settling` is non-zero, tick frequently so frames keep being produced;
/// this picks up the image once iced's background upload has finished, even if
/// its own redraw request was dropped.
fn subscription(state: &Background) -> Subscription<Message> {
    if state.settling == 0 {
        Subscription::none()
    } else {
        iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick)
    }
}

fn update(state: &mut Background, message: Message) -> Command<Message> {
    match message {
        Message::Reload => state.apply(&config::reload().wallpaper),
        Message::Tick => state.settling = state.settling.saturating_sub(1),
        _ => {}
    }

    Command::none()
}

fn view(state: &Background) -> Element<'_, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;

    let layer: Element<Message> = if let Some(warning) = &state.warning {
        // No wallpaper to show: say why, in the middle of the screen.
        container(
            text(warning.clone())
                .font(fonts.text)
                .size(16.0)
                .color(palette.red)
                .width(Length::Fill)
                .align_x(iced::alignment::Horizontal::Center),
        )
        .padding(24.0)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    } else if let Some(handle) = &state.image {
        image_widget(handle.clone())
            .content_fit(state.fit)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        Space::new().width(Length::Fill).height(Length::Fill).into()
    };

    container(layer)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(palette.bg.into()),
            ..container::Style::default()
        })
        .into()
}
