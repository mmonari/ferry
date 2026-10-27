//! ferry's pure core.
//!
//! Nothing here touches Wayland, D-Bus, the network, the filesystem, a clock
//! or a thread ("the core rule"). Time enters as [`Micros`];
//! timers leave as `next_deadline()`; effects leave as actions pushed into a
//! caller-owned [`Actions`] buffer. Everything is therefore testable on any
//! machine.
//!
//! M1: the types and signatures are the contract; the design note says what
//! each one does.

#![forbid(unsafe_code)]

pub mod actions;
pub mod anchor;
pub mod api;
pub mod chord;
pub mod config;
pub mod doctor;
pub mod geom;
pub mod held;
pub mod pair;
pub mod peer;
pub mod repeat;
pub mod source;
pub mod target;
pub mod time;
pub mod wire;

#[cfg(test)]
mod testkit;

pub use actions::Actions;
pub use peer::PeerId;
pub use time::Micros;

/// Evdev keycodes of the eight modifiers released by the start-up modifier
/// reset: Ctrl L/R, Shift L/R, Alt L/R, Super L/R.
pub const MODIFIER_KEYCODES: [u16; 8] = [29, 97, 42, 54, 56, 100, 125, 126];

/// `KEY_WAKEUP`. EIS delivers it on resume; it is never forwarded.
pub const KEY_WAKEUP: u16 = 143;
