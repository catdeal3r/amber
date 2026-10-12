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
| `crates/launcher`      | `amber-launcher` | stub        |
| `crates/notifd`        | `amber-notifd`   | stub        |
| `crates/locker`        | `amber-locker`   | stub        |

The panel is the only surface implemented so far; the rest are placeholders so
the workspace resolves.

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

- **Radius:** `10` for cards/overlays, `6` for tight chips.
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
│  ❄       1  2  3  4  5                          ▮▮▯  09:41 AM      │
└───────────────────────────────────────────────────────────────────┘
   logo            workspace pager                  battery  clock
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
- **Right:** battery indicator followed by the clock, formatted `%I:%M %p`
  (e.g. `09:41 AM`).
  - icon + percentage, read from `/sys/class/power_supply` (`shell-common::battery`)
  - charging → `green` bolt glyph, full → `green`, ≤15% → `red` alert glyph,
    otherwise a stepped `battery_*_bar` glyph in `text`
  - hidden entirely when the machine has no battery

Data comes from `niri msg --json workspaces`, polled every `400ms` (occupancy is
cross-referenced from `niri msg --json windows`, since workspace objects no
longer carry their window list); the clock ticks once per second and the
battery every `10s`. All are subscriptions in
`crates/panel/src/main.rs`. The left/right clusters sit on a base row while the
pager is stacked on top and centered on the bar's full width.

### Not yet in the bar

- Battery via `upower` over D-Bus (currently sysfs).
- Scroll-wheel to cycle workspaces.

## The wallpaper (`amber-wallpaper`)

A `Layer::Background` surface anchored to every edge, so it fills the output
behind all windows; input passes through (`events_transparent`) and the keyboard
interaction is `None`. The configured image is decoded up front and handed to
iced as raw RGBA (the same path as the bundled assets), so any format the `image`
crate can read works without iced's codec features. With no `path` set it just
fills with the theme `bg`. Passing an output name as the first argument pins it
to that output, otherwise it follows the active one.

## Planned surfaces (spec captured, not built)

- **Launcher** (`500px` centered overlay, r10, keyboard-exclusive): search pill
  (`field`, h40) + Apps/Windows tabs (active `accent_fill`); rows `surface` r10,
  selected `accent_fill`; fuzzy `.desktop` search, detached spawn; Windows tab via
  `niri msg --json windows`.
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
`config::wallpaper()` are conveniences over it. Colours are intentionally
pinned to `fuji`; a wallust-driven `colours.json` watcher is a possible later
addition.

## Building & running

The iced/wgpu + Wayland client stack needs native libraries that aren't on a
NixOS default path, so use the devshell:

```sh
nix develop -c cargo run -p panel
```

Run it from inside a niri session (it needs a Wayland display). Until the flake
packages the binaries (see *Later*), autostart the debug build through the
devshell so it picks up the native libraries:

```kdl
spawn-at-startup "sh" "-c" "cd /home/catdealer/amber && nix develop -c cargo run -p panel"
```

## Later

- **Packaging & release script.** Build the binaries with `rustPlatform`
  (`buildRustPackage`), wrapping each with an `LD_LIBRARY_PATH` that points at the
  Wayland/GL/fontconfig libraries, and add a release build script. Then switch
  niri `spawn-at-startup` from the `nix develop` debug invocation above to the
  packaged `amber-panel`.

## Dependency notes

- `iced 0.14` + `iced_layershell 0.19.1` (lockstep). iced features:
  `image-without-codecs` (image widget), `svg`, `tokio` (the thread-pool backend
  has an empty `time` module, so `time::every` needs the tokio backend).
- `image` is depended on directly with only the `png` codec, to avoid pulling the
  AVIF/`rav1e` codecs through iced's full `image` feature.
- `winit-core`/`winit-common` are pinned to `0.31.0-beta.2` in `Cargo.lock`:
  `iced_exdevtools 0.19.1` (a mandatory dep of iced_layershell) does not compile
  against `beta.3`, which added `#[non_exhaustive]` to `NativeKeyCode`.
- Nix logo is rasterised to PNG at build-asset time rather than shipped as SVG
  (see `crates/shell-common/src/assets.rs` for the regeneration command).
