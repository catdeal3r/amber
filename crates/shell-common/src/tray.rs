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
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::blocking::connection;
use zbus::names::BusName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Structure, Value};

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
    /// Stable identity (bus name plus object path), used to route clicks.
    pub key: String,
    /// The item's own `Id`, used for display/grouping.
    pub id: String,
    /// Human-readable title, if the item provides one.
    pub title: Option<String>,
    /// Attention state.
    pub status: TrayStatus,
    /// Whether the item only offers a menu, so a left click should open it.
    pub is_menu: bool,
    /// Largest icon pixmap the item offers, or a themed icon resolved from
    /// `IconName`. `None` when neither could be decoded.
    pub icon: Option<Icon>,
}

/// Kind of toggle a menu item draws, mirroring DBusMenu's `toggle-type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Toggle {
    /// A plain item.
    #[default]
    None,
    /// A checkmark that stands alone.
    Checkbox,
    /// A radio button within a group.
    Radio,
}

/// One selectable row in a tray context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// The DBusMenu item id, echoed back when the row is clicked.
    pub id: i32,
    /// Display label, with mnemonics stripped.
    pub label: String,
    /// Whether the item can currently be activated.
    pub enabled: bool,
    /// Toggle style, if any.
    pub toggle: Toggle,
    /// Whether a toggle is currently on.
    pub checked: bool,
    /// Nesting level, used for indentation. The top level is `0`.
    pub depth: u8,
    /// Whether this row opens a submenu (its children follow it inline).
    pub has_submenu: bool,
}

/// A row of a context menu: an item or a horizontal rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A horizontal separator.
    Separator,
    /// A selectable item.
    Item(MenuItem),
}

/// A snapshot of one item's context menu, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenu {
    /// The item this menu belongs to.
    pub key: String,
    /// The DBusMenu revision the snapshot was read at.
    pub revision: u32,
    /// The flattened menu, depth-first, with submenu children inlined.
    pub entries: Vec<MenuEntry>,
}

/// A request from the UI to drive an item's mouse actions.
enum Command {
    Activate { key: String, x: i32, y: i32 },
    SecondaryActivate { key: String, x: i32, y: i32 },
    ContextMenu { key: String, x: i32, y: i32 },
    MenuEvent { key: String, id: i32 },
}

impl Command {
    /// The item this command targets.
    fn key(&self) -> &str {
        match self {
            Command::Activate { key, .. }
            | Command::SecondaryActivate { key, .. }
            | Command::ContextMenu { key, .. }
            | Command::MenuEvent { key, .. } => key,
        }
    }
}

/// A handle to the running tray watcher.
///
/// Cloning shares the same snapshot and command queue. Dropping every handle
/// stops the background thread, which releases the watcher's bus name.
#[derive(Clone)]
pub struct Tray {
    items: Arc<Mutex<Vec<TrayItem>>>,
    menus: Arc<Mutex<Option<TrayMenu>>>,
    commands: Sender<Command>,
}

impl Tray {
    /// Start the watcher on a background thread and return a handle to it.
    ///
    /// On a machine with no session bus this returns an empty handle and logs
    /// the reason to stderr; it never panics or blocks startup.
    pub fn spawn() -> Self {
        let items = Arc::new(Mutex::new(Vec::new()));
        let menus = Arc::new(Mutex::new(None));
        let (commands, receiver) = mpsc::channel();

        let shared = items.clone();
        let menu_slot = menus.clone();
        let spawned = std::thread::Builder::new()
            .name("amber-tray".into())
            .spawn(move || {
                if let Err(err) = run(shared, menu_slot, receiver) {
                    eprintln!("amber: tray watcher stopped: {err}");
                }
            });

        if let Err(err) = spawned {
            eprintln!("amber: could not start tray watcher: {err}");
        }

        Self {
            items,
            menus,
            commands,
        }
    }

    /// A copy of the current snapshot.
    pub fn items(&self) -> Vec<TrayItem> {
        self.items.lock().expect("tray snapshot poisoned").clone()
    }

