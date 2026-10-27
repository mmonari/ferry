// from gliff crates/hypr-ipc/src/lib.rs @ 089662f
//! Raw access to the Hyprland IPC socket (`hyprctl` without the binary).
//!
//! Instance discovery works without `HYPRLAND_INSTANCE_SIGNATURE`, so a server
//! spawned by ssh can find the running compositor from `$XDG_RUNTIME_DIR`.

#![forbid(unsafe_code)]

mod events;

pub use events::{Event, EventStream};

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("XDG_RUNTIME_DIR is not set and /run/user/<uid> does not exist")]
    NoRuntimeDir,
    #[error("no Hyprland instance found under {0}")]
    NoInstance(PathBuf),
    #[error("Hyprland instance {0} does not exist")]
    UnknownInstance(String),
    #[error("lock file {0} has no Wayland socket name")]
    BadLockFile(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("hyprctl `{command}` failed: {response}")]
    Command { command: String, response: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// `$XDG_RUNTIME_DIR`, or `/run/user/<uid>` when the variable is not set and
/// that directory exists. Tailscale SSH, for one, starts commands without it.
pub fn runtime_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let dir = PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()));
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(Error::NoRuntimeDir)
    }
}

/// A running Hyprland instance.
#[derive(Debug, Clone)]
pub struct Instance {
    pub signature: String,
    pub dir: PathBuf,
}

impl Instance {
    /// Locate an instance. Order: explicit signature, `HYPRLAND_INSTANCE_SIGNATURE`,
    /// then the newest instance under the [`runtime_dir`]'s `hypr` whose socket
    /// answers (a crashed compositor leaves its directory and socket behind).
    pub fn discover(explicit: Option<&str>) -> Result<Self> {
        let runtime_dir = runtime_dir()?;
        let from_env = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
        Self::discover_in(&runtime_dir, explicit.or(from_env.as_deref()))
    }

    /// Like [`discover`](Self::discover), but reads nothing from the environment.
    pub fn discover_in(runtime_dir: &Path, explicit: Option<&str>) -> Result<Self> {
        let hypr_dir = runtime_dir.join("hypr");
        if let Some(sig) = explicit.map(str::to_owned) {
            let dir = hypr_dir.join(&sig);
            if !dir.join(".socket.sock").exists() {
                return Err(Error::UnknownInstance(sig));
            }
            return Ok(Self {
                signature: sig,
                dir,
            });
        }
        let mut candidates: Vec<(std::time::SystemTime, String, PathBuf)> = Vec::new();
        for entry in
            std::fs::read_dir(&hypr_dir).map_err(|_| Error::NoInstance(hypr_dir.clone()))?
        {
            let entry = entry?;
            let dir = entry.path();
            if !dir.join(".socket.sock").exists() {
                continue;
            }
            let modified = entry.metadata()?.modified()?;
            candidates.push((
                modified,
                entry.file_name().to_string_lossy().into_owned(),
                dir,
            ));
        }
        candidates.sort();
        candidates
            .into_iter()
            .rev()
            .find(|(_, _, dir)| listening(&dir.join(".socket.sock")))
            .map(|(_, signature, dir)| Self { signature, dir })
            .ok_or(Error::NoInstance(hypr_dir))
    }

    pub fn socket_path(&self) -> PathBuf {
        self.dir.join(".socket.sock")
    }

    /// The event socket (`socket2`).
    pub fn events_socket_path(&self) -> PathBuf {
        self.dir.join(".socket2.sock")
    }

    /// Subscribe to the event socket. Connect before reading state, so an
    /// event that lands in between is not lost.
    pub fn events(&self) -> Result<EventStream> {
        EventStream::connect(self)
    }

    /// Wayland socket name (e.g. `wayland-1`) as recorded in `hyprland.lock`.
    pub fn wayland_display(&self) -> Result<String> {
        let lock = self.dir.join("hyprland.lock");
        let text = std::fs::read_to_string(&lock)?;
        text.lines()
            .nth(1)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(Error::BadLockFile(lock))
    }

