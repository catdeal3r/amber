//! `amber-panel` — the top bar.
//!
//! A single layer-shell surface anchored to the top edge: the Nix snowflake
//! on the left, the workspace pager in the middle, and the clock on the
//! right. Cozy "fuji" styling throughout.

mod niri;

use std::time::Duration;

use iced::widget::{button, container, image, row, text, Space, Stack};
use iced::{Alignment, Border, Color, ContentFit, Element, Length, Task};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};
use iced_layershell::{application, to_layer_message};
use shell_common::battery::{Battery, Status};
use shell_common::bluetooth::{self, Bluetooth};
use shell_common::theme::{Metrics, Theme};
use shell_common::wifi::{self, Wifi};
use shell_common::{assets, config};

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    /// Re-read the workspace list from niri.
    PollWorkspaces,
    /// Re-read the clock text.
    Clock,
    /// Re-read the battery, Wi-Fi and Bluetooth state.
    PollStatus,
    /// Switch to a workspace by index.
    FocusWorkspace(u32),
    /// Toggle the Wi-Fi radio.
    ToggleWifi,
    /// Toggle the Bluetooth radio.
    ToggleBluetooth,
}

struct Panel {
    theme: Theme,
    workspaces: Vec<niri::Workspace>,
    clock: String,
    battery: Option<Battery>,
    wifi: Option<Wifi>,
    bluetooth: Option<Bluetooth>,
    logo: image::Handle,
    config_error: Option<String>,
}

impl Panel {
    fn new(theme: Theme) -> Self {
        let nix = assets::nix_bar();
        let logo = image::Handle::from_rgba(nix.width, nix.height, nix.pixels.clone());

        Self {
            theme,
            workspaces: Vec::new(),
            clock: clock_now(),
            battery: Battery::read(),
            wifi: wifi::read(),
            bluetooth: bluetooth::read(),
            logo,
            config_error: config::startup_error(),
        }
    }
}

fn clock_now() -> String {
    chrono::Local::now().format("%I:%M %p").to_string()
}

fn update(state: &mut Panel, message: Message) -> Task<Message> {
    match message {
        Message::PollWorkspaces => {
            if let Ok(list) = niri::workspaces() {
                state.workspaces = list;
            }
        }
        Message::Clock => state.clock = clock_now(),
        Message::PollStatus => {
            state.battery = Battery::read();
            state.wifi = wifi::read();
            state.bluetooth = bluetooth::read();
        }
        Message::FocusWorkspace(idx) => niri::focus_workspace(idx),
        Message::ToggleWifi => {
            // Flip the radio optimistically so the glyph reacts immediately; the
            // next poll fills in connectivity once NetworkManager has applied it.
            if let Some(wifi) = &mut state.wifi {
                let next = !wifi.enabled;
                wifi::set_enabled(next);
                wifi.enabled = next;
                if !next {
                    wifi.ssid = None;
                    wifi.signal = None;
                }
            }
        }
        Message::ToggleBluetooth => {
            if let Some(bluetooth) = &mut state.bluetooth {
                let next = !bluetooth.powered;
                bluetooth::set_powered(next);
                bluetooth.powered = next;
                if !next {
                    bluetooth.connected = 0;
                }
            }
        }
        // Layer-shell actions injected by `#[to_layer_message]`.
        _ => {}
    }

    Task::none()
}

fn subscription(_state: &Panel) -> iced::Subscription<Message> {
    iced::Subscription::batch([
        iced::time::every(Duration::from_millis(400)).map(|_| Message::PollWorkspaces),
        iced::time::every(Duration::from_secs(1)).map(|_| Message::Clock),
        iced::time::every(Duration::from_secs(5)).map(|_| Message::PollStatus),
    ])
}

