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
use std::time::{Duration, Instant};

use iced::animation::Easing;
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

/// How long the lock content takes to slide down (and back up) the screen.
const SLIDE_DURATION: Duration = Duration::from_millis(450);

/// A beat after the surface appears before the slide begins, so the surface is
/// on screen before the motion starts and none of it is spent waiting for the
/// first frame.
const APPEAR_DELAY: Duration = Duration::from_millis(120);

/// How long the blurred wallpaper takes to fade in once it has loaded.
const FADE_DURATION: Duration = Duration::from_millis(320);

/// The animation frame interval while something is moving.
const FRAME: Duration = Duration::from_millis(16);

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
    /// Advance the clock and the animations to this instant.
    Tick(Instant),
    /// The surface is up; begin the slide-down.
    Appear,
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
    /// Whether the slide-down has been kicked off yet.
    appearing: bool,
    /// `false` = slid up off the top (hidden), `true` = at rest (shown).
    progress: iced::Animation<bool>,
    /// `false` = the backdrop is invisible, `true` = fully faded in.
    backdrop: iced::Animation<bool>,
    /// The instant of the most recent frame, used to project the animations.
    now: Instant,
    /// Set once authenticated; the slide-up plays before the process exits.
    closing: bool,
}

impl Locker {
    fn new(theme: Theme) -> Self {
        let now = chrono::Local::now();

        Locker {
            theme,
            background: None,
            fit: config::wallpaper().fit,
            time: now.format("%I:%M %p").to_string(),
            date: now.format("%A, %-d %B").to_string(),
            password: String::new(),
            error: None,
            authenticating: false,
            user: current_user(),
            pam_service: config::lock().pam_service,
            settling: 0,
            appearing: false,
            progress: iced::Animation::new(false)
                .duration(SLIDE_DURATION)
                .easing(Easing::EaseOutCubic),
            backdrop: iced::Animation::new(false)
                .duration(FADE_DURATION)
                .easing(Easing::EaseOutCubic),
            now: Instant::now(),
            closing: false,
        }
    }

    /// Kicks off the slide-down, once.
    fn begin_appear(&mut self) {
        if !self.appearing {
            self.appearing = true;
            self.now = Instant::now();
            self.progress.go_mut(true, self.now);
        }
    }

    /// Starts the slide-up, once. The process exits on the first frame after it
    /// settles.
    fn begin_close(&mut self) {
        if !self.closing {
            self.now = Instant::now();
            self.closing = true;
            self.password.clear();
            self.progress.go_mut(false, self.now);
        }
    }

    /// Reveals the backdrop, fading it in from the solid background.
    fn show_backdrop(&mut self) {
        self.now = Instant::now();
        self.backdrop.go_mut(true, self.now);
    }

    fn refresh(&mut self) {
        let now = chrono::Local::now();
        self.time = now.format("%I:%M %p").to_string();
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

    // Give the surface a moment to come up, then start the slide-down, so the
    // whole motion is visible rather than partly spent before the first frame.
    let appear = Task::perform(
        async {
            tokio::time::sleep(APPEAR_DELAY).await;
        },
        |()| Message::Appear,
    );

    (state, Task::batch([initial_focus(), appear, backdrop]))
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
        Message::Tick(now) => {
            state.now = now;
            state.refresh();
            state.settling = state.settling.saturating_sub(1);
            // Fallback in case the `Appear` task was ever missed.
            state.begin_appear();

            // Hold still until the slide and the backdrop fade have settled.
            if state.progress.is_animating(now) || state.backdrop.is_animating(now) {
                return Task::none();
            }
            if state.closing {
                return iced::exit();
            }
            if state.password.is_empty() && !state.authenticating && state.error.is_none() {
                focus()
            } else {
                Task::none()
            }
        }
        Message::Focus => focus(),
        Message::Appear => {
            state.begin_appear();
            focus()
        }
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
        Message::Authenticated(Ok(())) => {
            state.authenticating = false;
            state.begin_close();
            Task::none()
        }
        Message::Authenticated(Err(error)) => {
            state.authenticating = false;
            state.password.clear();
            state.error = Some(error);
            focus()
        }
        Message::Background(handle) => {
            if handle.is_some() {
                state.settling = SETTLE_TICKS;
                state.show_backdrop();
            }
            state.background = handle;
            Task::none()
        }
        // Layer-shell messages are handled by the runtime.
        _ => Task::none(),
    }
}