    /// Send one command and return the raw response: [`request_within`](Self::request_within)
    /// with [`REQUEST_TIMEOUT`]. The connect does not block either, so a wedged compositor
    /// (its listen queue full) costs nothing, and a silent one at most the timeout (A-10).
    pub fn request(&self, command: &str) -> Result<String> {
        self.request_within(command, REQUEST_TIMEOUT)
    }

    /// Like [`request`](Self::request), but the whole exchange (connect, write,
    /// read) must finish within `timeout`. The connect does not block: a
    /// compositor whose listen queue is full fails at once. On expiry the error
    /// is `Io` with kind `TimedOut` (or `WouldBlock` from the connect).
    pub fn request_within(&self, command: &str, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let remaining = || {
            deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::TimedOut))
        };
        let mut stream = connect_nonblocking(&self.socket_path())?;
        stream.set_nonblocking(false)?;
        stream.set_write_timeout(Some(remaining()?))?;
        stream.write_all(command.as_bytes())?;
        stream.flush()?;
        let mut response = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            stream.set_read_timeout(Some(remaining()?))?;
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => response.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into());
                }
                Err(e) => return Err(e.into()),
            }
        }
        let response = String::from_utf8_lossy(&response).into_owned();
        tracing::trace!(command, response = response.trim(), "hyprctl (timed)");
        Ok(response)
    }

    /// Dispatch `hl.dsp.release_input_capture()` (Lua config). Ends any input
    /// capture, including one orphaned by a killed client.
    /// Hyprland answers `ok` whether or not a capture was active.
    pub fn release_input_capture(&self) -> Result<()> {
        self.dispatch(RELEASE_INPUT_CAPTURE)
    }

    /// Warp the cursor to `(x, y)`, global logical (`hl.dsp.cursor.move`). A warp never
    /// activates an input-capture barrier; only relative motion does.
    pub fn move_cursor(&self, x: f64, y: f64) -> Result<()> {
        self.dispatch(&move_cursor_command(x, y))
    }

    /// Send a command that must answer `ok`.
    pub fn dispatch(&self, command: &str) -> Result<()> {
        let response = self.request(command)?;
        if response.trim() == "ok" {
            Ok(())
        } else {
            Err(Error::Command {
                command: command.to_owned(),
                response: response.trim().to_owned(),
            })
        }
    }

    pub fn request_json<T: DeserializeOwned>(&self, command: &str) -> Result<T> {
        let response = self.request(&format!("j/{command}"))?;
        Ok(serde_json::from_str(&response)?)
    }

    pub fn monitors(&self) -> Result<Vec<Monitor>> {
        self.request_json("monitors all")
    }

    /// Global cursor position in logical layout coordinates.
    pub fn cursor_pos(&self) -> Result<CursorPos> {
        self.request_json("cursorpos")
    }

    pub fn get_option(&self, name: &str) -> Result<OptionValue> {
        self.request_json(&format!("getoption {name}"))
    }

    pub fn create_headless_output(&self, name: &str) -> Result<()> {
        self.dispatch(&format!("output create headless {name}"))
    }

    pub fn remove_output(&self, name: &str) -> Result<()> {
        self.dispatch(&format!("output remove {name}"))
    }

    /// Apply a monitor rule for `name`. A legacy (hyprlang) config takes the
    /// `monitor` keyword; a Lua config rejects keywords and takes the same
    /// rule through `eval hl.monitor`.
    pub fn set_monitor_mode(
        &self,
        name: &str,
        width: u32,
        height: u32,
        hz: u32,
        scale: f32,
    ) -> Result<()> {
        let keyword = format!("keyword monitor {name},{width}x{height}@{hz},auto,{scale}");
        match self.dispatch(&keyword) {
            Err(Error::Command { response, .. }) if response.contains("non-legacy") => {
                self.dispatch(&format!(
                    "eval hl.monitor({{ output = \"{name}\", mode = \"{width}x{height}@{hz}\", position = \"auto\", scale = {scale} }})"
                ))
            }
            other => other,
        }
    }
}

