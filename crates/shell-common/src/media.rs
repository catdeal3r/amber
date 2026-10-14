//! Now-playing media, from MPRIS2 over D-Bus.
//!
//! Media players advertise themselves with a bus name under the well-known
//! prefix `org.mpris.MediaPlayer2.`; the interface we read is
//! `org.mpris.MediaPlayer2.Player`, always at `/org/mpris/MediaPlayer2`. This
//! module polls the session bus for whichever player is most interesting — the
//! one that is playing, else the first with a track loaded — and republishes its
//! state as a plain snapshot the UI can read without touching D-Bus.
//!
//! Like [`crate::tray`], the reader runs on its own thread with zbus's blocking
//! API so the iced event loop never blocks on a bus round-trip.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{ObjectPath, OwnedValue};

/// How often the background thread refreshes the snapshot.
const POLL: Duration = Duration::from_millis(500);

/// Prefix every player's bus name starts with, e.g.
/// `org.mpris.MediaPlayer2.firefox`.
const BUS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// The fixed object path every MPRIS player exposes.
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";

/// What the active player is doing, mirroring MPRIS's `PlaybackStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Playback {
    /// No track is loaded, or playback has finished.
    #[default]
    Stopped,
    /// A track is playing.
    Playing,
    /// A track is loaded but paused.
    Paused,
    /// A status we don't recognise.
    Unknown,
}

impl Playback {
    fn parse(text: &str) -> Self {
        match text {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            "Stopped" => Self::Stopped,
            _ => Self::Unknown,
        }
    }
}

/// The loop mode the active player reports (`LoopStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStatus {
    /// Do not repeat.
    None,
    /// Repeat the current track.
    Track,
    /// Repeat the whole queue.
    Playlist,
}

impl LoopStatus {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "None" => Some(Self::None),
            "Track" => Some(Self::Track),
            "Playlist" => Some(Self::Playlist),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Track => "Track",
            Self::Playlist => "Playlist",
        }
    }

    /// The next mode when cycling `None -> Playlist -> Track -> None`.
    fn next(self) -> Self {
        match self {
            Self::None => Self::Playlist,
            Self::Playlist => Self::Track,
            Self::Track => Self::None,
        }
    }
}

/// A playback command to hand to the active player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Toggle between playing and paused.
    PlayPause,
    /// Skip to the next track.
    Next,
    /// Go back to the previous track.
    Previous,
    /// Turn shuffle on or off.
    SetShuffle(bool),
    /// Advance the loop mode (`None -> Playlist -> Track -> None`).
    CycleLoop,
    /// Move the playhead to an absolute position.
    Seek(Duration),
}

/// A snapshot of the active media player.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaState {
    /// Whether any MPRIS player was reachable.
    pub has_player: bool,
    /// What the active player is doing.
    pub playback: Playback,
    /// Track title (`xesam:title`).
    pub title: Option<String>,
    /// Track artist(s) (`xesam:artist`), joined with `", "`.
    pub artist: Option<String>,
    /// Cover art URL (`mpris:artUrl`), if the player provides one.
    pub art_url: Option<String>,
    /// Shuffle state, or `None` when the player does not expose it.
    pub shuffle: Option<bool>,
    /// Loop mode, or `None` when the player does not expose it.
    pub loop_status: Option<LoopStatus>,
    /// Playhead position, or `None` when the player does not expose it.
    pub position: Option<Duration>,
    /// Track length, or `None` when the metadata does not carry it.
    pub length: Option<Duration>,
    /// Whether the player supports seeking, or `None` when it does not say.
    pub can_seek: Option<bool>,
    /// Whether the player can skip forward.
    pub can_go_next: bool,
    /// Whether the player can skip back.
    pub can_go_previous: bool,
}

impl MediaState {
    /// Whether a track is loaded and worth showing to the user.
    pub fn visible(&self) -> bool {
        self.title.is_some() && self.playback != Playback::Stopped
    }
}

/// A handle to the running media watcher.
///
/// Cloning shares the same snapshot. Dropping every handle stops the background
/// thread.
#[derive(Clone)]
pub struct Media {
    state: Arc<Mutex<MediaState>>,
    commands: Sender<Command>,
}

impl Media {
    /// Start the watcher on a background thread and return a handle to it.
    ///
    /// On a machine with no session bus this returns an empty handle and logs
    /// the reason to stderr; it never panics or blocks startup.
    pub fn spawn() -> Self {
        let state = Arc::new(Mutex::new(MediaState::default()));
        let shared = state.clone();
        let (commands, receiver) = std::sync::mpsc::channel();

        let spawned = std::thread::Builder::new()
            .name("amber-media".into())
            .spawn(move || {
                if let Err(err) = run(shared, receiver) {
                    eprintln!("amber: media watcher stopped: {err}");
                }
            });

        if let Err(err) = spawned {
            eprintln!("amber: could not start media watcher: {err}");
        }

        Self { state, commands }
    }

