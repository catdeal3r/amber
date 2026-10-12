//! `amber-panel` — the top bar.
//!
//! A single layer-shell surface anchored to the top edge: the Nix snowflake
//! on the left, the workspace pager in the middle, and the clock on the
//! right. Cozy "fuji" styling throughout.

mod niri;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, image, mouse_area, row, text, Space, Stack};
use iced::{Alignment, Border, Color, ContentFit, Element, Length, Padding, Task};
use iced_layershell::reexport::{
    Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, StartMode};
use iced_layershell::{daemon, to_layer_message};
use shell_common::battery::{Battery, Status};
use shell_common::bluetooth::{self, Bluetooth};
use shell_common::theme::{Metrics, Theme};
use shell_common::tray::{self, Tray, TrayItem};
use shell_common::wifi::{self, Wifi};
use shell_common::{assets, config};

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    /// Re-read the workspace list from niri.
    PollWorkspaces,
    /// Re-read the clock text.
    Clock,
    /// Re-read the battery, Wi-Fi and Bluetooth state.
    PollStatus,
    /// Re-read the system tray snapshot.
    PollTray,
    /// Pick up a context menu the tray thread has finished reading.
    MenuReady,
    /// Switch to a workspace by index.
    FocusWorkspace(u32),
    /// Toggle the Wi-Fi radio.
    ToggleWifi,
    /// Toggle the Bluetooth radio.
    ToggleBluetooth,
    /// A tray item was left-clicked.
    TrayActivate(String),
    /// A tray item was middle-clicked.
    TraySecondaryActivate(String),
    /// A tray item was right-clicked.
    TrayContextMenu(String),
    /// A row of the open context menu was chosen.
    MenuAction(i32),
    /// Dismiss the open context menu.
    CloseMenu,
    /// A window was closed by the compositor.
    WindowClosed(iced::window::Id),
    /// The pointer moved within a window (its local x), used to anchor menus.
    PointerMoved(f32),
    /// Absorb a click inside the menu card.
    Noop,
}

/// The context menu currently shown, on its own overlay layer surface.
struct OpenMenu {
    /// The window the menu is drawn on.
    window: iced::window::Id,
    /// The tray item the menu belongs to, used to route row clicks back.
    key: String,
    /// The frozen, flattened rows to draw.
    entries: Vec<tray::MenuEntry>,
    /// The screen x the menu's right edge is aligned to (the clicked icon).
    anchor_x: f32,
}

struct Panel {
    theme: Theme,
    workspaces: Vec<niri::Workspace>,
    clock: String,
    battery: Option<Battery>,
    wifi: Option<Wifi>,
    bluetooth: Option<Bluetooth>,
    /// Live handle to the tray watcher; `tray_items` is its last snapshot.
    tray: Tray,
    tray_items: Vec<TrayItem>,
    /// Decoded iced handles, keyed by item key. Cached so an unchanged icon
    /// isn't re-uploaded to the GPU every poll.
    tray_icons: HashMap<String, (tray::Icon, image::Handle)>,
    /// A context menu we've asked for but not yet received, with the instant of
    /// the request so we can give up if the item never answers.
    menu_pending: Option<(String, Instant)>,
    /// The context menu currently shown, if any.
    menu: Option<OpenMenu>,
    /// The pointer's last known x within the bar, used to anchor a menu under
    /// the icon that was clicked.
    pointer_x: f32,
    logo: image::Handle,
    config_error: Option<String>,
}

