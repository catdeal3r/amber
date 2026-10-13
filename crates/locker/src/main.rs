//! `amber-locker` — the lock screen.
//!
//! A full-output layer-shell surface on every monitor, above everything else,
//! with an exclusive keyboard grab. It shows the Nix snowflake, the time and
//! date, and a password pill; a correct password (verified through PAM) exits
//! the process and releases the lock.
//!
//! It is a one-shot locker: `swayidle` (or a keybinding) invokes it, and it
//! exits as soon as the user authenticates. A `flock`ed lock file makes extra
//! invocations no-ops so a second trigger can't stack lock screens.

mod pam;

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use iced::widget::{Space, column, container, image, text, text_input};
use iced::{Alignment, Border, Color, ContentFit, Element, Length, Padding, Task};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};
use iced_layershell::{daemon, to_layer_message};
use shell_common::theme::{Palette, Theme};
use shell_common::{assets, config};

/// Identifier of the (single) password field, kept focused while unlocked.
const PASSWORD: &str = "amber-locker-password";

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    /// Advance the clock.
    Tick,
    /// Give the password field keyboard focus.
    Focus,
    /// The password was edited.
    Edited(String),
    /// Clear the password (Escape).
    Clear,
    /// The user pressed Enter.
    Submit,
    /// A PAM attempt finished.
    Authenticated(Result<(), String>),
}

struct Locker {
    theme: Theme,
    logo: image::Handle,
    time: String,
    date: String,
    password: String,
    error: Option<String>,
    authenticating: bool,
    user: String,
    pam_service: String,
}

impl Locker {
    fn new(theme: Theme) -> Self {
        let nix = assets::nix_lock();
        let logo = image::Handle::from_rgba(nix.width, nix.height, nix.pixels.clone());
        let now = chrono::Local::now();

        Locker {
            theme,
            logo,
            time: now.format("%H:%M").to_string(),
            date: now.format("%A, %-d %B").to_string(),
            password: String::new(),
            error: None,
            authenticating: false,
            user: current_user(),
            pam_service: config::lock().pam_service,
        }
    }

    fn refresh(&mut self) {
        let now = chrono::Local::now();
        self.time = now.format("%H:%M").to_string();
        self.date = now.format("%A, %-d %B").to_string();
    }
}

fn main() -> Result<(), iced_layershell::Error> {
    // A second locker would just stack another surface; if one is already
    // holding the lock file's flock, bail out.
    let Some(_instance) = single_instance() else {
        eprintln!("amber-locker: already running");
        return Ok(());
    };

    let theme = config::theme();
    let default_font = theme.fonts.text;

    daemon(
        move || (Locker::new(theme), initial_focus()),
        "amber-locker",
        update,
        view,
    )
    .settings(iced_layershell::Settings {
        default_font,
        default_text_size: iced::Pixels(15.0),
        layer_settings: LayerShellSettings {
            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            layer: Layer::Overlay,
            // `-1` covers the full output, ignoring the bar's exclusive zone.
            exclusive_zone: -1,
            size: Some((0, 0)),
            margin: (0, 0, 0, 0),
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            start_mode: StartMode::AllScreens,
            events_transparent: false,
        },
        ..iced_layershell::Settings::default()
    })
    .antialiasing(true)
    .font(assets::COMMIT_MONO)
    .subscription(subscription)
    .theme(iced::Theme::Dark)
    // The lock paints its own full-output background.
    .style(|_state: &Locker, _theme: &iced::Theme| iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: Color::WHITE,
    })
    .run()
}

