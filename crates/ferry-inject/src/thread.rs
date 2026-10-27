// from gliff crates/hypr-input/src/thread.rs @ 089662f
//! The inject thread. Changes from gliff: whole-desktop pointer (unbound or
//! one bound pointer per output, S8), relative motion, a `Sync` command, rustix
//! memfd, and the M1 flush path: one flush per wake-up that never blocks, a
//! bounded ring for what the socket cannot take yet, a held set of its own for
//! `release_all`, and a modifier reset on every new keyboard. The mock-socket
//! keyboard swap test is kept.

use std::io::Write;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use calloop::channel::{self, SyncSender};
use ferry_wl::{LoopState, Outputs, Seat, now_ms};
use rustix::fs::{MemfdFlags, memfd_create};
use wayland_client::backend::WaylandError;
use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use crate::held::HeldSet;
use crate::keymap::KeyState;
use crate::pointer::Layout;
use crate::ring::{Push, Ring};
use crate::{
    Axis, AxisSource, CHANNEL_CAPACITY, Error, EventSink, InputCmd, InputConfig, InputEvent,
    MODIFIER_KEYCODES, PointerMode, Result, Shared,
};

const KEYMAP_FORMAT_XKB_V1: u32 = 1;

/// How long a keyboard replaced by a keymap change lives on. Destroying it at
/// once, while an input method (fcitx5) may still be handling its last keys,
/// left a stray modifier on those keys.
const RETIRED_KEYBOARD_GRACE: Duration = Duration::from_secs(1);

/// The request buffer of a Wayland connection holds 4096 bytes
/// (`wayland-backend` `MAX_BYTES_OUT`), and a request that does not fit while
/// the socket is blocked is lost with a connection error. The thread therefore
/// never lets more than this many bytes of requests pile up before a flush,
/// and never issues a request while the socket is blocked.
const OUT_BUDGET: usize = 3072;
/// Upper bound of the bytes one key, button, motion or scroll command writes
/// (a key is 20 + 24 bytes, a scroll 20 + 12 + 8).
const CMD_COST: usize = 64;
/// Upper bound of creating a keyboard: the request, the keymap with its fd,
/// eight key releases and `modifiers`.
const KEYBOARD_COST: usize = 320;
/// How long shutdown waits for a blocked socket to take the final release.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(250);

/// One output-bound virtual pointer.
struct Bound {
    name: String,
    output: WlOutput,
    ptr: ZwlrVirtualPointerV1,
    /// Physical width, height, integer scale and transform, per `wl_output`;
    /// a change triggers a rebind (Hyprland freezes a bound pointer after
    /// its output's mode changes, gliff `49fa165`).
    mode: (i32, i32, i32, u32),
}

struct State {
    shared: Arc<Shared>,
    sink: EventSink,
    conn: Connection,
    outputs: Outputs,
    seat: Seat,
    qh: QueueHandle<State>,
    kb_mgr: ZwpVirtualKeyboardManagerV1,
    seat_proxy: WlSeat,
    keyboard: ZwpVirtualKeyboardV1,
    /// Keyboards replaced by a keymap change, with when, until their grace
    /// period is over.
    retired: Vec<(Instant, ZwpVirtualKeyboardV1)>,
    ptr_mgr: ZwlrVirtualPointerManagerV1,
    mode: PointerMode,
    layout: Layout,
    /// Always present: absolute motion in `Unbound` mode, relative motion in
    /// both modes.
    unbound: ZwlrVirtualPointerV1,
    bound: Vec<Bound>,
    /// Index into `bound` of the pointer that took the last motion; `None`
    /// means `unbound`.
    active: Option<usize>,
    keys: KeyState,
    /// What the compositor has been told is held (not what is queued).
    held: HeldSet,
    /// Commands not yet issued. Everything goes through it, in order.
    ring: Ring,
    /// A flush hit `WouldBlock`: nothing is issued until the socket drains.
    blocked: bool,
    /// Request bytes issued since the last flush that completed.
    out_bytes: usize,
    /// After the next `release_all`, replace every device.
    rebuild_pending: bool,
    fatal: Option<String>,
    quit: bool,
}

pub fn spawn(
    cfg: InputConfig,
    sink: EventSink,
    ready_tx: mpsc::Sender<Result<()>>,
    shared: Arc<Shared>,
) -> Result<(SyncSender<InputCmd>, std::thread::JoinHandle<()>)> {
    let (tx, rx) = channel::sync_channel::<InputCmd>(CHANNEL_CAPACITY);
    let join = std::thread::Builder::new()
        .name("ferry-inject".into())
        .spawn(move || {
            let mut sink = sink;
            if let Err(e) = run(cfg, &mut sink, rx, ready_tx.clone(), shared) {
                let _ = ready_tx.send(Err(Error::Input(e.to_string())));
            }
        })?;
    Ok((tx, join))
}

fn run(
    cfg: InputConfig,
    sink: &mut EventSink,
    rx: channel::Channel<InputCmd>,
    ready_tx: mpsc::Sender<Result<()>>,
    shared: Arc<Shared>,
) -> Result<()> {
    let (conn, globals, queue) = ferry_wl::init::<State>(&cfg.target)?;
    let qh = queue.handle();
    let kb_mgr: ZwpVirtualKeyboardManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| ferry_wl::Error::MissingGlobal("zwp_virtual_keyboard_manager_v1"))?;
    let ptr_mgr: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 2..=2, ())
        .map_err(|_| ferry_wl::Error::MissingGlobal("zwlr_virtual_pointer_manager_v1 v2"))?;
    let outputs = Outputs::bind(&globals, &qh)?;
    let seat = Seat::bind(&globals, &qh)?;

    let keys = match &cfg.keymap {
        Some(text) => KeyState::from_text(text)?,
        None => KeyState::system(&cfg.target)?,
    };
    let seat_proxy = seat.wl_seat()?.clone();
    let mut state = State::new(
        shared,
        conn.clone(),
        outputs,
        seat,
        qh.clone(),
        kb_mgr,
        seat_proxy,
        ptr_mgr,
        cfg.pointer,
        cfg.layout.clone(),
        keys,
    )?;
    std::mem::swap(&mut state.sink, sink);
    let result = input_loop(&mut state, conn, queue, rx, ready_tx);
    let result = match (result, state.fatal.take()) {
        (Ok(()), Some(message)) => Err(Error::Input(message)),
        (result, _) => result,
    };
    if let Err(e) = &result {
        state.emit(InputEvent::Error(e.to_string()));
    }
    state.shutdown();
    std::mem::swap(&mut state.sink, sink);
    result
}

