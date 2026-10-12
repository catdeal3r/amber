//! End-to-end test for the StatusNotifierItem host.
//!
//! Spins up a private session bus, starts the watcher, publishes a fake tray
//! item, and checks that the watcher registers it and decodes its icon. Skipped
//! (not failed) when no `dbus-daemon` is available.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use shell_common::tray::{Tray, TrayStatus};
use zbus::blocking::Connection;
use zbus::interface;
use zbus::names::BusName;

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
}

fn wait_until(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
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
    item_connection
        .object_server()
        .at("/StatusNotifierItem", FakeItem { pixels })
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

    match previous {
        Some(value) => unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value) },
        None => unsafe { std::env::remove_var("DBUS_SESSION_BUS_ADDRESS") },
    }
}