    /// Take the most recently prepared context menu, if one is waiting.
    ///
    /// The UI calls this after requesting a menu; a successful take consumes
    /// the snapshot, since menus are frozen once shown.
    pub fn take_menu(&self) -> Option<TrayMenu> {
        self.menus.lock().expect("tray menu poisoned").take()
    }

    /// Ask an item to activate itself (a left click).
    pub fn activate(&self, key: &str) {
        self.send(Command::Activate {
            key: key.to_string(),
            x: 0,
            y: 0,
        });
    }

    /// Ask an item for a secondary activation (a middle click).
    pub fn secondary_activate(&self, key: &str) {
        self.send(Command::SecondaryActivate {
            key: key.to_string(),
            x: 0,
            y: 0,
        });
    }

    /// Ask an item to show its context menu (a right click).
    ///
    /// When the item publishes a DBusMenu it is snapshot into the shared slot
    /// for the UI to draw; otherwise the item's own `ContextMenu` is invoked.
    pub fn context_menu(&self, key: &str) {
        self.send(Command::ContextMenu {
            key: key.to_string(),
            x: 0,
            y: 0,
        });
    }

    /// Tell an item that one of its menu rows was clicked.
    pub fn menu_click(&self, key: &str, id: i32) {
        self.send(Command::MenuEvent {
            key: key.to_string(),
            id,
        });
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
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

    #[zbus(property(emits_changed_signal = "false"))]
    fn item_is_menu(&self) -> zbus::Result<bool>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn menu(&self) -> zbus::Result<OwnedObjectPath>;

    /// Primary activation (left click).
    fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;

    /// Secondary activation (middle click).
    fn secondary_activate(&self, x: i32, y: i32) -> zbus::Result<()>;

    /// Ask the item to show its context menu (right click).
    fn context_menu(&self, x: i32, y: i32) -> zbus::Result<()>;
}

/// One node of a `com.canonical.dbusmenu` layout: `(ia{sv}av)`.
///
/// Children stay as raw [`OwnedValue`]s and are decoded one level at a time by
/// [`parse_node`]. A self-referential `Type` can't be spelled out, and a
/// recursive `Deserialize` trips over the variant each child is wrapped in.
#[derive(Debug, serde::Deserialize)]
struct RawNode {
    id: i32,
    properties: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
}

// Borrow the signature of an equivalent non-recursive shape (`(ia{sv}av)`),
// since the derived version would recurse forever on `children`.
impl zbus::zvariant::Type for RawNode {
    const SIGNATURE: &'static zbus::zvariant::Signature =
        <(i32, HashMap<String, OwnedValue>, Vec<OwnedValue>) as zbus::zvariant::Type>::SIGNATURE;
}

/// The subset of `com.canonical.dbusmenu` we drive.
#[zbus::proxy(
    interface = "com.canonical.dbusmenu",
    gen_async = false,
    gen_blocking = true,
    assume_defaults = true
)]
trait DBusMenu {
    /// Read the whole menu tree in one call (`recursion_depth = -1`).
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[String],
    ) -> zbus::Result<(u32, RawNode)>;

    /// Let the item populate lazy properties before we read the layout.
    fn about_to_show(&self, id: i32) -> zbus::Result<bool>;

    /// Deliver an interaction (a `"clicked"`, `"opened"`, ... event).
    fn event(&self, id: i32, event_id: &str, data: OwnedValue, timestamp: u32) -> zbus::Result<()>;
}

/// Own the watcher name, then serve mouse commands and refresh the snapshot.
fn run(
    shared: Arc<Mutex<Vec<TrayItem>>>,
    menus: Arc<Mutex<Option<TrayMenu>>>,
    commands: Receiver<Command>,
) -> zbus::Result<()> {
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
        match commands.recv_timeout(POLL) {
            Ok(command) => handle_command(&connection, &registrations, &menus, command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }

        refresh(&connection, &dbus, &registrations, &shared, &mut cache);
    }
}

