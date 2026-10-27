import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../views"
import "../Glyphs.js" as Glyphs
import "../Model.js" as Model
import "../Strings.js" as Strings

// The escape chord as key caps, and a button that records a new one: press Record, hold two or
// more of Super, Ctrl, Alt and Shift, then press the last key. Esc alone cancels (the mockup's
// rule). What was pressed is spelt by Model.chordFromKey; ferryd checks the chord and its refusal
// is shown by the row, so this file only listens and draws.
//
// While recording, this item owns every key (Tab included), so the panel's own keys cannot fire
// under the user's fingers. Leaving the item, by a click elsewhere, stops the recording.
FocusScope {
  id: root

  // The chord in force, as ferryd saved it ("Super+Ctrl+Escape").
  property string chord: ""
  // The go chord: modifiers only, finished with an arrow key.
  property bool modifiersOnly: false
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  readonly property color dim: Qt.darker(foreground, 1.4)

  property bool recording: false
  // The modifiers that are down right now, and what to tell the user about the last key.
  property var held: []
  property string message: ""
  property alias button: recordButton

  signal chosen(string chord)

  implicitHeight: column.implicitHeight
  implicitWidth: column.implicitWidth

  function start() {
    held = [];
    message = "";
    recording = true;
    recorder.forceActiveFocus();
  }

  function stop() {
    recording = false;
    held = [];
    message = "";
    recordButton.forceActiveFocus();
  }

  Column {
    id: column
    width: parent.width
    spacing: Style.space(8)

    Flow {
      width: parent.width
      spacing: Style.space(8)

      Row {
        id: caps
        spacing: Style.space(4)

        // The chord in force, or what is held while recording, then a ghost where the key goes.
        Repeater {
          model: root.recording ? root.held : Model.chordCaps(root.chord)

          Keycap {
            required property string modelData
            text: modelData
            held: root.recording
            foreground: root.foreground
            fontFamily: root.fontFamily
          }
        }

        Keycap {
          visible: root.recording
          text: "?"
          ghost: true
          foreground: root.foreground
          fontFamily: root.fontFamily
        }
      }

      Button {
        id: recordButton
        text: root.recording ? Strings.t("settings.record.cancel") : Strings.t("settings.record")
        iconText: root.recording ? Glyphs.ui("close") : Glyphs.ui("keyboard")
        bordered: true
        focusable: true
        active: root.recording
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        Accessible.name: text
        onClicked: root.recording ? root.stop() : root.start()
        FocusRing {}
      }
    }

    Text {
      width: parent.width
      visible: root.recording
      textFormat: Text.PlainText
      text: root.message !== "" ? root.message
        : (root.held.length > 0 ? Strings.t(root.modifiersOnly ? "settings.record.held.arrow" : "settings.record.held", [root.held.join("+")]) : Strings.t("settings.recording"))
      color: root.message !== "" ? Commons.Color.urgent : root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.WordWrap
      Accessible.role: Accessible.StaticText
    }

    Text {
      width: parent.width
      visible: root.recording
      textFormat: Text.PlainText
      text: Strings.t(root.modifiersOnly ? "settings.record.help.arrow" : "settings.record.help")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.WordWrap
      opacity: 0.85
    }
  }

  // The key catcher. Focused only while recording; it takes every key then.
  Item {
    id: recorder
    // Zero size and still visible: an invisible item cannot hold the focus.
    width: 0
    height: 0
    activeFocusOnTab: false

    Keys.priority: Keys.BeforeItem
    Keys.onPressed: function (event) {
      if (!root.recording) return;
      var r = Model.chordFromKey(event.key, event.modifiers, event.text, root.modifiersOnly);
      event.accepted = true;
      switch (r.state) {
        case "holding":
          root.held = r.held;
          root.message = "";
          break;
        case "cancel":
          root.stop();
          break;
        case "refused":
          root.held = r.held;
          root.message = r.message;
          break;
        case "done":
          root.stop();
          root.chosen(r.chord);
          break;
      }
    }
    Keys.onReleased: function (event) {
      if (!root.recording) return;
      root.held = Model.chordModifiers(event.modifiers);
      event.accepted = true;
    }
    onActiveFocusChanged: if (!activeFocus && root.recording) root.recording = false
  }
}