/// The IPC command behind [`Instance::release_input_capture`]: what
/// `hyprctl dispatch 'hl.dsp.release_input_capture()'` sends.
pub const RELEASE_INPUT_CAPTURE: &str = "dispatch hl.dsp.release_input_capture()";

/// The IPC command of [`Instance::move_cursor`]: whole pixels, as Hyprland's Lua table reads them.
pub fn move_cursor_command(x: f64, y: f64) -> String {
    format!("dispatch hl.dsp.cursor.move({{x={x:.0},y={y:.0}}})")
}

/// The whole exchange of [`Instance::request`]: connect, write and read.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether something listens on `path`: the connect succeeds, or the listen
/// queue is full (a busy or wedged compositor is still a live one). Never
/// blocks.
fn listening(path: &Path) -> bool {
    match connect_nonblocking(path) {
        Ok(_) => true,
        Err(Error::Io(e)) => e.kind() == std::io::ErrorKind::WouldBlock,
        Err(_) => false,
    }
}

/// Connect to a Unix socket without blocking: a full listen queue is an
/// immediate `WouldBlock`, never a hang.
fn connect_nonblocking(path: &Path) -> Result<UnixStream> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    let fd = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .map_err(std::io::Error::from)?;
    let addr = SocketAddrUnix::new(path).map_err(std::io::Error::from)?;
    rustix::net::connect(&fd, &addr).map_err(std::io::Error::from)?;
    Ok(UnixStream::from(fd))
}

/// Whether the session is locked, from the JSON `monitors` answers with: any
/// monitor whose `solitaryBlockedBy` contains `LOCK`.
/// Hyprland 0.56.2 shows `["LOCK", "WINDOWED", "CANDIDATE"]` on the lock
/// screen and `["WINDOWED", "CANDIDATE"]` otherwise.
///
/// The field is `null` when nothing blocks solitary on that monitor, for
/// example a fullscreen window with no overlay layer above it (Omarchy's
/// screensaver on a monitor without a notification). A lock is always a
/// blocker, so `null` reads as "not locked" there.
///
/// Fails (rather than answer "unlocked") when the answer cannot tell: not
/// JSON, no monitors, or a monitor without the field.
pub fn session_locked_from_json(json: &str) -> Result<bool> {
    #[derive(Deserialize)]
    struct Blocked {
        // No `default`: a missing field still fails.
        #[serde(rename = "solitaryBlockedBy", deserialize_with = "null_as_empty")]
        solitary_blocked_by: Vec<String>,
    }
    fn null_as_empty<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Vec<String>, D::Error> {
        Ok(Option::<Vec<String>>::deserialize(d)?.unwrap_or_default())
    }
    let monitors: Vec<Blocked> = serde_json::from_str(json)?;
    if monitors.is_empty() {
        return Err(Error::Command {
            command: "j/monitors".to_owned(),
            response: "no monitors".to_owned(),
        });
    }
    Ok(monitors
        .iter()
        .any(|m| m.solitary_blocked_by.iter().any(|b| b == "LOCK")))
}

/// Parse the JSON `monitors all` answers with.
pub fn monitors_from_json(json: &str) -> Result<Vec<Monitor>> {
    Ok(serde_json::from_str(json)?)
}

/// The keyboards in the JSON `devices` answers with.
pub fn keyboards_from_json(json: &str) -> Result<Vec<Keyboard>> {
    #[derive(Deserialize)]
    struct Devices {
        #[serde(default)]
        keyboards: Vec<Keyboard>,
    }
    Ok(serde_json::from_str::<Devices>(json)?.keyboards)
}

