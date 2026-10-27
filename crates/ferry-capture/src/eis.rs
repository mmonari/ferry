//! The EIS receiver, a calloop source for the `capture` thread (design note
//! §9). It owns the reis context on the fd from [`crate::Portal::open`], runs
//! the ei handshake, and turns EIS events into [`SourceEvent`]s:
//!
//! | EIS | `SourceEvent` |
//! |---|---|
//! | `start_emulating` on the first device | `StartEmulating` (the activation edge) |
//! | `stop_emulating` on the last device | `StopEmulating` |
//! | pointer motion | `Motion{dx, dy}` |
//! | key | `Key{code, pressed, t_eis}`, `t_eis` the EIS frame time (`CLOCK_MONOTONIC` µs) |
//! | button | `Button{code, pressed}` |
//! | scroll discrete / delta / stop / cancel | `Scroll(ScrollIn)`, one per axis per frame |
//! | keyboard `DeviceAdded` with a keymap | `Keymap{text}` (every time: S1 saw it re-sent) |
//! | end of one read batch | `BatchEnd` (S17: the repeat filter decides there) |
//!
//! When the stream ends (EOF, `Disconnected`, a protocol or IO error) the
//! source emits [`EisOutput::Ended`] once and removes itself. ctl then
//! re-creates the portal session.
//!
//! One source per session. When ctl re-creates the session for another reason
//! (xdph restarted), the old session's EIS also ends, usually *after* the new
//! session exists (seen in the lab: owner change, then `Disconnected` 6 ms
//! later). So the capture thread removes the old source when a new fd arrives,
//! and tags each end it reports with the fd's generation; ctl ignores an end
//! whose generation is not the current one. Otherwise one restart costs two
//! sessions (and two leaked `eis-N.lock`s, xdph #419).
//!
//! Hyprland's EIS carries no scroll source. A frame with a discrete part is a
//! wheel; one with only a delta is reported as finger; a stop or cancel is a
//! finger stop.
//!
//! **Wheel value120.** Hyprland 0.56.2 sends one notch as `scroll_discrete`
//! **15** with `scroll_delta` 15 (lab VM `a`, 2026-10-16), although libei
//! defines the discrete value as value120 (120 per notch). So a wheel frame's
//! value120 is taken from its delta, × 8 (Hyprland's own value-to-value120
//! rule, S9), and the discrete value is used only when no delta came with it.
//! This stays right if Hyprland starts sending real value120 alongside the
//! same delta.

use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;

use calloop::generic::Generic;
use calloop::{EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory};
use ferry_core::Micros;
use ferry_core::source::{ScrollIn, SourceEvent};
use reis::PendingRequestResult;
use reis::ei;
use reis::event::{DeviceCapability, EiEvent, EiEventConverter};
use reis::handshake::EiHandshaker;

/// The name ferry gives itself in the ei handshake.
pub const EI_CLIENT_NAME: &str = "ferry";

/// What the source hands its callback.
#[derive(Debug, Clone, PartialEq)]
pub enum EisOutput {
    Event(SourceEvent),
    /// The keyboard stopped emulating, or was removed, while another device (the pointer)
    /// still emulates: Hyprland re-created the EIS keyboard during a capture (it does when its
    /// active keyboard changes, S1). It sends no `start_emulating` for the new keyboard until
    /// the next activation, so no key reaches ferry for the rest of this one. Seen in lab VM `c`
    /// on a modifier press mid-capture (2026-10-16). The owner should end the crossing.
    KeyboardLost,
    /// The EIS stream is over; the source has removed itself. Re-create the
    /// portal session.
    Ended(EisEnd),
}

/// Why the EIS stream ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EisEnd {
    /// The socket hit end-of-file.
    Eof,
    /// The EIS server sent `disconnected`.
    Disconnected {
        reason: String,
        explanation: Option<String>,
    },
    /// A socket or protocol error.
    Error(String),
}

enum Phase {
    Handshake(EiHandshaker<'static>),
    Running(Box<EiEventConverter>),
    Ended,
}

/// A calloop [`EventSource`] that reads the EIS socket. `Event` is
/// [`EisOutput`].
pub struct EisSource {
    ctx: ei::Context,
    socket: Generic<ei::Context>,
    phase: Phase,
    tr: Translator<reis::event::Device>,
    /// Scratch for one batch, kept to avoid allocating per wake-up.
    batch: Vec<SourceEvent>,
    /// A keyboard stopped or went away in this batch.
    keyboard_gone: bool,
}

impl std::fmt::Debug for EisSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let phase = match self.phase {
            Phase::Handshake(_) => "handshake",
            Phase::Running(_) => "running",
            Phase::Ended => "ended",
        };
        f.debug_struct("EisSource")
            .field("phase", &phase)
            .finish_non_exhaustive()
    }
}

