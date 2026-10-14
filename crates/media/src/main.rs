//! `amber-media` — the now-playing widget.
//!
//! A small card that hangs just below the centred workspace indicator in the
//! bar and shows whatever is currently playing. Collapsed it is a single-line
//! `"{artist} - {title}"` tag; hovering it grows the card into a fuller player
//! with the (blurred and darkened) cover art as its background and transport
//! controls. It paints nothing at all when no track is loaded, so it only
//! appears while there is media to show.
//!
//! The surface is a fixed-size, top-anchored overlay whose *input region* is
//! carved out per state: nothing when hidden, just the tag while collapsed, and
//! the whole card while expanded. This keeps clicks outside the card passing
//! through to the desktop and makes the card interactive only where it is drawn.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::animation::Easing;
use iced::widget::text::Wrapping;
use iced::widget::{
    Space, Stack, button, column, container, image as image_widget, mouse_area, row, text,
};
use iced::{Alignment, Border, Color, ContentFit, Element, Length, Subscription, Task};
use iced_layershell::actions::ActionCallback;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};
use iced_layershell::{application, to_layer_message};
use shell_common::media::{Command, LoopStatus, Media, MediaState, Playback};
use shell_common::theme::Theme;
use shell_common::{assets, config};

/// How often the snapshot is re-read from the MPRIS watcher.
const POLL: Duration = Duration::from_millis(500);

/// The animation frame interval while the card is growing or shrinking.
const FRAME: Duration = Duration::from_millis(16);

/// How long the tag ⇄ player transition takes.
const EXPAND_DURATION: Duration = Duration::from_millis(220);

/// Gap between the bottom of the bar and the top of the card.
///
/// niri reserves the bar's exclusive zone and places this overlay surface at
/// the top of the remaining area, so the surface origin already sits below the
/// bar; this is the only extra offset needed.
const GAP: i32 = 4;

/// The window is always laid out at the expanded player's size; the collapsed
/// tag is centred inside it and the input region hides the rest.
const PLAYER_W: u32 = 420;
const PLAYER_H: u32 = 150;

/// Height of the collapsed tag, and therefore of its input region.
const TAG_H: i32 = 34;

/// Font size of the tag label.
const LABEL_SIZE: f32 = 12.0;

/// Advance width of the monospace label, as a fraction of its font size. The
/// label is monospace, so every cell has the same width and how many fit can be
/// computed exactly.
const MONO_ADVANCE: f32 = 0.6;

/// Font size of the leading tag icon and the gap after it.
const ICON_SIZE: f32 = 14.0;
const ICON_GAP: f32 = 6.0;

/// Padding inside the collapsed tag.
const PADDING_X: f32 = 10.0;

/// Border width around the card.
const BORDER: f32 = 1.0;

/// Padding inside the expanded player, and the font sizes of its text.
const PLAYER_PADDING: f32 = 14.0;
const TITLE_SIZE: f32 = 14.0;
const ARTIST_SIZE: f32 = 12.0;

/// Largest cover-art side we upload; anything bigger is downscaled first.
const ART_MAX: u32 = 512;

/// Gaussian blur sigma applied to the cover art, in the (downscaled) art's
/// pixels. The art is drawn behind the text, so this softens it into a wash.
const ART_BLUR: f32 = 5.0;

/// Opacity of the black scrim laid over the cover art, to keep the text legible.
const SCRIM_ALPHA: f32 = 0.55;

/// Redraws owed while the cover-art texture upload settles (see `subscription`).
const SETTLE_TICKS: u32 = 40;

/// Material Symbols codepoints.
const MUSIC_NOTE: u32 = 0xe405;
const PLAY: u32 = 0xe037;
const PAUSE: u32 = 0xe034;
const SKIP_PREVIOUS: u32 = 0xe045;
const SKIP_NEXT: u32 = 0xe044;
const SHUFFLE: u32 = 0xe043;
const REPEAT: u32 = 0xe040;
const REPEAT_ONE: u32 = 0xe041;

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    /// Advance the clock (and the grow/shrink animation) to this instant, and
    /// re-read the media snapshot.
    Tick(Instant),
    /// The pointer entered the card.
    HoverEnter,
    /// The pointer left the card.
    HoverExit,
    /// Toggle between playing and paused.
    PlayPause,
    /// Skip to the next track.
    Next,
    /// Go back to the previous track.
    Previous,
    /// Turn shuffle on or off.
    ToggleShuffle,
    /// Advance the loop mode.
    CycleLoop,
}

