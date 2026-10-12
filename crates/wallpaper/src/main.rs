//! `amber-wallpaper` — a background layer-shell surface that paints the
//! configured image across an output.
//!
//! The image is decoded up front and handed to iced as raw RGBA (the same
//! approach as the bundled assets), so any format the `image` crate can read
//! works without depending on iced's heavy codec features. With no `[wallpaper]`
//! `path` configured, it just fills the output with the theme `bg`.

use iced::widget::{Space, container, image as image_widget};
use iced::{ContentFit, Element, Length, Task as Command};
use iced_layershell::application;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode, Settings};
use iced_layershell::to_layer_message;
use shell_common::config::{self, WallpaperConfig};
use shell_common::theme::Theme;

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    /// Re-read the wallpaper from disk.
    #[allow(dead_code)]
    Reload,
}

struct Background {
    theme: Theme,
    image: Option<image_widget::Handle>,
    fit: ContentFit,
}

impl Background {
    fn new(theme: Theme, cfg: &WallpaperConfig) -> Self {
        Self {
            theme,
            image: cfg.path.as_deref().and_then(load),
            fit: cfg.fit,
        }
    }
}

/// Decode an image file to RGBA. Returns `None` if it can't be read, in which
/// case the solid background shows through.
fn load(path: &std::path::Path) -> Option<image_widget::Handle> {
    let decoded = image::open(path).ok()?.into_rgba8();
    let (width, height) = decoded.dimensions();
    Some(image_widget::Handle::from_rgba(
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
    .run()
}

fn update(state: &mut Background, message: Message) -> Command<Message> {
    match message {
        Message::Reload => {
            let cfg = config::reload().wallpaper;
            state.image = cfg.path.as_deref().and_then(load);
            state.fit = cfg.fit;
            Command::none()
        }
        _ => Command::none(),
    }
}

fn view(state: &Background) -> Element<'_, Message> {
    let layer: Element<Message> = match &state.image {
        Some(handle) => image_widget(handle.clone())
            .content_fit(state.fit)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        None => Space::new().width(Length::Fill).height(Length::Fill).into(),
    };

    let bg = state.theme.palette.bg;

    container(layer)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(bg.into()),
            ..container::Style::default()
        })
        .into()
}