impl EisSource {
    /// Wrap the EIS socket from `ConnectToEIS`. The handshake runs inside the
    /// event loop, so this never blocks.
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        let ctx = ei::Context::new(UnixStream::from(fd))?;
        let socket = Generic::new(ctx.clone(), Interest::READ, Mode::Level);
        Ok(Self {
            ctx,
            socket,
            phase: Phase::Handshake(EiHandshaker::new(
                EI_CLIENT_NAME,
                ei::handshake::ContextType::Receiver,
            )),
            tr: Translator::default(),
            batch: Vec::with_capacity(64),
            keyboard_gone: false,
        })
    }

    /// Read everything available, convert it, and push the events of this
    /// batch to `self.batch`. Returns how the stream ended, if it did.
    fn read_batch(&mut self) -> Option<EisEnd> {
        let mut end = match self.ctx.read() {
            Ok(_) => None,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Some(EisEnd::Eof),
            Err(e) => Some(EisEnd::Error(format!("read: {e}"))),
        };
        // Events read before an EOF still count.
        while let Some(pending) = self.ctx.pending_event() {
            let ev = match pending {
                PendingRequestResult::Request(ev) => ev,
                PendingRequestResult::ParseError(e) => {
                    end.get_or_insert(EisEnd::Error(format!("parse: {e}")));
                    break;
                }
                // An event for an object already destroyed: harmless.
                PendingRequestResult::InvalidObject(_) => continue,
            };
            match &mut self.phase {
                Phase::Handshake(h) => match h.handle_event(ev) {
                    Ok(Some(resp)) => {
                        tracing::debug!(serial = resp.serial, "ei handshake done");
                        self.phase =
                            Phase::Running(Box::new(EiEventConverter::new(&self.ctx, resp)));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        end.get_or_insert(EisEnd::Error(format!("handshake: {e}")));
                        break;
                    }
                },
                Phase::Running(conv) => {
                    if let Err(e) = conv.handle_event(ev) {
                        end.get_or_insert(EisEnd::Error(format!("event: {e}")));
                        break;
                    }
                    while let Some(ev) = conv.next_event() {
                        if let EiEvent::DeviceStopEmulating(reis::event::DeviceStopEmulating {
                            device,
                            ..
                        })
                        | EiEvent::DeviceRemoved(reis::event::DeviceRemoved { device }) = &ev
                            && device.has_capability(DeviceCapability::Keyboard)
                            && self.tr.emulating.contains(device)
                        {
                            self.keyboard_gone = true;
                        }
                        if let Some(raw) = raw_of(&self.ctx, ev, &mut end) {
                            self.tr.feed(raw, &mut self.batch);
                        }
                    }
                }
                Phase::Ended => break,
            }
        }
        self.tr.end_batch(&mut self.batch);
        end
    }
}