fn subscription(state: &Locker) -> iced::Subscription<Message> {
    let animating = state.closing
        || state.progress.is_animating(state.now)
        || state.backdrop.is_animating(state.now);

    let mut subscriptions = vec![
        iced::event::listen_with(on_event),
        iced::time::every(Duration::from_secs(1)).map(Message::Tick),
    ];

    if animating {
        subscriptions.push(iced::time::every(FRAME).map(Message::Tick));
    } else if state.settling > 0 {
        // Keep producing frames until iced's wallpaper upload has landed, so the
        // backdrop shows up promptly even if its redraw request was dropped.
        subscriptions.push(iced::time::every(Duration::from_millis(200)).map(Message::Tick));
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
    // Slide progress (0 = hidden at the top, 1 = at rest) and backdrop fade.
    let t: f32 = state.progress.interpolate(0.0, 1.0, state.now);
    let fade: f32 = state.backdrop.interpolate(0.0, 1.0, state.now);
    // The content becomes fully visible early in the slide (rather than exactly
    // tracking its position) so the motion is obvious from the very top.
    let vis: f32 = (t * 2.0).min(1.0);

    let time = text(&state.time)
        .size(56.0)
        .font(fonts.text)
        .color(palette.text.scale_alpha(vis));

    let date = text(&state.date)
        .size(16.0)
        .font(fonts.text)
        .color(palette.muted.scale_alpha(vis));

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
            password_style(palette, invalid, vis)
        });

    let prompt: Element<'_, Message> = match &state.error {
        Some(error) => text(error)
            .size(13.0)
            .font(fonts.text)
            .color(palette.red.scale_alpha(vis))
            .into(),
        None => Space::new().height(Length::Fixed(16.0)).into(),
    };

    let content = column![
        time,
        date,
        Space::new().height(Length::Fixed(28.0)),
        password,
        Space::new().height(Length::Fixed(10.0)),
        prompt,
    ]
    .align_x(Alignment::Center);

    let background: Element<'_, Message> = match &state.background {
        Some(handle) => image_widget(handle.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(state.fit)
            .opacity(fade)
            .into(),
        None => Space::new().width(Length::Fill).height(Length::Fill).into(),
    };

    // Slide down from the top edge on lock, and back up on unlock: with an
    // empty top spacer the content hugs the top when hidden, and at rest the
    // spacers are equal so it sits centred. The content is always in the tree
    // — even fully transparent — so the field can be focused from the first
    // frame.
    let top = 1 + (t * 1000.0) as u16;
    let bottom = 1001;
    let slider = column![
        Space::new().height(Length::FillPortion(top)),
        content,
        Space::new().height(Length::FillPortion(bottom)),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center);

    container(
        Stack::new()
            .push(background)
            .push(slider)
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

fn password_style(palette: Palette, invalid: bool, alpha: f32) -> text_input::Style {
    let border = (if invalid { palette.red } else { palette.muted }).scale_alpha(alpha);
    text_input::Style {
        background: palette.field.scale_alpha(alpha).into(),
        border: Border {
            color: border,
            width: 1.0,
            radius: 0.0.into(),
        },
        icon: palette.muted.scale_alpha(alpha),
        placeholder: palette.muted.scale_alpha(alpha),
        value: palette.text.scale_alpha(alpha),
        selection: palette.accent.scale_alpha(alpha),
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