    /// A copy of the current snapshot.
    pub fn state(&self) -> MediaState {
        self.state.lock().expect("media snapshot poisoned").clone()
    }

    /// Hand a playback command to the active player.
    ///
    /// The watcher applies it on its next cycle and refreshes the snapshot with
    /// the result; it is a no-op when no player is reachable.
    pub fn command(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

/// The subset of `org.mpris.MediaPlayer2.Player` we read and drive.
#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    gen_async = false,
    gen_blocking = true,
    assume_defaults = true
)]
trait MprisPlayer {
    #[zbus(property(emits_changed_signal = "false"))]
    fn playback_status(&self) -> zbus::Result<String>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn shuffle(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn set_shuffle(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn loop_status(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn set_loop_status(&self, value: String) -> zbus::Result<()>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn can_go_next(&self) -> zbus::Result<bool>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn can_go_previous(&self) -> zbus::Result<bool>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> zbus::Result<i64>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn can_seek(&self) -> zbus::Result<bool>;

    fn play_pause(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;
    fn seek(&self, offset: i64) -> zbus::Result<()>;
    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) -> zbus::Result<()>;
}

/// Refresh the snapshot forever, applying queued commands as they arrive.
fn run(shared: Arc<Mutex<MediaState>>, commands: Receiver<Command>) -> zbus::Result<()> {
    let connection = zbus::blocking::Connection::session()?;
    let dbus = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;

    loop {
        let (state, active) = read_active(&connection, &dbus);
        *shared.lock().expect("media snapshot poisoned") = state;

        // Wake on a command (applying it and re-reading at once) or after the
        // poll interval, whichever comes first.
        match commands.recv_timeout(POLL) {
            Ok(command) => apply(&connection, active.as_deref(), command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

/// Build a proxy for one player's bus name.
fn player_proxy<'a>(
    connection: &'a zbus::blocking::Connection,
    name: &str,
) -> Option<MprisPlayerProxy<'a>> {
    MprisPlayerProxy::builder(connection)
        .destination(name.to_string())
        .ok()?
        .path(PLAYER_PATH)
        .ok()?
        .build()
        .ok()
}

/// Apply a playback command to the active player, if there is one.
fn apply(connection: &zbus::blocking::Connection, active: Option<&str>, command: Command) {
    let Some(name) = active else {
        return;
    };
    let Some(proxy) = player_proxy(connection, name) else {
        return;
    };

    match command {
        Command::PlayPause => {
            let _ = proxy.play_pause();
        }
        Command::Next => {
            let _ = proxy.next();
        }
        Command::Previous => {
            let _ = proxy.previous();
        }
        Command::SetShuffle(on) => {
            let _ = proxy.set_shuffle(on);
        }
        Command::CycleLoop => {
            if let Ok(current) = proxy.loop_status()
                && let Some(mode) = LoopStatus::parse(&current)
            {
                let _ = proxy.set_loop_status(mode.next().as_str().to_string());
            }
        }
        Command::Seek(target) => {
            // Prefer `SetPosition`, which seeks to an exact position. Some
            // players (Chromium) clamp the relative `Seek` offset to a few
            // seconds, so it can't reach an arbitrary target. Fall back to a
            // relative seek only when the track has no id to address.
            let micros = target.as_micros() as i64;
            match proxy.metadata().ok().as_ref().and_then(prop_track_id) {
                Some(track_id) => {
                    let _ = proxy.set_position(track_id, micros);
                }
                None => {
                    if let Ok(current) = proxy.position() {
                        let _ = proxy.seek(micros - current);
                    }
                }
            }
        }
    }
}

/// Pick the most interesting player: the one that is playing, else the first
/// that has a track loaded, else an empty snapshot. Returns the winning bus
/// name alongside the snapshot so commands can target the same player.
fn read_active(
    connection: &zbus::blocking::Connection,
    dbus: &zbus::blocking::Proxy<'_>,
) -> (MediaState, Option<String>) {
    let names: Vec<String> = match dbus.call("ListNames", &()) {
        Ok(names) => names,
        Err(_) => return (MediaState::default(), None),
    };

    let mut fallback: Option<(MediaState, String)> = None;
    for name in names {
        if !name.starts_with(BUS_PREFIX) {
            continue;
        }
        let Some(state) = read_player(connection, &name) else {
            continue;
        };
        if state.playback == Playback::Playing {
            return (state, Some(name));
        }
        if fallback.is_none() {
            fallback = Some((state, name));
        }
    }

    match fallback {
        Some((state, name)) => (state, Some(name)),
        None => (MediaState::default(), None),
    }
}

/// Read one player's properties into a snapshot entry.
fn read_player(connection: &zbus::blocking::Connection, name: &str) -> Option<MediaState> {
    let proxy = player_proxy(connection, name)?;

    let playback = Playback::parse(&proxy.playback_status().ok()?);
    let metadata = proxy.metadata().ok();

    Some(MediaState {
        has_player: true,
        playback,
        title: metadata
            .as_ref()
            .and_then(|meta| prop_string(meta, "xesam:title")),
        artist: metadata
            .as_ref()
            .and_then(|meta| prop_artist(meta, "xesam:artist")),
        art_url: metadata
            .as_ref()
            .and_then(|meta| prop_string(meta, "mpris:artUrl")),
        shuffle: proxy.shuffle().ok(),
        loop_status: proxy.loop_status().ok().and_then(|s| LoopStatus::parse(&s)),
        position: proxy.position().ok().and_then(micros),
        length: metadata
            .as_ref()
            .and_then(|meta| prop_i64(meta, "mpris:length"))
            .and_then(micros),
        can_seek: proxy.can_seek().ok(),
        can_go_next: proxy.can_go_next().unwrap_or(false),
        can_go_previous: proxy.can_go_previous().unwrap_or(false),
    })
}

/// Turn a non-negative MPRIS microsecond count into a [`Duration`].
fn micros(value: i64) -> Option<Duration> {
    (value >= 0).then(|| Duration::from_micros(value as u64))
}

/// Read a string metadata value, treating an empty string as absent.
fn prop_string(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = metadata.get(key)?;
    let text: &str = value.try_into().ok()?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Read the `xesam:artist` array and join it with `", "`.
fn prop_artist(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = metadata.get(key)?.try_clone().ok()?;
    let artists: Vec<String> = value.try_into().ok()?;
    if artists.is_empty() {
        None
    } else {
        Some(artists.join(", "))
    }
}

/// Read an integer metadata value (MPRIS uses these for microsecond lengths).
fn prop_i64(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    metadata.get(key)?.try_into().ok()
}

/// Read the `mpris:trackid` object path, which `SetPosition` needs.
fn prop_track_id(metadata: &HashMap<String, OwnedValue>) -> Option<ObjectPath<'_>> {
    metadata.get("mpris:trackid")?.downcast_ref().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn plays_status_round_trips() {
        assert_eq!(Playback::parse("Playing"), Playback::Playing);
        assert_eq!(Playback::parse("Paused"), Playback::Paused);
        assert_eq!(Playback::parse("Stopped"), Playback::Stopped);
        assert_eq!(Playback::parse("nonsense"), Playback::Unknown);
    }

    #[test]
    fn loop_status_parses_and_cycles() {
        assert_eq!(LoopStatus::parse("None"), Some(LoopStatus::None));
        assert_eq!(LoopStatus::parse("Track"), Some(LoopStatus::Track));
        assert_eq!(LoopStatus::parse("Playlist"), Some(LoopStatus::Playlist));
        assert_eq!(LoopStatus::parse("Loop"), None);

        assert_eq!(LoopStatus::None.next(), LoopStatus::Playlist);
        assert_eq!(LoopStatus::Playlist.next(), LoopStatus::Track);
        assert_eq!(LoopStatus::Track.next(), LoopStatus::None);
    }

    #[test]
    fn loop_status_round_trips_through_str() {
        for mode in [LoopStatus::None, LoopStatus::Track, LoopStatus::Playlist] {
            assert_eq!(LoopStatus::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn visible_needs_a_title_and_something_but_stopped() {
        let mut state = MediaState {
            title: Some("Song".into()),
            playback: Playback::Playing,
            ..MediaState::default()
        };
        assert!(state.visible());

        state.playback = Playback::Stopped;
        assert!(!state.visible());

        state.playback = Playback::Playing;
        state.title = None;
        assert!(!state.visible());
    }

    #[test]
    fn prop_string_reads_and_skips_empty() {
        let mut metadata = HashMap::new();
        metadata.insert("xesam:title".to_string(), value("Song"));
        metadata.insert("mpris:artUrl".to_string(), value(""));

        assert_eq!(
            prop_string(&metadata, "xesam:title").as_deref(),
            Some("Song")
        );
        assert_eq!(prop_string(&metadata, "mpris:artUrl"), None);
        assert_eq!(prop_string(&metadata, "missing"), None);
    }

    #[test]
    fn prop_artist_joins_the_array() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "xesam:artist".to_string(),
            OwnedValue::try_from(Value::from(vec!["A".to_string(), "B".to_string()])).unwrap(),
        );

        assert_eq!(
            prop_artist(&metadata, "xesam:artist").as_deref(),
            Some("A, B")
        );
    }

    fn value(text: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(text)).unwrap()
    }
}
