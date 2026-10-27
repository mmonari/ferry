//! The InputCapture portal session, for the `ctl` thread.
//!
//! One session per daemon lifetime. ferryd opens it once at start-up,
//! after the IPC release ([`crate::release_input_capture`]):
//!
//! ```text
//! Portal::open()        CreateSession(keyboard | pointer), ConnectToEIS -> fd
//!   fd                  -> the capture thread's EisSource
//! Portal::execute()     PortalCmd in the order the SourceMachine emits them:
//!                       SetBarriers (GetZones, SetPointerBarriers) -> BarriersSet,
//!                       Enable, Disable, Release
//! Portal::next_signal() Activated, Deactivated, Disabled, ZonesChanged,
//!                       or Lost (re-create the session)
//! ```
//!
//! Re-create the session only when [`PortalSignal::Lost`] arrives, when the EIS
//! receiver reports its end, or when [`Portal::execute`] fails on
//! `SetBarriers` (a failed re-zone). Never for a layout change, a lock or a
//! suspend.
//!
//! Every position here is local logical: zones from `GetZones`, the
//! `Activated` cursor, `Release` positions. Nothing reads the EIS region or
//! `wl_output.scale`.

use std::fmt;
use std::os::fd::OwnedFd;
use std::pin::Pin;

use ashpd::desktop::Session;
use ashpd::desktop::input_capture::{
    Activated, ActivatedBarrier, Barrier as PortalBarrier, BarrierID, Capabilities,
    ConnectToEISOptions, CreateSessionOptions, Deactivated, DisableOptions, Disabled,
    EnableOptions, GetZonesOptions, InputCapture, ReleaseOptions, SetPointerBarriersOptions,
    ZonesChanged,
};
use ashpd::zbus;
use ashpd::zvariant::{self, OwnedObjectPath};
use ferry_core::geom::{Barrier, Point, Side};
use ferry_core::source::{LockCheck, PortalCmd, SourceEvent};
use futures_util::stream::{SelectAll, Stream, StreamExt};

/// xdph's bus name. When it changes owner, every session it served is gone.
pub const XDPH_BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.hyprland";
/// The xdg-desktop-portal frontend, which owns the session objects.
pub const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";

