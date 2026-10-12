//! `amber-launcher` — the application launcher.
//!
//! A one-shot, keyboard-exclusive overlay: it grabs the keyboard, fuzzy-matches
//! the installed `.desktop` applications as you type, and exits as soon as you
//! launch something, press Escape, or click outside the card.

mod apps;
mod exec;

use iced::animation::Easing;
use iced::widget::{
    Id, Space, Stack, button, column, container, mouse_area, row, text, text_input,
};
use iced::{Alignment, Border, Color, Element, Length, Subscription, Task};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};
use iced_layershell::{application, to_layer_message};
use shell_common::theme::Theme;
use shell_common::{assets, config};
use std::time::{Duration, Instant};

/// How many result rows fit on screen at once.
const MAX_ROWS: usize = 8;
const ROW_HEIGHT: f32 = 38.0;
const SEARCH_HEIGHT: f32 = 40.0;

/// How long the open/close slide-and-fade takes.
const SLIDE_DURATION: Duration = Duration::from_millis(200);
/// The animation frame interval while the launcher is animating.
const FRAME: Duration = Duration::from_millis(16);

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    /// Focus the search field once the surface is mapped.
    Focus,
    /// The query text changed.
    QueryChanged(String),
    /// Move the selection down (wrapping).
    SelectNext,
    /// Move the selection up (wrapping).
    SelectPrev,
    /// Launch the selected application.
    Launch,
    /// Launch the application shown on a given result row.
    LaunchIndex(usize),
    /// Dismiss the launcher without doing anything.
    Dismiss,
    /// Absorb a click on the card's own background.
    Noop,
    /// Advance the open/close animation to this instant.
    Tick(Instant),
}

/// The action to run once the launcher finishes its close animation.
enum Close {
    /// Close without launching anything.
    Dismiss,
    /// Launch the app at this index into `apps`.
    Launch(usize),
}

struct Launcher {
    theme: Theme,
    apps: Vec<apps::App>,
    query: String,
    /// Index into `results` of the highlighted row.
    selected: usize,
    /// Indices into `apps`, best match first.
    results: Vec<usize>,
    input_id: Id,
    /// `false` = hidden, `true` = shown; drives the slide-and-fade.
    progress: iced::Animation<bool>,
    /// The instant of the most recent frame, used to project `progress`.
    now: Instant,
    /// Set while closing, carried out once the animation settles.
    closing: Option<Close>,
}

impl Launcher {
    fn new(theme: Theme) -> Self {
        let apps = apps::load();
        let results = (0..apps.len()).collect();
        Self {
            theme,
            apps,
            query: String::new(),
            selected: 0,
            results,
            input_id: Id::unique(),
            progress: iced::Animation::new(false)
                .duration(SLIDE_DURATION)
                .easing(Easing::EaseOutCubic),
            now: Instant::now(),
            closing: None,
        }
    }

    /// Starts closing, once. The reverse animation plays first; the action is
    /// carried out by the final `Tick`, after the animation settles.
    fn begin_close(&mut self, close: Close) {
        if self.closing.is_none() {
            self.now = Instant::now();
            self.closing = Some(close);
            self.progress.go_mut(false, self.now);
        }
    }

    fn recompute(&mut self) {
        self.results = apps::search(&self.apps, &self.query);
        self.selected = 0;
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.results.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let current = self.selected.min(len - 1) as i32;
        self.selected = (current + delta).rem_euclid(len as i32) as usize;
    }

    /// The half-open range of `results` currently on screen, scrolled so the
    /// selection stays visible.
    fn window(&self) -> std::ops::Range<usize> {
        let len = self.results.len();
        if len <= MAX_ROWS {
            return 0..len;
        }
        let start = if self.selected < MAX_ROWS {
            0
        } else {
            self.selected + 1 - MAX_ROWS
        };
        start..(start + MAX_ROWS).min(len)
    }
}

fn update(state: &mut Launcher, message: Message) -> Task<Message> {
    match message {
        Message::Focus => {
            state.now = Instant::now();
            state.progress.go_mut(true, state.now);
            Task::none()
        }
        Message::QueryChanged(query) => {
            state.query = query;
            state.recompute();
            Task::none()
        }
        Message::SelectNext => {
            state.move_selection(1);
            Task::none()
        }
        Message::SelectPrev => {
            state.move_selection(-1);
            Task::none()
        }
        Message::Launch => {
            let close = match state.results.get(state.selected) {
                Some(&app_index) => Close::Launch(app_index),
                None => Close::Dismiss,
            };
            state.begin_close(close);
            Task::none()
        }
        Message::LaunchIndex(index) => {
            state.selected = index;
            if let Some(&app_index) = state.results.get(index) {
                state.begin_close(Close::Launch(app_index));
            }
            Task::none()
        }
        Message::Dismiss => {
            state.begin_close(Close::Dismiss);
            Task::none()
        }
        Message::Noop => Task::none(),
        Message::Tick(now) => {
            state.now = now;
            if state.progress.is_animating(now) {
                // Keep the search field focused while the surface opens. It can
                // only take focus once it is laid out, which may be a frame
                // after the `Focus` message, so reissue it until the animation
                // settles rather than trying to focus it too early.
                return if state.closing.is_none() {
                    iced::widget::operation::focus(state.input_id.clone())
                } else {
                    Task::none()
                };
            }
            match state.closing.take() {
                Some(Close::Launch(app_index)) => {
                    spawn(&state.apps[app_index]);
                    iced::exit()
                }
                Some(Close::Dismiss) => iced::exit(),
                None => Task::none(),
            }
        }
        // Layer-shell actions injected by `#[to_layer_message]`.
        _ => Task::none(),
    }
}