/// Round a mode down until `scale` divides it into whole logical pixels.
/// Hyprland rejects a scale that leaves a fractional logical size ("Invalid
/// scale ... failed to find a clean divisor") and falls back to one of its
/// own, so a 1366x768 window at 1.25 would come up at scale 1. A scale of p/q
/// in lowest terms needs a mode that is a multiple of p; the result stays
/// even for the encoder. A scale with no small denominator, or none that is positive and sane, is left alone.
pub fn clean_mode(width: u32, height: u32, scale: f32) -> (u32, u32) {
    let s = f64::from(scale);
    let Some(q) = (1..=8u32).find(|q| {
        let numerator = s * f64::from(*q);
        (numerator - numerator.round()).abs() < 1e-4
    }) else {
        return (width, height);
    };
    let p = (s * f64::from(q)).round();
    // A scale of zero, a negative one or an absurd one is left alone, like one with no small
    // denominator: `p` of 0 would make the step 0 and `fit` divide by it.
    if !(1.0..=65_536.0).contains(&p) {
        return (width, height);
    }
    let p = p as u32;
    let step = if p.is_multiple_of(2) { p } else { 2 * p };
    let fit = |v: u32| (v - v % step).max(step);
    (fit(width), fit(height))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub refresh_rate: f64,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub transform: i32,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub disabled: bool,
}