impl Panel {
    fn new(theme: Theme) -> Self {
        let nix = assets::nix_bar();
        let logo = image::Handle::from_rgba(nix.width, nix.height, nix.pixels.clone());
        let tray = Tray::spawn();
        let tray_items = tray.items();

        Self {
            theme,
            workspaces: Vec::new(),
            clock: clock_now(),
            battery: Battery::read(),
            wifi: wifi::read(),
            bluetooth: bluetooth::read(),
            tray,
            tray_items,
            tray_icons: HashMap::new(),
            menu_pending: None,
            menu: None,
            pointer_x: 0.0,
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
        Message::PollTray => {
            let items = state.tray.items();

            // Re-upload only icons whose pixels actually changed.
            for item in &items {
                match &item.icon {
                    Some(icon) => {
                        let unchanged = state
                            .tray_icons
                            .get(&item.key)
                            .is_some_and(|(cached, _)| cached == icon);
                        if !unchanged {
                            let handle = image::Handle::from_rgba(
                                icon.width,
                                icon.height,
                                icon.pixels.clone(),
                            );
                            state
                                .tray_icons
                                .insert(item.key.clone(), (icon.clone(), handle));
                        }
                    }
                    None => {
                        state.tray_icons.remove(&item.key);
                    }
                }
            }

            state
                .tray_icons
                .retain(|key, _| items.iter().any(|item| &item.key == key));
            state.tray_items = items;
        }
        Message::TrayActivate(key) => state.tray.activate(&key),
        Message::TraySecondaryActivate(key) => state.tray.secondary_activate(&key),
        Message::TrayContextMenu(key) => {
            state.tray.context_menu(&key);
            state.menu_pending = Some((key, Instant::now()));
        }
        Message::MenuReady => return menu_ready(state),
        Message::MenuAction(id) => {
            let key = state.menu.as_ref().map(|menu| menu.key.clone());
            if let Some(key) = key {
                state.tray.menu_click(&key, id);
            }
            return close_menu(state);
        }
        Message::CloseMenu => return close_menu(state),
        Message::WindowClosed(id) => {
            // The compositor may close the overlay for us; drop our handle too.
            if state.menu.as_ref().is_some_and(|menu| menu.window == id) {
                state.menu = None;
            }
        }
        Message::PointerMoved(x) => state.pointer_x = x,
        Message::Noop => {}
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

/// Pick up a menu the tray thread has read, and open it on a fresh overlay
/// surface. If the item never produced one (it may have popped its own window
/// instead), stop waiting after a short grace period.
fn menu_ready(state: &mut Panel) -> Task<Message> {
    let timed_out = state
        .menu_pending
        .as_ref()
        .is_none_or(|(_, at)| at.elapsed() > Duration::from_secs(2));
    if timed_out {
        state.menu_pending = None;
        return Task::none();
    }

    let Some(menu) = state.tray.take_menu() else {
        return Task::none();
    };
    if state.menu_pending.as_ref().map(|(key, _)| key) != Some(&menu.key) || state.menu.is_some() {
        return Task::none();
    }
    state.menu_pending = None;

    let window = iced::window::Id::unique();
    state.menu = Some(OpenMenu {
        window,
        key: menu.key,
        entries: menu.entries,
        anchor_x: state.pointer_x,
    });

    let settings = NewLayerShellSettings {
        size: None,
        layer: Layer::Overlay,
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        exclusive_zone: None,
        margin: None,
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        output_option: OutputOption::Active,
        events_transparent: false,
        namespace: Some("amber-tray-menu".to_string()),
    };
    Task::done(Message::NewLayerShell {
        settings,
        id: window,
    })
}

/// Tear down the open menu, if any, and remove its overlay surface.
fn close_menu(state: &mut Panel) -> Task<Message> {
    state.menu_pending = None;
    match state.menu.take() {
        Some(menu) => Task::done(Message::RemoveWindow(menu.window)),
        None => Task::none(),
    }
}

fn subscription(state: &Panel) -> iced::Subscription<Message> {
    let mut subscriptions = vec![
        iced::time::every(Duration::from_millis(400)).map(|_| Message::PollWorkspaces),
        iced::time::every(Duration::from_secs(1)).map(|_| Message::Clock),
        iced::time::every(Duration::from_secs(5)).map(|_| Message::PollStatus),
        iced::time::every(Duration::from_secs(1)).map(|_| Message::PollTray),
        iced::window::close_events().map(Message::WindowClosed),
        iced::event::listen_with(on_event),
    ];

    // Poll quickly only while a menu is outstanding, so it appears promptly
    // without waking up every second the rest of the time.
    if state.menu_pending.is_some() {
        subscriptions
            .push(iced::time::every(Duration::from_millis(80)).map(|_| Message::MenuReady));
    }

    iced::Subscription::batch(subscriptions)
}

/// Escape dismisses an open menu; pointer movement is tracked so a menu can be
/// anchored under the icon that was clicked.
fn on_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    use iced::keyboard::Key;
    use iced::keyboard::key::Named;

    match event {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) => {
            if matches!(key, Key::Named(Named::Escape)) {
                return Some(Message::CloseMenu);
            }
            None
        }
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
            Some(Message::PointerMoved(position.x))
        }
        _ => None,
    }
}