fn subscription(state: &Launcher) -> Subscription<Message> {
    // Only drive frames while there is something to animate, including the
    // final tick that carries out a pending close.
    let animating = state.closing.is_some() || state.progress.is_animating(state.now);
    let tick = if animating {
        iced::time::every(FRAME).map(Message::Tick)
    } else {
        Subscription::none()
    };

    Subscription::batch([
        iced::event::listen_with(on_key),
        Subscription::run(focus_once),
        tick,
    ])
}

/// A stream that fires exactly once, to focus the search field on startup.
fn focus_once() -> impl iced::futures::Stream<Item = Message> {
    iced::futures::stream::once(async { Message::Focus })
}

fn on_key(
    event: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    use iced::keyboard::Key;
    use iced::keyboard::key::Named;

    let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) = event else {
        return None;
    };

    // Escape always dismisses, even if a widget claims it.
    if matches!(key, Key::Named(Named::Escape)) {
        return Some(Message::Dismiss);
    }

    // Leave anything a widget (like the focused text input) already handled
    // alone. Enter arrives here only when the field is not focused.
    if status == iced::event::Status::Captured {
        return None;
    }

    match key {
        Key::Named(Named::ArrowDown) => Some(Message::SelectNext),
        Key::Named(Named::ArrowUp) => Some(Message::SelectPrev),
        Key::Named(Named::Enter) => Some(Message::Launch),
        _ => None,
    }
}

fn view(state: &Launcher) -> Element<'_, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;
    let metrics = state.theme.metrics;

    // `0.0` when hidden, `1.0` when fully open. Drives both the fade and the
    // slide, so everything fades and moves together.
    let t: f32 = state.progress.interpolate(0.0, 1.0, state.now);

    let search = text_input("Search applications", &state.query)
        .id(state.input_id.clone())
        .on_input(Message::QueryChanged)
        .on_submit(Message::Launch)
        .padding(0)
        .size(15.0)
        .font(fonts.text)
        .style(move |_theme: &iced::Theme, _status| text_input::Style {
            background: Color::TRANSPARENT.into(),
            border: Border::default(),
            icon: palette.muted.scale_alpha(t),
            placeholder: palette.muted.scale_alpha(t),
            value: palette.text.scale_alpha(t),
            selection: palette.accent_fill.scale_alpha(t),
        });

    let pill = container(
        row![
            text(glyph_char(0xe8b6))
                .font(fonts.icon)
                .size(18.0)
                .color(palette.muted.scale_alpha(t)),
            search,
        ]
        .spacing(8.0)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(SEARCH_HEIGHT))
    .padding([0.0, 12.0])
    .align_y(Alignment::Center)
    .style(move |_theme: &iced::Theme| container::Style {
        background: Some(palette.field.scale_alpha(t).into()),
        border: Border::default(),
        ..container::Style::default()
    });

    let window = state.window();
    let rows: Vec<Element<'_, Message>> = window
        .clone()
        .map(|index| result_row(state, index, index == state.selected, t))
        .collect();

    let body: Element<'_, Message> = if rows.is_empty() {
        container(
            text("No matches")
                .size(13.0)
                .font(fonts.text)
                .color(palette.muted.scale_alpha(t)),
        )
        .width(Length::Fill)
        .height(Length::Fixed(ROW_HEIGHT + 8.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
    } else {
        column(rows).spacing(4.0).into()
    };

    let card = container(column![pill, body].spacing(8.0))
        .width(Length::Fixed(metrics.overlay_width))
        .padding(10.0)
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(palette.bg.scale_alpha(t).into()),
            border: Border::default(),
            ..container::Style::default()
        });

    // A click catcher fills the output so anywhere outside the card dismisses;
    // the card itself absorbs its own clicks with a no-op.
    let mut overlays = Stack::new().width(Length::Fill).height(Length::Fill).push(
        mouse_area(container(
            Space::new().width(Length::Fill).height(Length::Fill),
        ))
        .on_press(Message::Dismiss),
    );

    // Slide down from the top edge: with an empty top spacer the card hugs the
    // top when hidden, and at rest the spacers are equal so it sits centred.
    // The card is always in the tree — even fully transparent — so the search
    // field exists (and can be focused) from the very first frame. Only the
    // card is wrapped in a `mouse_area`, so clicks in the empty spacers fall
    // through to the dismiss catcher below.
    let top = 1 + (t * 1000.0) as u16;
    let bottom = 1001;

    overlays = overlays.push(
        column![
            Space::new().height(Length::FillPortion(top)),
            mouse_area(card).on_press(Message::Noop),
            Space::new().height(Length::FillPortion(bottom)),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center),
    );

    overlays.into()
}

