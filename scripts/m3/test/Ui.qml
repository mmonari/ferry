import QtQuick
import Quickshell
import "ferry" as Ferry

// The popover and the Pair panel, as a user drives them, against the fake daemon: the real QML, the real
// FerryClient, and the same functions the buttons call. It renames and unpairs a peer, answers a
// code from here and from "another connection", cancels, lets a window run out, opens the panel in the
// middle of a pairing, and checks after each what is on screen. Prints one line per check and a verdict;
// ui.sh turns that into an exit status.
ShellRoot {
  id: root

  readonly property var c: Ferry.FerryClient
  readonly property var pop: popover
  readonly property var pp: card.viewItem
  property int failures: 0
  property int checks: 0

  function check(name, ok, detail) {
    checks++;
    if (!ok) failures++;
    console.log("UI " + (ok ? "ok   " : "FAIL ") + name + (ok ? "" : "  <- " + detail));
  }

  function fp(name) {
    var peers = c.status ? c.status.peers : [];
    for (var i = 0; i < peers.length; i++) if (peers[i].name === name) return peers[i].fingerprint;
    return "";
  }

  function names() {
    return pop.rows.map(function (r) { return r.name; }).join(",");
  }

  // a fresh daemon in a scene, the panel back at its start, the popover plain
  function fresh(scene, script, done) {
    c.call("fake.reset", { scene: scene || "ready" }, function () {
      c.call("fake.pair_next", Object.assign({ connect_ms: 60, other_ms: 400, dial_delay_ms: 30 }, script || {}), function () {
        if (pp && pp.reset) pp.reset();
        pop.collapseAll();
        c.notices = [];
        waitFor(function () { return c.status && c.status.peers.length === (scene === "idle-first" ? 0 : 2) && c.state !== "off"; }, 4000, function () { waitMs(150, done); });
      });
    });
  }

  property int step: -1
  function next() { step++; Qt.callLater(steps[step] || finish); }

  property var steps: [
    // ------------------------------------------------------------------------------ the popover
    function () { waitFor(function () { return c.connected && c.status && c.peers && c.layout && pp; }, 5000, function (ok) {
      check("connects, and the Pair panel is the view that is up", ok && card.view === "pair", "connected=" + c.connected + " view=" + card.view);
      fresh("ready", null, next); }); },

    function () {
      check("the popover lists the trusted peers, and its hero says Ready", pop.rows.length === 2 && pop.hero.title === "Ready", names() + " / " + pop.hero.title);
      check("the keys of the rows are the fingerprints", pop.peerKeys.length === 2 && pop.peerKeys.indexOf(fp("lap")) >= 0, JSON.stringify(pop.peerKeys));
      next(); },

    function () {
      var item = pop.peerItem(fp("lap"));
      check("a row is there to press", item !== null, "no row");
      item.askRename();
      waitFor(function () { return item.mode === "rename"; }, 1000, function (ok) {
        check("pressing Rename opens the field in place", ok, "mode=" + item.mode);
        item.renameField.field.text = "studio";
        check("a valid name is accepted by the rule", item.renameField.check.ok === true, JSON.stringify(item.renameField.check));
        item.renameField.submit();
        waitFor(function () { return names().indexOf("studio") >= 0; }, 3000, function (ok2) {
          check("the rename reaches ferryd and the row follows", ok2, names());
          check("the row is the same item, not a rebuilt one (focus and edits survive an update)", pop.peerItem(fp("studio") || fp("lap")) === item, "rebuilt");
          check("the row closes when it is done", item.mode === "", "mode=" + item.mode);
          next(); }); }); },

    function () {
      var item = pop.peerItem(fp("studio"));
      item.askRename();
      waitFor(function () { return item.mode === "rename"; }, 1000, function () {
        item.renameField.field.text = "my laptop";
        check("a name with a space is refused by the rule, in words, before anything is sent", item.renameField.check.ok === false && item.renameField.shownError !== "", item.renameField.shownError);
        item.renameField.submit();
        waitMs(300, function () {
          check("and nothing was sent: the name is as it was, the field still open", names().indexOf("studio") >= 0 && item.mode === "rename", names() + " " + item.mode);
          item.renameField.field.text = "tower";
          check("a name another peer has is refused by the rule", item.renameField.check.ok === false && /already has that name/.test(item.renameField.shownError), item.renameField.shownError);
          // what the rule would let through and ferryd refuses, ferryd says under the field
          pop.renamePeer(fp("studio"), "a:b");
          waitFor(function () { return item.renameField.refusal !== ""; }, 2000, function (ok) {
            check("a refusal from ferryd is shown under the field in ferryd's words", ok && /cannot contain/.test(item.renameField.refusal), item.renameField.refusal);
            item.close(false);
            next(); }); }); }); },

    function () {
      var item = pop.peerItem(fp("tower"));
      item.askUnpair(false);
      waitFor(function () { return item.mode === "unpair"; }, 1000, function (ok) {
        check("pressing Unpair asks first, in place", ok && item.thenPair === false, "mode=" + item.mode);
        pop.unpairPeer(fp("tower"));
        waitFor(function () { return pop.rows.length === 1; }, 3000, function (ok2) {
          check("confirming unpairs it: the list is one shorter", ok2, names());
          check("and a peer that is left stays", names() === "studio", names());
          next(); }); }); },

    function () {
      fresh("attention", null, function () {
        check("an attention state shows a card per problem, with the button that can fix it",
          pop.attentionItems.length === 2 && pop.attentionItems[0].action.kind === "unpair" && pop.attentionItems[1].action.kind === "copy",
          JSON.stringify(pop.attentionItems.map(function (i) { return i.action; })));
        pop.askUnpair("lap", true);
        var item = pop.peerItem(fp("lap"));
        waitFor(function () { return item && item.mode === "unpair" && item.thenPair === true; }, 1000, function (ok) {
          check("the card's button opens the question on the peer's own row, 'and pair again'", ok, item ? item.mode : "no row");
          Ferry.FerryUi.closePanel();
          pop.unpairPeer(fp("lap"));
          waitFor(function () { return Ferry.FerryUi.panelOpen && Ferry.FerryUi.panelView === "pair"; }, 3000, function (ok2) {
            check("once unpaired it goes straight on to the Pair panel", ok2, "open=" + Ferry.FerryUi.panelOpen + " view=" + Ferry.FerryUi.panelView);
            Ferry.FerryUi.closePanel();
            next(); }); }); }); },

    function () {
      fresh("ready", null, function () {
        c.call("fake.notice", { kind: "paired", title: "Paired with tower", peer: "tower" }, function () {
          c.call("fake.notice", { kind: "pair_code", title: "Pairing code 418 207", body: "tower2 is asking to pair.", peer: "tower2", urgency: "critical" }, function () {
            c.call("fake.notice", { kind: "peer_lost", title: "Lost lap", peer: "lap" }, function () {
              waitFor(function () { return c.notices.length === 3; }, 2000, function (ok) {
                check("every notice is kept with the time it arrived", ok && c.notices.every(function (n) { return typeof n.at === "number" && n.at > 0; }), JSON.stringify(c.notices));
                check("the pairing code is not kept anywhere in the client", JSON.stringify(c.notices).indexOf("418") < 0, JSON.stringify(c.notices));
                check("the activity list shows the last, newest first, and not the code's toast",
                  pop.activity.map(function (a) { return a.title; }).join("|") === "Lost lap|Paired with tower", JSON.stringify(pop.activity));
                next(); }); }); }); }); });
    },

    function () {
      fresh("ready", { other_ms: 600000 }, function () {
        c.pairAccept(function () {
          c.call("fake.pair_arrive", { name: "tower2", addr: "198.51.100.88:4243" }, function () {
            waitFor(function () { return pop.banner !== null && pop.banner.urgent === true; }, 3000, function (ok) {
              check("a request that arrives while the popover is open is a strip that asks for the user", ok && /tower2 is asking to pair/.test(pop.banner.text), JSON.stringify(pop.banner));
              check("the strip never carries the code", JSON.stringify(pop.banner).indexOf("418") < 0, JSON.stringify(pop.banner));
              c.pairCancel(function () { next(); }); }); }); }); });
    },

    function () {
      fresh("idle-first", null, function () {
        check("with nothing paired the popover is the first-run one", c.firstRun === true && pop.hero.title === "Pair your first machine", pop.hero.title);
        next(); });
    },

    // --------------------------------------------------------------------------- the Pair panel
    function () {
      fresh("ready", null, function () {
        check("the panel starts at its start screen", pp.screen === "start" && pp.flow.hero.title === "Pair a machine", pp.screen);
        check("an address that is not one is refused before the call, in words", pp.dial("") === false && pp.dialError === "Type an address first." && pp.dial("not an address") === false, pp.dialError);
        check("the trusted machines are listed, with Rename on a daemon that can", pp.rows.length === 2 && pp.canRename === true, "rows=" + pp.rows.length);
        check("this machine's address is known for the other one to type", pp.selfAddr !== null && pp.selfAddr.addr === "192.0.2.111", JSON.stringify(pp.selfAddr));
        check("a daemon with no discover.list lists nothing nearby, and the manual address is all there is", pp.canDiscover === false, "canDiscover");
        next(); });
    },

    function () {
      fresh("ready", { other_ms: 500 }, function () {
        check("dialing starts at once", pp.dial("198.51.100.88") === true && pp.screen === "connecting" && pp.flow.hero.title === "Pairing with 198.51.100.88", pp.screen + " " + pp.flow.hero.title);
        waitFor(function () { return pp.screen === "code"; }, 4000, function (ok) {
          check("then the code, big, in two groups, with who it is shared with", ok && pp.flow.codeText === "418 207" && pp.flow.peer.name === "workshop" && pp.flow.role === "dialer", JSON.stringify(pp.flow.peer) + " " + pp.flow.codeText);
          check("and a clock that runs", pp.flow.remainingText !== "" && pp.flow.fraction > 0.9, pp.flow.remainingText);
          pp.answer(true);
          waitFor(function () { return pp.screen === "paired"; }, 4000, function (ok2) {
            check("answering yes, and the other person too: paired", ok2 && pp.flow.hero.title === "Paired with workshop", pp.screen);
            check("the new peer is not placed, so Arrange is what is left", pp.flow.placed === false, String(pp.flow.placed));
            waitFor(function () { return pp.rows.length === 3; }, 3000, function (ok3) {
              check("the trusted list has it a moment later, when the state follows", ok3, "rows=" + pp.rows.length);
              pp.dismiss();
              check("Pair another goes back to the start", pp.screen === "start", pp.screen);
              next(); }); }); }); });
    },

    function () {
      fresh("ready", { other: "mismatch", other_ms: 300 }, function () {
        pp.dial("198.51.100.89");
        waitFor(function () { return pp.screen === "failed"; }, 4000, function (ok) {
          check("the other person says the codes differ: the failure, in words", ok && pp.flow.failure.title === "The codes did not match" && /Nothing was saved/.test(pp.flow.failure.body), ok ? pp.flow.failure.title : pp.screen);
          check("nothing was pinned", pp.rows.length === 2, "rows=" + pp.rows.length);
          pp.retry();
          check("Try again dials the same address again", pp.screen === "connecting" && pp.dialAddr === "198.51.100.89", pp.screen + " " + pp.dialAddr);
          waitFor(function () { return pp.screen === "failed"; }, 4000, function () {
            pp.dismiss();
            check("Back returns to the start", pp.screen === "start", pp.screen);
            next(); }); }); });
    },

    function () {
      fresh("ready", null, function () {
        pp.dial("10.254.0.9");
        waitFor(function () { return pp.dialError !== ""; }, 4000, function (ok) {
          check("an address that is not accepting says so in our words", ok && /is not waiting for a pairing request\. Open Pair on it first/.test(pp.dialError), pp.dialError);
          waitMs(2600, function () {
            check("and the connecting event ferryd leaves behind does not keep the panel waiting", pp.screen === "start", pp.screen);
            next(); }); }); });
    },

    function () {
      fresh("ready", { other_ms: 600000 }, function () {
        pp.dial("198.51.100.90");
        waitFor(function () { return pp.screen === "code"; }, 4000, function () {
          pp.cancel();
          waitFor(function () { return pp.screen === "start"; }, 3000, function (ok) {
            check("Cancel goes back to the start", ok, pp.screen);
            waitMs(600, function () {
              check("and your own cancel is not shown as a failure", pp.screen === "start" && pp.flow.failure === null, pp.screen);
              next(); }); }); }); });
    },

    function () {
      fresh("ready", { other_ms: 600000, name: "tower2" }, function () {
        pp.accept();
        waitFor(function () { return pp.screen === "listening"; }, 3000, function (ok) {
          check("Accept opens the window: listening, with the address to give", ok && pp.flow.role === "acceptor" && pp.flow.remainingText !== "", pp.screen);
          c.call("fake.pair_arrive", { addr: "198.51.100.88:4243", other_ms: 1500 }, function () {
            waitFor(function () { return pp.screen === "code"; }, 4000, function (ok2) {
              check("a request arrives: the code, for the acceptor", ok2 && pp.flow.role === "acceptor" && pp.flow.hero.title === "Pairing request from tower2", pp.flow.hero.title);
              c.call("fake.pair_answer", { match: true }, function () {
                waitFor(function () { return pp.screen === "waiting"; }, 3000, function (ok3) {
                  check("an answer from another connection counts: the panel follows without having answered", ok3, pp.screen);
                  waitFor(function () { return pp.screen === "paired"; }, 5000, function (ok4) {
                    check("and ends paired when the other person confirms", ok4 && pp.flow.hero.title === "Paired with tower2", pp.screen);
                    pp.dismiss();
                    next(); }); }); }); }); }); }); });
    },

    // the window stays open after a failed attempt, until the third
    function () {
      fresh("ready", { other: "mismatch", other_ms: 150, name: "tower2" }, function () {
        pp.accept();
        waitFor(function () { return pp.screen === "listening"; }, 3000, function () {
          var round = function (n, done) {
            var before = pp.failure ? pp.failure.at : 0;
            c.call("fake.pair_arrive", {}, function () {
              if (n === 3) { waitFor(function () { return pp.screen === "closed"; }, 4000, done); return; }
              waitFor(function () { return pp.screen === "listening" && pp.failure !== null && pp.failure.at > before; }, 4000, done);
            });
          };
          round(1, function (ok) {
            check("a failed attempt leaves the window open and the panel says what happened", ok && pp.flow.lastFailure !== null && pp.flow.lastFailure.title === "The codes did not match", ok ? "" : pp.screen);
            round(2, function () {
              round(3, function (ok2) {
                check("the third failure closes the window, and says so", ok2 && pp.flow.failure.title === "Too many attempts failed", ok2 ? "" : pp.screen);
                pp.dismiss();
                next(); }); }); }); }); });
    },

    function () {
      fresh("ready", { other: "silent", window_s: 1.2 }, function () {
        pp.dial("198.51.100.91");
        waitFor(function () { return pp.screen === "code"; }, 4000, function () {
          pp.answer(true);
          waitFor(function () { return pp.screen === "failed"; }, 6000, function (ok) {
            check("a window that runs out ends in a failure that says it timed out", ok && pp.flow.failure.title === "Pairing timed out", ok ? pp.flow.failure.title : pp.screen);
            pp.dismiss();
            next(); }); }); });
    },

    function () {
      fresh("ready", { other_ms: 600000, name: "tower2" }, function () {
        c.call("fake.pair_elsewhere", { role: "acceptor" }, function () {
          waitFor(function () { return pp.screen === "listening"; }, 3000, function (ok) {
            check("a window some terminal opened shows here too", ok && pp.flow.role === "acceptor", pp.screen);
            c.call("fake.pair_arrive", {}, function () {
              waitFor(function () { return pp.screen === "code"; }, 4000, function () {
                pp.cancel();
                // a cancel from here is a cancel: the attempt fails and the window closes, and the user's own
                // Cancel is not news, so the panel is back at its start
                waitFor(function () { return pp.screen === "start" && !pp.live; }, 3000, function (ok3) {
                  check("at the code, Cancel from here cancels a pairing a terminal opened", ok3 && pp.notice === "", pp.screen + " " + pp.notice);
                  pp.dismiss();
                  next(); }); }); }); }); }); });
    },

    function () {
      // a daemon that tells the code only to the connection that started the pairing: status has the stage and no code
      fresh("ready", { other_ms: 600000, name: "tower2" }, function () {
        c.call("fake.pair_elsewhere", { role: "acceptor" }, function () {
          c.call("fake.pair_arrive", {}, function () {
            waitFor(function () { return pp.screen === "code"; }, 3000, function () {
              c.pairing = null;
              waitFor(function () { return pp.screen === "waiting" && pp.flow.noCode; }, 3000, function (ok) {
                check("a pairing only status shows has no code, and the panel says where it is shown", ok && pp.flow.code === "", pp.screen);
                check("it offers no Cancel that could only fail, and no Same code?", ok && !pp.cancelButton.visible && !pp.flow.canAnswer, "cancel=" + pp.cancelButton.visible);
                pp.cancel();
                waitMs(300, function () {
                  check("and asking anyway says nothing and changes nothing", pp.notice === "" && pp.screen === "waiting", pp.screen + " " + pp.notice);
                  pp.dismiss();
                  next(); }); }); }); }); }); });
    },

    function () {
      fresh("ready", null, function () {
        var item = pp.trustedItem(fp("lap"));
        check("a trusted machine has its row", item !== null, "no row");
        item.askRename();
        waitFor(function () { return item.mode === "rename"; }, 1000, function (ok) {
          check("Rename opens in place in the panel too", ok, item.mode);
          item.renameField.field.text = "laptop";
          item.renameField.submit();
          waitFor(function () { return pp.rows.some(function (r) { return r.name === "laptop"; }); }, 3000, function (ok2) {
            check("and renames", ok2, JSON.stringify(pp.rows.map(function (r) { return r.name; })));
            untrustedCheck(next);
          });
        });
      });
    },

    function () {
      // the panel opened by the code's notification lands on the code; opened long after a result, on its start
      fresh("ready", { other_ms: 600000 }, function () {
        c.pairAccept(function () {
          c.call("fake.pair_arrive", { name: "tower2" }, function () {
            waitFor(function () { return c.pairing && c.pairing.stage === "code"; }, 3000, function () { reopenInTheMiddle(next); });
          });
        });
      });
    }
  ]

  // a peer the layout knows and this machine does not trust
  function untrustedCheck(done) {
    c.call("fake.untrusted", { name: "stranger" }, function () {
      waitFor(function () { return pp.untrusted.length === 1; }, 2000, function (ok) {
        check("a peer the layout knows and this machine does not trust is listed, by its short fingerprint", ok && pp.untrusted[0].name.length === 8, JSON.stringify(pp.untrusted));
        done();
      });
    });
  }

  // the panel closed and opened again: a new view, as the shell makes one
  function reopen(done) {
    card.view = "arrange";
    waitMs(150, function () {
      card.view = "pair";
      waitMs(150, done);
    });
  }

  function reopenInTheMiddle(done) {
    reopen(function () {
      check("a panel opened in the middle of a pairing lands on the code", pp.screen === "code" && pp.flow.codeText === "418 207", pp.screen);
      c.pairCancel(function () {
        waitFor(function () { return c.pairing && c.pairing.stage === "closed"; }, 3000, function () {
          c.pairingAt = Date.now() - 60000;
          reopen(function () {
            check("and one opened long after a result starts clean", pp.screen === "start", pp.screen);
            done();
          });
        });
      });
    });
  }

  function finish() {
    console.log("UI " + (failures === 0 ? "PASS" : "FAIL") + " " + (checks - failures) + "/" + checks);
    Qt.quit();
  }

  function waitMs(ms, done) {
    var timer = Qt.createQmlObject('import QtQuick; Timer { repeat: false; running: true }', root);
    timer.interval = ms;
    timer.triggered.connect(function () { timer.destroy(); done(); });
  }

  function waitFor(pred, ms, done) {
    var t0 = Date.now();
    var timer = Qt.createQmlObject('import QtQuick; Timer { interval: 25; repeat: true; running: true }', root);
    timer.triggered.connect(function () {
      var ok = false;
      try { ok = !!pred(); } catch (e) { ok = false; }
      if (ok || Date.now() - t0 > ms) { timer.running = false; timer.destroy(); done(ok); }
    });
  }

  FloatingWindow {
    visible: true
    implicitWidth: 1300
    implicitHeight: 720
    color: "#101018"

    Ferry.PopoverContent {
      id: popover
      x: 10
      y: 10
      width: 360
      height: 700
    }

    Ferry.PanelCard {
      id: card
      x: 390
      y: 10
      width: 890
      height: 700
      view: "pair"
    }
  }

  Timer { interval: 600; running: true; onTriggered: root.next() }
}
