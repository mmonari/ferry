//! `org.freedesktop.Notifications` over zbus: the production [`Sink`].
//!
//! zbus is the version ashpd already pulls in, with its `tokio` feature, and this runs on the
//! `ctl` runtime beside the portal driver.
//!
//! - **Lazy.** Nothing connects until the first toast. A missing bus or notification daemon is
//!   an error of that toast, reported to the caller, who logs it once. The next toast tries
//!   again, so a daemon that starts later (the shell, after ferryd) is found.
//! - **Bounded.** zbus sets no method timeout of its own: every connect and call has one here
//!   (B-06's lesson), so a notification daemon that hangs costs the notify task a second, and
//!   nothing else.
//! - **Plain text.** A daemon that advertises `body-markup` would render a peer's name as
//!   markup, so `&`, `<` and `>` are escaped for it. The summary is always plain text.
//! - **Ids are the daemon's.** A daemon that restarts forgets every id, and the next daemon may
//!   hand the same numbers to other applications' notifications. The owner of the name is
//!   watched, and a change is reported as [`SinkEvent::Reset`] so no stale id is replaced or
//!   closed.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use futures_util::StreamExt;
use zbus::connection::Builder;
use zbus::proxy::{OwnerChangedStream, SignalStream};
use zbus::zvariant::Value;
use zbus::{Message, Proxy};

use super::sink::{APP_ICON, APP_NAME, Request, Sink, SinkError, SinkEvent};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const IFACE: &str = "org.freedesktop.Notifications";

/// Connecting to the bus, with the handshake and the match rules.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// One method call. A notification daemon answers in milliseconds.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(1);

/// Which bus to talk to.
#[derive(Debug, Clone)]
pub(crate) enum Target {
    /// The session bus of the environment (`DBUS_SESSION_BUS_ADDRESS`): the real desktop.
    Session,
    /// A bus at this address: tests and `ferryd --notify-bus`.
    Address(String),
}

pub(crate) struct DbusSink {
    target: Target,
    live: Option<Live>,
}

/// One bus connection, with the signals ferryd listens to.
struct Live {
    proxy: Proxy<'static>,
    actions: SignalStream<'static>,
    closed: SignalStream<'static>,
    owner: OwnerChangedStream<'static>,
    /// The daemon advertises `body-markup`; unknown until asked.
    markup: Option<bool>,
}

impl DbusSink {
    pub(crate) fn new(target: Target) -> Self {
        Self { target, live: None }
    }

    async fn ensure(&mut self) -> Result<&mut Live, SinkError> {
        if self.live.is_none() {
            self.live = Some(connect(&self.target).await?);
        }
        self.live
            .as_mut()
            .ok_or_else(|| SinkError::Unavailable("not connected".into()))
    }

    async fn send(&mut self, req: &Request) -> Result<u32, SinkError> {
        let live = self.ensure().await?;
        if live.markup.is_none() {
            let caps: Vec<String> =
                within(CALL_TIMEOUT, live.proxy.call("GetCapabilities", &())).await?;
            live.markup = Some(caps.iter().any(|c| c == "body-markup"));
        }
        let body = if live.markup == Some(true) {
            escape(&req.body)
        } else {
            req.body.clone()
        };
        let hints: HashMap<&str, Value<'_>> =
            HashMap::from([("urgency", Value::U8(req.urgency.hint()))]);
        let actions: Vec<&str> = req
            .action
            .map(|(key, label)| vec![key, label])
            .unwrap_or_default();
        let expire: i32 = req
            .expire_ms
            .map_or(-1, |ms| i32::try_from(ms).unwrap_or(i32::MAX));
        within(
            CALL_TIMEOUT,
            live.proxy.call::<_, _, u32>(
                "Notify",
                &(
                    APP_NAME,
                    req.replaces.unwrap_or(0),
                    APP_ICON,
                    req.summary.as_str(),
                    body.as_str(),
                    actions,
                    hints,
                    expire,
                ),
            ),
        )
        .await
    }
}

