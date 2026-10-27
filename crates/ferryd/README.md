# ferryd

The ferry daemon. It joins the pure machines (`ferry-core`), the transport (`ferry-net`), the
capture side (`ferry-capture`) and injection (`ferry-inject`) on four threads, and serves the
socket API that the `ferry` CLI talks to (`docs/api/socket-api.md`).

| Thread | Owns |
|---|---|
| `ctl` | socket API, config and state files, trust store, layout, the portal session, lock checks, logind |
| `net` | the QUIC endpoint, the Hello exchange, the TargetMachine, Ping/Pong |
| `capture` | the EIS source, the SourceMachine, its one deadline |
| `inject` | ferry-inject's virtual devices, **on demand** (see below) |

## Run

```bash
ferryd                       # capture and inject, as the user's session
ferryd --no-input            # network only: never capture, never inject
ferryd --log-level debug     # error | warn | info | debug | trace; FERRY_LOG sets it too
```

- `--no-input` makes a daemon that pairs, connects and answers `status`, but never grabs or
  injects input. Every `Enter` from a peer is refused (`Ended(refused)`). Use it for anything that
  runs on a live desktop and only needs the network side.
- `input.capture = "off"` in the config does the same for capture only: the peer can still be a
  target.
- `Options::new` (the library API) is `--no-input` by default. Only the `ferryd` binary turns input
  on, so a test cannot grab or inject input by accident.
- Socket: `$XDG_RUNTIME_DIR/ferry/ferryd.sock`. Config: `~/.config/ferry/config.toml`. Pins:
  `~/.local/share/ferry/trusted.toml`. Layout: `~/.local/state/ferry/layout.toml`.
- SIGINT and SIGTERM run the shutdown order of design §9. A panic releases capture over Hyprland
  IPC, requests the shutdown and exits with a failure, so systemd restarts it.

**Inject runs on demand.** The virtual devices exist for the start-up modifier reset and from a
target session's first input until 2 s after the target is idle. On a peer that also captures,
a virtual keyboard that merely exists makes Hyprland 0.56.2 re-create the EIS keyboard on the
first modifier press of a crossing.

## Errors

`ferry --json` prints a failure as one line on stderr, `{"error":{"code","message","hint?"}}`, the
same shape as the socket's errors. The exit codes do not change: 0 success, 1 failure, 2 usage,
3 ferryd unreachable. Without `--json` the line is `error: …`, then `  hint: …`.

## fake-peer (lab only)

`examples/fake-peer.rs` is a stand-in for the other machine. It speaks the wire protocol to a real
ferryd as a **fake source** or a **fake target**. It never captures or injects, so it may run on
the host against a lab VM. It exists so a drill can see exactly what a ferryd sends and when, and
can send precise key and pointer sequences, without a second compositor.

```bash
cargo build --release -p ferryd --example fake-peer
fake-peer id <key-dir>                                              # print its fingerprint
fake-peer source <key-dir> <ip:port> <daemon-fp> [--name N] [--monitor WxH] [--layout us | --keymap-file F]
fake-peer target <key-dir> <ip:port> <daemon-fp> [--name N] [--monitor WxH]
```

- The daemon must pin the fake's fingerprint first (`ferry trust <fp>`).
- Both roles dial the daemon, send `Hello` and Ping every 2 s. Every output line starts with the
  host's `CLOCK_MONOTONIC` in µs.
- `source` reads commands from stdin (`enter`, `move`, `glide`, `key`, `tap`, `type`, `click`,
  `wheel`, `leave`, `keymap`, `mute`, …; the list is at the top of the file). `--keymap-file`
  sends a given xkb keymap instead of one compiled from `--layout`, for example the target's own, as
  two Omarchy machines would; `keymap F` changes it mid-session. `mute` stops the `Held`
  snapshots so the target's watchdog fires.
- `target` acknowledges every `Held` and prints what it receives. `mute` stops the `HeldAck`s.
- It is a test tool. Do not ship it and do not point it at a peer you do not control.

## Tests

`cargo test -p ferryd`: loopback only. The target path runs against a `Recorder` instead of real
injection, so no test touches input.