fn input_loop(
    state: &mut State,
    conn: Connection,
    mut queue: wayland_client::EventQueue<State>,
    rx: channel::Channel<InputCmd>,
    ready_tx: mpsc::Sender<Result<()>>,
) -> Result<()> {
    queue.roundtrip(state)?;
    queue.roundtrip(state)?;
    if state.mode == PointerMode::PerOutput {
        state.bind_outputs()?;
    }
    // The roundtrip above carried the keyboard's start-up reset to the
    // compositor: it was the first thing written to the keyboard.
    state.out_bytes = 0;
    tracing::info!(mode = ?state.mode, layout = ?state.layout, "input ready");
    state.emit(InputEvent::Ready);
    let _ = ready_tx.send(Ok(()));
    Ok(ferry_wl::run_loop(conn, queue, rx, state)?)
}

/// Upload `text` to `keyboard`, then release the eight modifier keycodes and
/// send `modifiers(0,0,0,0)`. A predecessor that died with
/// keys down leaves Hyprland's bind modifiers stuck; a new keyboard that
/// releases them clears both the seat and the binds. Nothing else may be sent
/// on `keyboard` before this.
fn init_keyboard(keyboard: &ZwpVirtualKeyboardV1, text: &str) -> Result<()> {
    upload_keymap_to(keyboard, text)?;
    let t = now_ms();
    for code in MODIFIER_KEYCODES {
        keyboard.key(t, code, 0);
    }
    keyboard.modifiers(0, 0, 0, 0);
    Ok(())
}

fn upload_keymap_to(keyboard: &ZwpVirtualKeyboardV1, text: &str) -> Result<()> {
    let fd = memfd_create(c"ferry-keymap", MemfdFlags::CLOEXEC)
        .map_err(|e| Error::Input(format!("memfd: {e}")))?;
    let mut file = std::fs::File::from(fd);
    file.write_all(text.as_bytes())?;
    file.write_all(&[0])?;
    file.flush()?;
    let size = text.len() as u32 + 1;
    keyboard.keymap(KEYMAP_FORMAT_XKB_V1, file.as_fd(), size);
    Ok(())
}

