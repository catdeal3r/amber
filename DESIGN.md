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
| `crates/locker`        | `amber-locker`   | stub        |

The panel and the launcher are implemented; the rest are placeholders so the
workspace resolves.

## Design language — `fuji`

Semantic tokens (see `crates/shell-common/src/theme.rs`):

| token         | role                                   | value     |
| ------------- | -------------------------------------- | --------- |
| `bg`          | bar, overlays, lock background         | `#0f0f0f` |
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

- **Radius:** `10` for cards/overlays, `6` for tight chips. The launcher opts
  out of rounding entirely: its card, search pill and rows are square-cornered.
- **Fonts:** `monospace` for text, `Material Symbols Sharp` for icons.
  Both are resolved from the system font stack and overridable.
  CommitMono Regular is bundled (`shell-common/assets/`, OFL-1.1) and registered
  at startup, because iced's font database only sees faces surfaced by the host
  fontconfig config — on NixOS a stale fontconfig cache can omit it.
- **Metrics:** bar height `42`, cluster gap `15`, gap `7`, overlay width `500`.

## The bar (`amber-panel`)

A single surface anchored to the **top** edge, full width, `42px` tall, with a
`42px` exclusive zone. Background `bg`, no rounding.

```
┌───────────────────────────────────────────────────────────────────┐
│  ❄       1  2  3  4  5                   wifi bt  ▮▮▯  09:41 AM     │
└───────────────────────────────────────────────────────────────────┘
   logo            workspace pager         wifi bt battery  clock
```

- **Left:** the Nix snowflake (`shell-common::assets::nix_bar()`), vertically
  centered.
- **Center:** workspace pager of plain text indices, in niri's `(output, idx)`
  order.
  - focused → `text` (white)
  - occupied (has windows) → `accent` (brown)
  - empty → `muted`
  - hover → `hover` fill, `6px` radius
  - left click → `niri msg action focus-workspace <idx>`
- **Right:** Wi-Fi, Bluetooth, then battery, then the clock; each indicator is
  omitted entirely when its hardware is absent.
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
battery, Wi-Fi and Bluetooth every `5s`. The Wi-Fi query passes `--rescan no` so
polling never triggers a scan. All are subscriptions in
`crates/panel/src/main.rs`. The left/right clusters sit on a base row while the
pager is stacked on top and centered on the bar's full width.

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
  has one. Unselected rows are `muted` with a `hover` fill on hover; the
  highlighted row uses `accent_fill` with both texts in `text` (white). Clicking
  a row launches it.
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
- **Focus & dismissal:** the search field is focused once as soon as the surface
  maps (a `Subscription::run` stream that fires a single `Focus` message). A
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

## Planned surfaces (spec captured, not built)

- **Notifications** (`400px` toasts, `surface` r10, 10px gaps): 4s timeout with a
  3px status-coloured progress bar, click/swipe dismiss, hover pauses; default
  top-right, configurable.
- **Lock** (per-output full-frame `bg`): Nix snowflake (48px), time + date,
  password pill (`field` r10) with accent cursor; Enter verifies, error flashes
  `red`.

## Configuration

Optional `~/.config/amber/config.toml` overrides the pinned defaults:

```toml
[font]
text = "monospace"
icon = "Material Symbols Sharp"

[wallpaper]
path = "/home/me/pictures/wall.png"
fit = "cover" # cover | contain | fill | none
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
- Nix logo is rasterised to PNG at build-asset time rather than shipped as SVG
  (see `crates/shell-common/src/assets.rs` for the regeneration command).