/// Errors from portal calls.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    #[error("portal: {0}")]
    Portal(#[from] ashpd::Error),
    #[error("D-Bus: {0}")]
    Bus(#[from] zbus::Error),
    #[error("D-Bus value: {0}")]
    Value(#[from] zvariant::Error),
    #[error("CreateSession granted {0:?}, without keyboard and pointer")]
    Capabilities(String),
}

/// One zone from `GetZones`: a monitor's local logical rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Why the session is gone. ctl answers every variant by re-creating it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lost {
    /// A bus name ([`XDPH_BUS_NAME`] or [`PORTAL_BUS_NAME`]) lost the owner the
    /// session was created under.
    OwnerChanged { name: String },
    /// The portal emitted `Session.Closed`.
    SessionClosed,
    /// A D-Bus signal stream ended: the bus connection is gone.
    BusClosed,
}

/// A portal signal for this session, as ctl receives it.
#[derive(Debug, Clone, PartialEq)]
pub enum PortalSignal {
    /// `Activated`. Forward it as [`SourceEvent::Activated`] only with a fresh
    /// lock check ([`PortalSignal::into_source_event`],
    /// [`PortalSignal::checked`]).
    ///
    /// `activation_id` is 0 if xdph omitted it, `barrier_id` 0 if the barrier
    /// is unknown (ids are 1-based, so 0 never matches and the machine
    /// bounces). `x, y` are local logical, unclamped (up to one motion step
    /// past the edge, S1/S12), NaN if xdph omitted the position.
    Activated {
        activation_id: u32,
        barrier_id: u32,
        x: f64,
        y: f64,
    },
    Deactivated {
        activation_id: u32,
    },
    Disabled,
    ZonesChanged,
    /// The session is dead: re-create it.
    Lost(Lost),
}

impl PortalSignal {
    /// The [`SourceEvent`] for this signal; `lock` is used only by
    /// `Activated`. `Err` for [`PortalSignal::Lost`], which is ctl's business,
    /// not the machine's.
    pub fn into_source_event(self, lock: LockCheck) -> Result<SourceEvent, Lost> {
        Ok(match self {
            PortalSignal::Activated {
                activation_id,
                barrier_id,
                x,
                y,
            } => SourceEvent::Activated {
                activation_id,
                barrier_id,
                x,
                y,
                lock,
            },
            PortalSignal::Deactivated { activation_id } => {
                SourceEvent::Deactivated { activation_id }
            }
            PortalSignal::Disabled => SourceEvent::Disabled,
            PortalSignal::ZonesChanged => SourceEvent::ZonesChanged,
            PortalSignal::Lost(lost) => return Err(lost),
        })
    }

    /// The [`SourceEvent`] for this signal, with the lock check ctl makes
    /// before it forwards an `Activated`: a fresh read of
    /// Hyprland's monitors, `budget` at most ([`crate::LOCK_CHECK_TIMEOUT`]
    /// by default). Other signals cost no IPC.
    pub async fn checked(
        self,
        ipc: &ferry_hypr_ipc::Instance,
        budget: std::time::Duration,
    ) -> Result<SourceEvent, Lost> {
        let lock = match self {
            PortalSignal::Activated { .. } => crate::hypr::lock_check_async(ipc, budget).await,
            _ => LockCheck::Unknown,
        };
        self.into_source_event(lock)
    }
}

type SignalStream = Pin<Box<dyn Stream<Item = PortalSignal>>>;
type ScopedStream = Pin<Box<dyn Stream<Item = (OwnedObjectPath, PortalSignal)>>>;

/// The live portal session. Owned by the `ctl` thread.
pub struct Portal {
    ic: InputCapture,
    session: Session<InputCapture>,
    path: OwnedObjectPath,
    signals: SelectAll<SignalStream>,
    zone_set: Option<u32>,
    zones: Vec<Zone>,
}

impl fmt::Debug for Portal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Portal")
            .field("session", &self.path.as_str())
            .field("zone_set", &self.zone_set)
            .field("zones", &self.zones)
            .finish_non_exhaustive()
    }
}

impl Portal {
    /// `CreateSession` (keyboard and pointer), then `ConnectToEIS`. Returns the
    /// session and the EIS socket for the capture thread
    /// ([`crate::EisSource::new`]). Signals are subscribed before the session
    /// exists, so none is lost. Capture stays off until `SetBarriers` and
    /// `Enable`.
    pub async fn open() -> Result<(Self, OwnedFd), PortalError> {
        let conn = zbus::Connection::session().await?;
        Self::open_on(conn).await
    }

