# amber

A minimal, cozy Wayland desktop shell for [niri](https://github.com/YaLTeR/niri).

It is **not** a compositor. Every piece is an ordinary layer-shell client app
running on top of niri, written in Rust with [iced] through
[iced_layershell]. The look is ported from [garado/cozy](https://github.com/garado/cozy)
(AwesomeWM), pinned to its warm-brown `fuji` colorscheme.

## Components

| crate                  | binary           | status      |
| ---------------------- | ---------------- | ----------- |
| `crates/shell-common`  | —                | done        |
| `crates/panel`         | `amber-panel`    | done (v1)   |
| `crates/launcher`      | `amber-launcher` | done (v1)   |
| `crates/notifd`        | `amber-notifd`   | stub        |
| `crates/locker`        | `amber-locker`   | done (v1)   |

The panel, the launcher and the locker are implemented; the rest are
placeholders so the workspace resolves.

## Design language — `fuji`

Semantic tokens (see `crates/shell-common/src/theme.rs`):

| token         | role                                   | value     |
| ------------- | -------------------------------------- | --------- |
| `bg`          | bar, overlays, lock backdrop fallback  | `#0f0f0f` |
| `surface`     | cards, rows, notification background   | `#131313` |
| `hover`       | hover fill                             | `#181818` |
| `field`       | search field, progress track           | `#1d1d1d` |
| `muted`       | secondary text, empty workspaces       | `#515151` |
| `text`        | primary text, occupied workspaces      | `#dedede` |
| `accent`      | focused workspace, cursors, focus ring | `#92826b` |
| `accent_fill` | selected rows / pressed states         | `#554836` |
| `red`         | battery low, critical notices          | `#c49ea0` |
| `green`       | status green                           | `#89ab8a` |
| `yellow`      | attention                              | `#c4c19e` |

- **Radius:** none. Every surface, card, chip and field is square-cornered —
  there are no rounded corners anywhere, and the theme carries no radius metric
  to reach for. New widgets must stay square too.
- **Fonts:** `monospace` for text, `Material Symbols Sharp` for icons.
  Both are resolved from the system font stack and overridable.
  CommitMono Regular is bundled (`shell-common/assets/`, OFL-1.1) and registered
  at startup, because iced's font database only sees faces surfaced by the host
  fontconfig config — on NixOS a stale fontconfig cache can omit it.
- **Metrics:** bar height `42`, cluster gap `15`, gap `7`, overlay width `500`.

## The bar (`amber-panel`)

A single surface anchored to the **top** edge, full width, `42px` tall, with a
`42px` exclusive zone. Background `bg`, no rounding. The panel runs as an
iced_layershell **daemon** (multi-window) rather than a single `application`, so
it can create extra layer surfaces — the tray context-menu overlay (see
[System tray](#system-tray)) and the always-mapped tooltip overlay (see
[Hover tooltips](#hover-tooltips)).

```
┌───────────────────────────────────────────────────────────────────┐
│  ❄       1  2  3  4  5            ▣ ▣  wifi bt  ▮▮▯  09:41 AM      │
└───────────────────────────────────────────────────────────────────┘
   logo            workspace pager   tray    wifi bt battery  clock
```

- **Left:** the Nix snowflake (`shell-common::assets::nix_bar()`), vertically
  centered.
- **Center:** workspace pager of plain text indices, in niri's `(output, idx)`
  order.
  - focused → `text` (white)
  - occupied (has windows) → `accent` (brown)
  - empty → `muted`
  - hover → `hover` fill (square corners)
  - left click → `niri msg action focus-workspace <idx>`
- **Right:** tray icons, then Wi-Fi, Bluetooth, battery, then the clock; each
  indicator is omitted entirely when its hardware is absent.
  - **Tray** (`shell-common::tray`): one icon per registered StatusNotifierItem,
    in registration order (see [System tray](#system-tray)).
  - **Wi-Fi** (`shell-common::wifi`, via `nmcli`): `wifi_off` in `muted` when
    the radio is off, a stepped `network_wifi_*` glyph in `text` when connected,
    and `wifi_find` in `muted` when on but not associated. Left click toggles
    the radio (`nmcli radio wifi on|off`).
  - **Bluetooth** (`shell-common::bluetooth`, via `bluetoothctl`):
    `bluetooth_disabled` in `muted` when off, `bluetooth` in `text` when on, and
    `bluetooth_connected` in `green` when a device is connected. Left click
    toggles power (`bluetoothctl power on|off`).
  - **Battery** (icon + percentage), read from `/sys/class/power_supply`
    (`shell-common::battery`). charging → `green` bolt glyph, full → `green`,
    ≤15% → `red` alert glyph, otherwise a stepped `battery_*_bar` glyph in
    `text`.
  - **Clock**, formatted `%I:%M %p` (e.g. `09:41 AM`).

Data comes from `niri msg --json workspaces`, polled every `400ms` (occupancy is
cross-referenced from `niri msg --json windows`, since workspace objects no
longer carry their window list); the clock ticks once per second and the
battery, Wi-Fi, Bluetooth and power profile every `5s`. The Wi-Fi query passes
`--rescan no` so polling never triggers a scan. All are subscriptions in
`crates/panel/src/main.rs`. The left/right clusters sit on a base row while the
pager is stacked on top and centered on the bar's full width.

### Hover tooltips

Hovering the clock, battery, Wi-Fi or Bluetooth indicator reveals extra detail in
a small `bg` card with square corners below the bar, its right edge anchored to
the pointer:

- **Clock:** the full date (`%A, %-d %B %Y`).
- **Battery:** percentage and status, then health (full-charge vs design
  capacity), cycle count, and the active `power-profiles-daemon` profile
  (`shell-common::battery::power_profile`).
- **Wi-Fi:** the connected SSID and signal percentage (or `Wi-Fi off` / `Not
  connected`).
- **Bluetooth:** the names of connected devices (or `Bluetooth off` / `No
  connected devices`).

The card is drawn on a single **always-mapped** `Layer::Overlay` surface created
at startup. It is fully **input-transparent** (`events_transparent` sets an empty
input region) and never takes keyboard focus, so the bar beneath it keeps the
pointer and hover works normally. Leaving the bar clears the card and hovering a
different indicator swaps its content; the surface stays mapped (showing nothing
when idle) to avoid the map/unmap churn of creating it per hover. Indicators are
wrapped in a `mouse_area` for `on_enter`, and the bar in one for `on_exit`.

### System tray

niri has no tray, so `amber-panel` hosts the freedesktop StatusNotifierItem
protocol itself (`shell-common::tray`). It owns the well-known D-Bus name
`org.kde.StatusNotifierWatcher` and exports the watcher interface at
`/StatusNotifierWatcher`: items call `RegisterStatusNotifierItem`, and the
snapshot is refreshed once per second from each item's `Id`, `Title`, `Status`,
`IconPixmap` and `IconName`.

- Icons prefer the item's own `IconPixmap` (ARGB32, converted to RGBA; the
  largest valid pixmap wins), falling back to a themed `IconName` resolved
  across the XDG icon directories, preferring larger size variants. Items with
  no decodable icon show a small `muted` (or `yellow`, when the item wants
  attention) dot.
- The watcher runs on its own thread with zbus's blocking API, so the UI never
  waits on the bus. Items whose bus name disappears are pruned from the
  snapshot and from `RegisteredStatusNotifierItems`.
- Clicks are handled: left click calls `Activate`, middle click
  `SecondaryActivate`, and right click `ContextMenu` (a menu-only item, where
  `ItemIsMenu` is set, opens its menu on left click instead). Requests cross to
  the watcher thread over a channel so the UI never blocks on the bus.
- When an item publishes a `com.canonical.dbusmenu` menu, a `ContextMenu` request
  snapshots it on the watcher thread: `AboutToShow` (to let lazy items fill
  themselves in) then `GetLayout`, flattened depth-first with submenu children
  inlined and mnemonics stripped. The UI picks the snapshot up on its next poll
  and draws it; items without a DBusMenu fall back to calling the item's own
  `ContextMenu`.

The menu is drawn on its own full-output `Layer::Overlay` surface, created on
demand and removed when it closes. The surface is keyboard-exclusive and
captures the pointer, so a click anywhere outside the card (or `Escape`)
dismisses it. The card has square corners and a 1px `muted` border, and is
placed just below the bar with its right edge under the tray icon that was
clicked: the bar tracks the pointer's last x and hands it to the menu as an
anchor. (A layer surface's origin already sits below the bar's exclusive zone,
so only a small gap is added on top.) Over-long item labels are shortened with
an ellipsis, because iced has no built-in text ellipsis. Menus are frozen once
shown, so row clicks are sent back to the item as DBusMenu `Event(id,
"clicked")` without re-reading the layout.

The watcher is verified by
`crates/shell-common/tests/tray_host.rs`, which starts a private `dbus-daemon`,
publishes a fake item, and asserts it appears in the snapshot with its icon
decoded, receives a routed `Activate`, and exposes a DBusMenu whose flattened
rows and `clicked` events round-trip.

### Not yet in the bar

- Battery via `upower` over D-Bus (currently sysfs).
- Wi-Fi/Bluetooth via D-Bus (currently `nmcli`/`bluetoothctl`).
- Scroll-wheel to cycle workspaces.

## The wallpaper (`amber-wallpaper`)

A `Layer::Background` surface anchored to every edge, so it fills the output
behind all windows; input passes through (`events_transparent`) and the keyboard
interaction is `None`. The configured image is decoded up front and handed to
iced as raw RGBA (the same path as the bundled assets), so any format the `image`
crate can read works without iced's codec features. The format is sniffed from
the file contents, so the extension doesn't have to match. Images whose longest
side exceeds 2048px are downscaled first: iced uploads images through a texture
atlas that is capped at 2048 (`iced_wgpu`'s `image::atlas::MAX_SIZE`), and larger
images are split into fragments that don't render correctly. Large images are also
uploaded by iced on a background thread, which asks for a redraw once it finishes;
that request can be dropped for a static layer surface, leaving the theme `bg` on
screen, so the wallpaper keeps forcing its own redraws for a short window after
startup. With no `path` set it just fills with the theme `bg`. If the configured image is missing, unreadable or
corrupted, a `warning: ...` line is printed to stderr and shown in the centre of
the screen. Passing an output name as the first argument pins it to that output,
otherwise it follows the active one.

## The launcher (`amber-launcher`)

A one-shot, keyboard-exclusive overlay. It slides down and fades in as it maps
and grabs the keyboard, so you can start typing immediately; launching an app,
pressing Escape, or clicking outside plays the reverse animation before it
exits.

```
┌───────────────────────────────────────────────┐
│  ⌕  Search applications                        │
│  ┌─────────────────────────────────────────┐  │
│  │ Firefox                                 │  │  ← selected (accent_fill)
│  │ Web Browser                             │  │     └ subtext, 60% opacity
│  │ Files                                   │  │
│  │ Blender                                 │  │
│  └─────────────────────────────────────────┘  │
└───────────────────────────────────────────────┘
            500px centred card, square corners
```

- **Surface:** `Layer::Overlay`, anchored to every edge with `size: None` so it
  fills the output; the `500px` card (`bg`, square corners) centres itself
  inside. Keyboard interaction is `Exclusive`; Escape and clicks outside the
  card dismiss it. The wrapped surface is transparent, so any sliver around the
  card shows the desktop beneath.
- **Search pill** (`field`, h40, square corners): a `search` glyph and a focused
  `text_input` (transparent, `text` on a `muted` placeholder, `accent_fill`
  selection).
- **Rows** (`surface`, square corners, h38): the app name, with its
  `GenericName` underneath (11px) in the row color at 60% opacity when the entry
  has one. Unselected rows are `muted`; the selected row's texts are `text`
  (white). Hover is tracked in the app state and paints `hover` on the row
  background; clicking a row launches it. The list is three stacked layers — row
  backgrounds, the `accent_fill` highlight band, then the labels (drawn on top
  and taking the pointer) — so the highlight can move without dragging the text.
- **Selection highlight:** a single `accent_fill` band that translates between
  rows over `160ms` (`EaseOutCubic`) as the selection moves, rather than the
  fill jumping row to row. It animates the on-screen row position, so scrolling
  the visible window doesn't trigger a spurious slide; a second
  `Animation<f32>`, advanced by the same `16ms` tick subscription, positions it.
- **Keys:** type to filter, `↑`/`↓` move the selection (wrapping), `Enter`
  launches, `Escape` closes. Up to eight rows fit at once (`MAX_ROWS`) and the
  window scrolls to keep the selection visible; an empty result set shows a
  centred `No matches`.
- **Open/close animation:** on open the card slides down from the top edge to
  its resting spot while fading in over `200ms` (`EaseOutCubic`); launching,
  pressing Escape, or clicking outside plays the reverse before the launcher
  actually exits. A launch only spawns once the close animation settles (the
  pending action rides on the final frame). The fade is per-element alpha
  (`Color::scale_alpha`) because the layer surface itself is transparent, and
  the slide comes from two `FillPortion` spacers around the card — `≈0:1` at the
  top edge when hidden, `1:1` (centred) when open. Frames come from a `16ms`
  tick subscription that only runs while something is animating.
- **Focus & dismissal:** a `Subscription::run` stream fires a single `Focus`
  message that starts the open animation, and focus is (re)issued on each
  opening frame. The field can only take focus once it is laid out — which can
  be a frame after the surface maps — so the card stays in the widget tree even
  while transparent, and focus is retried until the animation settles. A
  full-output click-catcher sits under the card, so a click anywhere outside
  dismisses while the card swallows its own clicks. Escape is handled first and
  always dismisses, even if the text field would consume it.
- **Discovery** (`crates/launcher/src/apps.rs`): scans the XDG data directories
  with `freedesktop-desktop-entry`, keeping `Type=Application` entries that are
  neither `NoDisplay` nor `Hidden` and that show on the current desktop set
  (`NotShowIn` excludes; `OnlyShowIn` is applied only when the session
  advertises a desktop). Duplicate ids keep the higher-priority directory, and
  names are read in the environment locale.
- **Matching:** `nucleo-matcher` fuzzy-scores the query against each app name;
  an empty query lists everything alphabetically, and only matches are shown.
- **Launching:** the `Exec` line is tokenized into argv
  (`crates/launcher/src/exec.rs`) honouring the spec's quoting and escaping plus
  field-code substitution. The crate's own `parse_exec` splits on whitespace and
  would tear `"an arg"` apart, hence the local tokenizer: `%i`/`%c`/`%k` expand,
  while `%f`/`%F`/`%u`/`%U` and unknown or deprecated codes drop out.
  `Terminal=true` entries run inside `$TERMINAL` (or the first of a few common
  emulators found on `PATH`), and the child is spawned detached with its
  standard streams pointed at `/dev/null`; a spawn failure is reported on
  stderr with the app name. The launcher then exits immediately with success.

## The lock screen (`amber-locker`)

A one-shot locker. `swayidle` (idle timeout, resume from sleep) or the `Super+L`
keybinding spawns it; it takes over every output and, once the password is
accepted, exits — which releases the lock. There is no long-running daemon or
IPC surface to drive: a trigger simply spawns the binary.

```
┌───────────────────────────────────────────────┐
│      (full-output blurred + darkened           │
│              wallpaper)                        │
│                                               │
│                  09:41                        │  ← %H:%M, 56px
│            Tuesday, 13 October                │  ← %A, %-d %B
│                                               │
│         ┌───────────────────────┐             │
│         │        Password       │             │  ← secure text_input, 260px
│         └───────────────────────┘             │
└───────────────────────────────────────────────┘
```

- **Surface:** one `Layer::Overlay` surface per output (`StartMode::AllScreens`,
  `size: Some((0, 0))`, anchored to every edge) with `exclusive_zone: -1`, so it
  covers the bar's strut as well as the desktop. Keyboard interaction is
  `Exclusive`; the wrapped surface is opaque, so nothing below shows through.
- **Backdrop:** the configured `[wallpaper]` image, decoded, thumbnailed into a
  1280px box, Gaussian-blurred and darkened to half brightness, then laid behind
  the content (`ContentFit` from the config). It is built on a background thread,
  so the surface covers the screen with the solid `bg` immediately and the
  wallpaper swaps in a moment later; a short burst of ticks then keeps frames
  coming until iced's asynchronous image upload lands (see `amber-wallpaper`).
  With no wallpaper configured — or if it can't be read — it stays on the solid
  `bg`, with the problem reported on stderr.
- **Layout:** the clock (`%H:%M`) and the date stack above a 260px password
  field (`field`, square corners, 1px `muted` border) holding a centred, secure
  `text_input`. The column is centred on the output, with the error line below
  the field.
- **Authentication:** `Enter` runs `pam::authenticate` off the UI thread
  (`tokio::task::spawn_blocking`); success exits the process, failure clears the
  field, flashes the border and message in `red`, and refocuses.
- **Focus:** the field is focused at startup (and again after a short delay), on
  window focus, and on any click; the once-a-second clock tick refocuses it
  while it is empty. Layer-shell gives no focus guarantee, so the field keeps
  taking focus back.
- **Single instance:** the process takes a `flock` on `amber-locker.lock` in
  `$XDG_RUNTIME_DIR`; a second invocation (say the idle timeout and a `Super+L`
  press racing) finds it held and exits without drawing anything.
- **PAM binding** (`crates/locker/src/pam.rs`): a hand-written `libpam` binding
  (`pam_start`/`pam_authenticate`/`pam_acct_mgmt`/`pam_end`) whose conversation
  callback answers the echo-off prompt with the password and the echo-on prompt
  with the login name. Linking `libpam` directly (from the devshell's
  `linux-pam`) avoids the `pam-sys`/bindgen dependency; the PAM service defaults
  to `login` and is configurable.

## Planned surfaces (spec captured, not built)

- **Notifications** (`400px` toasts, `surface`, 10px gaps): 4s timeout with a
  3px status-coloured progress bar, click/swipe dismiss, hover pauses; default
  top-right, configurable.

## Configuration

Optional `~/.config/amber/config.toml` overrides the pinned defaults:

```toml
[font]
text = "monospace"
icon = "Material Symbols Sharp"

[wallpaper]
path = "/home/me/pictures/wall.png"
fit = "cover" # cover | contain | fill | none

[lock]
pam_service = "login" # /etc/pam.d service amber-locker authenticates against
```

Loaded once at startup by `shell-common::config`, which exposes a small manager:
`config::get()` returns the current snapshot, `config::reload()` re-reads the
file, and `config::path()` reports the location. `config::theme()` and
`config::wallpaper()` are conveniences over it. Invalid TOML is reported
prominently on stderr (with the file path and the parse error) and surfaced as a
red `config error` in the bar; the shell then runs on the built-in defaults. Colours
are intentionally pinned to `fuji`; a wallust-driven `colours.json` watcher is a
possible later addition.

## Building & running

The iced/wgpu + Wayland client stack needs native libraries that aren't on a
NixOS default path, so build through the devshell. `build.sh` does this and
collects ready-to-run binaries in one directory:

```sh
./build.sh                       # release build of the whole workspace -> ./dist
./build.sh amber-panel           # just one binary -> ./dist
./build.sh amber-panel amber-locker
./build.sh -o /tmp/out amber-locker
./dist/amber-panel               # run straight from the output directory
```

It re-execs into the devshell, runs `cargo build --release --locked` (the whole
workspace, or just the named packages), copies each binary into `dist/`, and
`patchelf`s an RPATH onto them pointing at the devshell's Wayland/GL/fontconfig
libraries, so no `cargo run` or `nix develop` is needed at runtime. Naming one or
more binaries builds and patches only those, leaving the rest of the output
directory untouched — handy when iterating on a single component. Binaries are
unlinked before being replaced, so a running instance doesn't block the copy.
For quick iteration a debug build still works via
`nix develop -c cargo run -p panel`.

Run it from inside a niri session (it needs a Wayland display). niri autostarts
the release binaries directly:

```kdl
spawn-at-startup "sh" "-c" "/home/catdealer/amber/dist/amber-panel"
spawn-at-startup "sh" "-c" "/home/catdealer/amber/dist/amber-wallpaper"
```

The launcher is on demand rather than always-on, so bind it to a key instead:

```kdl
binds {
    Mod+D { spawn "sh" "-c" "/home/catdealer/amber/dist/amber-launcher"; }
}
```

Re-run `./build.sh` after pulling changes so those paths stay up to date. When
iterating on one component, `./build.sh amber-panel` (etc.) is usually enough.

## Later

- **Flake packaging.** Build the binaries with `rustPlatform`
  (`buildRustPackage`), wrapping each with an `LD_LIBRARY_PATH` that points at the
  Wayland/GL/fontconfig libraries, so `nix build` produces them without the
  ad-hoc `build.sh`.

## Dependency notes

- `iced 0.14` + `iced_layershell 0.19.1` (lockstep). iced features:
  `image-without-codecs` (image widget), `svg`, `tokio` (the thread-pool backend
  has an empty `time` module, so `time::every` needs the tokio backend).
- `image` is depended on directly with only the `png` codec, to avoid pulling the
  AVIF/`rav1e` codecs through iced's full `image` feature.
- `winit-core`/`winit-common` are pinned to `0.31.0-beta.2` in `Cargo.lock`:
  `iced_exdevtools 0.19.1` (a mandatory dep of iced_layershell) does not compile
  against `beta.3`, which added `#[non_exhaustive]` to `NativeKeyCode`.
- The launcher adds `freedesktop-desktop-entry 0.8` (`.desktop` discovery and
  localized fields, MPL-2.0) and `nucleo-matcher 0.3` (fuzzy scoring, MPL-2.0).
- The tray host adds `zbus 5` (MIT). It uses the default `async-io` runtime and
  the blocking API from a dedicated thread; no tokio integration is needed.
- Nix logo is rasterised to PNG at build-asset time rather than shipped as SVG
  (see `crates/shell-common/src/assets.rs` for the regeneration command).