/// Turn one reis event into the translator's input, handling the parts that
/// need the connection (seat binding, keymap reading).
fn raw_of(
    ctx: &ei::Context,
    ev: EiEvent,
    end: &mut Option<EisEnd>,
) -> Option<Raw<reis::event::Device>> {
    // Device lifecycle, for diagnosis: Hyprland re-creates the keyboard when its active
    // keyboard changes and after resume, also during a capture.
    match &ev {
        EiEvent::DeviceAdded(d) => tracing::debug!(
            device = d.device.name().unwrap_or("?"),
            keymap = d.device.keymap().is_some(),
            "EIS device added"
        ),
        EiEvent::DeviceRemoved(d) => tracing::debug!(
            device = d.device.name().unwrap_or("?"),
            "EIS device removed"
        ),
        EiEvent::DeviceResumed(d) => tracing::debug!(
            device = d.device.name().unwrap_or("?"),
            "EIS device resumed"
        ),
        EiEvent::DevicePaused(d) => {
            tracing::debug!(device = d.device.name().unwrap_or("?"), "EIS device paused")
        }
        EiEvent::DeviceStartEmulating(d) => tracing::debug!(
            device = d.device.name().unwrap_or("?"),
            sequence = d.sequence,
            "EIS start_emulating"
        ),
        EiEvent::DeviceStopEmulating(d) => tracing::debug!(
            device = d.device.name().unwrap_or("?"),
            "EIS stop_emulating"
        ),
        _ => {}
    }
    Some(match ev {
        EiEvent::SeatAdded(s) => {
            s.seat.bind_capabilities(
                DeviceCapability::Pointer
                    | DeviceCapability::PointerAbsolute
                    | DeviceCapability::Keyboard
                    | DeviceCapability::Scroll
                    | DeviceCapability::Button,
            );
            if let Err(e) = ctx.flush() {
                end.get_or_insert(EisEnd::Error(format!("flush: {e}")));
            }
            return None;
        }
        EiEvent::DeviceAdded(d) => {
            let km = d.device.keymap()?;
            if km.type_ != ei::keyboard::KeymapType::Xkb {
                tracing::warn!(kind = ?km.type_, "EIS keymap is not XKB; ignored");
                return None;
            }
            let file = km.fd.try_clone().map(std::fs::File::from);
            match file.and_then(|f| read_keymap(&f, km.size)) {
                Ok(text) => Raw::Keymap(text),
                Err(e) => {
                    tracing::warn!(error = %e, size = km.size, "EIS keymap unreadable");
                    return None;
                }
            }
        }
        EiEvent::DeviceRemoved(d) => Raw::Removed(d.device),
        EiEvent::DeviceStartEmulating(d) => Raw::Start(d.device),
        EiEvent::DeviceStopEmulating(d) => Raw::Stop(d.device),
        EiEvent::PointerMotion(m) => Raw::Motion { dx: m.dx, dy: m.dy },
        EiEvent::Button(b) => Raw::Button {
            code: b.button,
            pressed: b.state == ei::button::ButtonState::Press,
        },
        EiEvent::KeyboardKey(k) => Raw::Key {
            code: k.key,
            pressed: k.state == ei::keyboard::KeyState::Press,
            time: k.time,
        },
        EiEvent::ScrollDelta(s) => Raw::ScrollDelta { dx: s.dx, dy: s.dy },
        EiEvent::ScrollDiscrete(s) => Raw::ScrollDiscrete {
            dx: s.discrete_dx,
            dy: s.discrete_dy,
        },
        EiEvent::ScrollStop(s) => Raw::ScrollStop { x: s.x, y: s.y },
        EiEvent::ScrollCancel(s) => Raw::ScrollStop { x: s.x, y: s.y },
        EiEvent::Frame(_) => Raw::Frame,
        EiEvent::Disconnected(d) => {
            end.get_or_insert(EisEnd::Disconnected {
                reason: format!("{:?}", d.reason),
                explanation: d.explanation,
            });
            return None;
        }
        // Seat removal, pause/resume, modifiers (none arrive with
        // capture_modifiers = false, S1), absolute motion and touch carry
        // nothing the source machine uses.
        _ => return None,
    })
}

/// The keymap text from its memfd of `size` bytes (NUL padding stripped).
/// A size over the frame cap is refused before anything is allocated: a
/// confused EIS server that says 4 GiB would otherwise abort ferryd on the
/// allocation, past the panic hook. Such a keymap could not be sent
/// to a peer anyway.
fn read_keymap(file: &std::fs::File, size: u32) -> io::Result<String> {
    let size = usize::try_from(size)
        .ok()
        .filter(|&n| n <= ferry_core::wire::MAX_FRAME_BODY)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "keymap over 1 MiB"))?;
    let mut buf = vec![0u8; size];
    let mut off = 0;
    while off < buf.len() {
        let n = file.read_at(&mut buf[off..], off as u64)?;
        if n == 0 {
            break;
        }
        off += n;
    }
    buf.truncate(off);
    while buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