fn view(state: &Panel) -> Element<'_, Message> {
    let palette = state.theme.palette;
    let metrics = state.theme.metrics;
    let fonts = state.theme.fonts;

    let logo = image(state.logo.clone())
        .width(Length::Fixed(metrics.bar_height * 0.55))
        .height(Length::Fixed(metrics.bar_height * 0.55))
        .content_fit(ContentFit::Contain);

    let taglist = row(
        state
            .workspaces
            .iter()
            .map(|workspace| tag(state, workspace))
            .collect::<Vec<_>>(),
    )
    .spacing(metrics.gap)
    .align_y(Alignment::Center);

    // If the config file failed to parse at startup, say so where the clock
    // would be (the full detail is printed to stderr).
    let clock = match &state.config_error {
        Some(_) => text("config error")
            .size(13.0)
            .font(fonts.text)
            .color(palette.red),
        None => text(state.clock.clone())
            .size(13.0)
            .font(fonts.text)
            .color(palette.text),
    };

    let mut right_items: Vec<Element<'_, Message>> = Vec::new();
    if state.wifi.is_some() {
        right_items.push(wifi_indicator(state));
    }
    if state.bluetooth.is_some() {
        right_items.push(bluetooth_indicator(state));
    }
    if state.battery.is_some() {
        right_items.push(battery_indicator(state));
    }
    right_items.push(clock.into());

    let right = row(right_items)
        .spacing(metrics.gap + 3.0)
        .align_y(Alignment::Center);

    // Left and right clusters on the base layer, with a greedy spacer between
    // them. The workspace pager is stacked on top and centred on the full width
    // of the bar, so it stays put regardless of how wide the side clusters are.
    // them. The workspace pager is stacked on top and centred on the full width
    // of the bar, so it stays put regardless of how wide the side clusters are.
    let clusters = row![
        logo,
        Space::new().width(Length::Fill),
        right,
    ]
    .spacing(metrics.bar_cluster_gap)
    .align_y(Alignment::Center)
    .padding([0.0, 10.0])
    .height(Length::Fill);

    let pager = container(taglist)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);

    let bar = Stack::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(clusters)
        .push(pager);

    container(bar)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(palette.bg.into()),
            ..container::Style::default()
        })
        .into()
}

/// The battery glyph and level, or nothing on a machine without a battery.
fn battery_indicator(state: &Panel) -> Element<'_, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;

    let Some(battery) = state.battery else {
        return Space::new().into();
    };

    let (glyph, color) = match battery.status {
        Status::Charging => (0xe1a3, palette.green),
        Status::Full => (0xe1a4, palette.green),
        _ if battery.percentage <= 15 => (0xe19c, palette.red),
        _ => (level_glyph(battery.percentage), palette.text),
    };

    let icon = text(glyph_char(glyph))
        .font(fonts.icon)
        .size(16.0)
        .color(color);
    let label = text(format!("{}%", battery.percentage))
        .font(fonts.text)
        .size(13.0)
        .color(color);

    row![icon, label]
        .spacing(3.0)
        .align_y(Alignment::Center)
        .into()
}

/// The Wi-Fi glyph, or nothing on a machine without a Wi-Fi radio. Clicking
/// toggles the radio.
fn wifi_indicator(state: &Panel) -> Element<'_, Message> {
    let palette = state.theme.palette;
    let metrics = state.theme.metrics;
    let fonts = state.theme.fonts;

    let Some(wifi) = &state.wifi else {
        return Space::new().into();
    };

    let (glyph, color) = if !wifi.enabled {
        (0xe648, palette.muted) // wifi_off
    } else if wifi.ssid.is_some() {
        (wifi_level_glyph(wifi.signal.unwrap_or(0)), palette.text)
    } else {
        (0xeb31, palette.muted) // wifi_find
    };

    status_button(
        metrics,
        fonts.icon,
        glyph,
        color,
        Message::ToggleWifi,
    )
}

/// The Bluetooth glyph, or nothing when the machine has no controller. Clicking
/// toggles the controller.
fn bluetooth_indicator(state: &Panel) -> Element<'_, Message> {
    let palette = state.theme.palette;
    let metrics = state.theme.metrics;
    let fonts = state.theme.fonts;

    let Some(bluetooth) = state.bluetooth else {
        return Space::new().into();
    };

    let (glyph, color) = if !bluetooth.powered {
        (0xe1a9, palette.muted) // bluetooth_disabled
    } else if bluetooth.connected > 0 {
        (0xe1a8, palette.green) // bluetooth_connected
    } else {
        (0xe1a7, palette.text) // bluetooth
    };

    status_button(
        metrics,
        fonts.icon,
        glyph,
        color,
        Message::ToggleBluetooth,
    )
}