/// The interactive part of the surface, in window-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    /// Nothing is interactive (no media, or hidden).
    Empty,
    /// A single rectangle.
    Rect(i32, i32, i32, i32),
}

struct MediaApp {
    theme: Theme,
    media: Media,
    state: MediaState,
    /// The *target* of the grow/shrink transition: `true` once hovered.
    expanded: bool,
    /// The actual `expanded` ⇄ `collapsed` transition, projected per frame.
    progress: iced::Animation<bool>,
    /// The instant of the most recent frame, used to project the animation.
    now: Instant,
    /// The cover-art URL currently decoded, if any.
    art_url: Option<String>,
    /// The decoded cover art, if the current track has a local one.
    art: Option<image_widget::Handle>,
    /// Redraws still owed while the cover-art upload settles.
    settling: u32,
    /// Whether the focused window is fullscreen, in which case nothing is shown.
    fullscreen: bool,
    /// The input region last applied to the surface.
    region: Region,
}

impl MediaApp {
    fn new(theme: Theme) -> Self {
        Self {
            theme,
            media: Media::spawn(),
            state: MediaState::default(),
            expanded: false,
            progress: iced::Animation::new(false)
                .duration(EXPAND_DURATION)
                .easing(Easing::EaseOutCubic),
            now: Instant::now(),
            art_url: None,
            art: None,
            settling: 0,
            fullscreen: false,
            region: Region::Empty,
        }
    }

    /// Whether the card is drawn at all: there is media to show and the focused
    /// window is not fullscreen.
    fn shown(&self) -> bool {
        self.state.visible() && !self.fullscreen
    }
}

fn update(state: &mut MediaApp, message: Message) -> Task<Message> {
    match message {
        Message::Tick(now) => {
            state.now = now;
            state.state = state.media.state();
            state.settling = state.settling.saturating_sub(1);
            // Only ask niri while there is something to hide; a fullscreen
            // check costs two `niri msg` invocations.
            state.fullscreen = state.state.visible() && shell_common::niri::is_fullscreen();
            // Collapse while hidden so the card doesn't reappear expanded.
            if !state.shown() && state.expanded {
                state.expanded = false;
                state.progress.go_mut(false, state.now);
            }
            update_art(state);
        }
        Message::HoverEnter => {
            state.expanded = true;
            state.now = Instant::now();
            state.progress.go_mut(true, state.now);
        }
        Message::HoverExit => {
            state.expanded = false;
            state.now = Instant::now();
            state.progress.go_mut(false, state.now);
        }
        Message::PlayPause => state.media.command(Command::PlayPause),
        Message::Next => state.media.command(Command::Next),
        Message::Previous => state.media.command(Command::Previous),
        Message::ToggleShuffle => {
            if let Some(on) = state.state.shuffle {
                state.media.command(Command::SetShuffle(!on));
            }
        }
        Message::CycleLoop => state.media.command(Command::CycleLoop),
        // Layer-shell actions injected by `#[to_layer_message]`.
        _ => return Task::none(),
    }

    let region = desired_region(state);
    if region == state.region {
        Task::none()
    } else {
        state.region = region;
        Task::done(Message::SetInputRegion(region_callback(region)))
    }
}

/// Drive frames while the card is growing/shrinking; tick faster while the
/// cover-art upload settles; otherwise just poll the media snapshot.
fn subscription(state: &MediaApp) -> Subscription<Message> {
    let interval = if state.progress.is_animating(state.now) {
        FRAME
    } else if state.settling > 0 {
        Duration::from_millis(250)
    } else {
        POLL
    };
    iced::time::every(interval).map(Message::Tick)
}

fn view(state: &MediaApp) -> Element<'_, Message> {
    if !state.shown() {
        return Space::new().width(Length::Fill).height(Length::Fill).into();
    }

    // `0.0` fully collapsed (the tag), `1.0` fully expanded (the player).
    let t = state
        .progress
        .interpolate(0.0, 1.0, state.now)
        .clamp(0.0, 1.0);

    mouse_area(
        container(animated_card(state, t))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Start),
    )
    .on_enter(Message::HoverEnter)
    .on_exit(Message::HoverExit)
    .into()
}