    /// [`Portal::open`] on a given session-bus connection.
    pub async fn open_on(conn: zbus::Connection) -> Result<(Self, OwnedFd), PortalError> {
        let ic = InputCapture::with_connection(conn.clone()).await?;
        // Subscribed before CreateSession, so no signal is lost. The portal's
        // signals name their session; only this session's pass.
        let ic_proxy = zbus::Proxy::new(
            &conn,
            PORTAL_BUS_NAME,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.InputCapture",
        )
        .await?;
        let mut scoped: SelectAll<ScopedStream> = SelectAll::new();
        scoped.push(
            scoped_signal(&ic_proxy, "Activated", |a: Activated| {
                let (x, y) = a
                    .cursor_position()
                    .map_or((f64::NAN, f64::NAN), |(x, y)| (f64::from(x), f64::from(y)));
                let barrier_id = match a.barrier_id() {
                    Some(ActivatedBarrier::Barrier(b)) => b.get(),
                    Some(ActivatedBarrier::UnknownBarrier) | None => 0,
                };
                let sig = PortalSignal::Activated {
                    activation_id: a.activation_id().unwrap_or(0),
                    barrier_id,
                    x,
                    y,
                };
                (a.session_handle().into(), sig)
            })
            .await?,
        );
        scoped.push(
            scoped_signal(&ic_proxy, "Deactivated", |d: Deactivated| {
                let sig = PortalSignal::Deactivated {
                    activation_id: d.activation_id().unwrap_or(0),
                };
                (d.session_handle().into(), sig)
            })
            .await?,
        );
        scoped.push(
            scoped_signal(&ic_proxy, "Disabled", |d: Disabled| {
                (d.session_handle().into(), PortalSignal::Disabled)
            })
            .await?,
        );
        scoped.push(
            scoped_signal(&ic_proxy, "ZonesChanged", |z: ZonesChanged| {
                (z.session_handle().into(), PortalSignal::ZonesChanged)
            })
            .await?,
        );

        let (session, caps) = ic
            .create_session(
                None,
                CreateSessionOptions::default()
                    .set_capabilities(Capabilities::Keyboard | Capabilities::Pointer),
            )
            .await?;
        if !(caps.contains(Capabilities::Keyboard) && caps.contains(Capabilities::Pointer)) {
            let _ = session.close().await;
            return Err(PortalError::Capabilities(format!("{caps:?}")));
        }
        let path = session_path(&session)?;
        tracing::info!(session = path.as_str(), ?caps, "portal session created");

        let mine = path.clone();
        let mut signals: SelectAll<SignalStream> = SelectAll::new();
        signals.push(Box::pin(scoped.filter_map(move |(p, sig)| {
            std::future::ready((p == mine).then_some(sig))
        })));
        let session_proxy = zbus::Proxy::new(
            &conn,
            PORTAL_BUS_NAME,
            path.clone(),
            "org.freedesktop.portal.Session",
        )
        .await?;
        let closed = session_proxy.receive_signal("Closed").await?;
        signals.push(Box::pin(
            closed
                .map(|_| PortalSignal::Lost(Lost::SessionClosed))
                .chain(futures_util::stream::once(std::future::ready(
                    PortalSignal::Lost(Lost::BusClosed),
                ))),
        ));
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        for name in [XDPH_BUS_NAME, PORTAL_BUS_NAME] {
            // The owner the session lives under. xdph is D-Bus activated, so
            // it may have just started for this very session: only its loss
            // counts, not its arrival.
            let owner = dbus
                .get_name_owner(zbus::names::BusName::try_from(name).map_err(zbus::Error::from)?)
                .await
                .ok()
                .map(|o| o.to_string());
            let changes = dbus
                .receive_name_owner_changed_with_args(&[(0, name)])
                .await?;
            signals.push(Box::pin(changes.filter_map(move |sig| {
                let lost = sig.args().ok().and_then(|a| {
                    let old = a.old_owner().as_ref().map(|o| o.to_string());
                    (old.is_some() && old == owner).then(|| {
                        PortalSignal::Lost(Lost::OwnerChanged {
                            name: name.to_owned(),
                        })
                    })
                });
                std::future::ready(lost)
            })));
        }

        let fd = match ic
            .connect_to_eis(&session, ConnectToEISOptions::default())
            .await
        {
            Ok(fd) => fd,
            Err(e) => {
                // Each session leaks a lock in xdph (#419); at least close it.
                let _ = session.close().await;
                return Err(e.into());
            }
        };
        Ok((
            Self {
                ic,
                session,
                path,
                signals,
                zone_set: None,
                zones: Vec::new(),
            },
            fd,
        ))
    }

    /// The session's object path, for logs.
    pub fn session_path(&self) -> &str {
        self.path.as_str()
    }

    /// The next signal for this session. Cancel-safe, so it can sit in a
    /// `tokio::select!`.
    pub async fn next_signal(&mut self) -> PortalSignal {
        match self.signals.next().await {
            Some(sig) => sig,
            None => PortalSignal::Lost(Lost::BusClosed),
        }
    }