/// Picks a `network_wifi_*` glyph for a signal level.
fn wifi_level_glyph(signal: u8) -> u32 {
    match signal {
        75.. => 0xe1ba,    // network_wifi
        50..=74 => 0xebe1, // network_wifi_3_bar
        25..=49 => 0xebd6, // network_wifi_2_bar
        1..=24 => 0xebe4,  // network_wifi_1_bar
        _ => 0xf0b0,       // signal_wifi_0_bar
    }
}

/// A clickable status glyph. Unlike the workspace pager, it keeps the panel's
/// flat background on hover — only the press action matters.
fn status_button<'a>(
    metrics: Metrics,
    font: iced::Font,
    glyph: u32,
    color: Color,
    message: Message,
) -> Element<'a, Message> {
    button(
        container(
            text(glyph_char(glyph))
                .font(font)
                .size(16.0)
                .color(color),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center),
    )
    .width(Length::Fixed(25.0))
    .height(Length::Fixed(metrics.bar_height - 11.0))
    .padding(0)
    .on_press(message)
    .style(move |_theme: &iced::Theme, _status: button::Status| button::Style {
        text_color: color,
        ..button::Style::default()
    })
    .into()
}

/// A codepoint from Material Symbols, rendered through the icon font.
fn glyph_char(codepoint: u32) -> String {
    char::from_u32(codepoint).unwrap_or_default().to_string()
}

/// Picks a `battery_*_bar` glyph for a discharging level.
fn level_glyph(percentage: u8) -> u32 {
    match percentage {
        95.. => 0xe1a4, // battery_full
        80..=94 => 0xebd2, // battery_6_bar
        65..=79 => 0xebd4, // battery_5_bar
        50..=64 => 0xebe2, // battery_4_bar
        35..=49 => 0xebdd, // battery_3_bar
        20..=34 => 0xebe0, // battery_2_bar
        _ => 0xebd9,    // battery_1_bar
    }
}

/// A single workspace button: a bare index number, coloured by state.
fn tag<'a>(state: &'a Panel, workspace: &'a niri::Workspace) -> Element<'a, Message> {
    let palette = state.theme.palette;
    let metrics = state.theme.metrics;
    let fonts = state.theme.fonts;

    let color = if workspace.is_focused {
        palette.text
    } else if workspace.is_occupied() {
        palette.accent
    } else {
        palette.muted
    };

    let idx = workspace.idx;

    button(
        container(
            text(idx.to_string())
                .size(13.0)
                .font(fonts.text)
                .color(color),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center),
    )
    .width(Length::Fixed(25.0))
    .height(Length::Fixed(metrics.bar_height - 11.0))
    .padding(0)
    .on_press(Message::FocusWorkspace(idx))
    .style(move |_theme: &iced::Theme, status: button::Status| button::Style {
        background: Some(
            match status {
                button::Status::Hovered | button::Status::Pressed => palette.hover,
                _ => Color::TRANSPARENT,
            }
            .into(),
        ),
        text_color: color,
        border: Border::default().rounded(metrics.radius_sm),
        ..button::Style::default()
    })
    .into()
}

fn main() -> Result<(), iced_layershell::Error> {
    let theme = config::theme();

    application(
        move || Panel::new(theme),
        "amber-panel",
        update,
        view,
    )
    .settings(iced_layershell::Settings {
        default_font: theme.fonts.text,
        default_text_size: iced::Pixels(13.0),
        layer_settings: LayerShellSettings {
            anchor: Anchor::Top | Anchor::Left | Anchor::Right,
            layer: Layer::Top,
            exclusive_zone: theme.metrics.bar_height as i32,
            size: Some((0, theme.metrics.bar_height as u32)),
            margin: (0, 0, 0, 0),
            keyboard_interactivity: KeyboardInteractivity::None,
            start_mode: StartMode::default(),
            events_transparent: false,
        },
        ..iced_layershell::Settings::default()
    })
    .antialiasing(true)
    .font(assets::COMMIT_MONO)
    .subscription(subscription)
    .theme(iced::Theme::Dark)
    .run()
}