impl Monitor {
    /// Logical rectangle `(x, y, w, h)` in the global layout: the mode size
    /// after transform, divided by the (fractional) scale.
    pub fn logical_rect(&self) -> (f64, f64, f64, f64) {
        let (w, h) = if self.transform % 2 == 1 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let s = f64::from(self.scale).max(0.01);
        (
            f64::from(self.x),
            f64::from(self.y),
            (f64::from(w) / s).round(),
            (f64::from(h) / s).round(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct CursorPos {
    pub x: f64,
    pub y: f64,
}

fn one() -> f32 {
    1.0
}

/// A keyboard as `devices` lists it. The xkb names are the ones Hyprland compiles its keymap
/// from: the `input:kb_*` options, or the device's own override. A virtual keyboard lists them
/// too, although its keymap is the one its client uploaded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Keyboard {
    pub name: String,
    #[serde(default)]
    pub rules: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub layout: String,
    #[serde(default)]
    pub variant: String,
    #[serde(default)]
    pub options: String,
    /// The keyboard Hyprland treats as the seat's own (`hl-virtual-keyboard-fcitx5` while
    /// fcitx5 holds the input-method grab).
    #[serde(default)]
    pub main: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OptionValue {
    pub option: String,
    #[serde(default)]
    pub set: bool,
    #[serde(default)]
    pub int: Option<i64>,
    #[serde(default)]
    pub float: Option<f64>,
    #[serde(default, rename = "str")]
    pub string: Option<String>,
    #[serde(default, rename = "bool")]
    pub boolean: Option<bool>,
}

impl OptionValue {
    pub fn as_bool(&self) -> Option<bool> {
        self.boolean.or_else(|| self.int.map(|i| i != 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verb comes first, as for the release (the bare Lua answered "unknown request" on the
    /// lab VM, 2026-10-23).
    #[test]
    fn the_cursor_warp_is_a_dispatch_of_whole_pixels() {
        assert_eq!(
            move_cursor_command(1917.0, 540.4),
            "dispatch hl.dsp.cursor.move({x=1917,y=540})"
        );
    }

    #[test]
    fn discovers_newest_live_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let hypr = tmp.path().join("hypr");
        let make = |name: &str, age: u64| {
            let dir = hypr.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let listener =
                std::os::unix::net::UnixListener::bind(dir.join(".socket.sock")).unwrap();
            std::fs::write(dir.join("hyprland.lock"), format!("123\nwayland-{age}\n")).unwrap();
            let t = std::time::SystemTime::now() - Duration::from_secs(age);
            std::fs::File::open(&dir).unwrap().set_modified(t).unwrap();
            listener
        };
        let _old = make("old_1_1", 20);
        let live = make("live_2_2", 10);
        // A dead instance keeps its socket file, but nothing answers it.
        drop(make("dead_3_3", 1));

        let inst = Instance::discover_in(tmp.path(), None).unwrap();
        assert_eq!(inst.signature, "live_2_2");
        assert_eq!(inst.wayland_display().unwrap(), "wayland-10");
        drop(live);
        let explicit = Instance::discover_in(tmp.path(), Some("old_1_1")).unwrap();
        assert_eq!(explicit.signature, "old_1_1");
        assert!(Instance::discover_in(tmp.path(), Some("missing")).is_err());
    }

    #[test]
    fn lock_state_from_solitary_blocked_by() {
        let unlocked = r#"[{"name":"HDMI-A-1","solitaryBlockedBy":["WINDOWED","CANDIDATE"]},
                           {"name":"DVI-D-1","solitaryBlockedBy":["WINDOWED","CANDIDATE"]}]"#;
        assert!(!session_locked_from_json(unlocked).unwrap());
        let locked = r#"[{"name":"A","solitaryBlockedBy":["WINDOWED"]},
                         {"name":"B","solitaryBlockedBy":["LOCK","WINDOWED","CANDIDATE"]}]"#;
        assert!(session_locked_from_json(locked).unwrap());
        // An answer that cannot tell is an error, never "unlocked".
        assert!(session_locked_from_json(r#"[{"name":"A"}]"#).is_err());
        assert!(session_locked_from_json("[]").is_err());
        assert!(session_locked_from_json("unknown request").is_err());
        // A substring is not the flag.
        let other = r#"[{"name":"A","solitaryBlockedBy":["UNLOCKED"]}]"#;
        assert!(!session_locked_from_json(other).unwrap());
    }

    #[test]
    fn lock_state_when_nothing_blocks_solitary() {
        // Shaped like the desktop's answer while the screensaver ran
        // (2026-10-17): the second monitor's field was `null`, and lock checks
        // failed to parse on and off for 14 minutes.
        let screensaver = r#"[{"name":"HDMI-A-1","solitaryBlockedBy":["WINDOWED"]},
                              {"name":"DVI-D-1","solitaryBlockedBy":null}]"#;
        assert!(!session_locked_from_json(screensaver).unwrap());
        let locked = r#"[{"name":"HDMI-A-1","solitaryBlockedBy":null},
                         {"name":"DVI-D-1","solitaryBlockedBy":["LOCK","OVERLAYS"]}]"#;
        assert!(session_locked_from_json(locked).unwrap());
        // `null` is an answer; a missing field still is not.
        let missing = r#"[{"name":"HDMI-A-1","solitaryBlockedBy":null},{"name":"DVI-D-1"}]"#;
        assert!(session_locked_from_json(missing).is_err());
    }

    #[test]
    fn request_within_times_out_and_fails_on_a_missing_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("hypr").join("sig");
        std::fs::create_dir_all(&dir).unwrap();
        let inst = Instance {
            signature: "sig".into(),
            dir: dir.clone(),
        };
        // No socket at all.
        assert!(
            inst.request_within("j/monitors", Duration::from_millis(50))
                .is_err()
        );
        // A socket that accepts and never answers.
        let listener = std::os::unix::net::UnixListener::bind(dir.join(".socket.sock")).unwrap();
        let t0 = Instant::now();
        let err = inst
            .request_within("j/monitors", Duration::from_millis(50))
            .unwrap_err();
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "{:?}",
            t0.elapsed()
        );
        assert!(matches!(err, Error::Io(ref e) if e.kind() == std::io::ErrorKind::TimedOut));
        drop(listener);
        std::fs::remove_file(dir.join(".socket.sock")).unwrap();
        // A socket that answers.
        let listener = std::os::unix::net::UnixListener::bind(dir.join(".socket.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"j/monitors");
            s.write_all(b"[]").unwrap();
        });
        let answer = inst
            .request_within("j/monitors", Duration::from_millis(500))
            .unwrap();
        assert_eq!(answer, "[]");
        server.join().unwrap();
    }

    /// A listener whose queue is full and that never accepts: Hyprland wedged. Backlog 0 lets one
    /// connection wait, which `listener_full` makes, so the next connect would block.
    fn listener_full(path: &Path) -> (std::os::unix::net::UnixListener, UnixStream) {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        rustix::net::listen(&listener, 0).unwrap();
        let waiting = UnixStream::connect(path).unwrap();
        (listener, waiting)
    }

    /// Runs `f` on a thread of its own and gives up waiting after `limit`, so a call that
    /// blocks forever fails the test instead of hanging it.
    fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(limit).expect("blocked past the limit")
    }

    /// A-10: `request` (and with it `release_input_capture`) never blocks on a wedged
    /// compositor's connect, and ends within its deadline when nothing answers.
    #[test]
    fn request_does_not_block_on_a_full_listen_queue() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("hypr").join("sig");
        std::fs::create_dir_all(&dir).unwrap();
        let (_listener, _waiting) = listener_full(&dir.join(".socket.sock"));
        let inst = Instance {
            signature: "sig".into(),
            dir: dir.clone(),
        };
        let t0 = Instant::now();
        let r = within(Duration::from_secs(2), move || inst.release_input_capture());
        assert!(r.is_err());
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "{:?}",
            t0.elapsed()
        );
        // discovery sees the wedged instance as alive, without blocking either
        let run = tmp.path().to_path_buf();
        let found = within(Duration::from_secs(2), move || {
            Instance::discover_in(&run, None).map(|i| i.signature)
        });
        assert_eq!(found.unwrap(), "sig");
    }

