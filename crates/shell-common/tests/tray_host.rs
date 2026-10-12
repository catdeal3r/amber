//! End-to-end test for the StatusNotifierItem host.
//!
//! Spins up a private session bus, starts the watcher, publishes a fake tray
//! item, and checks that the watcher registers it and decodes its icon. Skipped
//! (not failed) when no `dbus-daemon` is available.

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use shell_common::tray::{MenuEntry, Tray, TrayStatus};
use zbus::blocking::Connection;
use zbus::interface;
use zbus::names::BusName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// `DBUS_SESSION_BUS_ADDRESS` is process-global; keep D-Bus tests serial.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct PrivateBus {
    child: Child,
    address: String,
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start a throwaway `dbus-daemon` and return its address.
fn start_private_bus() -> Option<PrivateBus> {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--print-address", "--nofork"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let stdout = child.stdout.take()?;
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).ok()?;

    let address = line.trim().to_string();
    (!address.is_empty()).then_some(PrivateBus { child, address })
}

/// A minimal `org.kde.StatusNotifierItem` to register with the watcher.
struct FakeItem {
    pixels: Vec<u8>,
    activated: Arc<AtomicBool>,
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl FakeItem {
    #[zbus(property)]
    fn id(&self) -> String {
        "test-item".to_string()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        "Test Item".to_string()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "NeedsAttention".to_string()
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        vec![(2, 2, self.pixels.clone())]
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/MenuBar").unwrap()
    }

    fn activate(&self, _x: i32, _y: i32) {
        self.activated.store(true, Ordering::SeqCst);
    }
}

/// A DBusMenu node value, matching the layout wire format `(ia{sv}av)`.
type Layout = (i32, HashMap<String, OwnedValue>, Vec<Value<'static>>);

/// A minimal `com.canonical.dbusmenu` object served by the fake item.
struct FakeMenu {
    events: Arc<Mutex<Vec<(i32, String)>>>,
}

#[interface(name = "com.canonical.dbusmenu")]
impl FakeMenu {
    fn get_layout(
        &self,
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, Layout) {
        let root = (
            0i32,
            HashMap::new(),
            vec![
                node(1, &[("label", text("Open"))], vec![]),
                node(
                    4,
                    &[
                        ("label", text("Show")),
                        ("toggle-type", text("checkmark")),
                        ("toggle-state", OwnedValue::from(1i32)),
                    ],
                    vec![],
                ),
                node(3, &[("type", text("separator"))], vec![]),
            ],
        );
        (1, root)
    }

    fn about_to_show(&self, _id: i32) -> bool {
        true
    }

    fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
        self.events.lock().unwrap().push((id, event_id));
    }
}

/// Build a menu node wrapped as a variant, as the `av` children require.
fn node(
    id: i32,
    properties: &[(&str, OwnedValue)],
    children: Vec<Value<'static>>,
) -> Value<'static> {
    let properties: HashMap<String, OwnedValue> = properties
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect();
    Value::from((id, properties, children))
}

/// A string-valued DBusMenu property.
fn text(value: &str) -> OwnedValue {
    OwnedValue::try_from(Value::from(value.to_string())).unwrap()
}

fn wait_until(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Poll the shared menu slot until the watcher snapshots one.
fn wait_for_menu(tray: &Tray) -> shell_common::tray::TrayMenu {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(menu) = tray.take_menu() {
            return menu;
        }
        assert!(Instant::now() < deadline, "timed out waiting for menu");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn watcher_tracks_a_registered_item() {
    let _guard = ENV_LOCK.lock().unwrap();

    let Some(bus) = start_private_bus() else {
        eprintln!("skipping: dbus-daemon unavailable");
        return;
    };

    let previous = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
    // SAFETY: test is serialized by ENV_LOCK and no other thread reads the
    // environment concurrently.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.address) };

    let tray = Tray::spawn();
    let item_connection = Connection::session().expect("connect to private bus");

    // Items only register once the watcher owns its well-known name.
    let dbus = zbus::blocking::fdo::DBusProxy::new(&item_connection).unwrap();
    wait_until(
        || {
            matches!(
                dbus.name_has_owner(BusName::try_from("org.kde.StatusNotifierWatcher").unwrap()),
                Ok(true)
            )
        },
        "watcher name",
    );

    // Publish the fake item and announce it to the watcher.
    // ARGB32, network order: (A, R, G, B).
    let pixels = vec![
        255, 255, 0, 0, // opaque red
        255, 0, 255, 0, // opaque green
        255, 0, 0, 255, // opaque blue
        255, 255, 255, 255, // white
    ];
    let activated = Arc::new(AtomicBool::new(false));
    item_connection
        .object_server()
        .at(
            "/StatusNotifierItem",
            FakeItem {
                pixels,
                activated: activated.clone(),
            },
        )
        .unwrap();

    let menu_events = Arc::new(Mutex::new(Vec::new()));
    item_connection
        .object_server()
        .at(
            "/MenuBar",
            FakeMenu {
                events: menu_events.clone(),
            },
        )
        .unwrap();

    item_connection
        .call_method(
            Some("org.kde.StatusNotifierWatcher"),
            "/StatusNotifierWatcher",
            Some("org.kde.StatusNotifierWatcher"),
            "RegisterStatusNotifierItem",
            &("/StatusNotifierItem",),
        )
        .unwrap();

    // The watcher refreshes on a timer, so poll the snapshot.
    wait_until(
        || {
            tray.items()
                .iter()
                .any(|item| item.title.as_deref() == Some("Test Item"))
        },
        "tray snapshot",
    );

    let items = tray.items();
    let item = items
        .iter()
        .find(|item| item.title.as_deref() == Some("Test Item"))
        .expect("item present");

    assert_eq!(item.id, "test-item");
    assert_eq!(item.status, TrayStatus::NeedsAttention);

    let icon = item.icon.as_ref().expect("icon decoded");
    assert_eq!((icon.width, icon.height), (2, 2));
    // ARGB opaque red (A=255,R=255,G=0,B=0) becomes RGBA (255, 0, 0, 255).
    assert_eq!(&icon.pixels[0..4], &[255, 0, 0, 255]);

    // A left click routes through the command channel to the item's `Activate`.
    let key = item.key.clone();
    tray.activate(&key);
    wait_until(|| activated.load(Ordering::SeqCst), "item activation");

    // A right click snapshots the item's DBusMenu for the UI to draw.
    tray.context_menu(&key);
    let menu = wait_for_menu(&tray);
    assert_eq!(menu.key, key);
    assert_eq!(menu.revision, 1);
    let labels: Vec<&str> = menu
        .entries
        .iter()
        .filter_map(|entry| match entry {
            MenuEntry::Item(item) => Some(item.label.as_str()),
            MenuEntry::Separator => None,
        })
        .collect();
    assert_eq!(labels, ["Open", "Show"]);
    assert!(menu.entries.contains(&MenuEntry::Separator));
    let open = menu
        .entries
        .iter()
        .find_map(|entry| match entry {
            MenuEntry::Item(item) if item.label == "Open" => Some(item.id),
            _ => None,
        })
        .expect("an Open item");

    // Clicking a row reports the event back to the menu object.
    tray.menu_click(&key, open);
    wait_until(
        || {
            menu_events
                .lock()
                .unwrap()
                .iter()
                .any(|(id, event)| *id == open && event == "clicked")
        },
        "menu event",
    );

    match previous {
        Some(value) => unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value) },
        None => unsafe { std::env::remove_var("DBUS_SESSION_BUS_ADDRESS") },
    }
}
