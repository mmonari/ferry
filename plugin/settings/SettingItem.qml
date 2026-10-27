import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../views"
import "../Model.js" as Model
import "../Strings.js" as Strings

// One setting: the frame of SettingRow, the control its schema entry calls for, and the little
// state machine between them. Nothing here names a key. The control comes from Model.controlFor,
// the words from Model.settingLabel, the limits from the spec, and every value goes through
// FerryClient.setConfig, so ferryd checks it with the same code its file loader uses.
//
// State: `saved` is what ferryd says (and follows the `config` event, so a change made by
// `ferry config set` shows here at once). While the user steers a slider or types, `draft` is
// shown instead. A value is sent when it is final; a slider's steps are debounced; a send that
// is still out holds the next one back (the last wins). ferryd's refusal is kept in `error` and
// cleared by the next edit or by a change from elsewhere.
SettingRow {
  id: row

  property var spec: ({})
  readonly property string key: spec && spec.key ? String(spec.key) : ""
  readonly property string kind: Model.controlFor(spec)
  readonly property var applies: Model.appliesOf(spec)

  readonly property var saved: FerryClient.config ? FerryClient.config[key] : undefined
  readonly property bool pending: Model.isPending(FerryClient.configPending, key)
  property var draft: undefined
  property bool editing: false
  readonly property var shown: editing ? draft : saved

  // A `setup` key that was saved in this session still owes a `ferry setup`.
  property bool setupOwed: false
  property bool inflight: false
  property var queued: null

  // Tools for a test or the dev harness: set a value as if the user had, and focus a control.
  function simulateSet(value) { row.settle(value); }

  function startRecording(held) {
    if (control.item && control.item.start) {
      control.item.start();
      if (held && held.length) control.item.held = held;
    }
  }

  // Put the keys on the control of this row (never on its Reset button).
  function focusControl() {
    if (row.toggle) {
      row.forceActiveFocus(Qt.TabFocusReason);
      return;
    }
    var it = row.nextItemInFocusChain(true);
    for (var guard = 0; it && guard < 50; guard++) {
      if (it === row.resetButton) {
        it = it.nextItemInFocusChain(true);
        continue;
      }
      for (var p = it; p; p = p.parent) {
        if (p === row) {
          it.forceActiveFocus(Qt.TabFocusReason);
          return;
        }
      }
      return;
    }
  }

  // The chord recorder is listening for the chord (a `chord` row only).
  readonly property bool recording: control.item !== null && control.item.recording === true

  label: Model.settingLabel(spec)
  help: Model.settingHelp(spec)
  // A key with nothing to choose never changes, so it has no moment to apply at.
  unavailable: spec && spec.available === false
  appliesBadge: unavailable ? Strings.t("unavailable.badge") : (kind === "static" ? "" : applies.badge)
  // The badge is quiet until something has to be done: a saved change that waits for a restart, or
  // a chord that owes a ferry setup.
  appliesEmphasis: (applies.id === "restart" && pending) || (applies.id === "setup" && setupOwed)
  appliesTip: unavailable ? Strings.t("unavailable.tip") : applies.long
  toggle: kind === "toggle"
  // a switch for a feature that is not there is never drawn on
  checked: !unavailable && shown === true
  canReset: saved !== undefined && !Model.isDefault(spec, saved)
  resetTip: Strings.t("settings.reset.tip", [Model.defaultText(spec)])
  note: pending ? Strings.t("settings.restart.note") : (setupOwed ? Strings.t("settings.setup.note") : "")
  noteCommand: pending ? Strings.t("settings.banner.cmd") : (setupOwed ? Strings.t("settings.setup.cmd") : "")

  onClicked: row.settle(!(row.saved === true))
  onResetClicked: row.settle(row.spec.default)
  onSavedChanged: if (!row.editing && !row.inflight) row.error = null

  // ---- the state machine
  function steer(v) {
    row.draft = v;
    row.editing = true;
    row.error = null;
    debounce.restart();
  }

  function settle(v) {
    debounce.stop();
    row.draft = v;
    row.editing = true;
    row.error = null;
    row.send(v);
  }

  function reject(message) {
    debounce.stop();
    row.error = { code: "bad_input", message: message, hint: "" };
  }

  function send(v) {
    if (row.inflight) {
      row.queued = { value: v };
      return;
    }
    if (v === row.saved) {
      row.editing = false;
      return;
    }
    row.inflight = true;
    FerryClient.setConfig(row.key, v, function (result, error) {
      row.inflight = false;
      if (error) {
        row.error = Model.settingError(error, row.key);
      } else {
        row.error = null;
        if (result && result.applies === "setup") row.setupOwed = true;
      }
      if (row.queued !== null) {
        var next = row.queued.value;
        row.queued = null;
        row.send(next);
      } else {
        row.editing = false;
      }
    });
  }

  Timer {
    id: debounce
    interval: 250
    onTriggered: row.send(row.draft)
  }

  // ---- the control
  Loader {
    id: control
    width: parent.width
    sourceComponent: {
      switch (row.kind) {
        case "slider": return sliderControl;
        case "number": return numberControl;
        case "dropdown": return dropdownControl;
        case "chord": return chordControl;
        case "static": return staticControl;
        case "text": return textControl;
        default: return null;
      }
    }
  }

  Component {
    id: sliderControl

    SettingSlider {
      spec: row.spec
      value: row.shown
      foreground: row.foreground
      fontFamily: row.fontFamily
      accessibleName: row.label
      refused: row.error !== null
      onSteered: function (v) { row.steer(v) }
      onSettled: function (v) { row.settle(v) }
      onRejected: function (m) { row.reject(m) }
      onEdited: row.error = null
    }
  }

  Component {
    id: numberControl

    NumberEntry {
      spec: row.spec
      value: row.shown
      foreground: row.foreground
      fontFamily: row.fontFamily
      accessibleName: row.label
      refused: row.error !== null
      onCommitted: function (v) { row.settle(v) }
      onRejected: function (m) { row.reject(m) }
      onEdited: row.error = null
    }
  }

  Component {
    id: dropdownControl

    // The Loader sizes what it loads to its own width, so the dropdown sits in an Item and keeps
    // the width it asks for.
    Item {
      height: dropdown.implicitHeight

      Dropdown {
        id: dropdown
        width: Math.min(parent.width, Style.space(280))
        showLabel: false
        value: row.shown === undefined ? "" : String(row.shown)
        options: Model.choiceOptions(row.spec)
        foreground: row.foreground
        fontFamily: row.fontFamily
        Accessible.name: row.label
        onChanged: function (v) { row.settle(v) }

        GroupFocusRing {}
      }
    }
  }

  Component {
    id: staticControl

    Text {
      textFormat: Text.PlainText
      text: Model.formatValue(row.spec, row.shown)
      color: row.foreground
      font.family: row.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      Accessible.name: row.label
      Accessible.description: text
    }
  }

  Component {
    id: textControl

    TextField {
      id: field
      width: Math.min(parent.width, Style.space(300))
      foreground: row.foreground
      verticalPadding: Style.space(4)
      font.family: row.fontFamily
      font.pixelSize: Style.font.body
      placeholderText: row.spec.default === "" ? Strings.t("settings.hostname") : ""
      // the placeholder must not read as a value, in light themes too (#40): faint, and in italics
      placeholderTextColor: Util.alpha(row.foreground, 0.45)
      font.italic: field.text === ""
      selectByMouse: true
      activeFocusOnTab: true
      Accessible.name: row.label
      Accessible.role: Accessible.EditableText

      // The user typed since the field last showed the saved value: only then does a change made
      // elsewhere wait (D-3).
      property bool typed: false
      // a refused name stays in the field to be fixed
      readonly property bool refused: row.error !== null
      onRefusedChanged: if (refused) typed = true
      function sync() {
        field.text = row.shown === undefined || row.shown === null ? "" : String(row.shown);
        field.typed = false;
      }
      Component.onCompleted: sync()
      Connections {
        target: row
        function onShownChanged() { if (!field.typed) field.sync(); }
      }
      onTextEdited: {
        field.typed = true;
        row.error = null;
      }
      onEditingFinished: {
        var r = Model.parseText(field.text);
        field.text = r.value;
        field.typed = false;
        if (r.value !== row.saved) row.settle(r.value);
      }
      onActiveFocusChanged: if (!activeFocus) field.sync()
      Keys.onPressed: function (event) {
        if (event.key === Qt.Key_Escape && field.text !== (row.saved === undefined ? "" : String(row.saved))) {
          field.sync();
          event.accepted = true;
        }
      }

      FocusRing {}
    }
  }

  Component {
    id: chordControl

    ChordRecorder {
      chord: row.shown === undefined ? "" : String(row.shown)
      modifiersOnly: Model.chordIsModifiersOnly(row.key)
      foreground: row.foreground
      fontFamily: row.fontFamily
      onChosen: function (c) { row.settle(c) }
    }
  }
}