impl EventSource for EisSource {
    type Event = EisOutput;
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        if matches!(self.phase, Phase::Ended) {
            return Ok(PostAction::Remove);
        }
        let mut readable = false;
        self.socket.process_events(readiness, token, |_, _| {
            readable = true;
            Ok(PostAction::Continue)
        })?;
        if !readable {
            return Ok(PostAction::Continue);
        }
        let end = self.read_batch();
        for ev in self.batch.drain(..) {
            callback(EisOutput::Event(ev), &mut ());
        }
        // Still emulating (the pointer), but without the keyboard it started with.
        if std::mem::take(&mut self.keyboard_gone) && !self.tr.emulating.is_empty() {
            callback(EisOutput::KeyboardLost, &mut ());
        }
        match end {
            Some(end) => {
                self.phase = Phase::Ended;
                callback(EisOutput::Ended(end), &mut ());
                Ok(PostAction::Remove)
            }
            None => Ok(PostAction::Continue),
        }
    }

    fn register(&mut self, poll: &mut Poll, tf: &mut TokenFactory) -> calloop::Result<()> {
        self.socket.register(poll, tf)
    }

    fn reregister(&mut self, poll: &mut Poll, tf: &mut TokenFactory) -> calloop::Result<()> {
        self.socket.reregister(poll, tf)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.socket.unregister(poll)
    }
}

/// One EIS event, reduced to what the translator needs. `D` identifies a
/// device (reis' `Device` in the source, a plain value in tests).
#[derive(Debug, Clone, PartialEq)]
enum Raw<D> {
    Start(D),
    Stop(D),
    Removed(D),
    Motion { dx: f32, dy: f32 },
    Button { code: u32, pressed: bool },
    Key { code: u32, pressed: bool, time: u64 },
    ScrollDelta { dx: f32, dy: f32 },
    ScrollDiscrete { dx: i32, dy: i32 },
    ScrollStop { x: bool, y: bool },
    Frame,
    Keymap(String),
}

/// The pure part of the receiver: per-device emulation state folded into one
/// activation edge, and scroll folded per frame and axis.
#[derive(Debug)]
struct Translator<D> {
    /// Devices between `start_emulating` and `stop_emulating`. Hyprland starts
    /// the pointer and the keyboard with the same sequence.
    emulating: Vec<D>,
    scroll: [AxisAcc; 2],
    /// Input events emitted in the current batch (decides `BatchEnd`).
    in_batch: bool,
}

impl<D> Default for Translator<D> {
    fn default() -> Self {
        Self {
            emulating: Vec::new(),
            scroll: [AxisAcc::default(); 2],
            in_batch: false,
        }
    }
}

/// One scroll axis within a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct AxisAcc {
    value: f32,
    value120: i32,
    stop: bool,
}

/// Wire codes (`ScrollIn`).
const AXIS_VERTICAL: u8 = 0;
const AXIS_HORIZONTAL: u8 = 1;
const SOURCE_WHEEL: u8 = 0;
const SOURCE_FINGER: u8 = 1;

impl<D: PartialEq> Translator<D> {
    fn feed(&mut self, raw: Raw<D>, out: &mut Vec<SourceEvent>) {
        match raw {
            Raw::Start(d) => {
                if !self.emulating.contains(&d) {
                    self.emulating.push(d);
                    if self.emulating.len() == 1 {
                        self.emit(SourceEvent::StartEmulating, out);
                    }
                }
            }
            Raw::Stop(d) | Raw::Removed(d) => {
                if let Some(i) = self.emulating.iter().position(|e| *e == d) {
                    self.emulating.swap_remove(i);
                    if self.emulating.is_empty() {
                        self.flush_scroll(out);
                        self.emit(SourceEvent::StopEmulating, out);
                    }
                }
            }
            Raw::Motion { dx, dy } => self.emit(SourceEvent::Motion { dx, dy }, out),
            Raw::Button { code, pressed } => match u16::try_from(code) {
                Ok(code) => self.emit(SourceEvent::Button { code, pressed }, out),
                Err(_) => tracing::warn!(code, "EIS button code out of range"),
            },
            Raw::Key {
                code,
                pressed,
                time,
            } => match u16::try_from(code) {
                Ok(code) => self.emit(
                    SourceEvent::Key {
                        code,
                        pressed,
                        t_eis: Micros(time),
                    },
                    out,
                ),
                Err(_) => tracing::warn!(code, "EIS key code out of range"),
            },
            Raw::ScrollDelta { dx, dy } => {
                self.scroll[usize::from(AXIS_VERTICAL)].value += dy;
                self.scroll[usize::from(AXIS_HORIZONTAL)].value += dx;
            }
            // Saturating: two huge discrete values in one frame must not
            // wrap into a scroll the other way.
            Raw::ScrollDiscrete { dx, dy } => {
                let v = &mut self.scroll[usize::from(AXIS_VERTICAL)].value120;
                *v = v.saturating_add(dy);
                let h = &mut self.scroll[usize::from(AXIS_HORIZONTAL)].value120;
                *h = h.saturating_add(dx);
            }
            Raw::ScrollStop { x, y } => {
                self.scroll[usize::from(AXIS_VERTICAL)].stop |= y;
                self.scroll[usize::from(AXIS_HORIZONTAL)].stop |= x;
            }
            Raw::Frame => self.flush_scroll(out),
            Raw::Keymap(text) => self.emit(SourceEvent::Keymap { text }, out),
        }
    }

