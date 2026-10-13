//! `amber-locker` — the lock screen.
//!
//! A full-output layer-shell surface on every monitor, above everything else,
//! with an exclusive keyboard grab. Over a darkened, blurred copy of the
//! configured wallpaper it shows the time, the date and a password pill; a
//! correct password (verified through PAM) exits the process and releases the
//! lock.
//!
//! It is a one-shot locker: `swayidle` (or a keybinding) invokes it, and it
//! exits as soon as the user authenticates. A `flock`ed lock file makes extra
//! invocations no-ops so a second trigger can't stack lock screens.

mod pam;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use iced::widget::{Space, Stack, column, container, image as image_widget, text, text_input};
use iced::{Alignment, Border, Color, ContentFit, Element, Length, Padding, Task};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};
use iced_layershell::{daemon, to_layer_message};
use shell_common::assets;
use shell_common::config::{self, WallpaperConfig};
use shell_common::theme::{Palette, Theme};

/// Identifier of the (single) password field, kept focused while unlocked.
const PASSWORD: &str = "amber-locker-password";

/// The wallpaper is downscaled to fit within this box before it is blurred: it
/// is blurred anyway, and a smaller source keeps the off-thread processing
/// quick. Comfortably under iced's texture-atlas cap (`iced_wgpu`'s `MAX_SIZE`,
/// 2048), past which images render as garbage.
const BLUR_MAX_SIDE: u32 = 1280;

/// Blur radius, in source pixels, applied to the wallpaper.
const BLUR_SIGMA: f32 = 24.0;

/// How much to darken the wallpaper behind the lock (0 = untouched, 1 = black).
const DARKEN: f32 = 0.5;

/// Frames still owed while iced's asynchronous image upload settles; its own
/// redraw request can be dropped for a static layer surface, so keep ticking
/// for a short while after the wallpaper arrives.
const SETTLE_TICKS: u32 = 40;

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
    /// The blurred wallpaper backdrop finished loading (`None` if unavailable).
    Background(Option<image_widget::Handle>),
}

struct Locker {
    theme: Theme,
    background: Option<image_widget::Handle>,
    fit: ContentFit,
    time: String,
    date: String,
    password: String,
    error: Option<String>,
    authenticating: bool,
    user: String,
    pam_service: String,
    settling: u32,
}

impl Locker {
    fn new(theme: Theme) -> Self {
        let now = chrono::Local::now();

        Locker {
            theme,
            background: None,
            fit: config::wallpaper().fit,
            time: now.format("%H:%M").to_string(),
            date: now.format("%A, %-d %B").to_string(),
            password: String::new(),
            error: None,
            authenticating: false,
            user: current_user(),
            pam_service: config::lock().pam_service,
            settling: 0,
        }
    }

    fn refresh(&mut self) {
        let now = chrono::Local::now();
        self.time = now.format("%H:%M").to_string();
        self.date = now.format("%A, %-d %B").to_string();
    }
}

/// Start the lock with a solid `bg` and load the blurred wallpaper backdrop in
/// the background, so the screen is covered immediately rather than after a
/// second or two of image processing.
fn boot(theme: Theme) -> (Locker, Task<Message>) {
    let state = Locker::new(theme);
    let wallpaper = config::wallpaper();

    let backdrop = Task::perform(
        async move {
            tokio::task::spawn_blocking(move || load_wallpaper(&wallpaper))
                .await
                .ok()
                .flatten()
        },
        Message::Background,
    );

    (state, Task::batch([initial_focus(), backdrop]))
}

/// Decode the configured wallpaper, blur it and darken it for use as the lock
/// backdrop. Returns `None` (falling back to the solid `bg`) when there is no
/// wallpaper configured or it can't be read; problems are reported on stderr.
fn load_wallpaper(cfg: &WallpaperConfig) -> Option<image_widget::Handle> {
    let path = cfg.path.as_deref()?;

    match blurred(path) {
        Ok(handle) => Some(handle),
        Err(message) => {
            eprintln!("warning: {message}");
            None
        }
    }
}

/// Read an image file, blur and darken it, and hand it to iced as raw RGBA.
fn blurred(path: &Path) -> Result<image_widget::Handle, String> {
    let decoded = image::ImageReader::open(path)
        .map_err(|err| format!("cannot open wallpaper {}: {err}", path.display()))?
        .with_guessed_format()
        .map_err(|err| format!("cannot read wallpaper {}: {err}", path.display()))?
        .decode()
        .map_err(|err| format!("wallpaper is corrupted: {}: {err}", path.display()))?;

    // A fast thumbnail is plenty: the result is blurred beyond recognition.
    let small = if decoded.width() > BLUR_MAX_SIDE || decoded.height() > BLUR_MAX_SIDE {
        decoded.thumbnail(BLUR_MAX_SIDE, BLUR_MAX_SIDE)
    } else {
        decoded
    };

    let rgba = small.into_rgba8();
    let blurred = image::imageops::fast_blur(&rgba, BLUR_SIGMA);
    let darkened = image::imageops::brighten(&blurred, -((DARKEN * 255.0).round() as i32));

    let (width, height) = darkened.dimensions();
    Ok(image_widget::Handle::from_rgba(
        width,
        height,
        darkened.into_raw(),
    ))
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

    daemon(move || boot(theme), "amber-locker", update, view)
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
            state.settling = state.settling.saturating_sub(1);
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
        Message::Background(handle) => {
            if handle.is_some() {
                state.settling = SETTLE_TICKS;
            }
            state.background = handle;
            Task::none()
        }
        // Layer-shell messages are handled by the runtime.
        _ => Task::none(),
    }
}

fn subscription(state: &Locker) -> iced::Subscription<Message> {
    let mut subscriptions = vec![
        iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick),
        iced::event::listen_with(on_event),
    ];

    // Keep producing frames until iced's wallpaper upload has landed, so the
    // backdrop shows up promptly even if its redraw request was dropped.
    if state.settling > 0 {
        subscriptions.push(iced::time::every(Duration::from_millis(200)).map(|_| Message::Tick));
    }

    iced::Subscription::batch(subscriptions)
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

    let content = container(
        column![
            time,
            date,
            Space::new().height(Length::Fixed(28.0)),
            password,
            Space::new().height(Length::Fixed(10.0)),
            prompt,
        ]
        .align_x(Alignment::Center)
        .width(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill);

    let background: Element<'_, Message> = match &state.background {
        Some(handle) => image_widget(handle.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(state.fit)
            .into(),
        None => Space::new().width(Length::Fill).height(Length::Fill).into(),
    };

    container(
        Stack::new()
            .push(background)
            .push(content)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
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
            radius: 0.0.into(),
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
