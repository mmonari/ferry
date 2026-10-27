//! The one test that reaches a D-Bus: ferryd's notification client against a
//! fake `org.freedesktop.Notifications` on a **private** session bus.
//!
//! **Never the user's bus.** The daemon is given the private bus's address explicitly
//! (`Mode::Bus`); it never reads `DBUS_SESSION_BUS_ADDRESS`. The bus is a `dbus-daemon` of its
//! own, started with a configuration that lists no service directory, so nothing on the host
//! can be activated on it (a real notification daemon would draw on the user's screen). The test
//! skips, and says so, when there is no `dbus-daemon`.
//!
//! What it shows, in one run: a pairing request's code is a critical notification with the
//! `open` action; answering closes it; the action reaches ferryd and a `ui` subscriber; a
//! result replaces the code toast in place; an `attention` entry is a critical toast and a lost
//! layout edit a normal one; a body is escaped for a daemon that renders markup and left alone
//! for one that does not; a missing notification daemon costs a counter and nothing else; and
//! a daemon that comes back is found again, with no id of the old one used. Every toast is also
//! a `notice` event, with the words ferryd wrote, whether or not a daemon showed it.
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "integration test: an unwrap that fails is the test failing; the skip says why"
)]

mod support;

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferry_core::PeerId;
use ferry_core::api::LinkState;
use ferry_core::geom::{Layout, Origin, Revision};
use ferry_core::pair::PairCode;
use ferry_net::Fingerprint;
use ferryd::chan::{CtlMsg, LinkUpdate};
use ferryd::notify::{Mode, Options};
use ferryd::pairing::PairNote;
use serde_json::{Value, json};
use support::*;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value as Variant;

const PATH: &str = "/org/freedesktop/Notifications";
const NAME: &str = "org.freedesktop.Notifications";

/// A `dbus-daemon` of the test's own: no service directory, so nothing can be activated, and a
/// socket under the test's temp dir.
struct Bus {
    child: Child,
    address: String,
    _dir: tempfile::TempDir,
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

impl Bus {
    /// `None` when the host has no `dbus-daemon`.
    fn start() -> Option<Self> {
        let daemon = find_in_path("dbus-daemon")?;
        let dir = tempfile::Builder::new().prefix("fyb").tempdir().unwrap();
        let socket = dir.path().join("bus");
        let conf = dir.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                socket.display()
            ),
        )
        .unwrap();
        let mut child = Command::new(daemon)
            .arg("--config-file")
            .arg(&conf)
            .arg("--print-address=1")
            .arg("--nofork")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_string();
        assert!(
            address.starts_with(&format!("unix:path={}", socket.display())),
            "the private bus is at {address:?}, not under {}",
            socket.display()
        );
        Some(Self {
            child,
            address,
            _dir: dir,
        })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the fake daemon saw in a `Notify`.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    id: u32,
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    urgency: Option<u8>,
    expire_timeout: i32,
}

#[derive(Default)]
struct Log {
    notified: Vec<Seen>,
    closed: Vec<u32>,
}

/// A notification daemon with the bare minimum.
struct Fake {
    log: Arc<Mutex<Log>>,
    /// Advertises `body-markup`.
    markup: bool,
    first_id: u32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Fake {
    fn get_capabilities(&self) -> Vec<String> {
        let mut caps = vec!["actions".to_string(), "body".to_string()];
        if self.markup {
            caps.push("body-markup".into());
        }
        caps
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the signature of the D-Bus method"
    )]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, Variant<'_>>,
        expire_timeout: i32,
    ) -> u32 {
        let mut log = self.log.lock().unwrap();
        // a replaced notification keeps its id
        let id = if replaces_id == 0 {
            self.first_id + u32::try_from(log.notified.len()).unwrap()
        } else {
            replaces_id
        };
        let urgency = match hints.get("urgency") {
            Some(Variant::U8(u)) => Some(*u),
            _ => None,
        };
        log.notified.push(Seen {
            id,
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            urgency,
            expire_timeout,
        });
        id
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.log.lock().unwrap().closed.push(id);
        // reason 3: closed by a call to CloseNotification
        emitter.notification_closed(id, 3).await?;
        Ok(())
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "fake".into(),
            "ferry tests".into(),
            "1".into(),
            "1.2".into(),
        )
    }

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

/// A running fake daemon on the bus.
struct Server {
    conn: zbus::Connection,
    log: Arc<Mutex<Log>>,
}

