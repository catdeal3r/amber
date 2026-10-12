//! System tray, as a freedesktop StatusNotifierItem host.
//!
//! niri has no built-in tray. Programs that want a tray icon instead register
//! themselves over D-Bus with whichever process owns the well-known name
//! `org.kde.StatusNotifierWatcher`, then answer property reads describing
//! their icon and title. This module owns that name and republishes each
//! item's icon as a plain snapshot the UI can read without touching D-Bus.
//!
//! The watcher runs on its own thread using zbus's blocking API, so the iced
//! event loop never blocks on a bus round-trip. Every second it re-reads the
//! registered items into a shared [`TrayItem`] list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::blocking::connection;
use zbus::names::BusName;
use zbus::object_server::SignalEmitter;

/// How often the background thread refreshes the snapshot.
const POLL: Duration = Duration::from_secs(1);

/// Cap on how many directory entries an icon lookup will visit before giving
/// up. Icon themes can be large; a stray huge directory shouldn't stall the
/// tray thread.
const ICON_WALK_LIMIT: usize = 100_000;

/// Accessibility state of a tray item, mirroring the SNI `Status` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrayStatus {
    /// The item is functioning normally.
    #[default]
    Active,
    /// The item is inactive (e.g. hidden); the host may still show it.
    Passive,
    /// The item is drawing attention to itself.
    NeedsAttention,
}

/// A decoded RGBA8 icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Everything the bar needs to draw one tray item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayItem {
    /// The item's own `Id`, used as a stable key (falls back to the bus name).
    pub id: String,
    /// Human-readable title, if the item provides one.
    pub title: Option<String>,
    /// Attention state.
    pub status: TrayStatus,
    /// Largest icon pixmap the item offers, or a themed icon resolved from
    /// `IconName`. `None` when neither could be decoded.
    pub icon: Option<Icon>,
}

/// A handle to the running tray watcher.
///
/// Cloning shares the same snapshot. Dropping the last handle does not stop
/// the background thread (it lives for the process), but the snapshot is only
/// as fresh as the last poll.
#[derive(Clone)]
pub struct Tray {
    items: Arc<Mutex<Vec<TrayItem>>>,
}

impl Tray {
    /// Start the watcher on a background thread and return a handle to it.
    ///
    /// On a machine with no session bus this returns an empty handle and logs
    /// the reason to stderr; it never panics or blocks startup.
    pub fn spawn() -> Self {
        let items = Arc::new(Mutex::new(Vec::new()));
        let shared = items.clone();

        let spawned = std::thread::Builder::new()
            .name("amber-tray".into())
            .spawn(move || {
                if let Err(err) = run(shared) {
                    eprintln!("amber: tray watcher stopped: {err}");
                }
            });

        if let Err(err) = spawned {
            eprintln!("amber: could not start tray watcher: {err}");
        }

        Self { items }
    }

    /// A copy of the current snapshot.
    pub fn items(&self) -> Vec<TrayItem> {
        self.items.lock().expect("tray snapshot poisoned").clone()
    }
}

/// A registered item: where to find it and how the host refers to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Registration {
    /// Bus name (unique or well-known) the item is reachable at.
    bus: String,
    /// Object path of the item.
    path: String,
    /// Canonical identifier exposed in `RegisteredStatusNotifierItems`.
    ident: String,
}

/// The `org.kde.StatusNotifierWatcher` interface we export.
struct Watcher {
    items: Arc<Mutex<Vec<Registration>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// Called by an item to announce itself.
    ///
    /// `service` is either the item's bus name (the object then lives at the
    /// conventional `/StatusNotifierItem` path) or an object path (combined
    /// with the caller's unique name).
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header.sender().map(ToString::to_string);
        let Some(registration) = resolve_registration(&service, sender.as_deref()) else {
            return Ok(());
        };

        let added = {
            let mut items = self.items.lock().expect("tray registrations poisoned");
            if items.iter().any(|item| item.ident == registration.ident) {
                false
            } else {
                items.push(registration.clone());
                true
            }
        };

        if added {
            emitter
                .status_notifier_item_registered(&registration.ident)
                .await?;
        }

        Ok(())
    }

    /// Called by a second tray host (we are the only one, but items probe for
    /// it). We answer truthfully.
    async fn register_status_notifier_host(
        &self,
        _service: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        emitter.status_notifier_host_registered().await?;
        Ok(())
    }

    /// The identifiers of every registered item.
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items
            .lock()
            .expect("tray registrations poisoned")
            .iter()
            .map(|item| item.ident.clone())
            .collect()
    }

    /// We are the host, so this is always true. Items gate registration on it.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    /// The SNI protocol version.
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The subset of `org.kde.StatusNotifierItem` we read.
#[zbus::proxy(
    interface = "org.kde.StatusNotifierItem",
    gen_async = false,
    gen_blocking = true,
    assume_defaults = true
)]
trait StatusNotifierItem {
    #[zbus(property(emits_changed_signal = "false"))]
    fn id(&self) -> zbus::Result<String>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn title(&self) -> zbus::Result<String>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn status(&self) -> zbus::Result<String>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn icon_name(&self) -> zbus::Result<String>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn icon_pixmap(&self) -> zbus::Result<Vec<(i32, i32, Vec<u8>)>>;
}