/// One result row: the app name, with its generic name underneath when known.
fn result_row<'a>(
    state: &'a Launcher,
    index: usize,
    selected: bool,
    t: f32,
) -> Element<'a, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;
    let app = &state.apps[state.results[index]];

    // Unselected rows sit back in grey; the selected row goes white. The generic
    // name is drawn at lower opacity so the title stays the focal point. Both are
    // faded by the open/close animation.
    let color = if selected {
        palette.text
    } else {
        palette.muted
    }
    .scale_alpha(t);
    let subtext_color = color.scale_alpha(0.6);

    let mut labels = column![
        text(app.name.clone())
            .size(14.0)
            .font(fonts.text)
            .color(color)
    ]
    .spacing(1.0);
    if let Some(generic) = &app.generic {
        labels = labels.push(
            text(generic.clone())
                .size(11.0)
                .font(fonts.text)
                .color(subtext_color),
        );
    }

    button(
        container(labels)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Start)
            .align_y(Alignment::Center)
            .padding([0.0, 12.0]),
    )
    .width(Length::Fill)
    .height(Length::Fixed(ROW_HEIGHT))
    .padding(0)
    .on_press(Message::LaunchIndex(index))
    .style(move |_theme: &iced::Theme, status: button::Status| {
        let background = if selected {
            palette.accent_fill
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => palette.hover,
                _ => palette.surface,
            }
        };
        button::Style {
            background: Some(background.scale_alpha(t).into()),
            text_color: color,
            border: Border::default(),
            ..button::Style::default()
        }
    })
    .into()
}

/// Spawns an application without waiting for it, detached from our streams.
fn spawn(app: &apps::App) {
    let mut argv = app.exec.clone();
    if app.terminal
        && let Some(term) = terminal_command()
    {
        argv = term.into_iter().chain(app.exec.iter().cloned()).collect();
    }

    let Some((program, args)) = argv.split_first() else {
        return;
    };

    let result = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    if let Err(error) = result {
        eprintln!("amber-launcher: failed to launch {}: {error}", app.name);
    }
}

/// The terminal to run `Terminal=true` entries in: `$TERMINAL` if set,
/// otherwise the first of a few common emulators found on `PATH`.
fn terminal_command() -> Option<Vec<String>> {
    if let Some(term) = std::env::var_os("TERMINAL") {
        let parts: Vec<String> = term
            .to_string_lossy()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        if !parts.is_empty() {
            return Some(parts);
        }
    }

    const CANDIDATES: [&str; 8] = [
        "foot",
        "kitty",
        "alacritty",
        "wezterm",
        "ghostty",
        "gnome-terminal",
        "konsole",
        "xterm",
    ];

    CANDIDATES
        .iter()
        .find(|candidate| on_path(candidate))
        .map(|candidate| vec![(*candidate).to_string()])
}

fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let is_executable = |path: &std::path::Path| {
        path.metadata()
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };

    if program.contains('/') {
        return is_executable(std::path::Path::new(program));
    }

    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(program))))
        .unwrap_or(false)
}

/// A codepoint from Material Symbols, rendered through the icon font.
fn glyph_char(codepoint: u32) -> String {
    char::from_u32(codepoint).unwrap_or_default().to_string()
}

fn main() -> Result<(), iced_layershell::Error> {
    let theme = config::theme();

    application(move || Launcher::new(theme), "amber-launcher", update, view)
        .settings(iced_layershell::Settings {
            default_font: theme.fonts.text,
            default_text_size: iced::Pixels(14.0),
            layer_settings: LayerShellSettings {
                // Anchor to every edge and let the card centre itself in the
                // resulting full-output surface.
                anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
                layer: Layer::Overlay,
                exclusive_zone: 0,
                size: None,
                margin: (0, 0, 0, 0),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                start_mode: StartMode::Active,
                events_transparent: false,
            },
            ..iced_layershell::Settings::default()
        })
        .antialiasing(true)
        .font(assets::COMMIT_MONO)
        .subscription(subscription)
        .style(
            |_state: &Launcher, _theme: &iced::Theme| iced::theme::Style {
                background_color: Color::TRANSPARENT,
                text_color: Color::WHITE,
            },
        )
        .run()
}