/// Re-read every registered item into the shared snapshot.
fn refresh(
    connection: &Connection,
    dbus: &zbus::blocking::fdo::DBusProxy<'_>,
    registrations: &Arc<Mutex<Vec<Registration>>>,
    shared: &Arc<Mutex<Vec<TrayItem>>>,
    cache: &mut IconCache,
) {
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
        if let Some(item) = read_item(connection, registration, cache) {
            snapshot.push(item);
        }
    }

    registrations
        .lock()
        .expect("tray registrations poisoned")
        .retain(|registration| alive.iter().any(|a| a.ident == registration.ident));

    *shared.lock().expect("tray snapshot poisoned") = snapshot;
}

/// Relay a UI mouse action to the target item.
fn handle_command(
    connection: &Connection,
    registrations: &Arc<Mutex<Vec<Registration>>>,
    menus: &Arc<Mutex<Option<TrayMenu>>>,
    command: Command,
) {
    let registration = registrations
        .lock()
        .expect("tray registrations poisoned")
        .iter()
        .find(|registration| registration.ident == command.key())
        .cloned();

    let Some(registration) = registration else {
        return;
    };
    let Some(proxy) = item_proxy(connection, &registration) else {
        return;
    };

    match command {
        Command::Activate { x, y, .. } => {
            let _ = proxy.activate(x, y);
        }
        Command::SecondaryActivate { x, y, .. } => {
            let _ = proxy.secondary_activate(x, y);
        }
        Command::ContextMenu { x, y, .. } => match menu_path(connection, &registration) {
            // Prefer rendering the item's own DBusMenu; only fall back to
            // asking the item to show a menu when it publishes none.
            Some(path) => match read_menu(connection, &registration, path.as_str()) {
                Some(menu) => {
                    *menus.lock().expect("tray menu poisoned") = Some(menu);
                }
                None => {
                    let _ = proxy.context_menu(x, y);
                }
            },
            None => {
                let _ = proxy.context_menu(x, y);
            }
        },
        Command::MenuEvent { id, .. } => {
            if let Some(path) = menu_path(connection, &registration)
                && let Some(menu) = menu_proxy(connection, &registration, path.as_str())
            {
                let _ = menu.event(id, "clicked", empty_variant(), 0);
            }
        }
    }
}

/// The object path of an item's DBusMenu, from the SNI `Menu` property.
///
/// A missing or root path means the item publishes no menu.
fn menu_path(connection: &Connection, registration: &Registration) -> Option<OwnedObjectPath> {
    let item = item_proxy(connection, registration)?;
    let path = item.menu().ok()?;
    let text = path.as_str();
    if text.is_empty() || text == "/" {
        return None;
    }

    Some(path)
}

/// Build a proxy for an item's DBusMenu at a known object path.
fn menu_proxy<'a>(
    connection: &Connection,
    registration: &'a Registration,
    path: &'a str,
) -> Option<DBusMenuProxy<'a>> {
    DBusMenuProxy::builder(connection)
        .destination(registration.bus.as_str())
        .ok()?
        .path(path)
        .ok()?
        .build()
        .ok()
}

/// Snapshot an item's context menu into a flattened [`TrayMenu`].
fn read_menu(connection: &Connection, registration: &Registration, path: &str) -> Option<TrayMenu> {
    let menu = menu_proxy(connection, registration, path)?;

    // Give lazy items a chance to fill in their labels before we read them.
    let _ = menu.about_to_show(0);

    let (revision, root) = menu.get_layout(0, -1, &[]).ok()?;
    Some(parse_menu(registration.ident.clone(), revision, root))
}

/// The variant DBusMenu events carry as `data`. Nothing we send needs it, so
/// an empty string is the smallest valid variant.
fn empty_variant() -> OwnedValue {
    OwnedValue::try_from(Value::from("")).expect("an owned string variant")
}