    fn emit(&mut self, ev: SourceEvent, out: &mut Vec<SourceEvent>) {
        self.in_batch = true;
        out.push(ev);
    }

    fn flush_scroll(&mut self, out: &mut Vec<SourceEvent>) {
        for axis in [AXIS_VERTICAL, AXIS_HORIZONTAL] {
            let acc = std::mem::take(&mut self.scroll[usize::from(axis)]);
            if acc.value120 != 0 {
                // A wheel. See the module note: the delta is the reliable
                // part; Hyprland's discrete value is not value120.
                let (value, value120) = if acc.value != 0.0 {
                    (acc.value, (acc.value * 8.0).round() as i32)
                } else {
                    (acc.value120 as f32 * 15.0 / 120.0, acc.value120)
                };
                self.emit(
                    SourceEvent::Scroll(ScrollIn {
                        axis,
                        source: SOURCE_WHEEL,
                        value,
                        value120: Some(value120),
                        stop: false,
                    }),
                    out,
                );
            } else if acc.value != 0.0 {
                self.emit(
                    SourceEvent::Scroll(ScrollIn {
                        axis,
                        source: SOURCE_FINGER,
                        value: acc.value,
                        value120: None,
                        stop: false,
                    }),
                    out,
                );
            }
            if acc.stop {
                self.emit(
                    SourceEvent::Scroll(ScrollIn {
                        axis,
                        source: SOURCE_FINGER,
                        value: 0.0,
                        value120: None,
                        stop: true,
                    }),
                    out,
                );
            }
        }
    }