impl Server {
    async fn start(bus: &Bus, markup: bool, first_id: u32) -> Self {
        let log = Arc::new(Mutex::new(Log::default()));
        let conn = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(NAME)
            .unwrap()
            .serve_at(
                PATH,
                Fake {
                    log: log.clone(),
                    markup,
                    first_id,
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        Self { conn, log }
    }

    async fn press(&self, id: u32, key: &str) {
        let emitter = SignalEmitter::new(&self.conn, PATH).unwrap();
        emitter.action_invoked(id, key).await.unwrap();
    }

    fn notified(&self) -> Vec<Seen> {
        self.log.lock().unwrap().notified.clone()
    }

    fn closed(&self) -> Vec<u32> {
        self.log.lock().unwrap().closed.clone()
    }
}

/// Waits until nobody owns the notification daemon's name on the bus: the fake is gone, and
/// the bus has noticed.
async fn wait_unowned(bus: &Bus) {
    let probe = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let dbus = zbus::fdo::DBusProxy::new(&probe).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while dbus
        .name_has_owner(zbus::names::BusName::try_from(NAME).unwrap())
        .await
        .unwrap()
    {
        assert!(
            Instant::now() < deadline,
            "the fake daemon still owns {NAME}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Polls `f` until it returns `Some` (up to 20 s).
fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn code_note(name: &str, code: u32) -> CtlMsg {
    CtlMsg::Pairing(PairNote::Code {
        peer: Fingerprint([7; 32]),
        name: name.into(),
        remote: SocketAddr::from(([127, 0, 0, 1], 4243)),
        code: PairCode(code),
    })
}

/// A client subscribed to `topic`.
fn watch(d: &ferryd::Daemon, topic: &str) -> Client {
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&[topic]);
    c
}

/// The data of the next `notice` event (up to 20 s).
fn notice(c: &mut Client) -> Value {
    c.event_within("notice", Duration::from_secs(20))
        .expect("no notice event within 20 s")["data"]
        .clone()
}

fn layout_by(fake: PeerId, counter: u64, origin: (i32, i32)) -> Layout {
    let mut l = Layout::empty(fake);
    l.rev = Revision {
        counter,
        peer: fake,
    };
    l.set_origin(Origin {
        peer: fake,
        x: origin.0,
        y: origin.1,
    });
    l
}

fn counter(c: &mut Client, name: &str) -> u64 {
    c.result("status", json!(null))["counters"][name]
        .as_u64()
        .unwrap_or(0)
}

#[test]
fn toasts_reach_a_notification_daemon_on_a_private_bus_and_survive_its_absence() {
    let Some(bus) = Bus::start() else {
        eprintln!("skipped: there is no dbus-daemon to start a private bus with");
        return;
    };
    let rt = rt();
    let env = Env::new();
    let mut o = options(&env);
    o.toasts = Options {
        mode: Mode::Bus(bus.address.clone()),
        ..Options::default()
    };
    let d = ferryd::Daemon::start(o).unwrap();
    // The pairing window is open (the connection that opened it stays idle), so a code that
    // `net` would report has somewhere to go. Its events are not read here.
    let mut opener = Client::connect(d.socket_path());
    opener.result("pair.accept", json!(null));
    let mut ctl = Client::connect(d.socket_path());
    let (mut notices, mut ui) = (watch(&d, "notice"), watch(&d, "ui"));

    // --- a notification daemon that advertises markup ---------------------------------------
    let server = rt.block_on(Server::start(&bus, true, 100));
    assert!(d.send_ctl(code_note("a&b<c>", 418_207)));
    let seen = until("the code toast", || server.notified().first().cloned());
    assert_eq!(
        seen,
        Seen {
            id: 100,
            app_name: "ferry".into(),
            replaces_id: 0,
            app_icon: "input-mouse".into(),
            summary: "Pairing code 418 207".into(),
            // `&`, `<` and `>` of a peer's name are escaped for a daemon that renders markup
            body: "a&amp;b&lt;c&gt; is asking to pair. Confirm only if it shows the same code."
                .into(),
            actions: vec!["open".into(), "Open".into()],
            urgency: Some(2),
            expire_timeout: seen.expire_timeout,
        }
    );
    assert!(
        (1..=120_000).contains(&seen.expire_timeout),
        "{}",
        seen.expire_timeout
    );
    // the notice has the words ferryd wrote, not the markup-safe ones the daemon was given
    let n = notice(&mut notices);
    assert_eq!(
        (
            n["kind"].as_str(),
            n["urgency"].as_str(),
            n["peer"].as_str()
        ),
        (Some("pair_code"), Some("critical"), Some("a&b<c>"))
    );
    assert_eq!(n["title"], "Pairing code 418 207");
    assert_eq!(
        n["body"],
        "a&b<c> is asking to pair. Confirm only if it shows the same code."
    );

    // the user answered here: the code toast is taken down
    assert!(d.send_ctl(CtlMsg::Pairing(PairNote::WaitingPeer)));
    until("the close", || (server.closed() == [100]).then_some(()));

    // a second request: the user presses the button, and ferryd hears it
    assert!(d.send_ctl(code_note("lap", 123_456)));
    let second = until("the second code", || server.notified().get(1).cloned());
    assert_eq!(second.id, 101);
    assert_eq!(
        second.body,
        "lap is asking to pair. Confirm only if it shows the same code."
    );
    assert_eq!(counter(&mut ctl, "notification_actions"), 0);
    // not our button, not our notification: neither counts
    rt.block_on(server.press(second.id, "default"));
    rt.block_on(server.press(second.id + 50, "open"));
    rt.block_on(server.press(second.id, "open"));
    until("the action", || {
        (counter(&mut ctl, "notification_actions") == 1).then_some(())
    });
    // the button asked the frontends to open the Pair tab, once
    assert_eq!(notice(&mut notices)["kind"], "pair_code");
    let asked = ui
        .event_within("ui", Duration::from_secs(20))
        .expect("no ui event");
    assert_eq!(asked["data"], json!({"open": "pair"}));

    // the peer rejects the codes: the failure takes the code's place, in the same notification
    assert!(d.send_ctl(CtlMsg::Pairing(PairNote::Failed {
        reason: "rejected".into(),
        message: "cancelled: codes differ".into(),
        cancelled_by: None,
    })));
    let result = until("the result", || server.notified().get(2).cloned());
    assert_eq!(
        (
            result.summary.as_str(),
            result.body.as_str(),
            result.replaces_id,
            result.urgency,
            result.actions.is_empty()
        ),
        (
            "Pairing cancelled",
            "Codes differ",
            second.id,
            Some(1),
            true
        )
    );
    let n = notice(&mut notices);
    assert_eq!(
        (n["kind"].as_str(), n["title"].as_str(), n["body"].as_str()),
        (
            Some("pair_failed"),
            Some("Pairing cancelled"),
            Some("Codes differ")
        )
    );

    // an `attention` entry: lap, which is pinned, refuses us. A critical toast, no button
    let lap = fpx(1);
    ctl.result("trust", json!({"fingerprint": lap, "name": "lap"}));
    assert!(d.send_ctl(CtlMsg::Link(LinkUpdate {
        peer: lap.parse().unwrap(),
        state: LinkState::Refused,
        remote: None,
        rtt_us: None,
        proto: Some("2.0".into()),
        detail: Some("fingerprint changed".into()),
    })));
    let attention = until("the attention toast", || server.notified().get(3).cloned());
    assert_eq!(attention.summary, "lap cannot connect: fingerprint changed");
    assert_eq!(
        (
            attention.urgency,
            attention.actions.is_empty(),
            attention.replaces_id
        ),
        (Some(2), true, 0)
    );
    let n = notice(&mut notices);
    assert_eq!(
        (
            n["kind"].as_str(),
            n["urgency"].as_str(),
            n["peer"].as_str()
        ),
        (Some("attention"), Some("critical"), Some("lap"))
    );
    assert_eq!(n["title"], attention.summary.as_str());
    assert_eq!(n["body"], attention.body.as_str());

    // a layout edit that lost: lap's monitors are known, the user places it, and lap, which
    // never held that edit, pushes a higher revision
    assert!(d.send_ctl(CtlMsg::PeerUp {
        peer: lap.parse().unwrap(),
        monitors: vec![mon("eDP-1", 0, 0, 1366, 768)],
    }));
    ctl.result(
        "layout.place",
        json!({"peer": "lap", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    assert!(d.send_ctl(CtlMsg::PeerLayout {
        peer: lap.parse().unwrap(),
        layout: layout_by(PeerId::parse(&lap).unwrap(), 5, (0, 1080)),
    }));
    let replaced = until("the layout toast", || server.notified().get(4).cloned());
    assert_eq!(replaced.summary, "Layout replaced by lap");
    assert_eq!(
        (replaced.urgency, replaced.actions.is_empty()),
        (Some(1), true)
    );
    let n = notice(&mut notices);
    assert_eq!(
        (n["kind"].as_str(), n["title"].as_str(), n["peer"].as_str()),
        (
            Some("layout_replaced"),
            Some("Layout replaced by lap"),
            Some("lap")
        )
    );

    // nothing was undelivered so far, and the one action was counted once
    assert_eq!(counter(&mut ctl, "toasts_undelivered"), 0);
    assert_eq!(counter(&mut ctl, "notification_actions"), 1);
    assert_eq!(counter(&mut ctl, "toasts_shown"), 5);

    // --- no notification daemon --------------------------------------------------------------
    drop(server);
    rt.block_on(wait_unowned(&bus));
    assert!(d.send_ctl(code_note("lap", 111_222)));
    until("the toast counted as undelivered", || {
        (counter(&mut ctl, "toasts_undelivered") == 1).then_some(())
    });
    assert_eq!(counter(&mut ctl, "toasts_shown"), 6);
    // nobody showed it, and a frontend still has it
    let n = notice(&mut notices);
    assert_eq!(n["title"], "Pairing code 111 222");

    // --- one that does not render markup, and has other ids ---------------------------------
    let server = rt.block_on(Server::start(&bus, false, 7));
    assert!(d.send_ctl(code_note("a&b", 333_444)));
    let again = until("the toast after the daemon is back", || {
        server.notified().first().cloned()
    });
    // the code toast that never reached the old daemon left no id behind: a new notification
    assert_eq!((again.id, again.replaces_id), (7, 0));
    assert_eq!(
        again.body,
        "a&b is asking to pair. Confirm only if it shows the same code."
    );
    assert_eq!(counter(&mut ctl, "toasts_undelivered"), 1);
    assert_eq!(notice(&mut notices)["title"], "Pairing code 333 444");

    d.shutdown();
    // ferryd takes down the code toast it left on screen
    until("the code toast closed at shutdown", || {
        (server.closed() == [7]).then_some(())
    });
    drop(server);
    drop(rt);
    drop(bus);
}