/// Own the watcher name, then refresh the snapshot forever.
fn run(shared: Arc<Mutex<Vec<TrayItem>>>) -> zbus::Result<()> {
    let registrations = Arc::new(Mutex::new(Vec::new()));
    let watcher = Watcher {
        items: registrations.clone(),
    };

    let connection = connection::Builder::session()?
        .name("org.kde.StatusNotifierWatcher")?
        .serve_at("/StatusNotifierWatcher", watcher)?
        .build()?;

    let dbus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    let mut cache = IconCache::default();

    loop {
        std::thread::sleep(POLL);

        let registered: Vec<Registration> = registrations
            .lock()
            .expect("tray registrations poisoned")
            .clone();

        let mut alive = Vec::with_capacity(registered.len());
        let mut snapshot = Vec::with_capacity(registered.len());

        for registration in &registered {
            let Ok(bus) = BusName::try_from(registration.bus.as_str()) else {
                continue;
            };
            if !matches!(dbus.name_has_owner(bus), Ok(true)) {
                continue;
            }

            alive.push(registration.clone());
            if let Some(item) = read_item(&connection, registration, &mut cache) {
                snapshot.push(item);
            }
        }

        registrations
            .lock()
            .expect("tray registrations poisoned")
            .retain(|registration| alive.iter().any(|a| a.ident == registration.ident));

        *shared.lock().expect("tray snapshot poisoned") = snapshot;
    }
}

/// Read one item's display properties into a snapshot entry.
fn read_item(
    connection: &Connection,
    registration: &Registration,
    cache: &mut IconCache,
) -> Option<TrayItem> {
    let proxy = StatusNotifierItemProxy::builder(connection)
        .destination(registration.bus.as_str())
        .ok()?
        .path(registration.path.as_str())
        .ok()?
        .build()
        .ok()?;

    let id = proxy
        .id()
        .ok()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| registration.ident.clone());
    let title = proxy.title().ok().filter(|title| !title.is_empty());
    let status = match proxy.status().as_deref() {
        Ok("Passive") => TrayStatus::Passive,
        Ok("NeedsAttention") => TrayStatus::NeedsAttention,
        _ => TrayStatus::Active,
    };

    Some(TrayItem {
        id,
        title,
        status,
        icon: icon_for(&proxy, cache),
    })
}

/// Prefer the item's own pixmap, falling back to its themed `IconName`.
fn icon_for(proxy: &StatusNotifierItemProxy<'_>, cache: &mut IconCache) -> Option<Icon> {
    if let Ok(pixmaps) = proxy.icon_pixmap()
        && let Some(icon) = largest_pixmap(&pixmaps)
    {
        return Some(icon);
    }

    let name = proxy.icon_name().ok()?;
    cache.load(name.trim())
}

/// The biggest valid pixmap in an `a(iiay)` list, as RGBA.
fn largest_pixmap(pixmaps: &[(i32, i32, Vec<u8>)]) -> Option<Icon> {
    let (width, height, data) = pixmaps
        .iter()
        .filter(|(width, height, data)| {
            *width > 0 && *height > 0 && data.len() == (*width as usize) * (*height as usize) * 4
        })
        .max_by_key(|(width, height, _)| width.saturating_mul(*height))?;

    Some(Icon {
        width: *width as u32,
        height: *height as u32,
        pixels: argb_to_rgba(data),
    })
}

/// SNI pixmaps are ARGB32 (network byte order); iced wants RGBA8.
fn argb_to_rgba(data: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(data.len());
    for pixel in data.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[1], pixel[2], pixel[3], pixel[0]]);
    }
    rgba
}

/// Caches icon-name lookups and their decoded pixels.
#[derive(Default)]
struct IconCache {
    /// Resolved file for each icon name, cached even when absent.
    paths: HashMap<String, Option<PathBuf>>,
    /// Decoded pixels for each resolved file.
    icons: HashMap<PathBuf, Option<Icon>>,
}

impl IconCache {
    fn load(&mut self, name: &str) -> Option<Icon> {
        if name.is_empty() {
            return None;
        }

        let path = self
            .paths
            .entry(name.to_string())
            .or_insert_with(|| resolve_icon_path(name))
            .clone()?;

        self.icons
            .entry(path.clone())
            .or_insert_with(|| decode_icon(&path))
            .clone()
    }
}

/// Load a PNG/JPEG icon file into RGBA. SVG and other vector formats are not
/// supported; the caller falls back to a placeholder.
fn decode_icon(path: &Path) -> Option<Icon> {
    let image = image::open(path).ok()?.into_rgba8();
    let (width, height) = image.dimensions();

    Some(Icon {
        width,
        height,
        pixels: image.into_raw(),
    })
}

/// Find a themed icon by name, or accept an absolute path directly.
fn resolve_icon_path(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        let path = PathBuf::from(name);
        return path.is_file().then_some(path);
    }

    // A themed name is a single component; anything else is malformed.
    if name.is_empty() || name.contains('/') {
        return None;
    }

    for root in icon_roots() {
        if let Some(path) = find_icon(&root, name) {
            return Some(path);
        }
    }

    None
}