    #[test]
    fn parses_option_values() {
        let v: OptionValue =
            serde_json::from_str(r#"{"option": "x", "bool": false, "set": false }"#).unwrap();
        assert_eq!(v.as_bool(), Some(false));
        let v: OptionValue =
            serde_json::from_str(r#"{"option": "x", "str": "us", "set": true }"#).unwrap();
        assert_eq!(v.string.as_deref(), Some("us"));
    }

    #[test]
    fn parses_keyboards_from_devices() {
        // Trimmed from the desktop's answer (Hyprland 0.56.2, fcitx5 running).
        let json = r#"{"mice": [{"address": "0x1", "name": "logitech-wireless-mouse"}],
            "keyboards": [
              {"address": "0x2", "name": "by-tech-gaming-keyboard", "rules": "", "model": "",
               "layout": "us", "variant": "intl",
               "options": "compose:caps,shift:both_capslock_cancel",
               "active_layout_index": 0,
               "active_keymap": "English (US, intl., with dead keys)",
               "capsLock": false, "numLock": true, "main": false},
              {"address": "0x3", "name": "hl-virtual-keyboard-fcitx5", "rules": "", "model": "",
               "layout": "us", "variant": "intl",
               "options": "compose:caps,shift:both_capslock_cancel", "main": true}],
            "tablets": [], "touch": [], "switches": []}"#;
        let kbs = keyboards_from_json(json).unwrap();
        assert_eq!(kbs.len(), 2);
        assert_eq!(
            kbs[0],
            Keyboard {
                name: "by-tech-gaming-keyboard".into(),
                layout: "us".into(),
                variant: "intl".into(),
                options: "compose:caps,shift:both_capslock_cancel".into(),
                ..Default::default()
            }
        );
        assert!(kbs[1].main);
        // A seat with no keyboard, and an answer that is not JSON.
        assert!(keyboards_from_json(r#"{"mice": []}"#).unwrap().is_empty());
        assert!(keyboards_from_json("unknown request").is_err());
    }

    #[test]
    fn clean_mode_divides_into_whole_logical_pixels() {
        // Already clean: unchanged.
        assert_eq!(clean_mode(1290, 1926, 3.0), (1290, 1926));
        assert_eq!(clean_mode(2560, 1440, 2.0), (2560, 1440));
        assert_eq!(clean_mode(1920, 1080, 1.0), (1920, 1080));
        // 1.25 = 5/4: multiples of 5, kept even.
        assert_eq!(clean_mode(1366, 768, 1.25), (1360, 760));
        // 3x with an odd leftover: multiples of 6.
        assert_eq!(clean_mode(1290, 1925, 3.0), (1290, 1920));
        // 1.5 = 3/2 and 2.5 = 5/2.
        assert_eq!(clean_mode(1001, 701, 1.5), (996, 696));
        assert_eq!(clean_mode(1083, 2341, 2.5), (1080, 2340));
        // f32 rounds these scales slightly below their exact fractions.
        assert_eq!(clean_mode(1366, 768, 1.4), (1358, 756));
        assert_eq!(clean_mode(1366, 768, 1.8), (1350, 756));
        for (w, h, s) in [(1366, 768, 1.25), (1290, 1925, 3.0), (1001, 701, 1.5)] {
            let (cw, ch) = clean_mode(w, h, s);
            assert_eq!((f64::from(cw) / f64::from(s)).fract(), 0.0);
            assert_eq!((f64::from(ch) / f64::from(s)).fract(), 0.0);
            assert!(cw.is_multiple_of(2) && ch.is_multiple_of(2));
        }
    }

    #[test]
    fn clean_mode_keeps_the_mode_for_a_scale_with_no_divisor() {
        // A-14: each of these made the step 0 or overflowed `2 * p` before the guard.
        for s in [0.0, -1.25, 0.0001, 1e9, f32::MAX, f32::NAN, f32::INFINITY] {
            assert_eq!(clean_mode(1366, 768, s), (1366, 768), "scale {s}");
        }
    }

    use proptest::prelude::*;

    /// A JSON value of the shape Hyprland answers with, near enough to reach the field checks.
    fn json_soup() -> impl Strategy<Value = String> {
        let leaf = prop_oneof![
            Just("null".to_owned()),
            Just("true".to_owned()),
            any::<i64>().prop_map(|n| n.to_string()),
            "\\PC{0,8}".prop_map(|s| serde_json::Value::String(s).to_string()),
            Just("\"LOCK\"".to_owned()),
        ];
        let key = proptest::sample::select(vec![
            "id",
            "name",
            "width",
            "height",
            "x",
            "y",
            "scale",
            "transform",
            "focused",
            "solitaryBlockedBy",
            "keyboards",
            "refreshRate",
            "disabled",
            "main",
            "layout",
        ]);
        leaf.prop_recursive(4, 48, 5, move |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..5)
                    .prop_map(|v| format!("[{}]", v.join(","))),
                proptest::collection::vec((key.clone(), inner), 0..5).prop_map(|kv| {
                    let body: Vec<String> =
                        kv.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
                    format!("{{{}}}", body.join(","))
                }),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// The compositor's answers are `Ok` or an `Err`, never a panic, and never a lock
        /// reading from an answer that cannot tell.
        #[test]
        fn the_json_parsers_never_panic(raw in "\\PC{0,200}", soup in json_soup()) {
            for text in [&raw, &soup] {
                let _ = monitors_from_json(text);
                let _ = keyboards_from_json(text);
                let _ = session_locked_from_json(text);
            }
        }

        #[test]
        fn a_locked_monitor_reads_as_locked(n in 0usize..4, others in 0usize..4) {
            let blocked = |by: &str| format!(r#"{{"solitaryBlockedBy":[{by}]}}"#);
            let mut ms: Vec<String> = (0..others).map(|_| blocked(r#""WINDOWED""#)).collect();
            ms.insert(n.min(ms.len()), blocked(r#""WINDOWED","LOCK""#));
            prop_assert_eq!(session_locked_from_json(&format!("[{}]", ms.join(","))).ok(), Some(true));
        }

        /// Hyprland reports any scale, including zero and non-finite ones: no panic.
        #[test]
        fn clean_mode_never_panics(w in any::<u32>(), h in any::<u32>(), scale in any::<f32>()) {
            let _ = clean_mode(w, h, scale);
        }

        /// For the scales a monitor really has, the result divides into whole logical pixels
        /// and stays even.
        #[test]
        fn clean_mode_gives_whole_logical_pixels_for_real_scales(
            w in 64u32..8192, h in 64u32..8192,
            scale in proptest::sample::select(vec![1.0f32, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0]),
        ) {
            let (cw, ch) = clean_mode(w, h, scale);
            prop_assert!((f64::from(cw) / f64::from(scale)).fract().abs() < 1e-9);
            prop_assert!((f64::from(ch) / f64::from(scale)).fract().abs() < 1e-9);
            prop_assert!(cw.is_multiple_of(2) && ch.is_multiple_of(2));
            prop_assert!(cw <= w && ch <= h);
        }
    }
}