/// The card, its size and contents interpolated between the tag and the player.
///
/// `t` runs from `0.0` (the small tag) to `1.0` (the full player). The card
/// grows, the cover art fades in behind a wash the colour of the tag, and the
/// tag and player contents cross-fade through the middle of the transition.
fn animated_card<'a>(state: &'a MediaApp, t: f32) -> Element<'a, Message> {
    let palette = state.theme.palette;

    let tag_w = tag_width(&collapsed_label(state)).min(PLAYER_W) as f32;
    let width = lerp(tag_w, PLAYER_W as f32, t);
    let height = lerp(TAG_H as f32, PLAYER_H as f32, t);

    let background: Element<'a, Message> = match &state.art {
        Some(handle) => image_widget(handle.clone())
            .content_fit(ContentFit::Cover)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        None => fill_color(Color::BLACK),
    };

    // Darkens the art so the text above it stays legible.
    let scrim = fill_color(Color::from_rgba(0.0, 0.0, 0.0, SCRIM_ALPHA));
    // Washes the whole card back to the tag colour while collapsed, so the art
    // appears to emerge as the card grows.
    let wash = fill_color(palette.bg.scale_alpha(1.0 - t));

    // Cross-fade the contents through the middle of the transition.
    let tag_alpha = (1.0 - 2.0 * t).clamp(0.0, 1.0);
    let player_alpha = (2.0 * t - 1.0).clamp(0.0, 1.0);

    let border = container(Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_theme: &iced::Theme| container::Style {
            border: Border::default().color(palette.muted).width(BORDER),
            ..container::Style::default()
        });

    Stack::new()
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .push(background)
        .push(scrim)
        .push(wash)
        .push(tag_body(state, tag_alpha))
        .push(player_body(state, player_alpha))
        .push(border)
        .into()
}

/// The collapsed tag's contents: the music-note icon and the single-line label,
/// centred in the card.
fn tag_body<'a>(state: &'a MediaApp, alpha: f32) -> Element<'a, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;

    let icon = text(glyph_char(MUSIC_NOTE))
        .font(fonts.icon)
        .size(ICON_SIZE)
        .color(palette.muted.scale_alpha(alpha));

    let label = text(collapsed_label(state))
        .size(LABEL_SIZE)
        .font(fonts.text)
        .color(palette.text.scale_alpha(alpha))
        .wrapping(Wrapping::None);

    container(
        row![icon, label]
            .spacing(ICON_GAP)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .into()
}

/// The expanded player's contents: the track text over the art, with the
/// transport controls along the bottom.
fn player_body<'a>(state: &'a MediaApp, alpha: f32) -> Element<'a, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;

    let text_width = PLAYER_W as f32 - 2.0 * PLAYER_PADDING;
    let title = ellipsize(
        state.state.title.as_deref().unwrap_or_default(),
        cells_for(text_width, TITLE_SIZE),
    );
    let artist = ellipsize(
        state.state.artist.as_deref().unwrap_or_default(),
        cells_for(text_width, ARTIST_SIZE),
    );

    column![
        text(title)
            .size(TITLE_SIZE)
            .font(fonts.text)
            .color(palette.text.scale_alpha(alpha))
            .wrapping(Wrapping::None),
        text(artist)
            .size(ARTIST_SIZE)
            .font(fonts.text)
            .color(palette.muted.scale_alpha(alpha))
            .wrapping(Wrapping::None),
        Space::new().width(Length::Fill).height(Length::Fill),
        controls(state, alpha),
    ]
    .spacing(2.0)
    .padding(PLAYER_PADDING)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The transport controls, centred along the bottom of the player.
fn controls<'a>(state: &'a MediaApp, alpha: f32) -> Element<'a, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;

    let mut items: Vec<Element<'a, Message>> = Vec::new();

    if let Some(shuffle) = state.state.shuffle {
        let color = if shuffle {
            palette.accent
        } else {
            palette.muted
        };
        items.push(control(
            fonts.icon,
            SHUFFLE,
            20.0,
            color.scale_alpha(alpha),
            Some(Message::ToggleShuffle),
        ));
    }

    let previous = state.state.can_go_previous.then_some(Message::Previous);
    items.push(control(
        fonts.icon,
        SKIP_PREVIOUS,
        20.0,
        enabled_color(palette, previous.is_some()).scale_alpha(alpha),
        previous,
    ));

    let playing = state.state.playback == Playback::Playing;
    items.push(control(
        fonts.icon,
        if playing { PAUSE } else { PLAY },
        22.0,
        palette.text.scale_alpha(alpha),
        state.state.has_player.then_some(Message::PlayPause),
    ));

    let next = state.state.can_go_next.then_some(Message::Next);
    items.push(control(
        fonts.icon,
        SKIP_NEXT,
        20.0,
        enabled_color(palette, next.is_some()).scale_alpha(alpha),
        next,
    ));

    if let Some(loop_status) = state.state.loop_status {
        let glyph = match loop_status {
            LoopStatus::Track => REPEAT_ONE,
            _ => REPEAT,
        };
        let color = match loop_status {
            LoopStatus::None => palette.muted,
            _ => palette.accent,
        };
        items.push(control(
            fonts.icon,
            glyph,
            20.0,
            color.scale_alpha(alpha),
            Some(Message::CycleLoop),
        ));
    }

    container(row(items).spacing(14.0).align_y(Alignment::Center))
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .into()
}