    /// Run one [`PortalCmd`]. `SetBarriers` sets exactly the barriers it
    /// carries (the SourceMachine's own snapshot, which it maps
    /// `Activated.barrier_id` against) and is answered with
    /// [`SourceEvent::BarriersSet`]. The other commands answer `None`.
    ///
    /// An `Err` from `SetBarriers` is a failed re-zone: re-create the session.
    /// Errors from `Enable`, `Disable` and `Release` are logged by
    /// the caller; a dead session also shows up as [`PortalSignal::Lost`].
    pub async fn execute(&mut self, cmd: &PortalCmd) -> Result<Option<SourceEvent>, PortalError> {
        match cmd {
            PortalCmd::SetBarriers {
                refresh_zones,
                barriers,
            } => {
                let failed = self.set_barriers(barriers, *refresh_zones).await?;
                return Ok(Some(SourceEvent::BarriersSet { failed }));
            }
            PortalCmd::Enable => self.enable().await?,
            PortalCmd::Disable => self.disable().await?,
            PortalCmd::Release { activation_id, pos } => {
                self.release(*activation_id, *pos).await?;
            }
        }
        Ok(None)
    }

    /// `GetZones`: the current zone set and its zones (local logical).
    pub async fn refresh_zones(&mut self) -> Result<&[Zone], PortalError> {
        let zones = self
            .ic
            .zones(&self.session, GetZonesOptions::default())
            .await?
            .response()?;
        self.zone_set = Some(zones.zone_set());
        self.zones = zones
            .regions()
            .iter()
            .map(|r| Zone {
                x: r.x_offset(),
                y: r.y_offset(),
                w: r.width(),
                h: r.height(),
            })
            .collect();
        tracing::debug!(zone_set = zones.zone_set(), zones = ?self.zones, "GetZones");
        Ok(&self.zones)
    }

    /// The zones from the last `GetZones`.
    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    /// `SetPointerBarriers` against the current zone set (`GetZones` first
    /// when `refresh_zones` or when none is known yet). Returns the ids that
    /// failed: those xdph rejected, plus any id 0, which the portal cannot
    /// carry. An empty list clears every barrier.
    pub async fn set_barriers(
        &mut self,
        barriers: &[Barrier],
        refresh_zones: bool,
    ) -> Result<Vec<u32>, PortalError> {
        if refresh_zones || self.zone_set.is_none() {
            self.refresh_zones().await?;
        }
        let zone_set = self.zone_set.unwrap_or_default();
        let (list, mut failed) = to_portal_barriers(barriers);
        for b in barriers {
            if !self.zones.iter().any(|z| edge_barrier(b.side, *z) == b.pos) {
                tracing::warn!(id = b.id, pos = ?b.pos, zones = ?self.zones,
                    "barrier is not a whole edge of any GetZones zone; xdph will likely fail it");
            }
        }
        let resp = self
            .ic
            .set_pointer_barriers(
                &self.session,
                &list,
                zone_set,
                SetPointerBarriersOptions::default(),
            )
            .await?
            .response()?;
        failed.extend(resp.failed_barriers().iter().map(|id| id.get()));
        tracing::debug!(zone_set, n = list.len(), ?failed, "SetPointerBarriers");
        Ok(failed)
    }

    pub async fn enable(&self) -> Result<(), PortalError> {
        Ok(self
            .ic
            .enable(&self.session, EnableOptions::default())
            .await?)
    }

    pub async fn disable(&self) -> Result<(), PortalError> {
        Ok(self
            .ic
            .disable(&self.session, DisableOptions::default())
            .await?)
    }

    /// `Release(activation_id, cursor_position)`; `pos` is local logical.
    pub async fn release(
        &self,
        activation_id: Option<u32>,
        pos: Option<Point>,
    ) -> Result<(), PortalError> {
        Ok(self
            .ic
            .release(
                &self.session,
                ReleaseOptions::default()
                    .set_activation_id(activation_id)
                    .set_cursor_position(pos.map(|p| (p.x, p.y))),
            )
            .await?)
    }