/// The icon directories to search, per the XDG icon-theme spec.
fn icon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".local/share/icons"));
        roots.push(home.join(".icons"));
    }

    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for dir in data_dirs.split(':').filter(|dir| !dir.is_empty()) {
        let dir = Path::new(dir);
        roots.push(dir.join("icons"));
        let pixmaps = dir.join("pixmaps");
        if pixmaps.is_dir() {
            roots.push(pixmaps);
        }
    }

    roots.retain(|root| root.is_dir());
    roots
}

/// Walk a directory tree for `<name>.png` (or jpeg), preferring the largest
/// size variant. Bounded by [`ICON_WALK_LIMIT`] entries.
fn find_icon(root: &Path, name: &str) -> Option<PathBuf> {
    let mut best: Option<(u32, PathBuf)> = None;
    let mut visited = 0usize;
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            visited += 1;
            if visited > ICON_WALK_LIMIT {
                return best.map(|(_, path)| path);
            }

            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            if file_type.is_dir() {
                stack.push(path);
                continue;
            }

            if !matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("png" | "jpg" | "jpeg")
            ) {
                continue;
            }
            if path.file_stem().and_then(|stem| stem.to_str()) != Some(name) {
                continue;
            }

            let rank = size_rank(&path);
            if best.as_ref().is_none_or(|(best_rank, _)| rank > *best_rank) {
                best = Some((rank, path));
            }
        }
    }

    best.map(|(_, path)| path)
}

/// Rank a path by the first numeric directory component (its nominal size),
/// so a 48px asset beats a 16px one.
fn size_rank(path: &Path) -> u32 {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .find_map(|component| {
            let digits: String = component
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect();
            digits.parse().ok()
        })
        .unwrap_or(0)
}

/// Split a `RegisterStatusNotifierItem` argument into a bus name and path.
fn resolve_registration(service: &str, sender: Option<&str>) -> Option<Registration> {
    if service.starts_with('/') {
        let bus = sender?.to_string();
        let ident = format!("{bus}{service}");
        Some(Registration {
            bus,
            path: service.to_string(),
            ident,
        })
    } else if service.is_empty() {
        None
    } else {
        Some(Registration {
            bus: service.to_string(),
            path: "/StatusNotifierItem".to_string(),
            ident: service.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_argb_pixmap_to_rgba() {
        // One opaque red pixel: A=255 R=255 G=0 B=0.
        assert_eq!(argb_to_rgba(&[255, 255, 0, 0]), [255, 0, 0, 255]);
    }

    #[test]
    fn picks_the_largest_pixmap() {
        let small = (16, 16, vec![0u8; 16 * 16 * 4]);
        let large = (32, 32, vec![0u8; 32 * 32 * 4]);
        let icon = largest_pixmap(&[small, large]).unwrap();
        assert_eq!((icon.width, icon.height), (32, 32));
    }

    #[test]
    fn ignores_pixmaps_with_a_bad_length() {
        let bad = (4, 4, vec![0u8; 3]);
        assert!(largest_pixmap(&[bad]).is_none());
    }

    #[test]
    fn resolves_bus_name_registration_to_conventional_path() {
        let registration = resolve_registration(":1.42", Some(":1.42")).unwrap();
        assert_eq!(registration.bus, ":1.42");
        assert_eq!(registration.path, "/StatusNotifierItem");
        assert_eq!(registration.ident, ":1.42");
    }

    #[test]
    fn resolves_path_registration_against_the_sender() {
        let registration = resolve_registration("/MenuBar", Some(":1.7")).unwrap();
        assert_eq!(registration.bus, ":1.7");
        assert_eq!(registration.path, "/MenuBar");
        assert_eq!(registration.ident, ":1.7/MenuBar");
    }

    #[test]
    fn rejects_empty_registration() {
        assert!(resolve_registration("", Some(":1.1")).is_none());
    }

    #[test]
    fn accepts_an_absolute_icon_path() {
        let path = std::env::temp_dir().join("amber-tray-absolute-icon.png");
        std::fs::write(&path, b"x").unwrap();

        assert_eq!(
            resolve_icon_path(path.to_str().unwrap()),
            Some(path.clone())
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resolves_the_largest_themed_icon() {
        let root = std::env::temp_dir().join("amber-tray-themed-icon-test");
        let _ = std::fs::remove_dir_all(&root);

        for (dir, file) in [
            ("16x16/apps", "findme.png"),
            ("48x48/apps", "findme.png"),
            ("scalable/apps", "findme.svg"),
        ] {
            let dir = root.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file), b"not-a-real-image").unwrap();
        }

        let found = find_icon(&root, "findme").expect("a matching png");
        assert!(found.to_string_lossy().contains("48x48"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ranks_icons_by_size_directory() {
        assert!(
            size_rank(Path::new("/icons/48x48/apps/a.png"))
                > size_rank(Path::new("/icons/16x16/apps/a.png"))
        );
        assert_eq!(size_rank(Path::new("/icons/scalable/a.svg")), 0);
    }
}
