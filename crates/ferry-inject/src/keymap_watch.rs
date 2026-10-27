// from gliff crates/hypr-input/src/keymap_watch.rs @ 089662f
//! Follow the keymap the compositor serves to its clients. A `wl_keyboard`
//! receives the exact keymap of the keyboard in use, per-device options
//! included, and a new one whenever the active keyboard or its layout
//! changes.

use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_keyboard::{self, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

use crate::{Error, Result, Target};

/// Called with the keymap text (xkb format v1) each time it changes.
pub type KeymapSink = Box<dyn FnMut(String) + Send>;

/// Watch the keymap on a thread of its own, for as long as the compositor
/// connection lasts.
pub fn watch_keymap(target: Target, sink: KeymapSink) -> Result<std::thread::JoinHandle<()>> {
    Ok(std::thread::Builder::new()
        .name("ferry-keymap".into())
        .spawn(move || {
            if let Err(e) = run(&target, sink) {
                tracing::warn!(error = %e, "stopped following the keymap");
            }
        })?)
}

struct State {
    sink: KeymapSink,
    keyboard: Option<WlKeyboard>,
    last: String,
}

fn run(target: &Target, sink: KeymapSink) -> Result<()> {
    let (_conn, globals, mut queue) = ferry_wl::init::<State>(target)?;
    let qh = queue.handle();
    let _seat: WlSeat = globals
        .bind(&qh, 1..=7, ())
        .map_err(ferry_wl::Error::from)?;
    let mut state = State {
        sink,
        keyboard: None,
        last: String::new(),
    };
    loop {
        queue.blocking_dispatch(&mut state)?;
    }
}

fn read_keymap(fd: OwnedFd, size: u32) -> Result<String> {
    // The fd may be shared with other clients, so read without moving its
    // file offset.
    let mut buf = vec![0; size as usize];
    File::from(fd).read_exact_at(&mut buf, 0)?;
    while buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|e| Error::Xkb(format!("keymap is not UTF-8: {e}")))
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        else {
            return;
        };
        if !caps.contains(wl_seat::Capability::Keyboard) {
            if let Some(kb) = state.keyboard.take()
                && kb.version() >= 3
            {
                kb.release();
            }
        } else if state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wl_keyboard::Event::Keymap { format, fd, size } = event else {
            return;
        };
        if format != WEnum::Value(KeymapFormat::XkbV1) {
            return;
        }
        match read_keymap(fd, size) {
            Ok(text) if text != state.last => {
                state.last.clone_from(&text);
                (state.sink)(text);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "cannot read the keymap"),
        }
    }
}