impl State {
    #[expect(
        clippy::too_many_arguments,
        reason = "one-shot constructor: each argument is a distinct Wayland handle or setting \
                  that `State` stores, built once in `run`"
    )]
    fn new(
        shared: Arc<Shared>,
        conn: Connection,
        outputs: Outputs,
        seat: Seat,
        qh: QueueHandle<State>,
        kb_mgr: ZwpVirtualKeyboardManagerV1,
        seat_proxy: WlSeat,
        ptr_mgr: ZwlrVirtualPointerManagerV1,
        mode: PointerMode,
        layout: Layout,
        keys: KeyState,
    ) -> Result<Self> {
        let keyboard = Self::create_keyboard(&shared, &kb_mgr, &seat_proxy, &qh, &keys.text)?;
        let unbound = ptr_mgr.create_virtual_pointer(Some(&seat_proxy), &qh, ());
        Ok(Self {
            shared,
            sink: Box::new(|_| {}),
            conn,
            outputs,
            seat,
            qh,
            kb_mgr,
            seat_proxy,
            keyboard,
            retired: Vec::new(),
            ptr_mgr,
            mode,
            layout,
            unbound,
            bound: Vec::new(),
            active: None,
            keys,
            held: HeldSet::default(),
            ring: Ring::default(),
            blocked: false,
            out_bytes: 0,
            rebuild_pending: false,
            fatal: None,
            quit: false,
        })
    }

    /// The only place a virtual keyboard is created: keymap, then the
    /// start-up reset, before anything else can be sent on it.
    fn create_keyboard(
        shared: &Shared,
        kb_mgr: &ZwpVirtualKeyboardManagerV1,
        seat: &WlSeat,
        qh: &QueueHandle<State>,
        text: &str,
    ) -> Result<ZwpVirtualKeyboardV1> {
        let keyboard = kb_mgr.create_virtual_keyboard(seat, qh, ());
        if let Err(e) = init_keyboard(&keyboard, text) {
            keyboard.destroy();
            return Err(e);
        }
        Shared::bump(&shared.counters.keyboards);
        Ok(keyboard)
    }

    fn emit(&mut self, ev: InputEvent) {
        (self.sink)(ev);
    }

    // ----- flush policy -----------------------------------------------------

    /// Make room for `bytes` more request bytes. False means the socket is
    /// blocked, and the caller must leave its command queued: issuing the
    /// request anyway could overflow the connection's buffer and lose it.
    fn reserve(&mut self, bytes: usize) -> bool {
        if self.blocked {
            return false;
        }
        if self.out_bytes + bytes > OUT_BUDGET {
            self.flush();
            if self.blocked {
                return false;
            }
        }
        self.out_bytes += bytes;
        true
    }

    /// Flush without blocking. `WouldBlock` is not an error: it marks the
    /// socket blocked, and the loop then waits for writability.
    fn flush(&mut self) {
        match self.conn.flush() {
            Ok(()) => {
                self.out_bytes = 0;
                self.blocked = false;
            }
            Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if !self.blocked {
                    Shared::bump(&self.shared.counters.would_block);
                }
                self.blocked = true;
            }
            Err(e) => {
                tracing::error!(error = %e, "wayland flush failed");
                self.fatal
                    .get_or_insert_with(|| format!("wayland flush: {e}"));
                self.quit = true;
            }
        }
    }

    /// Issue everything the socket will take, then flush once.
    fn pump(&mut self) {
        if self.shared.overflow.swap(false, Ordering::Relaxed) {
            self.overflow();
        }
        while !self.blocked && !self.quit {
            let Some(cmd) = self.ring.pop_front() else {
                break;
            };
            if let Err(cmd) = self.exec(cmd) {
                self.ring.push_front(cmd);
                break;
            }
        }
        if !self.blocked && self.out_bytes > 0 {
            self.flush();
        }
        if !self.blocked {
            self.destroy_retired_keyboards();
        }
    }

    /// The ring or the command channel overflowed. The defined outcome: drop
    /// the queued input, release everything held, then replace the devices.
    fn overflow(&mut self) {
        let queued = self.ring.len();
        Shared::bump(&self.shared.counters.overflows);
        tracing::warn!(
            queued,
            held_keys = self.held.key_count(),
            "inject queue overflow: dropping queued input, releasing everything held, rebuilding the devices"
        );
        self.rebuild_pending = true;
        self.ring.restart_with([InputCmd::ReleaseAll]);
        self.emit(InputEvent::Overflow { queued });
    }

    fn enqueue(&mut self, cmd: InputCmd) {
        let was_blocked = self.blocked;
        let cmd = match self.ring.push(cmd) {
            Push::Queued => {
                if was_blocked {
                    Shared::bump(&self.shared.counters.queued);
                }
                return;
            }
            Push::Coalesced => {
                Shared::bump(&self.shared.counters.coalesced);
                return;
            }
            Push::Full(cmd) => cmd,
        };
        // A burst can outrun one wake-up's pump; only a blocked socket is a
        // real overflow.
        if !self.blocked {
            self.pump();
        }
        match self.ring.push(cmd) {
            Push::Full(_) => self.overflow(),
            Push::Coalesced => Shared::bump(&self.shared.counters.coalesced),
            Push::Queued => {
                if self.blocked {
                    Shared::bump(&self.shared.counters.queued);
                }
            }
        }
    }

    // ----- executing one command ---------------------------------------------

    /// Issue one command. `Err` hands it back: the socket is blocked, and it
    /// stays at the head of the ring.
    fn exec(&mut self, cmd: InputCmd) -> std::result::Result<(), InputCmd> {
        let t = now_ms();
        // A NaN position maps to pixel 0 in `abs_in` and an infinity to the
        // last pixel: drop it and count it rather than warp the cursor.
        let finite = match &cmd {
            InputCmd::Motion { x, y } => x.is_finite() && y.is_finite(),
            InputCmd::MotionRel { dx, dy } => dx.is_finite() && dy.is_finite(),
            InputCmd::Axis {
                source,
                value,
                value120,
                ..
            } => matches!((source, value120), (AxisSource::Wheel, Some(_))) || value.is_finite(),
            _ => true,
        };
        if !finite {
            Shared::bump(&self.shared.counters.nonfinite_dropped);
            return Ok(());
        }
        match &cmd {
            InputCmd::Key { code, pressed } => {
                if !self.reserve(CMD_COST) {
                    return Err(cmd);
                }
                self.held.key(*code, *pressed);
                self.keyboard.key(t, *code, u32::from(*pressed));
                if let Some((dep, lat, lock, group)) = self.keys.update(*code, *pressed) {
                    self.keyboard.modifiers(dep, lat, lock, group);
                }
            }
            InputCmd::Motion { x, y } => {
                if !self.reserve(CMD_COST) {
                    return Err(cmd);
                }
                match self.mode {
                    PointerMode::Unbound => {
                        let a = self.layout.unbound(*x, *y);
                        self.unbound.motion_absolute(t, a.x, a.y, a.w, a.h);
                        self.unbound.frame();
                    }
                    PointerMode::PerOutput => {
                        if let Some((i, a)) = self.layout.bound(*x, *y)
                            && i < self.bound.len()
                        {
                            self.active = Some(i);
                            let p = &self.bound[i].ptr;
                            p.motion_absolute(t, a.x, a.y, a.w, a.h);
                            p.frame();
                        }
                    }
                }
            }
            InputCmd::MotionRel { dx, dy } => {
                if !self.reserve(CMD_COST) {
                    return Err(cmd);
                }
                self.active = None;
                self.unbound.motion(t, *dx, *dy);
                self.unbound.frame();
            }
            InputCmd::Button { button, pressed } => {
                if !self.reserve(CMD_COST) {
                    return Err(cmd);
                }
                self.held.button(*button, *pressed);
                let st = if *pressed {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                };
                let p = self.active_ptr();
                p.button(t, *button, st);
                p.frame();
            }
            InputCmd::Axis {
                axis,
                source,
                value,
                value120,
            } => {
                if !self.reserve(CMD_COST) {
                    return Err(cmd);
                }
                // Hyprland 0.56.2 turns a wheel-source `axis` into
                // `value120 = value * 8` for clients and emits the discrete
                // `value` (15) itself at each notch boundary, so hi-res and
                // whole notches both pass through as plain `axis` with
                // value = value120 * 15 / 120. `axis_discrete` is not used:
                // mixing it with partial `axis` events double-counts.
                let value = match (source, value120) {
                    (AxisSource::Wheel, Some(v)) => f64::from(*v) * 15.0 / 120.0,
                    _ => *value,
                };
                self.axis_raw(*axis, *source, value);
            }
            InputCmd::AxisStop { axis, source } => {
                if !self.reserve(CMD_COST) {
                    return Err(cmd);
                }
                let a = wl_axis(*axis);
                let p = self.active_ptr();
                p.axis_stop(t, a);
                p.axis_source(wl_source(*source));
                p.frame();
            }
            InputCmd::SetKeymap(text) => {
                if *text != self.keys.text && !self.swap_keyboard(text) {
                    return Err(cmd);
                }
            }
            InputCmd::SetLayout(layout) => {
                let names_changed = layout.outputs.iter().map(|o| &o.name).ne(self
                    .layout
                    .outputs
                    .iter()
                    .map(|o| &o.name));
                let rebind = self.mode == PointerMode::PerOutput && names_changed;
                if rebind && !self.reserve(CMD_COST * layout.outputs.len().max(1)) {
                    return Err(cmd);
                }
                self.layout = layout.clone();
                if rebind && let Err(e) = self.bind_outputs() {
                    self.emit(InputEvent::Error(e.to_string()));
                }
            }
            InputCmd::ReleaseAll => {
                if !self.release_all() || (self.rebuild_pending && !self.rebuild_devices()) {
                    return Err(cmd);
                }
            }
            InputCmd::Sync(tx) => {
                if !self.reserve(CMD_COST) {
                    return Err(cmd);
                }
                self.conn.display().sync(&self.qh, tx.clone());
            }
            InputCmd::Stop => self.quit = true,
        }
        Ok(())
    }

    /// Release every key and button this crate holds, then `modifiers(0,0,0,0)`.
    /// Resumable: it works from the held set, so after a blocked socket the
    /// next call continues where this one stopped. True when done.
    fn release_all(&mut self) -> bool {
        let t = now_ms();
        while let Some(code) = self.held.first_key() {
            if !self.reserve(CMD_COST) {
                return false;
            }
            self.keyboard.key(t, code, 0);
            self.held.key(code, false);
        }
        if !self.reserve(CMD_COST) {
            return false;
        }
        self.keys.reset();
        self.keyboard.modifiers(0, 0, 0, 0);
        while let Some(button) = self.held.first_button() {
            if !self.reserve(CMD_COST * (1 + self.bound.len())) {
                return false;
            }
            self.unbound
                .button(t, button, wl_pointer::ButtonState::Released);
            self.unbound.frame();
            for bound in &self.bound {
                bound
                    .ptr
                    .button(t, button, wl_pointer::ButtonState::Released);
                bound.ptr.frame();
            }
            self.held.button(button, false);
        }
        true
    }

    /// A keymap change: a new keyboard rather than a new keymap on this one.
    /// Hyprland keeps the first keymap it saw from a keyboard and ignores a
    /// re-upload, with fcitx5 running or not, so there is no in-place
    /// path. True when done; false when the socket is blocked.
    fn swap_keyboard(&mut self, text: &str) -> bool {
        let mut ks = match KeyState::from_text(text) {
            Ok(ks) => ks,
            Err(e) => {
                self.emit(InputEvent::KeymapRejected(e.to_string()));
                return true;
            }
        };
        // Replaying a held set that cannot fit the request buffer is not a
        // real case (it would take 90 keys down at once); release instead.
        if KEYBOARD_COST + 2 * CMD_COST * self.held.key_count() > OUT_BUDGET {
            tracing::warn!(
                held = self.held.key_count(),
                "keymap change with too many keys held: releasing them instead of replaying"
            );
            if !self.release_all() {
                return false;
            }
        }
        if !self.reserve(KEYBOARD_COST + 2 * CMD_COST * self.held.key_count()) {
            return false;
        }
        let t = now_ms();
        let (dep, lat, lock, group) = ks.carry_over(&self.keys, self.held.keys());
        let keyboard = match Self::create_keyboard(
            &self.shared,
            &self.kb_mgr,
            &self.seat_proxy,
            &self.qh,
            &ks.text,
        ) {
            Ok(k) => k,
            Err(e) => {
                self.emit(InputEvent::KeymapRejected(e.to_string()));
                return true;
            }
        };
        // Held keys belong to a Wayland keyboard, not just its XKB state.
        // Release the retiring device before replaying them on its
        // replacement.
        for code in self.held.keys() {
            self.keyboard.key(t, code, 0);
        }
        self.keyboard.modifiers(0, 0, 0, 0);
        let old = std::mem::replace(&mut self.keyboard, keyboard);
        self.retired.push((Instant::now(), old));
        self.keys = ks;
        for code in self.held.keys() {
            self.keyboard.key(t, code, 1);
        }
        self.keyboard.modifiers(dep, lat, lock, group);
        true
    }

    /// After an overflow: replace the keyboard and the pointers, so no state
    /// of the old devices can outlive the release. Runs right after
    /// `release_all`. True when done.
    fn rebuild_devices(&mut self) -> bool {
        if !self.reserve(KEYBOARD_COST + CMD_COST * (2 + self.layout.outputs.len())) {
            return false;
        }
        self.rebuild_pending = false;
        match Self::create_keyboard(
            &self.shared,
            &self.kb_mgr,
            &self.seat_proxy,
            &self.qh,
            &self.keys.text,
        ) {
            Ok(keyboard) => {
                let old = std::mem::replace(&mut self.keyboard, keyboard);
                self.retired.push((Instant::now(), old));
            }
            Err(e) => self.emit(InputEvent::Error(e.to_string())),
        }
        let unbound = self
            .ptr_mgr
            .create_virtual_pointer(Some(&self.seat_proxy), &self.qh, ());
        std::mem::replace(&mut self.unbound, unbound).destroy();
        for b in self.bound.drain(..) {
            b.ptr.destroy();
        }
        self.active = None;
        if self.mode == PointerMode::PerOutput
            && let Err(e) = self.bind_outputs()
        {
            self.emit(InputEvent::Error(e.to_string()));
        }
        tracing::info!("inject devices rebuilt");
        self.emit(InputEvent::Reset);
        true
    }

    /// Final release at thread exit. Waits up to `SHUTDOWN_WAIT` for a
    /// blocked socket; a compositor that never drains it has lost the
    /// connection's devices anyway, and Hyprland cleans up on disconnect.
    fn shutdown(&mut self) {
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        self.ring.clear();
        self.fatal = None;
        self.quit = false;
        loop {
            self.flush();
            if self.fatal.is_some() || !self.blocked || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        while !self.blocked && self.fatal.is_none() && !self.release_all() {
            std::thread::sleep(Duration::from_millis(1));
            self.flush();
            if Instant::now() >= deadline {
                break;
            }
        }
        if !self.blocked && self.fatal.is_none() {
            self.keyboard.destroy();
            for (_, keyboard) in self.retired.drain(..) {
                keyboard.destroy();
            }
            for b in self.bound.drain(..) {
                b.ptr.destroy();
            }
            self.unbound.destroy();
        }
        let _ = self.conn.flush();
    }

    /// (Re)create one bound pointer per layout output.
    fn bind_outputs(&mut self) -> Result<()> {
        for b in self.bound.drain(..) {
            b.ptr.destroy();
        }
        self.active = None;
        for o in &self.layout.outputs {
            let (output, info) = self.outputs.find(&o.name)?;
            let ptr = self.ptr_mgr.create_virtual_pointer_with_output(
                Some(&self.seat_proxy),
                Some(&output),
                &self.qh,
                (),
            );
            self.bound.push(Bound {
                name: o.name.clone(),
                output,
                ptr,
                mode: (info.width, info.height, info.scale, info.transform),
            });
        }
        Ok(())
    }

    fn active_ptr(&self) -> &ZwlrVirtualPointerV1 {
        match self.active {
            Some(i) => &self.bound[i].ptr,
            None => &self.unbound,
        }
    }

    fn destroy_retired_keyboards(&mut self) {
        if self.retired.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut destroyed = 0;
        self.retired.retain(|(since, keyboard)| {
            let keep = now.duration_since(*since) < RETIRED_KEYBOARD_GRACE;
            if !keep {
                keyboard.destroy();
                destroyed += 1;
            }
            keep
        });
        if destroyed > 0 {
            self.flush();
        }
    }

    /// A `wl_output` burst finished. Hyprland (0.56, and current main)
    /// silently stops delivering a bound virtual pointer's motion after the
    /// output's mode changes; a freshly created binding works. Re-bind when
    /// the physical mode, scale or transform changed. S8 control path only:
    /// the shipping unbound pointer needs nothing.
    fn on_output_done(&mut self, output: &WlOutput) {
        let Some(i) = self.bound.iter().position(|b| &b.output == output) else {
            return;
        };
        let Some((_, info)) = self.outputs.list.iter().find(|(o, _)| o == output) else {
            return;
        };
        let mode = (info.width, info.height, info.scale, info.transform);
        if mode == self.bound[i].mode {
            return;
        }
        self.bound[i].mode = mode;
        let ptr = self.ptr_mgr.create_virtual_pointer_with_output(
            Some(&self.seat_proxy),
            Some(output),
            &self.qh,
            (),
        );
        let old = std::mem::replace(&mut self.bound[i].ptr, ptr);
        old.destroy();
        if self.active == Some(i) {
            let t = now_ms();
            for button in self.held.buttons() {
                self.bound[i]
                    .ptr
                    .button(t, button, wl_pointer::ButtonState::Pressed);
            }
            self.bound[i].ptr.frame();
        }
        self.out_bytes += CMD_COST * 2;
        let name = self.bound[i].name.clone();
        tracing::info!(output = %name, ?mode, "bound output mode changed; virtual pointer re-bound");
        self.emit(InputEvent::Rebound(name));
    }

    /// `axis`, then `axis_source`, then `frame`. Hyprland 0.56.2 resets the
    /// pending event on `axis`, source included, so a source sent first is lost
    /// and the event goes out as a wheel.
    fn axis_raw(&mut self, axis: Axis, source: AxisSource, value: f64) {
        let t = now_ms();
        let p = self.active_ptr();
        p.axis(t, wl_axis(axis), value);
        p.axis_source(wl_source(source));
        p.frame();
    }
}