fn update(state: &mut Locker, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {
            state.refresh();
            if state.password.is_empty() && !state.authenticating && state.error.is_none() {
                focus()
            } else {
                Task::none()
            }
        }
        Message::Focus => focus(),
        Message::Edited(password) => {
            state.password = password;
            state.error = None;
            Task::none()
        }
        Message::Clear => {
            state.password.clear();
            state.error = None;
            focus()
        }
        Message::Submit => {
            if state.authenticating {
                return Task::none();
            }
            if state.password.is_empty() {
                state.error = Some("Enter your password".to_string());
                return focus();
            }

            state.authenticating = true;
            state.error = None;

            let service = state.pam_service.clone();
            let user = state.user.clone();
            let password = std::mem::take(&mut state.password);

            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        pam::authenticate(&service, &user, &password)
                    })
                    .await
                    .unwrap_or_else(|_| Err("authentication task failed".to_string()))
                },
                Message::Authenticated,
            )
        }
        Message::Authenticated(Ok(())) => iced::exit(),
        Message::Authenticated(Err(error)) => {
            state.authenticating = false;
            state.password.clear();
            state.error = Some(error);
            focus()
        }
        // Layer-shell messages are handled by the runtime.
        _ => Task::none(),
    }
}

fn subscription(_state: &Locker) -> iced::Subscription<Message> {
    iced::Subscription::batch([
        iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick),
        iced::event::listen_with(on_event),
    ])
}

fn on_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    use iced::keyboard::Key;
    use iced::keyboard::key::Named;

    match event {
        iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus),
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_)) => Some(Message::Focus),
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) => {
            matches!(key, Key::Named(Named::Escape)).then_some(Message::Clear)
        }
        _ => None,
    }
}

fn view(state: &Locker, _window: iced::window::Id) -> Element<'_, Message> {
    let palette = state.theme.palette;
    let fonts = state.theme.fonts;
    let invalid = state.error.is_some();

    let logo = image(state.logo.clone())
        .width(Length::Fixed(48.0))
        .height(Length::Fixed(48.0))
        .content_fit(ContentFit::Contain);

    let time = text(&state.time)
        .size(56.0)
        .font(fonts.text)
        .color(palette.text);

    let date = text(&state.date)
        .size(16.0)
        .font(fonts.text)
        .color(palette.muted);

    let placeholder = if state.authenticating {
        "Authenticating…"
    } else {
        "Password"
    };

    let password = text_input(placeholder, &state.password)
        .id(iced::widget::Id::new(PASSWORD))
        .secure(true)
        .font(fonts.text)
        .size(15.0)
        .padding(Padding::from([10.0, 14.0]))
        .width(Length::Fixed(260.0))
        .align_x(Alignment::Center)
        .on_input(Message::Edited)
        .on_submit(Message::Submit)
        .style(move |_theme: &iced::Theme, _status: text_input::Status| {
            password_style(palette, invalid)
        });

    let prompt: Element<'_, Message> = match &state.error {
        Some(error) => text(error)
            .size(13.0)
            .font(fonts.text)
            .color(palette.red)
            .into(),
        None => Space::new().height(Length::Fixed(16.0)).into(),
    };

    let content = column![
        logo,
        Space::new().height(Length::Fixed(24.0)),
        time,
        date,
        Space::new().height(Length::Fixed(28.0)),
        password,
        Space::new().height(Length::Fixed(10.0)),
        prompt,
    ]
    .align_x(Alignment::Center)
    .width(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(palette.bg.into()),
            ..container::Style::default()
        })
        .into()
}

fn password_style(palette: Palette, invalid: bool) -> text_input::Style {
    text_input::Style {
        background: palette.field.into(),
        border: Border {
            color: if invalid { palette.red } else { palette.muted },
            width: 1.0,
            radius: 10.0.into(),
        },
        icon: palette.muted,
        placeholder: palette.muted,
        value: palette.text,
        selection: palette.accent,
    }
}

/// Focus the password field in every window.
fn focus() -> Task<Message> {
    iced::widget::operation::focus::<Message>(iced::widget::Id::new(PASSWORD))
}

/// Nudge focus once the lock surfaces have had a moment to be created.
fn initial_focus() -> Task<Message> {
    Task::batch([
        focus(),
        Task::perform(
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
            },
            |()| Message::Focus,
        ),
    ])
}

/// Hold an exclusive lock on a runtime file for the life of the process.
/// Returns `None` if another locker already holds it.
fn single_instance() -> Option<File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let file = File::create(dir.join("amber-locker.lock")).ok()?;

    match file.try_lock() {
        Ok(()) => Some(file),
        Err(_) => None,
    }
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_string())
}