    /// The read batch is drained: flush a frame-less scroll, then `BatchEnd`
    /// if this batch carried any event.
    fn end_batch(&mut self, out: &mut Vec<SourceEvent>) {
        self.flush_scroll(out);
        if std::mem::take(&mut self.in_batch) {
            out.push(SourceEvent::BatchEnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(raws: Vec<Raw<u8>>) -> Vec<SourceEvent> {
        let mut tr = Translator::<u8>::default();
        let mut out = Vec::new();
        for r in raws {
            tr.feed(r, &mut out);
        }
        tr.end_batch(&mut out);
        out
    }

    #[test]
    fn one_activation_edge_for_two_devices() {
        let out = run(vec![
            Raw::Start(1),
            Raw::Start(2),
            Raw::Motion { dx: 40.0, dy: 0.0 },
            Raw::Frame,
            Raw::Stop(1),
            Raw::Stop(2),
        ]);
        assert_eq!(
            out,
            vec![
                SourceEvent::StartEmulating,
                SourceEvent::Motion { dx: 40.0, dy: 0.0 },
                SourceEvent::StopEmulating,
                SourceEvent::BatchEnd,
            ]
        );
    }

    #[test]
    fn removal_while_emulating_ends_it() {
        let out = run(vec![Raw::Start(1), Raw::Removed(1), Raw::Stop(1)]);
        assert_eq!(
            out,
            vec![
                SourceEvent::StartEmulating,
                SourceEvent::StopEmulating,
                SourceEvent::BatchEnd,
            ]
        );
    }

    #[test]
    fn keys_carry_eis_time_and_batch_end_follows() {
        let out = run(vec![
            Raw::Key {
                code: 30,
                pressed: false,
                time: 1000,
            },
            Raw::Key {
                code: 30,
                pressed: true,
                time: 1015,
            },
            Raw::Frame,
        ]);
        assert_eq!(
            out,
            vec![
                SourceEvent::Key {
                    code: 30,
                    pressed: false,
                    t_eis: Micros(1000)
                },
                SourceEvent::Key {
                    code: 30,
                    pressed: true,
                    t_eis: Micros(1015)
                },
                SourceEvent::BatchEnd,
            ]
        );
    }

    #[test]
    fn empty_batch_has_no_batch_end() {
        assert!(run(vec![Raw::Frame]).is_empty());
        assert!(run(vec![Raw::Stop(9)]).is_empty());
    }

    #[test]
    fn wheel_frame_is_one_wheel_event_per_axis() {
        let out = run(vec![
            // What Hyprland 0.56.2 sends for one notch down.
            Raw::ScrollDelta { dx: 0.0, dy: 15.0 },
            Raw::ScrollDiscrete { dx: 0, dy: 15 },
            Raw::Frame,
            // A spec-following server, half a notch left, no delta.
            Raw::ScrollDiscrete { dx: -60, dy: 0 },
            Raw::Frame,
        ]);
        assert_eq!(
            out,
            vec![
                SourceEvent::Scroll(ScrollIn {
                    axis: 0,
                    source: 0,
                    value: 15.0,
                    value120: Some(120),
                    stop: false
                }),
                SourceEvent::Scroll(ScrollIn {
                    axis: 1,
                    source: 0,
                    value: -7.5,
                    value120: Some(-60),
                    stop: false
                }),
                SourceEvent::BatchEnd,
            ]
        );
    }

    #[test]
    fn wheel_value120_follows_the_delta() {
        // A real value120 with the same delta gives the same answer, and a
        // hi-res step (S9: 30 = a quarter notch) keeps its size.
        for discrete in [15, 120] {
            let out = run(vec![
                Raw::ScrollDelta { dx: 0.0, dy: -15.0 },
                Raw::ScrollDiscrete {
                    dx: 0,
                    dy: -discrete,
                },
                Raw::Frame,
                Raw::ScrollDelta { dx: 0.0, dy: 3.75 },
                Raw::ScrollDiscrete { dx: 0, dy: 1 },
                Raw::Frame,
            ]);
            let v120: Vec<_> = out
                .iter()
                .filter_map(|e| match e {
                    SourceEvent::Scroll(s) => s.value120,
                    _ => None,
                })
                .collect();
            assert_eq!(v120, vec![-120, 30]);
        }
    }

    #[test]
    fn discrete_scroll_saturates_within_a_frame() {
        let out = run(vec![
            Raw::ScrollDiscrete {
                dx: i32::MIN,
                dy: i32::MAX,
            },
            Raw::ScrollDiscrete {
                dx: i32::MIN,
                dy: i32::MAX,
            },
            Raw::Frame,
        ]);
        let v120: Vec<_> = out
            .iter()
            .filter_map(|e| match e {
                SourceEvent::Scroll(s) => s.value120,
                _ => None,
            })
            .collect();
        assert_eq!(v120, vec![i32::MAX, i32::MIN]);
    }

    #[test]
    fn a_keymap_over_1_mib_is_refused_before_allocating() {
        let mut f = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut f, b"xkb_keymap { };\0\0").unwrap();
        assert_eq!(read_keymap(&f, 17).unwrap(), "xkb_keymap { };");
        let e = read_keymap(&f, u32::MAX).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(read_keymap(&f, 1 << 20).is_ok());
        assert!(read_keymap(&f, (1 << 20) + 1).is_err());
    }

    #[test]
    fn smooth_scroll_then_stop() {
        let out = run(vec![
            Raw::ScrollDelta { dx: 0.5, dy: 1.25 },
            Raw::Frame,
            Raw::ScrollStop { x: true, y: true },
            Raw::Frame,
        ]);
        let s = |axis, value, stop| {
            SourceEvent::Scroll(ScrollIn {
                axis,
                source: 1,
                value,
                value120: None,
                stop,
            })
        };
        assert_eq!(
            out,
            vec![
                s(0, 1.25, false),
                s(1, 0.5, false),
                s(0, 0.0, true),
                s(1, 0.0, true),
                SourceEvent::BatchEnd,
            ]
        );
    }

    #[test]
    fn keymap_and_buttons() {
        let out = run(vec![
            Raw::Keymap("xkb_keymap {}".into()),
            Raw::Button {
                code: 0x110,
                pressed: true,
            },
            Raw::Button {
                code: 0x1_0000,
                pressed: true,
            },
        ]);
        assert_eq!(
            out,
            vec![
                SourceEvent::Keymap {
                    text: "xkb_keymap {}".into()
                },
                SourceEvent::Button {
                    code: 0x110,
                    pressed: true
                },
                SourceEvent::BatchEnd,
            ]
        );
    }
}