/// One transport glyph, or a dimmed, inert one when disabled.
fn control<'a>(
    font: iced::Font,
    glyph: u32,
    size: f32,
    color: Color,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let icon = text(glyph_char(glyph)).font(font).size(size).color(color);
    let button = button(icon).padding([2.0, 6.0]).style(transparent_button);

    match on_press {
        Some(message) => button.on_press(message).into(),
        None => button.into(),
    }
}

fn transparent_button(_theme: &iced::Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: None,
        ..button::Style::default()
    }
}

fn enabled_color(palette: shell_common::theme::Palette, enabled: bool) -> Color {
    if enabled { palette.text } else { palette.muted }
}

/// A fully-sized container painted a single colour, for layering in a `Stack`.
fn fill_color<'a>(color: Color) -> Element<'a, Message> {
    container(Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(color.into()),
            ..container::Style::default()
        })
        .into()
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// The collapsed label, shortened to fit the card.
fn collapsed_label(state: &MediaApp) -> String {
    ellipsize(
        &label(&state.state),
        cells_for(tag_text_width(), LABEL_SIZE),
    )
}

/// The width the tag label may occupy, inside the padding, border, icon & gap.
fn tag_text_width() -> f32 {
    PLAYER_W as f32 - 2.0 * PADDING_X - 2.0 * BORDER - ICON_SIZE - ICON_GAP
}

/// How many monospace cells of `size` fit in `width`.
fn cells_for(width: f32, size: f32) -> usize {
    (width / (size * MONO_ADVANCE)).floor().max(0.0) as usize
}

/// The tag's pixel width for `label`: the chrome plus one advance per cell.
fn tag_width(label: &str) -> u32 {
    let cells = label.chars().count() as f32;
    let width =
        2.0 * BORDER + 2.0 * PADDING_X + ICON_SIZE + ICON_GAP + cells * LABEL_SIZE * MONO_ADVANCE;
    width.ceil() as u32
}

/// The input region the surface should expose for the current state.
fn desired_region(state: &MediaApp) -> Region {
    if !state.shown() {
        return Region::Empty;
    }
    if state.expanded {
        return Region::Rect(0, 0, PLAYER_W as i32, PLAYER_H as i32);
    }
    let width = tag_width(&collapsed_label(state)).min(PLAYER_W) as i32;
    let x = (PLAYER_W as i32 - width) / 2;
    Region::Rect(x, 0, width, TAG_H)
}

/// Build the callback that carves `region` out of the surface's input region.
///
/// The layer-shell action subtracts the whole surface before calling this, so
/// with no rectangle added the surface is fully click-through.
fn region_callback(region: Region) -> ActionCallback {
    ActionCallback::new(move |wl_region| {
        if let Region::Rect(x, y, w, h) = region {
            wl_region.add(x, y, w, h);
        }
    })
}

/// Reload the cover art when the track changes.
fn update_art(state: &mut MediaApp) {
    if state.art_url == state.state.art_url {
        return;
    }
    state.art_url = state.state.art_url.clone();
    state.art = state.art_url.as_deref().and_then(load_art);
    if state.art.is_some() {
        state.settling = SETTLE_TICKS;
    }
}