    /// `Session.Close`. Best effort: a dead session errors, which is fine.
    pub async fn close(self) {
        if let Err(e) = self.session.close().await {
            tracing::debug!(error = %e, "portal session close");
        }
    }
}

/// Subscribe to one session-scoped portal signal, decoded into ashpd's type
/// for it. (ashpd's own `receive_*` streams borrow the `InputCapture` proxy,
/// so they cannot live next to it in [`Portal`].)
async fn scoped_signal<T>(
    proxy: &zbus::Proxy<'static>,
    name: &'static str,
    map: impl Fn(T) -> (OwnedObjectPath, PortalSignal) + 'static,
) -> Result<ScopedStream, PortalError>
where
    T: for<'d> serde::Deserialize<'d> + zvariant::Type,
{
    let stream = proxy.receive_signal(name).await?;
    Ok(Box::pin(stream.filter_map(move |msg| {
        let out = match msg.body().deserialize::<T>() {
            Ok(v) => Some(map(v)),
            Err(e) => {
                tracing::warn!(signal = name, error = %e, "undecodable portal signal");
                None
            }
        };
        std::future::ready(out)
    })))
}

/// ashpd keeps the session path private; it serializes as one, though.
fn session_path(session: &Session<InputCapture>) -> Result<OwnedObjectPath, PortalError> {
    let ctxt = zvariant::serialized::Context::new_dbus(zvariant::Endian::Little, 0);
    let data = zvariant::to_bytes(ctxt, session)?;
    let (path, _): (OwnedObjectPath, _) = data.deserialize()?;
    Ok(path)
}

/// `geom::Barrier`s as the portal takes them. Id 0 (never valid: the portal's
/// ids are non-zero) is left out and reported as failed.
pub fn to_portal_barriers(barriers: &[Barrier]) -> (Vec<PortalBarrier>, Vec<u32>) {
    let mut list = Vec::with_capacity(barriers.len());
    let mut failed = Vec::new();
    for b in barriers {
        match BarrierID::new(b.id) {
            Some(id) => list.push(PortalBarrier::new(id, b.pos)),
            None => failed.push(b.id),
        }
    }
    (list, failed)
}

/// The whole-edge barrier `(x1, y1, x2, y2)` on `side` of `zone`, as Hyprland
/// 0.56.2 `isBarrierValidAgainstMonitor` accepts it: left and top on the first
/// pixel, right and bottom one past the last, spanning first to last pixel
/// inclusive (S02 `edge.rs` `barrier`).
pub fn edge_barrier(side: Side, zone: Zone) -> (i32, i32, i32, i32) {
    let (x, y) = (zone.x, zone.y);
    let (w, h) = (zone.w as i32, zone.h as i32);
    match side {
        Side::Left => (x, y, x, y + h - 1),
        Side::Right => (x + w, y, x + w, y + h - 1),
        Side::Top => (x, y, x + w - 1, y),
        Side::Bottom => (x, y + h, x + w - 1, y + h),
    }
}