/// Flatten a DBusMenu tree into displayable rows.
fn parse_menu(key: String, revision: u32, root: RawNode) -> TrayMenu {
    let mut entries = Vec::new();
    for child in &root.children {
        if let Some(node) = parse_node(child) {
            walk(node, 0, &mut entries);
        }
    }

    TrayMenu {
        key,
        revision,
        entries,
    }
}

/// Decode one variant-wrapped `(ia{sv}av)` into a [`RawNode`].
fn parse_node(value: &OwnedValue) -> Option<RawNode> {
    let structure = Structure::try_from(value.try_clone().ok()?).ok()?;
    let mut fields = structure.fields().iter();

    let id = i32::try_from(OwnedValue::try_from(fields.next()?).ok()?).ok()?;
    let properties = HashMap::try_from(OwnedValue::try_from(fields.next()?).ok()?).ok()?;
    let children = Vec::<OwnedValue>::try_from(OwnedValue::try_from(fields.next()?).ok()?).ok()?;

    Some(RawNode {
        id,
        properties,
        children,
    })
}

/// Append one node and its descendants to the flattened menu.
fn walk(node: RawNode, depth: u8, out: &mut Vec<MenuEntry>) {
    if !prop_bool(&node.properties, "visible", true) {
        return;
    }

    if prop_string(&node.properties, "type").as_deref() == Some("separator") {
        out.push(MenuEntry::Separator);
        return;
    }

    let label = prop_string(&node.properties, "label")
        .map(|label| clean_label(&label))
        .unwrap_or_default();
    let toggle = match prop_string(&node.properties, "toggle-type").as_deref() {
        Some("checkmark") => Toggle::Checkbox,
        Some("radio") => Toggle::Radio,
        _ => Toggle::None,
    };

    out.push(MenuEntry::Item(MenuItem {
        id: node.id,
        label,
        enabled: prop_bool(&node.properties, "enabled", true),
        toggle,
        checked: prop_i32(&node.properties, "toggle-state") == Some(1),
        depth,
        has_submenu: prop_string(&node.properties, "children-display").as_deref()
            == Some("submenu"),
    }));

    for child in &node.children {
        if let Some(child) = parse_node(child) {
            walk(child, depth.saturating_add(1), out);
        }
    }
}

/// Read a boolean DBusMenu property, defaulting when absent or mistyped.
fn prop_bool(properties: &HashMap<String, OwnedValue>, key: &str, default: bool) -> bool {
    properties
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(default)
}

/// Read an integer DBusMenu property.
fn prop_i32(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    properties
        .get(key)
        .and_then(|value| i32::try_from(value).ok())
}

/// Read a string DBusMenu property.
fn prop_string(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = properties.get(key)?;
    let text: &str = value.try_into().ok()?;
    Some(text.to_string())
}

/// Strip DBusMenu label markup: `\_` becomes a literal underscore, a lone `_`
/// marks a mnemonic and is dropped.
fn clean_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'_') => {
                out.push('_');
                chars.next();
            }
            '_' => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Build a proxy for an item at its registered bus name and path.
fn item_proxy<'a>(
    connection: &Connection,
    registration: &'a Registration,
) -> Option<StatusNotifierItemProxy<'a>> {
    StatusNotifierItemProxy::builder(connection)
        .destination(registration.bus.as_str())
        .ok()?
        .path(registration.path.as_str())
        .ok()?
        .build()
        .ok()
}

