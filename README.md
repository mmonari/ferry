# ferry

Share one keyboard and mouse between Hyprland machines on your local network. Push the pointer off
the edge of one screen and it continues on the next machine; the keyboard follows the pointer.

ferry is built only for **Hyprland on [Omarchy](https://omarchy.org/)**. It has a Rust daemon
(`ferryd`), a `ferry` command-line tool, and an Omarchy shell plugin for the bar.

![How ferry works: the source machine's ferryd captures input at the screen edge and sends it over QUIC with mutual TLS to the other machine's ferryd, which types it through virtual devices](docs/assets/architecture.svg)

> **Status: 0.1.0, an early preview.** It is used daily between two machines. Expect rough edges,
> and run the same ferry version on every machine.

## What it does

- **Edge crossings.** Arrange the machines' monitors on one shared layout. A pointer that leaves
  through a linked edge enters the next machine at the matching point.
- **Keybind crossings.** `Super+Ctrl+Alt` plus an arrow key moves the keyboard and mouse to the
  machine on that side, or back home, without touching the pointer.
- **Bar widget.** Shows the state of each peer. Its panels arrange the layout, pair machines and
  change the settings.
- **Encrypted, LAN only.** Peers talk QUIC with mutual TLS 1.3. Each machine pins the other's key
  fingerprint when you pair them, and refuses everything else. There are no accounts and no cloud.

## Requirements

- Omarchy 4.5 with Hyprland 0.56.2 (Lua config) and xdg-desktop-portal-hyprland 1.4.1, the
  versions ferry is tested on.
- Two or more machines on the same network that can reach each other on **UDP port 4243**.
- To build: Arch's `rust` and `cargo` packages (edition 2024).

## Install

Build and install the Arch package from this checkout, then run setup as your own user, on
every machine:

```bash
bin/install          # makepkg -fsi in pkgbuild/; pacman asks for sudo
ferry setup          # shows each change and asks once; a second run changes nothing
ferry doctor         # PASS, WARN or FAIL for each check, with a fix for each
```

`ferry setup` links the bar plugin, enables the `ferryd` user unit, adds the escape bind and the
go chord to `~/.config/hypr/ferry.lua`, and offers to open the firewall port. `--dry-run` shows
every change without making it. `ferry setup --undo` takes it all back.

See [`docs/packaging.md`](docs/packaging.md) for upgrades, uninstalling and what the package
holds.

## Pair two machines

On the machine that will accept:

```bash
ferry pair --accept
```

On the other machine, within 120 seconds:

```bash
ferry pair <host-or-ip>
```

Both machines show the same 6-digit code. Confirm on both. The Pair panel on the bar does the
same (`ferry ui pair`).

## Arrange the layout

Drag the monitors in the Arrange panel (`ferry ui arrange`), or place a peer from the terminal:

```bash
ferry layout show                         # peer names and their monitors
ferry layout place netbook --right-of desktop
```

Edges that touch become links. Push the pointer through one to cross.

## Getting your input back

| How | What it does |
|---|---|
| `Super+Ctrl+Escape` | The escape chord. It always returns input to this machine, even if `ferryd` hangs. |
| `ferry release` | The same, from a terminal. |
| `ferry lock on` | No crossing starts from this machine until `ferry lock off`. |

ferry also releases by itself:

- If a peer goes silent, held keys are released on the other machine within about 650 ms.
- ferry never captures input while the screen is locked.

## Commands

| Command | What it does |
|---|---|
| `ferry status` | This peer, each peer's link, and the current state |
| `ferry go left\|right\|up\|down` | Move input to the machine on that side, or back home |
| `ferry peers`, `ferry unpair` | List the pinned peers, or remove one |
| `ferry id`, `ferry trust` | Pair by hand: print this machine's fingerprint, or pin another's |
| `ferry config` | Read and change `config.toml` through `ferryd`, which checks every value |
| `ferry events` | Stream `ferryd`'s events as JSON lines, for frontend authors |

Every command takes `--json`. The socket API is documented in
[`docs/api/socket-api.md`](docs/api/socket-api.md).

## Known limitations

- **Hyprland on Omarchy only.** Other compositors, macOS and Windows are out of scope.
- **Same version on every machine.** Mixed versions are not tested.
- **No clipboard sharing or network discovery yet.** Their settings exist, but nothing reads them.
- **With fcitx5:** the first modifier pressed in a crossing that starts right after the machine
  was a target can end the crossing, and the key chord lands on the local machine.

## License

MIT, see [`LICENSE`](LICENSE). ferry includes code from
[gliff](https://github.com/omacom/gliff) under the MIT License; see [`NOTICE`](NOTICE).
