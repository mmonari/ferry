import QtQuick
import QtTest
import Quickshell
import "ferry" as Ferry
// Quickshell lists the types of a folder only when a file it scans imports the folder; the panel
// is loaded by path, so the Settings controls need this line here (see scripts/m3/harness/shell.qml).
import "ferry/settings" as FerrySettings

// The Settings view driven by the keyboard, end to end, against the fake daemon: real key events
// (QtTest's TestEvent sends them to the focused window) go through the real panel, the real
// FerryClient and the real socket. It proves what a screenshot cannot: that Tab walks every control,
// that a slider steps, a field commits and refuses, a toggle flips, a dropdown selects, the chord
// recorder records, Reset resets, and Esc closes only when nothing had a use for it.
// Prints one line per check and a verdict; settings-keys.sh turns that into an exit status.
ShellRoot {
  id: root

  property int checks: 0
  property int failures: 0
  property int closes: 0

  readonly property var client: Ferry.FerryClient

  function check(name, ok, detail) {
    checks++;
    if (!ok) failures++;
    console.log("SETTINGS " + (ok ? "ok   " : "FAIL ") + name + (ok ? "" : "  <- " + detail));
  }

  FloatingWindow {
    id: win
    visible: true
    implicitWidth: 900
    implicitHeight: 660
    color: "#202030"

    Ferry.PanelCard {
      id: card
      anchors.fill: parent
      view: "settings"
      onCloseRequested: root.closes++
    }
  }

  TestEvent { id: ev }

  // ------------------------------------------------------------------------------ helpers
  function view() { return card.viewItem; }
  function item(key) { return view().items[key]; }
  function cfg(key) { return client.config ? client.config[key] : undefined; }
  function focused() { return win.contentItem.Window.window.activeFocusItem; }

  function rowOf(it) {
    for (var p = it; p; p = p.parent) if (p.isSettingRow === true) return p.key;
    return "";
  }

  function kind(it) { return String(it).replace(/_QMLTYPE.*|\(0x.*/, ""); }

  // As when the panel has just opened: a fresh view, which holds the keys itself, with nothing inside
  // it focused. (A focus scope hands the focus back to the item that had it, so the view is built
  // again, which is what a real opening does.)
  function reopen(done) {
    card.view = "pair";
    later(150, function () {
      card.view = "settings";
      waitFor(function () { return view() && Object.keys(view().items).length === 18; }, 4000, function (ok) {
        if (!ok) check("the view can be opened again", false, "items=" + (view() ? Object.keys(view().items).length : "no view"));
        done();
      });
    });
  }

  function press(key, mods) { ev.keyClick(key, mods === undefined ? Qt.NoModifier : mods, -1); }

  function typeText(text) {
    for (var i = 0; i < text.length; i++) ev.keyClickChar(text.charAt(i), Qt.NoModifier, -1);
  }

  // The focus chain, as Tab walks it: what takes the focus after each press.
  function tabbed(n, forward) {
    var out = [];
    for (var i = 0; i < n; i++) {
      press(forward === false ? Qt.Key_Backtab : Qt.Key_Tab, forward === false ? Qt.ShiftModifier : Qt.NoModifier);
      out.push(rowOf(focused()) || kind(focused()));
    }
    return out;
  }

  property int step: -1
  function next() { step++; Qt.callLater(steps[step] || finish); }

  function waitFor(pred, ms, done) {
    var t0 = Date.now();
    var timer = Qt.createQmlObject('import QtQuick; Timer { interval: 25; repeat: true; running: true }', root);
    timer.triggered.connect(function () {
      var ok = false;
      try { ok = !!pred(); } catch (e) { ok = false; }
      if (ok || Date.now() - t0 > ms) { timer.running = false; timer.destroy(); done(ok); }
    });
  }

  function later(ms, f) {
    var timer = Qt.createQmlObject('import QtQuick; Timer { repeat: false; running: false }', root);
    timer.interval = ms;
    timer.triggered.connect(function () { timer.destroy(); f(); });
    timer.start();
  }

  property var steps: [
    // ---- the view is up, from a fake daemon with the real schema
    function () {
      waitFor(function () { return client.connected && client.configSchema && client.config && view() && Object.keys(view().items).length === 18; }, 6000, function (ok) {
        check("the view draws one setting per key of the schema", ok, "items=" + (view() ? Object.keys(view().items).length : "no view"));
        check("every key of the schema has a row", ok && client.configSchema.every(function (k) { return view().items[k.key] !== undefined; }), "");
        next();
      });
    },

    // ---- Tab walks every control, in the order they are drawn, and comes back round
    function () {
      reopen(function () {
      var seen = tabbed(26, true);
      var rows = [];
      seen.forEach(function (r) { if (r.indexOf(".") > 0 && rows.indexOf(r) < 0) rows.push(r); });
      check("Tab reaches every setting that has a control (a row with one choice, or a feature not built yet, has nothing to focus)", rows.length === 12 && rows.indexOf("input.emulation") < 0 && rows.indexOf("clipboard.enabled") < 0, rows.length + ": " + rows.join(", "));
      check("the first Tab from the open view goes into the first setting, not the header", seen[0] === "crossing.edge_resistance_px", seen.slice(0, 3).join(" > "));
      check("the header (tabs, close) comes after the last setting", seen.indexOf("ButtonGroup") > 0 && seen[seen.indexOf("ButtonGroup") + 1] === "PanelActionButton" && seen[seen.indexOf("ButtonGroup") - 1] === "safety.lock_check_budget_ms", seen.join(" > "));
      check("and the chain comes back round to the first setting", seen.lastIndexOf("crossing.edge_resistance_px") > 0, seen.join(" > "));
      reopen(function () {
        var back = tabbed(3, false);
        check("Shift+Tab from the open view goes into the last setting and walks backwards", back[0] === "safety.lock_check_budget_ms" && back[2] === "peer.port", back.join(" < "));
        next();
      });
      });
    },

    // ---- Up, Down, j and k move between the settings once the view has the keys
    function () {
      reopen(function () {
        press(Qt.Key_Down);
        check("Down from the open view enters the first setting", rowOf(focused()) === "crossing.edge_resistance_px", rowOf(focused()) + " / " + kind(focused()));
        var a = rowOf(focused());
        ev.keyClickChar("j", Qt.NoModifier, -1);
        check("j moves on, from the track to the number beside it", rowOf(focused()) === a && kind(focused()) === "TextField", rowOf(focused()) + " / " + kind(focused()));
        press(Qt.Key_Backtab, Qt.ShiftModifier);
        check("Shift+Tab moves back to the track", rowOf(focused()) === a && kind(focused()) === "QQuickItem", rowOf(focused()) + " / " + kind(focused()));
        ev.keyClickChar("k", Qt.NoModifier, -1);
        check("k on the first setting stays there: Up and Down never leave the settings (Tab does)", rowOf(focused()) === a && kind(focused()) === "QQuickItem", rowOf(focused()) + " / " + kind(focused()));
        reopen(function () {
          press(Qt.Key_Up);
          check("Up from the open view enters the last setting", rowOf(focused()) === "safety.lock_check_budget_ms", rowOf(focused()));
          next();
        });
      });
    },

    // ---- a slider: arrows step by one, Page and End and Home jump, and it is saved
    function () {
      view().focusKey("crossing.edge_resistance_px");
      check("focusing a slider row puts the keys on its track", rowOf(focused()) === "crossing.edge_resistance_px" && kind(focused()) === "QQuickItem", rowOf(focused()) + " / " + kind(focused()));
      press(Qt.Key_Right); press(Qt.Key_Right); press(Qt.Key_Right);
      check("the number beside it follows at once", item("crossing.edge_resistance_px").shown === 3, "shown=" + item("crossing.edge_resistance_px").shown);
      check("a step is not sent while the keys are still going (debounce)", cfg("crossing.edge_resistance_px") === 0, "saved=" + cfg("crossing.edge_resistance_px"));
      waitFor(function () { return cfg("crossing.edge_resistance_px") === 3; }, 2000, function (ok) {
        check("three steps right are saved as one value, 3", ok, "saved=" + cfg("crossing.edge_resistance_px"));
        press(Qt.Key_End);
        waitFor(function () { return cfg("crossing.edge_resistance_px") === 200; }, 2000, function (ok2) {
          check("End jumps to the top of the range and saves it", ok2, "saved=" + cfg("crossing.edge_resistance_px"));
          press(Qt.Key_PageDown);
          waitFor(function () { return cfg("crossing.edge_resistance_px") === 180; }, 2000, function (ok3) {
            check("Page Down steps a tenth of the range", ok3, "saved=" + cfg("crossing.edge_resistance_px"));
            press(Qt.Key_Home);
            waitFor(function () { return cfg("crossing.edge_resistance_px") === 0; }, 2000, function (ok4) {
              check("Home jumps back to the bottom", ok4, "saved=" + cfg("crossing.edge_resistance_px"));
              check("a live key says nothing is waiting", client.configPending.length === 0, JSON.stringify(client.configPending));
              next();
            });
          });
        });
      });
    },

    // ---- Left at the bottom does nothing, and does not send
    function () {
      var before = cfg("crossing.edge_resistance_px");
      press(Qt.Key_Left);
      later(500, function () {
        check("Left at the bottom of the range changes nothing", cfg("crossing.edge_resistance_px") === before && item("crossing.edge_resistance_px").shown === before, "saved=" + cfg("crossing.edge_resistance_px"));
        next();
      });
    },

    // ---- a number field: type, Enter saves; a value out of range is refused with ferryd's words
    function () {
      // edge delay is a slider with its field beside it (slider_max): Tab from the track reaches the field
      view().focusKey("crossing.edge_dwell_ms");
      press(Qt.Key_Tab);
      check("a number row puts the keys in its field", kind(focused()) === "TextField" && rowOf(focused()) === "crossing.edge_dwell_ms", kind(focused()) + " " + rowOf(focused()));
      focused().selectAll();
      typeText("150");
      press(Qt.Key_Return);
      waitFor(function () { return cfg("crossing.edge_dwell_ms") === 150; }, 2000, function (ok) {
        check("typing 150 and Enter saves 150", ok, "saved=" + cfg("crossing.edge_dwell_ms"));
        focused().selectAll();
        typeText("99999");
        press(Qt.Key_Return);
        waitFor(function () { return item("crossing.edge_dwell_ms").error !== null; }, 2000, function (ok2) {
          var e = item("crossing.edge_dwell_ms").error;
          check("a value out of range is refused", ok2 && e.code === "bad_value", JSON.stringify(e));
          check("the refusal names the value, not the key path, and the hint says what is allowed", ok2 && e.message === "99999 is out of range." && e.hint === "Allowed: 0 to 65535 ms.", JSON.stringify(e));
          check("nothing was saved", cfg("crossing.edge_dwell_ms") === 150, "saved=" + cfg("crossing.edge_dwell_ms"));
          const closed = root.closes;
          press(Qt.Key_Escape);
          check("Esc in a field with unsaved text puts the saved value back and does not close the panel", focused().text === "150" && root.closes === closed, "text=" + focused().text + " closes=" + root.closes);
          next();
        });
      });
    },

    // ---- text that is not a number never leaves the panel
    function () {
      focused().selectAll();
      typeText("abc");
      press(Qt.Key_Return);
      var e = item("crossing.edge_dwell_ms").error;
      check("text that is not a whole number is refused in the panel, before a call", e !== null && e.code === "bad_input" && /whole number/.test(e.message), JSON.stringify(e));
      press(Qt.Key_Escape);
      press(Qt.Key_Up);
      waitFor(function () { return cfg("crossing.edge_dwell_ms") === 151; }, 2000, function (ok) {
        check("Up in a field steps by one and saves", ok, "saved=" + cfg("crossing.edge_dwell_ms"));
        next();
      });
    },

    // ---- Reset puts a key back to its default
    function () {
      check("a changed key shows Reset", item("crossing.edge_dwell_ms").canReset === true, "");
      item("crossing.edge_dwell_ms").resetButton.forceActiveFocus();
      press(Qt.Key_Return);
      waitFor(function () { return cfg("crossing.edge_dwell_ms") === 0; }, 2000, function (ok) {
        check("Reset, on Enter, saves the default", ok, "saved=" + cfg("crossing.edge_dwell_ms"));
        check("and Reset goes away", item("crossing.edge_dwell_ms").canReset === false, "");
        next();
      });
    },

    // ---- a toggle: Space and Enter flip it, Esc closes the panel
    function () {
      view().focusKey("crossing.block_while_button_held");
      check("a toggle row is itself the focus", kind(focused()) === "SettingItem", kind(focused()));
      press(Qt.Key_Space);
      waitFor(function () { return cfg("crossing.block_while_button_held") === false; }, 2000, function (ok) {
        check("Space turns it off", ok, "saved=" + cfg("crossing.block_while_button_held"));
        press(Qt.Key_Return);
        waitFor(function () { return cfg("crossing.block_while_button_held") === true; }, 2000, function (ok2) {
          check("Enter turns it on again", ok2, "saved=" + cfg("crossing.block_while_button_held"));
          next();
        });
      });
    },

    // ---- a key that applies at a restart: saved, listed as pending, with the note and the command
    function () {
      // discovery is not built yet (its switch is disabled), so the restart key here is the port
      view().focusKey("peer.port");
      focused().selectAll();
      typeText("4250");
      press(Qt.Key_Return);
      waitFor(function () { return cfg("peer.port") === 4250 && client.configPending.indexOf("peer.port") >= 0; }, 2000, function (ok) {
        check("a restart key is saved and waits for a restart", ok, "saved=" + cfg("peer.port") + " pending=" + JSON.stringify(client.configPending));
        var it = item("peer.port");
        check("its row says so, with the command to copy", it.note === "Saved. Restart ferryd to apply it." && it.noteCommand === "systemctl --user restart ferryd", it.note + " | " + it.noteCommand);
        check("its badge is emphasised while it waits", it.appliesEmphasis === true, "");
        focused().selectAll();
        typeText("4243");
        press(Qt.Key_Return);
        waitFor(function () { return client.configPending.length === 0; }, 2000, function (ok2) {
          check("putting it back clears the wait: saved equals what runs", ok2, JSON.stringify(client.configPending));
          next();
        });
      });
    },

    // ---- a dropdown: Enter opens it, Down moves, Enter chooses
    function () {
      view().focusKey("input.capture");
      check("a dropdown row puts the keys on its trigger", rowOf(focused()) === "input.capture", rowOf(focused()) + " / " + kind(focused()));
      press(Qt.Key_Return);
      later(500, function () {
        press(Qt.Key_Down);
        press(Qt.Key_Return);
        waitFor(function () { return cfg("input.capture") === "portal"; }, 2000, function (ok) {
          check("Enter, Down, Enter chooses the next choice", ok, "saved=" + cfg("input.capture"));
          waitFor(function () { return client.configPending.indexOf("input.capture") >= 0; }, 2000, function (ok2) {
            check("the choice is a restart key, so it waits", ok2, JSON.stringify(client.configPending));
            next();
          });
        });
      });
    },

    // ---- the chord recorder: Escape alone cancels; Super+Alt+F12 is recorded and saved
    function () {
      view().focusKey("crossing.escape_chord");
      check("the chord row puts the keys on Record", rowOf(focused()) === "crossing.escape_chord" && kind(focused()) === "Button", rowOf(focused()) + " / " + kind(focused()));
      press(Qt.Key_Return);
      var rec = item("crossing.escape_chord");
      check("Enter starts recording", rec.recording === true, "recording=" + rec.recording);
      later(200, function () {
        const closed = root.closes;
        press(Qt.Key_Escape);
        check("Esc alone cancels the recording and does not close the panel", root.closes === closed && cfg("crossing.escape_chord") === "Super+Ctrl+Escape", "closes=" + root.closes + " saved=" + cfg("crossing.escape_chord"));
        press(Qt.Key_Return);
        later(200, function () {
          ev.keyPress(Qt.Key_Meta, Qt.MetaModifier, -1);
          ev.keyPress(Qt.Key_Alt, Qt.MetaModifier | Qt.AltModifier, -1);
          ev.keyPress(Qt.Key_Tab, Qt.MetaModifier | Qt.AltModifier, -1);
          later(100, function () {
            check("a key ferryd has no name for is refused in words and recording goes on", cfg("crossing.escape_chord") === "Super+Ctrl+Escape", "saved=" + cfg("crossing.escape_chord"));
            ev.keyClick(Qt.Key_F12, Qt.MetaModifier | Qt.AltModifier, -1);
            ev.keyRelease(Qt.Key_Alt, Qt.MetaModifier, -1);
            ev.keyRelease(Qt.Key_Meta, Qt.NoModifier, -1);
            waitFor(function () { return cfg("crossing.escape_chord") === "Super+Alt+F12"; }, 2000, function (ok) {
              check("Super+Alt+F12 is recorded and saved in the spelling ferryd keeps", ok, "saved=" + cfg("crossing.escape_chord"));
              check("a setup key owes a ferry setup, and its row says so", item("crossing.escape_chord").note.indexOf("ferry setup") >= 0 && item("crossing.escape_chord").noteCommand === "ferry setup", item("crossing.escape_chord").note);
              waitFor(function () { return client.status && client.status.escape_chord === "Super+Alt+F12"; }, 2000, function (ok3) {
                check("ferryd matches the new chord at once: status carries it", ok3, client.status ? client.status.escape_chord : "no status");
                next();
              });
            });
          });
        });
      });
    },

    // ---- a chord with one modifier is ferryd's to refuse, and its words are shown
    function () {
      view().focusKey("crossing.escape_chord");
      press(Qt.Key_Return);
      later(200, function () {
        ev.keyPress(Qt.Key_Control, Qt.ControlModifier, -1);
        ev.keyClick(Qt.Key_F11, Qt.ControlModifier, -1);
        ev.keyRelease(Qt.Key_Control, Qt.NoModifier, -1);
        waitFor(function () { return item("crossing.escape_chord").error !== null; }, 2000, function (ok) {
          var e = item("crossing.escape_chord").error;
          check("one modifier is refused by ferryd with its hint", ok && e.code === "bad_value" && /two modifiers/.test(e.message) && e.hint === "", JSON.stringify(e));
          check("the saved chord is unchanged", cfg("crossing.escape_chord") === "Super+Alt+F12", cfg("crossing.escape_chord"));
          next();
        });
      });
    },

    // ---- a name: text, Enter; a name ferryd refuses shows its words; Esc leaves
    function () {
      view().focusKey("peer.name");
      check("a name row puts the keys in its field", kind(focused()) === "TextField" && rowOf(focused()) === "peer.name", kind(focused()));
      typeText("two words");
      press(Qt.Key_Return);
      waitFor(function () { return item("peer.name").error !== null; }, 2000, function (ok) {
        var e = item("peer.name").error;
        check("a name with a space is refused with ferryd's own words", ok && e.code === "bad_value" && /cannot contain/.test(e.message), JSON.stringify(e));
        press(Qt.Key_Escape);
        check("Esc puts the saved name back", focused().text === "", "text=" + focused().text);
        typeText("desk2");
        press(Qt.Key_Return);
        waitFor(function () { return cfg("peer.name") === "desk2"; }, 2000, function (ok2) {
          check("a good name is saved and clears the refusal", ok2 && item("peer.name").error === null, "saved=" + cfg("peer.name"));
          next();
        });
      });
    },

    // ---- a change made somewhere else (ferry config set) shows here, live
    function () {
      client.call("fake.config_external", { key: "crossing.corner_dead_zone_px", value: 77 }, function () {
        waitFor(function () { return item("crossing.corner_dead_zone_px").shown === 77; }, 2000, function (ok) {
          check("a change from the CLI reaches the slider and its number at once", ok, "shown=" + item("crossing.corner_dead_zone_px").shown);
          next();
        });
      });
    },

    // ---- D-3: the e2e sequence. The panel saves port 4244 (pending) and turns notifications off,
    // the focus stays in the panel, then `ferry config set` puts both back from a terminal
    function () {
      view().focusKey("peer.port");
      focused().selectAll();
      typeText("4244");
      press(Qt.Key_Return);
      waitFor(function () { return cfg("peer.port") === 4244 && client.configPending.indexOf("peer.port") >= 0; }, 2000, function (ok) {
        check("D-3: port 4244 is saved from the panel and waits for a restart", ok, "saved=" + cfg("peer.port") + " " + JSON.stringify(client.configPending));
        later(30, function () {
          view().focusKey("notify.enabled");
          press(Qt.Key_Space);
          waitFor(function () { return cfg("notify.enabled") === false; }, 2000, function (ok2) {
            check("D-3: notifications are turned off from the panel", ok2, "saved=" + cfg("notify.enabled"));
            view().focusKey("peer.port");
            next();
          });
        });
      });
    },
    function () {
      client.call("fake.config_external", { key: "peer.port", value: 4243 }, function () {
        client.call("fake.config_external", { key: "notify.enabled", value: true }, function () {
          waitFor(function () { return client.configPending.indexOf("peer.port") < 0 && cfg("notify.enabled") === true; }, 2000, function (ok) {
            later(100, function () {
              var port = item("peer.port");
              var field = focused();
              check("D-3: an outside set reaches the port field that still has the focus", ok && rowOf(field) === "peer.port" && field.text === "4243", "text=" + (field ? field.text : "?") + " row=" + rowOf(field));
              check("D-3: the restart note goes when the outside set clears it", port.note === "", "note=" + port.note);
              check("D-3: an outside set reaches the toggle", item("notify.enabled").checked === true, "checked=" + item("notify.enabled").checked);
              next();
            });
          });
        });
      });
    },
    function () {
      // typed but not committed: an outside change must not overwrite it
      focused().selectAll();
      typeText("4250");
      client.call("fake.config_external", { key: "peer.port", value: 4260 }, function () {
        waitFor(function () { return cfg("peer.port") === 4260; }, 2000, function (ok) {
          later(100, function () {
            check("D-3: a field the user is typing in keeps what they typed", ok && focused().text === "4250", "text=" + focused().text);
            press(Qt.Key_Escape);
            check("D-3: Esc then shows the value set elsewhere", focused().text === "4260", "text=" + focused().text);
            client.call("fake.config_external", { key: "peer.port", value: 4243 }, function () { later(100, next); });
          });
        });
      });
    },

    // ---- config.toml cannot be written: every change is refused as read_only, and nothing moves
    // (keys are pressed after the reply's handler has returned: QtTest runs the event loop inside a
    // key press, and a socket that is read from inside its own handler does not see the next reply)
    function () {
      client.call("fake.config_readonly", { on: true }, function () {
        later(30, function () {
          view().focusKey("notify.enabled");
          press(Qt.Key_Space);
          waitFor(function () { return item("notify.enabled").error !== null; }, 2000, function (ok) {
            var e = item("notify.enabled").error;
            check("read_only is shown in our words", ok && e.code === "read_only" && /nothing was saved/.test(e.message), JSON.stringify(e));
            check("and the toggle did not move", cfg("notify.enabled") === true, "saved=" + cfg("notify.enabled"));
            client.call("fake.config_readonly", { on: false }, function () { next(); });
          });
        });
      });
    },

    // ---- Esc on a control that has no use for it closes the panel
    function () {
      view().focusKey("notify.enabled");
      const closed = root.closes;
      press(Qt.Key_Escape);
      check("Esc on a toggle closes the panel", root.closes === closed + 1, "closes=" + root.closes);
      next();
    },

    // ---- a restarted ferryd runs what was saved: nothing waits any more
    function () {
      client.call("fake.restart", { seconds: 1.0 }, function () {
        waitFor(function () { return !client.connected; }, 3000, function (gone) {
          check("the daemon goes away", gone, "");
          waitFor(function () { return client.connected && client.config && view() && client.configPending.length === 0; }, 8000, function (ok) {
            check("after the restart no key is pending, and the saved values are what runs", ok && cfg("discovery.mdns") === true && cfg("input.capture") === "portal", JSON.stringify(client.configPending) + " " + cfg("input.capture"));
            check("the view comes back by itself", view() && Object.keys(view().items).length === 18, "");
            view().focusKey("peer.port");
            check("D-3: after the restart the port field shows the saved port", focused() && focused().text === String(cfg("peer.port")), "text=" + (focused() ? focused().text : "?") + " saved=" + cfg("peer.port"));
            check("D-3: after the restart the toggle shows the saved value", item("notify.enabled").checked === (cfg("notify.enabled") === true), "");
            next();
          });
        });
      });
    }
  ]

  function finish() {
    console.log("SETTINGS " + (failures === 0 ? "PASS" : "FAIL") + " " + (checks - failures) + "/" + checks);
    Qt.quit();
  }

  Timer { interval: 600; running: true; onTriggered: root.next() }
}
