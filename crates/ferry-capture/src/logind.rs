//! logind's `PrepareForSleep(bool)`, for the `ctl` thread.
//!
//! `PrepareForSleep(true)` makes the source release before the machine sleeps;
//! the capture session itself survives suspend. logind waits
//! for that release only while a **delay** inhibitor is held, so ctl keeps one
//! ([`Logind::inhibit_sleep`]): it drops the fd once the `Release` is sent, and
//! takes a new one after `PrepareForSleep(false)`.
//!
//! Uses zbus through ashpd's re-export, on the system bus.

use std::os::fd::OwnedFd;

use ashpd::zbus;
use ashpd::zvariant;
use ferry_core::source::SourceEvent;
use futures_util::StreamExt;

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";

/// A subscription to logind's `PrepareForSleep`.
pub struct Logind {
    proxy: zbus::Proxy<'static>,
    sleep: zbus::proxy::SignalStream<'static>,
}

impl std::fmt::Debug for Logind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logind").finish_non_exhaustive()
    }
}

impl Logind {
    /// Connect to the system bus and subscribe to `PrepareForSleep`.
    pub async fn connect() -> Result<Self, zbus::Error> {
        let conn = zbus::Connection::system().await?;
        let proxy = zbus::Proxy::new(&conn, LOGIND, LOGIND_PATH, MANAGER).await?;
        let sleep = proxy.receive_signal("PrepareForSleep").await?;
        Ok(Self { proxy, sleep })
    }

    /// The next `PrepareForSleep`, as [`SourceEvent::PrepareForSleep`].
    /// `None` when the bus connection is gone. Cancel-safe.
    pub async fn next(&mut self) -> Option<SourceEvent> {
        loop {
            let msg = self.sleep.next().await?;
            match msg.body().deserialize::<bool>() {
                Ok(start) => return Some(SourceEvent::PrepareForSleep { start }),
                Err(e) => tracing::warn!(error = %e, "PrepareForSleep with an unexpected body"),
            }
        }
    }

    /// Take a sleep **delay** inhibitor: logind then waits (up to its
    /// `InhibitDelayMaxSec`, 5 s by default) after `PrepareForSleep(true)`
    /// until this fd is closed. Drop it once the release is sent.
    pub async fn inhibit_sleep(&self) -> Result<OwnedFd, zbus::Error> {
        let fd: zvariant::OwnedFd = self
            .proxy
            .call(
                "Inhibit",
                &(
                    "sleep",
                    "ferry",
                    "release input capture before sleep",
                    "delay",
                ),
            )
            .await?;
        Ok(fd.into())
    }
}