/// Decode a local cover-art URL into a blurred, iced-ready image handle.
///
/// Only `file://` URLs are read; remote ones are ignored and fall back to the
/// solid background.
fn load_art(url: &str) -> Option<image_widget::Handle> {
    let path = art_path(url)?;
    let mut decoded = image::ImageReader::open(&path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;

    if decoded.width() > ART_MAX || decoded.height() > ART_MAX {
        decoded = decoded.resize(ART_MAX, ART_MAX, image::imageops::FilterType::Lanczos3);
    }

    let blurred = image::imageops::fast_blur(&decoded.to_rgba8(), ART_BLUR);
    let (width, height) = blurred.dimensions();
    Some(image_widget::Handle::from_rgba(
        width,
        height,
        blurred.into_raw(),
    ))
}

/// Turn a `file://` URL into a filesystem path, undoing percent-encoding.
fn art_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode(rest)))
}

/// Decode `%XX` escapes in a URL path.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
        {
            out.push(high * 16 + low);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The label shown in the tag: `"{artist} - {title}"`, or just the title when
/// the player reports no artist.
fn label(state: &MediaState) -> String {
    let title = state.title.as_deref().unwrap_or_default();
    match state.artist.as_deref() {
        Some(artist) if !artist.is_empty() => format!("{artist} - {title}"),
        _ => title.to_string(),
    }
}

/// Shorten `label` to at most `max` monospace cells, ending in `...` when it
/// does not fit.
fn ellipsize(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = label.chars().take(keep).collect();
    out.push_str("...");
    out
}

/// A codepoint from Material Symbols, rendered through the icon font.
fn glyph_char(codepoint: u32) -> String {
    char::from_u32(codepoint).unwrap_or_default().to_string()
}

fn main() -> Result<(), iced_layershell::Error> {
    let theme = config::theme();

    application(move || MediaApp::new(theme), "amber-media", update, view)
        .settings(iced_layershell::Settings {
            default_font: theme.fonts.text,
            default_text_size: iced::Pixels(12.0),
            layer_settings: LayerShellSettings {
                // Anchored to the top only, so the compositor centres the card
                // under the workspace indicator. The surface is laid out at the
                // expanded size and the input region hides what isn't drawn.
                anchor: Anchor::Top,
                layer: Layer::Overlay,
                exclusive_zone: 0,
                size: Some((PLAYER_W, PLAYER_H)),
                margin: (GAP, 0, 0, 0),
                keyboard_interactivity: KeyboardInteractivity::None,
                start_mode: StartMode::Active,
                // Start fully click-through; the first tick carves the tag out.
                events_transparent: true,
            },
            ..iced_layershell::Settings::default()
        })
        .antialiasing(true)
        .font(assets::COMMIT_MONO)
        .subscription(subscription)
        .style(
            |_state: &MediaApp, _theme: &iced::Theme| iced::theme::Style {
                background_color: Color::TRANSPARENT,
                text_color: Color::WHITE,
            },
        )
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_joins_artist_and_title() {
        let state = MediaState {
            title: Some("Song".into()),
            artist: Some("Band".into()),
            ..MediaState::default()
        };
        assert_eq!(label(&state), "Band - Song");
    }

    #[test]
    fn label_falls_back_to_the_title() {
        let state = MediaState {
            title: Some("Song".into()),
            ..MediaState::default()
        };
        assert_eq!(label(&state), "Song");
    }

    #[test]
    fn ellipsize_keeps_short_labels() {
        assert_eq!(ellipsize("short", 10), "short");
    }

    #[test]
    fn ellipsize_marks_the_cut() {
        let out = ellipsize("abcdefghij", 6);
        assert_eq!(out, "abc...");
        assert_eq!(out.chars().count(), 6);
    }

    #[test]
    fn tag_width_grows_with_the_label() {
        let empty = tag_width("");
        assert_eq!(empty, 42);
        assert!(tag_width("abcdef") > empty);
    }

    #[test]
    fn cells_for_accounts_for_the_advance() {
        // The tag reserves room for the chrome and fits 52 cells of the label.
        assert_eq!(cells_for(tag_text_width(), LABEL_SIZE), 52);
    }

    #[test]
    fn lerp_interpolates_between_the_tag_and_player() {
        assert_eq!(lerp(34.0, 150.0, 0.0), 34.0);
        assert_eq!(lerp(42.0, 420.0, 1.0), 420.0);
        assert_eq!(lerp(0.0, 100.0, 0.5), 50.0);
    }

    #[test]
    fn art_path_reads_local_files() {
        assert_eq!(
            art_path("file:///tmp/cover%20art.jpg"),
            Some(PathBuf::from("/tmp/cover art.jpg"))
        );
        assert_eq!(art_path("https://example.com/cover.jpg"), None);
    }
}