fn wl_source(source: AxisSource) -> wl_pointer::AxisSource {
    match source {
        AxisSource::Wheel => wl_pointer::AxisSource::Wheel,
        AxisSource::Finger => wl_pointer::AxisSource::Finger,
        AxisSource::Continuous => wl_pointer::AxisSource::Continuous,
    }
}

fn wl_axis(axis: Axis) -> wl_pointer::Axis {
    match axis {
        Axis::Vertical => wl_pointer::Axis::VerticalScroll,
        Axis::Horizontal => wl_pointer::Axis::HorizontalScroll,
    }
}

impl LoopState for State {
    type Cmd = InputCmd;

    fn on_cmd(&mut self, cmd: InputCmd) {
        match cmd {
            InputCmd::Stop => self.quit = true,
            // Drops queued input: whatever was waiting must not press a key
            // after the release.
            InputCmd::ReleaseAll => self.ring.restart_with([InputCmd::ReleaseAll]),
            cmd => self.enqueue(cmd),
        }
    }

    /// One flush per wake-up, after the channel and the Wayland queue were
    /// drained.
    fn after_dispatch(&mut self) {
        self.pump();
    }

    fn wants_write(&self) -> bool {
        self.blocked
    }

    fn on_writable(&mut self) {
        self.flush();
        self.pump();
    }

