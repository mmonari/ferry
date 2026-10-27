# ferry: the omarchy-shell plugin

The bar widget of ferry: a pure-QML plugin for `omarchy-shell`. It shows where the keyboard and
mouse are, and opens the panels to pair machines, arrange the layout and change the settings. It
keeps no state of its own: it draws what `ferryd` reports over its socket and sends commands back.

- Plugin id `ferry.bar`, kind `bar-widget`, widget setting `showPeerName` (default on).
- Installed to `/usr/share/ferry/plugin/` by the package; `ferry setup` links it into
  `~/.config/omarchy/plugins/ferry.bar` and puts the widget on the bar.
- Targets the shell of Omarchy 4.5 (quickshell 0.3.2, Qt 6.12).

## Files

| Path | What |
|---|---|
| `manifest.json`, `qmldir` | The plugin's id, entry point and setting; the singletons and root types |
| `FerryWidget.qml`, `FerryButton.qml` | The bar widget, one per monitor, and its button |
| `Popover.qml`, `PopoverContent.qml` | The popover and its body |
| `PanelHost.qml`, `PanelCard.qml`, `panels/` | The panel window and its views: Arrange, Pair, Settings |
| `FerryClient.qml` | The one socket connection: snapshots, events, calls, reconnect |
| `FerryUi.qml` | The IPC target and which popover or panel is open |
| `EdgeGlowHost.qml` | The glow on the edge a crossing went through |
| `Model.js`, `Geometry.js`, `Arrange.js` | Pure logic, tested with node: widget state, layout geometry, the Arrange draft |
| `Strings.js`, `Glyphs.js` | Every user-visible string and icon |
| `views/`, `settings/` | The pieces the popover, panels and Settings view are built from |

## IPC

The IPC target is `ferry`:

| Function | What it does |
|---|---|
| `toggle`, `open`, `close` | The popover; `close` also closes the panel |
| `arrange`, `pair`, `settings` | Open the panel on that view |
| `panel` | Toggle the panel |
| `lock`, `release` | Toggle "lock to this machine"; release input to this machine |
| `state` | Print the widget's state: `off idle ready away controlled locked attention` |

```bash
omarchy-shell ferry arrange     # or: ferry ui arrange|pair|settings|popover
```

A Hyprland bind can call any of them, for example in `~/.config/hypr/bindings.lua`:

```lua
o.bind("SUPER + CTRL + M", "ferry: peers", "omarchy-shell ferry toggle")
```

## Development

The checks run offscreen against a fake daemon. They never touch the running shell, a real
`ferryd` or your ferry config.

```bash
scripts/m3/check.sh             # manifest, qmllint (Qt 6), node tests, shellcheck
scripts/m3/check.sh --full      # also drives the views against the fake daemon and two real ferryd
scripts/m3/shots.sh             # screenshots of every state, light and dark themes
```

The fake daemon is `scripts/m3/fake-ferryd.py` (Python standard library only).

The shell does not hot-reload plugin QML. Run `omarchy restart shell` after every QML edit.
`omarchy-shell shell rescanPlugins` is enough for a manifest change.
