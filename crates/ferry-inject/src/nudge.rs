//! The nudge of a key crossing: warp the cursor next to an edge, then move it across
//! with one relative motion, so the compositor tests the edge's barrier and the portal activates.
//!
//! Hyprland tests barriers only in `PointerManager::move` (relative motion); a warp
//! (`hl.dsp.cursor.move`) never activates one. The device is a pointer only, made for the nudge
//! and destroyed after it: a virtual keyboard could change Hyprland's main keyboard and make it
//! re-create the EIS keyboard during the activation.

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use crate::{Result, Target};

struct State;

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

delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);

/// Warp the cursor to `(x, y)` (global logical) over Hyprland IPC, then move it by `(dx, dy)`
/// with a pointer-only virtual device. Blocks for two IPC and two Wayland round trips.
pub fn nudge(target: &Target, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
    let (conn, globals, mut queue) = ferry_wl::init::<State>(target)?;
    let qh = queue.handle();
    let mgr: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|_| ferry_wl::Error::MissingGlobal("zwlr_virtual_pointer_manager_v1"))?;
    let ptr = mgr.create_virtual_pointer(None, &qh, ());
    // The device exists before the warp, so the motion is the first thing it sends.
    queue.roundtrip(&mut State)?;
    target.instance()?.move_cursor(x, y)?;
    ptr.motion(0, dx, dy);
    ptr.frame();
    ptr.destroy();
    mgr.destroy();
    queue.roundtrip(&mut State)?;
    drop(conn);
    Ok(())
}