/// Draw the surface for `window`: the bar itself, or the context-menu overlay.
fn view(state: &Panel, window: iced::window::Id) -> Element<'_, Message> {
    match &state.menu {
        Some(menu) if menu.window == window => menu_view(state, menu),
        _ => bar_view(state),
    }
}

/// The top bar.
fn bar_view(state: &Panel) -> Element<'_, Message> {
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
    right_items.extend(tray_indicators(state));
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

/// Width of the context-menu card.
const MENU_WIDTH: f32 = 220.0;
/// Height of a single menu row.
const MENU_ROW_HEIGHT: f32 = 27.0;
/// Font size of menu labels.
const MENU_LABEL_SIZE: f32 = 13.0;
/// Approximate advance width of one character in the monospace UI font, as a
/// fraction of the font size. Used only to estimate label truncation; rounded
/// up from CommitMono's ~0.604 to be sure the label still fits.
const MENU_CHAR_RATIO: f32 = 0.61;

/// Truncate `label` so it fits within `width` pixels at [`MENU_LABEL_SIZE`],
/// appending an ellipsis when characters are dropped. iced has no built-in
/// text ellipsis, so the fit is estimated from the monospace advance width; the
/// label column also clips as a backstop.
fn clip_label(label: &str, width: f32) -> String {
    let max_chars = (width / (MENU_LABEL_SIZE * MENU_CHAR_RATIO))
        .floor()
        .max(1.0) as usize;
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    let mut out: String = label.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

/// The context-menu overlay: a full-output click catcher with the menu card
/// pinned just under the bar, its right edge under the tray icon clicked.
fn menu_view<'a>(state: &'a Panel, menu: &'a OpenMenu) -> Element<'a, Message> {
    let palette = state.theme.palette;

    let rows: Vec<Element<'a, Message>> = menu
        .entries
        .iter()
        .map(|entry| menu_row(state, entry))
        .collect();

    let card = container(column(rows).width(Length::Fill))
        .width(Length::Fixed(MENU_WIDTH))
        .padding([6.0, 0.0])
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(palette.bg.into()),
            border: Border::default().color(palette.muted).width(1.0),
            ..container::Style::default()
        });

    // Clicks on the card's own background are absorbed; clicks on a row are
    // handled by the row, which sits above this catcher.
    let card = mouse_area(card).on_press(Message::Noop);

    // Pin the card just below the bar, with its right edge under the tray icon
    // that was clicked. A layer surface's origin already sits below the bar's
    // exclusive zone, so the top padding here is only a small gap.
    let anchored = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Start)
        .align_y(Alignment::Start)
        .padding(Padding {
            top: 4.0,
            right: 0.0,
            bottom: 0.0,
            left: (menu.anchor_x - MENU_WIDTH).max(0.0),
        });

    // Anything outside the card dismisses the menu.
    let backdrop = mouse_area(container(
        Space::new().width(Length::Fill).height(Length::Fill),
    ))
    .on_press(Message::CloseMenu);

    Stack::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(backdrop)
        .push(anchored)
        .into()
}

/// One menu row: a separator or a selectable item.
fn menu_row<'a>(state: &'a Panel, entry: &'a tray::MenuEntry) -> Element<'a, Message> {
    match entry {
        tray::MenuEntry::Separator => menu_separator(state),
        tray::MenuEntry::Item(item) => menu_item(state, item),
    }
}

/// A thin horizontal rule, inset from the card's edges.
fn menu_separator(state: &Panel) -> Element<'_, Message> {
    let palette = state.theme.palette;
    let line = container(Space::new().width(Length::Fill).height(Length::Fixed(1.0)))
        .width(Length::Fill)
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(palette.muted.into()),
            ..container::Style::default()
        });

    container(line)
        .width(Length::Fill)
        .padding([4.0, 8.0])
        .into()
}

