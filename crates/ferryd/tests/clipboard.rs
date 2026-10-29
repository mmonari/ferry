//! Clipboard sharing (FR-CLIP-01–07), in process: a capturing daemon (fake portal)
//! whose clipboard is a [`FakeClipboard`], and a fake peer on loopback that speaks the
//! clipboard messages and data streams itself. Nothing here touches a compositor.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::time::Duration;

use ferry_core::source::{PortalCmd, SourceEvent};
use ferry_core::wire::clip::{AbortReason, DataHeader};
use ferry_core::wire::{Control, FEATURE_CLIPBOARD, FEATURE_PEER_STATE, LeaveReason};
use ferry_net::Identity;
use ferryd::fake_peer::{FakePeer, hello_now, monitor};
use ferryd::{Daemon, FakeClipboard, FakePortal, InjectBackend, Recorder};
use serde_json::{Value, json};
use support::*;

const SECOND: Duration = Duration::from_secs(1);

struct Rig {
    d: Option<Daemon>,
    c: Client,
    rt: tokio::runtime::Runtime,
    p: FakePeer,
    portal: FakePortal,
    clip: FakeClipboard,
    _env: Env,
}

impl Rig {
    /// A capturing daemon with a fake clipboard, and a fake peer below HDMI-A-1 that lists
    /// `clipboard`.
    fn new() -> Self {
        let env = Env::new();
        let portal = FakePortal::new();
        let clip = FakeClipboard::new();
        let mut o = options(&env);
        o.inject = InjectBackend::Custom(Box::new(Recorder::new()));
        o.fake_portal = Some(portal.clone());
        o.clipboard = clip.backend();
        let d = Daemon::start(o).unwrap();
        let mut c = Client::connect(d.socket_path());
        let id = Identity::ephemeral().unwrap();
        let fp = id.fingerprint().to_string();
        c.result("trust", json!({"fingerprint": fp, "name": "fake"}));
        let rt = rt();
        let mut p = rt
            .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
            .unwrap();
        let mut hello = hello_now("fake", vec![monitor("eDP-1", 1366, 768)]);
        if let Control::Hello { features, .. } = &mut hello {
            *features = vec![FEATURE_PEER_STATE.into(), FEATURE_CLIPBOARD.into()];
        }
        rt.block_on(p.exchange_hello(&hello))
            .expect("the daemon answers Hello");
        wait_for("the peer up", || {
            let st = c.result("status", json!(null));
            let up = st["peers"]
                .as_array()?
                .iter()
                .any(|q| q["fingerprint"] == fp && q["state"] == "connected");
            up.then_some(())
        });
        c.result(
            "layout.place",
            json!({"peer": "fake", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
        );
        let mut r = Rig {
            d: Some(d),
            c,
            rt,
            p,
            portal,
            clip,
            _env: env,
        };
        r.wait_gate("armed");
        wait_for("the bridge up", || (r.clip.starts() == 1).then_some(()));
        r
    }

    fn counter(&mut self, name: &str) -> u64 {
        self.c.result("status", json!(null))["counters"][name]
            .as_u64()
            .unwrap_or(0)
    }

    fn wait_gate(&mut self, gate: &str) -> Value {
        let c = &mut self.c;
        wait_for(&format!("gate {gate}"), || {
            let st = c.result("status", json!(null));
            (st["capture"]["gate"] == gate).then_some(st)
        })
    }

    /// The next control frame that `want` accepts within `within`, skipping the others.
    fn frame_within(
        &mut self,
        within: Duration,
        want: impl Fn(&Control) -> bool,
    ) -> Option<Control> {
        let until = std::time::Instant::now() + within;
        loop {
            let left = until.checked_duration_since(std::time::Instant::now())?;
            match self.rt.block_on(self.p.next_frame(left)) {
                Some(m) if want(&m) => return Some(m),
                Some(_) => {}
                None => return None,
            }
        }
    }

    fn frame_where(&mut self, what: &str, want: impl Fn(&Control) -> bool) -> Control {
        self.frame_within(3 * SECOND, want)
            .unwrap_or_else(|| panic!("no {what} within 3 s"))
    }

    fn offer(&mut self) -> (u32, Vec<String>) {
        match self.frame_where("a ClipOffer", |m| matches!(m, Control::ClipOffer { .. })) {
            Control::ClipOffer { serial, mimes } => (serial, mimes),
            _ => unreachable!(),
        }
    }

    fn no_offer(&mut self, why: &str) {
        let got = self.frame_within(Duration::from_millis(300), |m| {
            matches!(m, Control::ClipOffer { .. })
        });
        assert!(got.is_none(), "{why}: {got:?}");
    }

    /// The daemon crosses into the fake.
    fn cross(&mut self) {
        assert!(self.portal.event(SourceEvent::Keymap {
            text: "xkb_keymap { desk };".into()
        }));
        assert!(self.portal.event(SourceEvent::StartEmulating));
        assert!(self.portal.event(SourceEvent::Activated {
            activation_id: 10,
            barrier_id: 1,
            x: 800.0,
            y: 1081.0,
            lock: ferry_core::source::LockCheck::Unlocked,
        }));
        self.frame_where("the daemon's Enter", |m| matches!(m, Control::Enter { .. }));
    }

    /// The fake asks for offer `serial` in `mime` as request `id` and reads the data stream:
    /// `Ok(bytes)`, or `Err(reason)` when the daemon refuses.
    fn request(&mut self, serial: u32, id: u32, mime: &str) -> Result<Vec<u8>, AbortReason> {
        self.p
            .send(&Control::ClipRequest {
                serial,
                id,
                mime: mime.into(),
            })
            .unwrap();
        let conn = self.p.conn.clone();
        let data = self.rt.spawn(async move {
            let mut r = conn.accept_data().await.unwrap();
            let mut h = [0u8; DataHeader::LEN];
            r.read_exact(&mut h).await.unwrap();
            let h = DataHeader::decode(&h);
            let mut out = Vec::new();
            while let Some(c) = r.read_chunk(1 << 16).await.unwrap() {
                out.extend_from_slice(&c);
            }
            (h, out)
        });
        let until = std::time::Instant::now() + 3 * SECOND;
        loop {
            if data.is_finished() {
                let (h, out) = self.rt.block_on(data).unwrap();
                assert_eq!(h.id, id);
                assert_eq!(h.len, out.len() as u64);
                return Ok(out);
            }
            if let Some(Control::ClipAbort { id: got, reason }) = self
                .frame_within(Duration::from_millis(20), |m| {
                    matches!(m, Control::ClipAbort { .. })
                })
            {
                assert_eq!(got, id);
                data.abort();
                return Err(AbortReason::from_code(reason));
            }
            assert!(
                std::time::Instant::now() < until,
                "no data and no abort for {id}"
            );
        }
    }

    fn set(&mut self, key: &str, value: Value) {
        let r = self
            .c
            .result("config.set", json!({"key": key, "value": value}));
        assert_eq!(r["applies"], "live", "{key}");
    }

    fn shutdown(&mut self) {
        if let Some(d) = self.d.take() {
            d.shutdown();
        }
    }
}

/// A copy is offered only to the partner, the peer the pointer last crossed to; the
/// crossing sends the offer it missed. Only the types move until the peer pastes.
#[test]
fn a_copy_goes_to_the_peer_the_pointer_crosses_to_and_its_data_only_on_a_paste() {
    let mut r = Rig::new();
    r.clip.copy(&[
        ("text/plain;charset=utf-8", "héllo from desk".as_bytes()),
        ("text/uri-list", b"file:///home/x/secret.txt"),
    ]);
    r.no_offer("no crossing yet, so no partner");

    r.cross();
    let (serial, mimes) = r.offer();
    assert_eq!(mimes, ["text/plain;charset=utf-8"], "files are not shared");
    assert_eq!(r.counter("clip_served"), 0, "no data before a paste");
    assert_eq!(
        r.request(serial, 1, "text/plain;charset=utf-8").unwrap(),
        "héllo from desk".as_bytes()
    );
    assert_eq!(r.counter("clip_served"), 1);

    // While crossed, the next copy goes at once; a request for the old offer is stale.
    let png: Vec<u8> = (0..3u32 << 20).map(|i| (i * 7 % 251) as u8).collect();
    r.clip.copy(&[("image/png", &png)]);
    let (next, mimes) = r.offer();
    assert_eq!(mimes, ["image/png"]);
    assert!(next > serial);
    assert_eq!(
        r.request(serial, 3, "text/plain;charset=utf-8"),
        Err(AbortReason::Stale)
    );
    assert!(r.request(next, 5, "image/png").unwrap() == png);
    assert_eq!(r.request(next, 7, "text/html"), Err(AbortReason::Stale));
    r.shutdown();
}

/// The other way: the fake enters the daemon and so becomes its partner; the daemon's local
/// copies go to it. An offer from the fake makes the daemon the selection, and a paste there
/// asks the fake and gets its bytes; the same paste again is answered from the cache.
#[test]
fn an_offer_from_the_peer_pastes_here_once_over_the_network() {
    let mut r = Rig::new();
    r.p.keymap("xkb_keymap { fake };").unwrap();
    r.p.enter(523.0, 0.0).unwrap();
    r.frame_where("HeldAck#1", |m| matches!(m, Control::HeldAck { n: 1, .. }));
    r.clip
        .copy(&[("text/plain", b"typed on desk while lap drives it")]);
    let (serial, _) = r.offer();
    assert_eq!(
        r.request(serial, 1, "text/plain").unwrap(),
        b"typed on desk while lap drives it"
    );
    r.p.leave(LeaveReason::Home).unwrap();

    r.p.send(&Control::ClipOffer {
        serial: 7,
        mimes: vec!["image/png".into(), "application/x-ferry-source".into()],
    })
    .unwrap();
    assert_eq!(
        wait_for("the daemon's selection", || r.clip.offered()),
        ["image/png"]
    );
    let clip = r.clip.clone();
    let paste = std::thread::spawn(move || clip.paste("image/png").unwrap());
    let id = match r.frame_where("a ClipRequest", |m| {
        matches!(m, Control::ClipRequest { .. })
    }) {
        Control::ClipRequest { serial, id, mime } => {
            assert_eq!((serial, mime.as_str()), (7, "image/png"));
            assert_eq!(id % 2, 0, "the acceptor's ids are even");
            id
        }
        _ => unreachable!(),
    };
    let conn = r.p.conn.clone();
    r.rt.block_on(async {
        let mut s = conn.open_data().await.unwrap();
        s.write_all(&DataHeader { id, len: 5 }.encode())
            .await
            .unwrap();
        s.write_all(b"\x89PNG!").await.unwrap();
        s.finish();
    });
    assert_eq!(paste.join().unwrap(), b"\x89PNG!");
    wait_for("the cache", || {
        (r.counter("clip_pastes") == 1).then_some(())
    });
    // The cache fills just after the paste ends; a paste right behind it may still ask.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(r.clip.paste("image/png").unwrap(), b"\x89PNG!");
    assert_eq!(r.counter("clip_pastes_cached"), 1);

    // A refusal from the peer is an empty paste.
    r.p.send(&Control::ClipOffer {
        serial: 8,
        mimes: vec!["text/plain".into()],
    })
    .unwrap();
    wait_for("the new selection", || {
        (r.clip.offered()? == ["text/plain"]).then_some(())
    });
    let clip = r.clip.clone();
    let paste = std::thread::spawn(move || clip.paste("text/plain").unwrap());
    let id = match r.frame_where("a ClipRequest", |m| {
        matches!(m, Control::ClipRequest { .. })
    }) {
        Control::ClipRequest { id, .. } => id,
        _ => unreachable!(),
    };
    r.p.send(&Control::ClipAbort {
        id,
        reason: AbortReason::TooLarge.code(),
    })
    .unwrap();
    assert_eq!(paste.join().unwrap(), b"");

    // The offering peer leaving withdraws its item.
    let p = r.p;
    p.conn.close(ferry_net::CloseCode::SHUTDOWN, "bye");
    wait_for("the selection withdrawn", || {
        r.clip.offered().is_none().then_some(())
    });
    if let Some(d) = r.d.take() {
        d.shutdown();
    }
}

/// A password manager's item is never offered, an item over
/// `max_bytes` is refused, and sharing off (live) offers, takes and serves nothing until it is
/// on again.
#[test]
fn passwords_big_items_and_sharing_off() {
    let mut r = Rig::new();
    r.cross();
    r.clip.copy(&[
        ("text/plain", b"hunter2"),
        ("x-kde-passwordManagerHint", b"secret"),
    ]);
    r.no_offer("a password manager's item");
    r.clip.copy(&[("text/plain", b"public")]);
    let (serial, _) = r.offer();
    assert_eq!(r.request(serial, 1, "text/plain").unwrap(), b"public");

    r.set("clipboard.max_bytes", json!(1024));
    r.clip.copy(&[("text/plain", &[b'x'; 4096])]);
    let (serial, _) = r.offer();
    assert_eq!(
        r.request(serial, 3, "text/plain"),
        Err(AbortReason::TooLarge)
    );

    r.set("clipboard.enabled", json!(false));
    r.clip.copy(&[("text/plain", b"not shared")]);
    r.no_offer("sharing off");
    assert_eq!(
        r.request(serial, 5, "text/plain"),
        Err(AbortReason::Unavailable)
    );
    r.p.send(&Control::ClipOffer {
        serial: 1,
        mimes: vec!["text/plain".into()],
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(r.clip.offered().is_none(), "an offer while off is ignored");

    r.set("clipboard.enabled", json!(true));
    wait_for("the bridge again", || (r.clip.starts() == 2).then_some(()));
    r.clip.copy(&[("text/plain", b"back")]);
    let (serial, _) = r.offer();
    assert_eq!(r.request(serial, 7, "text/plain").unwrap(), b"back");
    assert!(r.portal.commands().contains(&PortalCmd::Enable));
    r.shutdown();
}