/// Read one item's display properties into a snapshot entry.
fn read_item(
    connection: &Connection,
    registration: &Registration,
    cache: &mut IconCache,
) -> Option<TrayItem> {
    let proxy = item_proxy(connection, registration)?;

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
        key: registration.ident.clone(),
        id,
        title,
        status,
        is_menu: proxy.item_is_menu().unwrap_or(false),
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

    fn string_prop(value: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(value.to_string())).unwrap()
    }

    fn props(pairs: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    /// A wire-shaped menu node: a `(ia{sv}av)` structure, children pre-wrapped.
    fn owned_node(id: i32, pairs: &[(&str, OwnedValue)], children: Vec<OwnedValue>) -> OwnedValue {
        OwnedValue::try_from(Value::from((id, props(pairs), children))).unwrap()
    }

    /// A layout root whose only role is to hold the top-level items.
    fn root(children: Vec<OwnedValue>) -> RawNode {
        RawNode {
            id: 0,
            properties: HashMap::new(),
            children,
        }
    }

    #[test]
    fn strips_mnemonics_from_labels() {
        assert_eq!(clean_label("_File"), "File");
        assert_eq!(clean_label("Save \\_As"), "Save _As");
        assert_eq!(clean_label("plain"), "plain");
    }

    #[test]
    fn flattens_a_menu_tree_with_depth() {
        let menu = parse_menu(
            "item".into(),
            3,
            root(vec![
                owned_node(
                    1,
                    &[("label", string_prop("File"))],
                    vec![owned_node(2, &[("label", string_prop("New"))], vec![])],
                ),
                owned_node(3, &[("type", string_prop("separator"))], vec![]),
                owned_node(
                    4,
                    &[
                        ("label", string_prop("Show")),
                        ("toggle-type", string_prop("checkmark")),
                        ("toggle-state", OwnedValue::from(1i32)),
                    ],
                    vec![],
                ),
            ]),
        );

        assert_eq!(menu.revision, 3);
        assert_eq!(menu.entries.len(), 4);
        let MenuEntry::Item(file) = &menu.entries[0] else {
            panic!("expected item");
        };
        assert_eq!(file.label, "File");
        assert_eq!(file.depth, 0);
        let MenuEntry::Item(new) = &menu.entries[1] else {
            panic!("expected item");
        };
        assert_eq!((new.label.as_str(), new.depth), ("New", 1));
        assert_eq!(menu.entries[2], MenuEntry::Separator);
        let MenuEntry::Item(show) = &menu.entries[3] else {
            panic!("expected item");
        };
        assert_eq!(show.toggle, Toggle::Checkbox);
        assert!(show.checked);
    }

    #[test]
    fn skips_invisible_items_and_their_children() {
        let menu = parse_menu(
            "item".into(),
            0,
            root(vec![owned_node(
                1,
                &[("visible", OwnedValue::from(false))],
                vec![owned_node(2, &[("label", string_prop("Hidden"))], vec![])],
            )]),
        );

        assert!(menu.entries.is_empty());
    }

    #[test]
    fn defaults_items_to_enabled_and_untoggled() {
        let menu = parse_menu(
            "item".into(),
            0,
            root(vec![owned_node(
                1,
                &[("label", string_prop("Run"))],
                vec![],
            )]),
        );

        let MenuEntry::Item(item) = &menu.entries[0] else {
            panic!("expected item");
        };
        assert!(item.enabled);
        assert_eq!(item.toggle, Toggle::None);
        assert!(!item.checked);
    }

    #[test]
    fn decodes_a_wire_layout_reply() {
        use zbus::zvariant::serialized::Context;
        use zbus::zvariant::{LE, to_bytes_for_signature};

        let reply = (
            7u32,
            (
                0i32,
                HashMap::<String, OwnedValue>::new(),
                vec![Value::from((
                    1i32,
                    props(&[("label", string_prop("Open"))]),
                    Vec::<Value<'static>>::new(),
                ))],
            ),
        );

        let ctx = Context::new_dbus(LE, 0);
        let bytes = to_bytes_for_signature(ctx, "u(ia{sv}av)", &reply).unwrap();
        let ((revision, root), _): ((u32, RawNode), _) = bytes.deserialize().unwrap();

        let menu = parse_menu("item".into(), revision, root);
        assert_eq!(menu.revision, 7);
        let MenuEntry::Item(item) = &menu.entries[0] else {
            panic!("expected item");
        };
        assert_eq!(item.label, "Open");
    }
}