/// A selectable menu row: an optional toggle glyph, an indented label, and a
/// disclosure arrow when the row heads a submenu. Rows that open a submenu or
/// are disabled are drawn plainly and do not react to clicks.
fn menu_item<'a>(state: &'a Panel, item: &'a tray::MenuItem) -> Element<'a, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;

    let indicator: Element<'a, Message> = match item.toggle {
        tray::Toggle::None => Space::new().width(Length::Fixed(18.0)).into(),
        tray::Toggle::Checkbox => menu_glyph(
            fonts.icon,
            if item.checked { 0xe834 } else { 0xe835 },
            if item.checked {
                palette.accent
            } else {
                palette.muted
            },
        ),
        tray::Toggle::Radio => menu_glyph(
            fonts.icon,
            if item.checked { 0xe837 } else { 0xe836 },
            if item.checked {
                palette.accent
            } else {
                palette.muted
            },
        ),
    };

    let color = if item.enabled {
        palette.text
    } else {
        palette.muted
    };

    let trailing: Element<'a, Message> = if item.has_submenu {
        menu_glyph(fonts.icon, 0xe5cf, palette.muted)
    } else {
        Space::new().into()
    };

    // Trim long labels with an ellipsis so they stay on one line; the label
    // column also clips, as a backstop against any overflow.
    let indent = 8.0 + item.depth as f32 * 14.0;
    let trailing_width = if item.has_submenu { 15.0 + 6.0 } else { 0.0 };
    let label_width = MENU_WIDTH - 2.0 - indent - 10.0 - 18.0 - 6.0 - trailing_width;

    let label = container(
        text(clip_label(&item.label, label_width))
            .size(MENU_LABEL_SIZE)
            .font(fonts.text)
            .color(color)
            .wrapping(iced::widget::text::Wrapping::None),
    )
    .width(Length::Fill)
    .clip(true);

    let content = row![indicator, label, trailing]
        .spacing(6.0)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let padded = container(content)
        .width(Length::Fill)
        .height(Length::Fixed(MENU_ROW_HEIGHT))
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 0.0,
            right: 10.0,
            bottom: 0.0,
            left: indent,
        });

    if item.enabled && !item.has_submenu {
        button(padded)
            .width(Length::Fill)
            .padding(0)
            .on_press(Message::MenuAction(item.id))
            .style(move |_theme: &iced::Theme, status| button::Style {
                background: Some(
                    match status {
                        button::Status::Hovered | button::Status::Pressed => palette.hover,
                        _ => Color::TRANSPARENT,
                    }
                    .into(),
                ),
                border: Border::default(),
                ..button::Style::default()
            })
            .into()
    } else {
        padded.into()
    }
}

/// An icon-font glyph for a menu row.
fn menu_glyph<'a>(font: iced::Font, codepoint: u32, color: Color) -> Element<'a, Message> {
    text(glyph_char(codepoint))
        .font(font)
        .size(15.0)
        .color(color)
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

/// One icon per registered tray item, in registration order.
fn tray_indicators(state: &Panel) -> Vec<Element<'_, Message>> {
    state
        .tray_items
        .iter()
        .map(|item| tray_icon(state, item))
        .collect()
}

/// A single tray icon: the item's decoded pixmap, or a muted dot when the item
/// offers no icon we can render. Left/middle/right clicks drive the item.
fn tray_icon<'a>(state: &'a Panel, item: &'a TrayItem) -> Element<'a, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;
    let size = state.theme.metrics.bar_height * 0.45;

    let content: Element<'a, Message> = match state.tray_icons.get(&item.key) {
        Some((_, handle)) => image(handle.clone())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .content_fit(ContentFit::Contain)
            .into(),
        None => text("\u{2022}")
            .font(fonts.text)
            .size(size * 0.6)
            .color(match item.status {
                tray::TrayStatus::NeedsAttention => palette.yellow,
                _ => palette.muted,
            })
            .into(),
    };

    // A menu-only item opens its menu on left click; otherwise left activates.
    let on_press = if item.is_menu {
        Message::TrayContextMenu(item.key.clone())
    } else {
        Message::TrayActivate(item.key.clone())
    };

    mouse_area(content)
        .on_press(on_press)
        .on_right_press(Message::TrayContextMenu(item.key.clone()))
        .on_middle_press(Message::TraySecondaryActivate(item.key.clone()))
        .into()
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

    daemon(
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
    // The bar paints its own background; the menu overlay must stay transparent
    // apart from its card, so the window's clear colour is transparent.
    .style(|_state: &Panel, _theme: &iced::Theme| iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: Color::WHITE,
    })
    .run()
}