/// `fut` within `limit`, its error as a [`SinkError`].
async fn within<T>(
    limit: Duration,
    fut: impl Future<Output = Result<T, zbus::Error>>,
) -> Result<T, SinkError> {
    match tokio::time::timeout(limit, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(classify(&e)),
        Err(_) => Err(SinkError::Unavailable(format!(
            "no answer within {} ms",
            limit.as_millis()
        ))),
    }
}

/// An error that says nobody is there to answer, against one that says somebody refused.
fn classify(e: &zbus::Error) -> SinkError {
    match e {
        zbus::Error::MethodError(name, ..)
            if !matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
                    | "org.freedesktop.DBus.Error.NoReply"
                    | "org.freedesktop.DBus.Error.Disconnected"
                    | "org.freedesktop.DBus.Error.TimedOut"
            ) =>
        {
            SinkError::Refused(e.to_string())
        }
        other => SinkError::Unavailable(other.to_string()),
    }
}

async fn connect(target: &Target) -> Result<Live, SinkError> {
    let attempt = async {
        let builder = match target {
            Target::Session => Builder::session()?,
            Target::Address(address) => Builder::address(address.as_str())?,
        };
        let conn = builder.method_timeout(CALL_TIMEOUT).build().await?;
        let proxy = Proxy::new(&conn, NAME, PATH, IFACE).await?;
        Ok::<_, zbus::Error>(Live {
            actions: proxy.receive_signal("ActionInvoked").await?,
            closed: proxy.receive_signal("NotificationClosed").await?,
            owner: proxy.receive_owner_changed().await?,
            proxy,
            markup: None,
        })
    };
    within(CONNECT_TIMEOUT, attempt).await
}

/// `&`, `<` and `>` escaped, for a daemon that renders the body as markup.
fn escape(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for c in body.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

fn action_event(msg: &Message) -> Option<SinkEvent> {
    let (id, key) = msg.body().deserialize::<(u32, String)>().ok()?;
    Some(SinkEvent::Action { id, key })
}

fn closed_event(msg: &Message) -> Option<SinkEvent> {
    let (id, _reason) = msg.body().deserialize::<(u32, u32)>().ok()?;
    Some(SinkEvent::Closed { id })
}

impl Sink for DbusSink {
    async fn show(&mut self, req: &Request) -> Result<u32, SinkError> {
        let sent = self.send(req).await;
        if matches!(&sent, Err(SinkError::Unavailable(_))) {
            // The daemon may be gone for good, or the bus may be: the next toast starts from a
            // fresh connection, and nothing of this one's ids is trusted.
            self.live = None;
        }
        sent
    }

    async fn close(&mut self, id: u32) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let closed: Result<(), SinkError> =
            within(CALL_TIMEOUT, live.proxy.call("CloseNotification", &(id,))).await;
        if let Err(e) = closed {
            tracing::debug!(error = %e, "CloseNotification failed");
        }
    }

    async fn next(&mut self) -> SinkEvent {
        loop {
            let Some(live) = self.live.as_mut() else {
                return std::future::pending().await;
            };
            let heard = tokio::select! {
                m = live.actions.next() => m.map(|m| action_event(&m)),
                m = live.closed.next() => m.map(|m| closed_event(&m)),
                o = live.owner.next() => {
                    // another process owns the name now, or none does: every id is void
                    live.markup = None;
                    o.map(|_| Some(SinkEvent::Reset))
                }
            };
            match heard {
                Some(Some(ev)) => return ev,
                // a signal whose body is not what the spec says: not for us
                Some(None) => {}
                // a stream ended: the connection is gone, and with it every id
                None => {
                    self.live = None;
                    return SinkEvent::Reset;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_characters_of_a_name_are_escaped_and_nothing_else_is() {
        assert_eq!(escape("a&b<c>d"), "a&amp;b&lt;c&gt;d");
        assert_eq!(
            escape("lap is back, ünï 418 207"),
            "lap is back, ünï 418 207"
        );
        // `&` first: an entity in a name is not decoded by the daemon
        assert_eq!(escape("&lt;"), "&amp;lt;");
    }
}