    fn stop(&mut self) {
        self.quit = true;
    }

    fn stop_requested(&self) -> bool {
        self.quit
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        s: &mut Self,
        o: &WlOutput,
        e: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if s.outputs.handle(o, e) {
            s.on_output_done(o);
            s.shared.wake_layout();
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        s: &mut Self,
        _: &WlSeat,
        e: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        s.seat.handle(e);
    }
}

impl Dispatch<WlCallback, mpsc::Sender<()>> for State {
    fn event(
        _: &mut Self,
        _: &WlCallback,
        e: wl_callback::Event,
        tx: &mpsc::Sender<()>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = e {
            let _ = tx.send(());
        }
    }
}

delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ZwpVirtualKeyboardV1);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;
    use wayland_client::Proxy;

    use crate::ring::RING_CAP;

    struct Registry;
    impl Dispatch<WlRegistry, ()> for Registry {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: wayland_client::protocol::wl_registry::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    /// One decoded request: object id, opcode and its argument words.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Req {
        id: u32,
        opcode: u16,
        args: Vec<u32>,
    }

    /// A `State` on a socket pair. The peer only records requests: no
    /// compositor is needed, and nothing touches a real display.
    struct Mock {
        state: State,
        peer: UnixStream,
        conn: Connection,
        events: Arc<Mutex<Vec<String>>>,
        inbox: Vec<u8>,
        _registry_queue: wayland_client::EventQueue<Registry>,
    }

    impl Mock {
        fn new() -> Self {
            let (socket, peer) = UnixStream::pair().unwrap();
            peer.set_nonblocking(true).unwrap();
            let conn = Connection::from_socket(socket).unwrap();
            let registry_queue = conn.new_event_queue::<Registry>();
            let registry = conn.display().get_registry(&registry_queue.handle(), ());
            let queue = conn.new_event_queue::<State>();
            let qh = queue.handle();
            let kb_mgr: ZwpVirtualKeyboardManagerV1 = registry.bind(1, 1, &qh, ());
            let seat_proxy: WlSeat = registry.bind(2, 1, &qh, ());
            let ptr_mgr: ZwlrVirtualPointerManagerV1 = registry.bind(3, 2, &qh, ());
            let mut state = State::new(
                Arc::new(Shared::default()),
                conn.clone(),
                Outputs::default(),
                Seat::default(),
                qh,
                kb_mgr,
                seat_proxy,
                ptr_mgr,
                PointerMode::Unbound,
                Layout::default(),
                KeyState::default_us().unwrap(),
            )
            .unwrap();
            let events = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&events);
            state.sink = Box::new(move |e| log.lock().unwrap().push(format!("{e:?}")));
            conn.flush().unwrap();
            Self {
                state,
                peer,
                conn,
                events,
                inbox: Vec::new(),
                _registry_queue: registry_queue,
            }
        }

        /// What the compositor has received so far, in order.
        fn requests(&mut self) -> Vec<Req> {
            let mut buf = [0; 65536];
            loop {
                match self.peer.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => self.inbox.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("read Wayland requests: {e}"),
                }
            }
            let mut out = Vec::new();
            let mut at = 0;
            while at + 8 <= self.inbox.len() {
                let word = |o: usize| u32::from_ne_bytes(self.inbox[o..o + 4].try_into().unwrap());
                let header = word(at + 4);
                let size = (header >> 16) as usize;
                if at + size > self.inbox.len() {
                    break;
                }
                out.push(Req {
                    id: word(at),
                    opcode: (header & 0xffff) as u16,
                    args: (8..size).step_by(4).map(|o| word(at + o)).collect(),
                });
                at += size;
            }
            self.inbox.drain(..at);
            out
        }

        /// Run one wake-up: the commands, then the loop's `after_dispatch`.
        fn wake(&mut self, cmds: impl IntoIterator<Item = InputCmd>) {
            for c in cmds {
                self.state.on_cmd(c);
            }
            self.state.after_dispatch();
        }

        /// Let the compositor drain, then the loop's write-ready callback.
        fn drain_and_resume(&mut self) -> Vec<Req> {
            let mut all = Vec::new();
            for _ in 0..1000 {
                all.extend(self.requests());
                if !self.state.blocked && self.state.ring_is_empty() {
                    break;
                }
                self.state.on_writable();
            }
            all.extend(self.requests());
            all
        }

        fn keyboard_id(&self) -> u32 {
            self.state.keyboard.id().protocol_id()
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl State {
        fn ring_is_empty(&self) -> bool {
            self.ring.len() == 0
        }
    }

    /// `zwp_virtual_keyboard_v1` requests: keymap 0, key 1, modifiers 2, destroy 3.
    const KB_KEY: u16 = 1;
    const KB_MODIFIERS: u16 = 2;

    fn key_edges(reqs: &[Req], keyboard: u32) -> Vec<(u32, bool)> {
        reqs.iter()
            .filter(|r| r.id == keyboard && r.opcode == KB_KEY)
            .map(|r| (r.args[1], r.args[2] != 0))
            .collect()
    }

    fn key(code: u32, pressed: bool) -> InputCmd {
        InputCmd::Key { code, pressed }
    }

    /// Replay every key request into per-keyboard held sets, as the
    /// compositor's seat would.
    fn compositor_held(reqs: &[Req]) -> BTreeMap<u32, BTreeSet<u32>> {
        let mut held: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        for r in reqs.iter().filter(|r| r.opcode == KB_KEY) {
            // Only keyboard objects send opcode 1 with three args (time, key, state).
            if r.args.len() == 3 {
                let set = held.entry(r.id).or_default();
                if r.args[2] != 0 {
                    set.insert(r.args[1]);
                } else {
                    set.remove(&r.args[1]);
                }
            }
        }
        held
    }

    /// Push key edges until a flush hits `WouldBlock`. The peer does not read,
    /// so the socket buffer fills.
    fn fill_socket(m: &mut Mock) {
        for i in 0..200_000u32 {
            if m.state.blocked {
                return;
            }
            // Codes 200+ are no modifier and have no side effect in the keymap.
            m.wake([key(200 + i % 40, i % 2 == 0)]);
            // Release the pair's other half so the held set stays small.
        }
        panic!("the socket never filled");
    }

    #[test]
    fn a_new_keyboard_resets_the_modifiers_before_any_forwarded_key() {
        let mut m = Mock::new();
        let kb = m.keyboard_id();
        m.wake([key(30, true), key(30, false)]);
        let reqs = m.drain_and_resume();
        let on_kb: Vec<&Req> = reqs.iter().filter(|r| r.id == kb).collect();
        // keymap, then eight releases in keycode order, then modifiers(0,0,0,0),
        // and only then the forwarded key.
        assert_eq!(on_kb[0].opcode, 0, "keymap first");
        let releases: Vec<(u32, u32)> = on_kb[1..9]
            .iter()
            .map(|r| {
                assert_eq!(r.opcode, KB_KEY);
                (r.args[1], r.args[2])
            })
            .collect();
        let expected: Vec<(u32, u32)> = MODIFIER_KEYCODES.iter().map(|&c| (c, 0)).collect();
        assert_eq!(releases, expected);
        assert_eq!(on_kb[9].opcode, KB_MODIFIERS);
        assert_eq!(on_kb[9].args, [0, 0, 0, 0]);
        assert_eq!(
            (on_kb[10].opcode, on_kb[10].args[1], on_kb[10].args[2]),
            (KB_KEY, 30, 1)
        );
        assert_eq!(MODIFIER_KEYCODES, [29, 97, 42, 54, 56, 100, 125, 126]);
    }

    #[test]
    fn held_keys_move_to_the_replacement_keyboard() {
        let mut m = Mock::new();
        let old_id = m.keyboard_id();
        m.wake([key(42, true), key(16, true)]);
        m.drain_and_resume();
        let text = crate::keymap_from_names(&crate::KeymapNames {
            layout: "fr".into(),
            ..Default::default()
        })
        .unwrap();
        m.wake([InputCmd::SetKeymap(text)]);
        let new_id = m.keyboard_id();
        assert_ne!(old_id, new_id, "a keymap change makes a new keyboard");
        let reqs = m.drain_and_resume();
        // The new keyboard starts with its reset, and a keymap is uploaded to
        // it exactly once; the old keyboard never sees a second keymap.
        assert_eq!(
            reqs.iter()
                .filter(|r| r.id == new_id && r.opcode == 0)
                .count(),
            1
        );
        assert_eq!(
            reqs.iter()
                .filter(|r| r.id == old_id && r.opcode == 0)
                .count(),
            0
        );
        let reset: Vec<(u32, bool)> = key_edges(&reqs, new_id)
            .into_iter()
            .take(MODIFIER_KEYCODES.len())
            .collect();
        assert!(reset.iter().all(|(_, pressed)| !pressed));
        // Old keyboard: 16 and 42 released, in code order; new: replayed.
        assert_eq!(key_edges(&reqs, old_id), [(16, false), (42, false)]);
        assert_eq!(
            key_edges(&reqs, new_id)[MODIFIER_KEYCODES.len()..],
            [(16, true), (42, true)]
        );
        m.wake([key(16, false), key(42, false)]);
        let reqs = m.drain_and_resume();
        assert_eq!(key_edges(&reqs, new_id), [(16, false), (42, false)]);
        // The same keymap again is a no-op, not a third keyboard.
        let same = m.state.keys.text.clone();
        m.wake([InputCmd::SetKeymap(same)]);
        assert_eq!(m.keyboard_id(), new_id);
        m.state.retired[0].0 -= RETIRED_KEYBOARD_GRACE;
        m.state.destroy_retired_keyboards();
        m.conn.flush().unwrap();
        let reqs = m.requests();
        assert!(
            reqs.iter().any(|r| r.id == old_id && r.opcode == 3),
            "old keyboard destroyed"
        );
        assert!(m.state.retired.is_empty());
        assert_eq!(m.state.shared.counters.keyboards.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_bad_keymap_is_a_typed_rejection_and_changes_nothing() {
        let mut m = Mock::new();
        let kb = m.keyboard_id();
        m.wake([InputCmd::SetKeymap("not a keymap".into())]);
        assert_eq!(m.events().len(), 1);
        assert!(m.events()[0].starts_with("KeymapRejected"));
        assert_eq!(m.keyboard_id(), kb, "the old keyboard stays");
    }

    #[test]
    fn a_non_finite_motion_or_axis_is_dropped_and_counted() {
        let mut m = Mock::new();
        m.drain_and_resume();
        m.wake([
            InputCmd::Motion {
                x: f64::NAN,
                y: 10.0,
            },
            InputCmd::MotionRel {
                dx: 1.0,
                dy: f64::INFINITY,
            },
            InputCmd::Axis {
                axis: Axis::Vertical,
                source: AxisSource::Finger,
                value: f64::NEG_INFINITY,
                value120: None,
            },
        ]);
        assert_eq!(m.drain_and_resume(), vec![]);
        assert_eq!(
            m.state
                .shared
                .counters
                .nonfinite_dropped
                .load(Ordering::Relaxed),
            3
        );
        // A wheel step travels as `value120`; its `value` is not used.
        m.wake([InputCmd::Axis {
            axis: Axis::Vertical,
            source: AxisSource::Wheel,
            value: f64::NAN,
            value120: Some(120),
        }]);
        assert!(!m.drain_and_resume().is_empty());
        m.wake([InputCmd::Motion { x: 5.0, y: 5.0 }]);
        assert!(!m.drain_and_resume().is_empty());
        assert_eq!(
            m.state
                .shared
                .counters
                .nonfinite_dropped
                .load(Ordering::Relaxed),
            3
        );
    }

    #[test]
    fn axis_stop_carries_its_source() {
        let mut m = Mock::new();
        m.wake([
            InputCmd::AxisStop {
                axis: Axis::Vertical,
                source: AxisSource::Continuous,
            },
            InputCmd::AxisStop {
                axis: Axis::Horizontal,
                source: AxisSource::Finger,
            },
        ]);
        let reqs = m.drain_and_resume();
        // zwlr_virtual_pointer_v1: axis_source is opcode 5 (axis_source), value
        // wl_pointer axis_source: wheel 0, finger 1, continuous 2.
        let sources: Vec<u32> = reqs
            .iter()
            .filter(|r| r.opcode == 5 && r.args.len() == 1)
            .map(|r| r.args[0])
            .collect();
        assert_eq!(sources, [2, 1]);
    }

    #[test]
    fn release_all_releases_exactly_what_this_crate_holds() {
        let mut m = Mock::new();
        let kb = m.keyboard_id();
        m.wake([
            key(125, true),
            key(42, true),
            key(30, true),
            key(30, false),
            InputCmd::Button {
                button: 0x110,
                pressed: true,
            },
        ]);
        m.drain_and_resume();
        assert_eq!(m.state.held.key_count(), 2);
        m.wake([InputCmd::ReleaseAll]);
        let reqs = m.drain_and_resume();
        assert_eq!(key_edges(&reqs, kb), [(42, false), (125, false)]);
        let mods = reqs
            .iter()
            .rfind(|r| r.id == kb && r.opcode == KB_MODIFIERS)
            .unwrap();
        assert_eq!(mods.args, [0, 0, 0, 0]);
        assert!(m.state.held.is_empty());
        // A second release has nothing to release but still zeroes the modifiers.
        m.wake([InputCmd::ReleaseAll]);
        let reqs = m.drain_and_resume();
        assert!(key_edges(&reqs, kb).is_empty());
        assert!(reqs.iter().any(|r| r.id == kb && r.opcode == KB_MODIFIERS));
    }

    #[test]
    fn a_blocked_socket_queues_keys_in_order_and_coalesces_motion() {
        let mut m = Mock::new();
        let kb = m.keyboard_id();
        fill_socket(&mut m);
        assert!(m.state.blocked);
        let sent_before = m.state.shared.counters.would_block.load(Ordering::Relaxed);
        assert_eq!(sent_before, 1);
        // Everything the socket could not take: a key sequence with motion
        // between the keys.
        let codes: Vec<u32> = (1..=60).collect();
        for &c in &codes {
            m.wake([
                key(c, true),
                InputCmd::MotionRel { dx: 1.0, dy: 0.0 },
                InputCmd::MotionRel { dx: 1.0, dy: 0.0 },
                key(c, false),
            ]);
        }
        assert!(m.state.blocked);
        assert_eq!(m.state.ring.len(), 60 * 3, "key, one merged motion, key");
        assert_eq!(
            m.state.shared.counters.coalesced.load(Ordering::Relaxed),
            60
        );
        assert_eq!(
            m.events(),
            Vec::<String>::new(),
            "no overflow, nothing dropped"
        );
        let reqs = m.drain_and_resume();
        assert!(!m.state.blocked);
        let edges = key_edges(&reqs, kb);
        let tail: Vec<(u32, bool)> = edges[edges.len() - 120..].to_vec();
        let expect: Vec<(u32, bool)> = codes
            .iter()
            .flat_map(|&c| [(c, true), (c, false)])
            .collect();
        assert_eq!(tail, expect, "every queued key arrived, in order");
        assert_eq!(
            m.state.held.key_count() as u32,
            compositor_held(&reqs)[&kb].len() as u32
        );
        // Motion arrived merged: 1 + 1 px per key became one 2 px request
        // (`wl_fixed` 24.8, so 2.0 is 512).
        let merged = reqs
            .iter()
            .filter(|r| r.id != kb && r.opcode == 0 && r.args.len() == 3 && r.args[1] == 512)
            .count();
        assert_eq!(merged, 60);
    }

    #[test]
    fn a_ring_overflow_is_a_logged_release_not_a_stuck_key() {
        let mut m = Mock::new();
        let old_kb = m.keyboard_id();
        // Hold two keys, then block the socket.
        m.wake([key(125, true), key(42, true)]);
        m.requests();
        fill_socket(&mut m);
        // More keys than the ring takes while nothing can be written.
        for c in 1..=(RING_CAP as u32 + 10) {
            m.wake([key(c % 100 + 1, true), key(c % 100 + 1, false)]);
        }
        let events = m.events();
        assert!(
            events.iter().any(|e| e.starts_with("Overflow")),
            "{events:?}"
        );
        assert!(m.state.shared.counters.overflows.load(Ordering::Relaxed) >= 1);
        let reqs = m.drain_and_resume();
        assert!(!m.state.blocked);
        // The socket drained: every key this crate pressed is released on
        // every keyboard, and the devices were replaced.
        let held = compositor_held(&reqs);
        for (kb, keys) in &held {
            assert!(keys.is_empty(), "keyboard {kb} still holds {keys:?}");
        }
        assert_ne!(m.keyboard_id(), old_kb, "devices rebuilt");
        assert!(m.state.held.is_empty());
        // The held modifiers were released on the old keyboard.
        assert!(key_edges(&reqs, old_kb).contains(&(125, false)));
        assert!(key_edges(&reqs, old_kb).contains(&(42, false)));
    }

    #[test]
    fn a_full_command_channel_makes_the_thread_release() {
        let mut m = Mock::new();
        m.wake([key(125, true), key(42, true)]);
        m.requests();
        // What `Input::send` does on a full channel.
        m.state.shared.overflow.store(true, Ordering::Relaxed);
        m.state.after_dispatch();
        let reqs = m.drain_and_resume();
        assert!(m.events().iter().any(|e| e.starts_with("Overflow")));
        for keys in compositor_held(&reqs).values() {
            assert!(keys.is_empty());
        }
        assert!(m.state.held.is_empty());
    }

    #[test]
    fn a_release_all_in_the_ring_drops_queued_presses_but_answers_syncs() {
        let mut m = Mock::new();
        let kb = m.keyboard_id();
        m.wake([key(125, true)]);
        m.requests();
        fill_socket(&mut m);
        let (tx, rx) = mpsc::channel();
        m.wake([
            key(30, true),
            InputCmd::Sync(tx),
            key(31, true),
            InputCmd::ReleaseAll,
        ]);
        assert_eq!(m.state.ring.len(), 2, "release_all, then the sync");
        let reqs = m.drain_and_resume();
        let edges = key_edges(&reqs, kb);
        assert!(!edges.contains(&(30, true)) && !edges.contains(&(31, true)));
        assert!(compositor_held(&reqs)[&kb].is_empty());
        // The sync request reached the wire (the display object is id 1).
        assert!(reqs.iter().any(|r| r.id == 1 && r.opcode == 0));
        drop(rx);
    }

    #[test]
    fn the_request_budget_flushes_before_the_buffer_can_overflow() {
        let mut m = Mock::new();
        // A burst far larger than one 4096-byte buffer in one wake-up must not
        // lose a request or raise a connection error.
        let n = 3000u32;
        for i in 0..n {
            m.state.on_cmd(key(200 + i % 40, i % 2 == 0));
        }
        m.state.after_dispatch();
        let reqs = m.drain_and_resume();
        let kb = m.keyboard_id();
        assert_eq!(
            key_edges(&reqs, kb).len(),
            n as usize + MODIFIER_KEYCODES.len()
        );
        assert!(m.state.fatal.is_none());
        assert_eq!(m.events(), Vec::<String>::new());
    }
}