/// A point `inset` px inside `zone` from `side`, at `along` (clamped into the
/// zone): where a bounce `Release` puts the cursor (S14 used 2 px). Local
/// logical.
pub fn inside_point(side: Side, zone: Zone, along: f64, inset: f64) -> Point {
    let (x, y) = (f64::from(zone.x), f64::from(zone.y));
    let (w, h) = (f64::from(zone.w), f64::from(zone.h));
    let clamp = |v: f64, lo: f64, len: f64| {
        if v.is_nan() {
            lo + len / 2.0
        } else {
            v.clamp(lo, lo + len - 1.0)
        }
    };
    match side {
        Side::Left => Point {
            x: x + inset,
            y: clamp(along, y, h),
        },
        Side::Right => Point {
            x: x + w - 1.0 - inset,
            y: clamp(along, y, h),
        },
        Side::Top => Point {
            x: clamp(along, x, w),
            y: y + inset,
        },
        Side::Bottom => Point {
            x: clamp(along, x, w),
            y: y + h - 1.0 - inset,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_core::PeerId;
    use ferry_core::geom::MonitorId;

    const FHD: Zone = Zone {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };

    #[test]
    fn edge_barriers_match_s02() {
        assert_eq!(edge_barrier(Side::Left, FHD), (0, 0, 0, 1079));
        assert_eq!(edge_barrier(Side::Right, FHD), (1920, 0, 1920, 1079));
        assert_eq!(edge_barrier(Side::Top, FHD), (0, 0, 1919, 0));
        assert_eq!(edge_barrier(Side::Bottom, FHD), (0, 1080, 1919, 1080));
        // Scale 1.6, GetZones reports 1200x675 logical.
        let scaled = Zone {
            x: 0,
            y: 0,
            w: 1200,
            h: 675,
        };
        assert_eq!(edge_barrier(Side::Right, scaled), (1200, 0, 1200, 674));
        assert_eq!(edge_barrier(Side::Bottom, scaled), (0, 675, 1199, 675));
        assert_eq!(edge_barrier(Side::Left, scaled), (0, 0, 0, 674));
        assert_eq!(edge_barrier(Side::Top, scaled), (0, 0, 1199, 0));
        // A second monitor at an offset (first light's DVI-D-1).
        let right = Zone {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert_eq!(edge_barrier(Side::Right, right), (3840, 0, 3840, 1079));
        assert_eq!(edge_barrier(Side::Bottom, right), (1920, 1080, 3839, 1080));
    }

    #[test]
    fn inside_points_are_on_the_zone() {
        let p = inside_point(Side::Right, FHD, 500.0, 2.0);
        assert_eq!((p.x, p.y), (1917.0, 500.0));
        let p = inside_point(Side::Left, FHD, 2000.0, 2.0);
        assert_eq!((p.x, p.y), (2.0, 1079.0));
        let p = inside_point(Side::Bottom, FHD, 10.0, 2.0);
        assert_eq!((p.x, p.y), (10.0, 1077.0));
        let p = inside_point(Side::Top, FHD, -8.0, 2.0);
        assert_eq!((p.x, p.y), (0.0, 2.0));
    }

    #[test]
    fn portal_barriers_keep_ids_and_positions() {
        let mon = MonitorId {
            peer: PeerId([0; 32]),
            index: 0,
        };
        let b = |id, side| Barrier {
            id,
            monitor: mon,
            side,
            pos: edge_barrier(side, FHD),
        };
        let (list, failed) = to_portal_barriers(&[b(1, Side::Right), b(0, Side::Left)]);
        assert_eq!(failed, vec![0]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].barrier_id().get(), 1);
        assert_eq!(list[0].position().as_tuple(), (1920, 0, 1920, 1079));
    }

    #[test]
    fn signals_map_to_source_events() {
        let a = PortalSignal::Activated {
            activation_id: 5,
            barrier_id: 1,
            x: 1920.0,
            y: 540.0,
        };
        assert_eq!(
            a.into_source_event(LockCheck::Unknown),
            Ok(SourceEvent::Activated {
                activation_id: 5,
                barrier_id: 1,
                x: 1920.0,
                y: 540.0,
                lock: LockCheck::Unknown,
            })
        );
        assert_eq!(
            PortalSignal::Deactivated { activation_id: 5 }.into_source_event(LockCheck::Unknown),
            Ok(SourceEvent::Deactivated { activation_id: 5 })
        );
        assert_eq!(
            PortalSignal::Disabled.into_source_event(LockCheck::Unknown),
            Ok(SourceEvent::Disabled)
        );
        assert_eq!(
            PortalSignal::ZonesChanged.into_source_event(LockCheck::Unknown),
            Ok(SourceEvent::ZonesChanged)
        );
        assert_eq!(
            PortalSignal::Lost(Lost::SessionClosed).into_source_event(LockCheck::Unknown),
            Err(Lost::SessionClosed)
        );
    }
}
