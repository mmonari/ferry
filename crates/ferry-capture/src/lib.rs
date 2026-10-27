//! ferry's capture side, portal backend.
//!
//! What runs where in ferryd:
//!
//! | Thread | Uses |
//! |---|---|
//! | `ctl` (tokio) | [`release_input_capture`] at start, then [`Portal`]: open, [`Portal::execute`] for every [`ferry_core::source::PortalCmd`], [`Portal::next_signal`] → [`PortalSignal::checked`] (the per-activation lock check) → `SourceEvent`; [`lock_check_async`] for `CheckLock`; [`Logind`] for `PrepareForSleep` |
//! | `capture` (calloop) | [`EisSource`] on the fd from [`Portal::open`]: EIS → `SourceEvent`, and [`EisOutput::Ended`] when the stream ends |
//!
//! The direct and layer-shell backends come later in M1.

#![forbid(unsafe_code)]

pub mod eis;
pub mod hypr;
pub mod logind;
pub mod portal;

pub use eis::{EisEnd, EisOutput, EisSource};
pub use hypr::{
    LOCK_CHECK_TIMEOUT, lock_check, lock_check_async, lock_from_answer, release_input_capture,
};
pub use logind::Logind;
pub use portal::{Lost, Portal, PortalError, PortalSignal, Zone, edge_barrier, inside_point};
