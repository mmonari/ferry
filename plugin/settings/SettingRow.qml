import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../views"
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// The frame every setting sits in, one line of a flat hairline list (docs/design/panel-ui.md rule 2):
// the name on the left, the control on the right of the same line (`inline`), or under it at full
// width (a slider), then ferryd's own help as one dim line, a line for ferryd's refusal, and a line
// for what still has to happen (a restart, or ferry setup). No border and no fill, except the urgent
// border of a row in error. It decides nothing; SettingItem fills it in.
//
// A toggle row is one big button, like the shell's Toggle: the whole row takes the click, Enter
// and Space, and the switch is only the picture of the state.
BorderSurface {
  id: root

  property string label: ""
  property string help: ""
  property string appliesBadge: ""
  property bool appliesEmphasis: false
  property string appliesTip: ""
  // The feature this key belongs to is not built yet (`available: false` in the schema): the whole row is
  // shown dimmed and cannot be used, with the word "Later" for the badge and a line that says so.
  property bool unavailable: false

  // A toggle row is the control: it is focusable and takes the press.
  property bool toggle: false
  property bool checked: false
  property bool canReset: false
  property string resetTip: ""
  // { code, message, hint } or null
  property var error: null
  // What still has to happen, and the command that does it (copied, never run from here).
  property string note: ""
  property string noteCommand: ""
  // The control sits on the name's line, at the right (true), or under it at full width (false).
  property bool inline: true
  // Room kept at the right end of the name's line for something the control puts there (a slider's
  // number), and the line's height, so the control can line up with it.
  property real headReserve: 0
  readonly property real headHeight: head.height
  readonly property real slotTop: slot.y
  readonly property real headCenter: column.y + head.height / 2

  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  readonly property color dim: Qt.darker(foreground, 1.4)

  // SettingsPanel looks for this when it scrolls a focused control into view.
  readonly property bool isSettingRow: true
  default property alias content: slot.data
  property alias resetButton: resetBtn
  readonly property Item controlSlot: slot

  signal clicked()
  signal resetClicked()

  readonly property real gap: Style.space(8)
  readonly property real pad: Style.space(10)

  enabled: !unavailable
  opacity: unavailable ? 0.6 : 1
  width: parent ? parent.width : implicitWidth
  implicitHeight: column.implicitHeight + 2 * pad
  height: implicitHeight
  radius: Style.cornerRadius
  color: "transparent"
  borderSpec: root.error !== null ? Border.flat(Commons.Color.urgent, 1) : Border.flat("transparent", 0)

  activeFocusOnTab: toggle && !unavailable
  Keys.onReturnPressed: if (toggle) root.clicked()
  Keys.onEnterPressed: if (toggle) root.clicked()
  Keys.onSpacePressed: if (toggle) root.clicked()

  Accessible.role: toggle ? Accessible.CheckBox : Accessible.Grouping
  Accessible.name: label
  Accessible.description: help
  Accessible.checkable: toggle
  Accessible.checked: checked

  // The whole row is the button of a toggle row. Under the controls, so a Reset press is its own.
  MouseArea {
    id: mouse
    anchors.fill: parent
    enabled: root.toggle
    hoverEnabled: true
    cursorShape: root.toggle ? Qt.PointingHandCursor : Qt.ArrowCursor
    onClicked: root.clicked()
  }

  Column {
    id: column
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.top: parent.top
    anchors.topMargin: root.pad
    anchors.leftMargin: root.error !== null ? Style.space(8) : 0
    anchors.rightMargin: root.error !== null ? Style.space(8) : 0
    spacing: Style.space(6)

    // ---- name, when it applies, reset, the switch of a toggle row, and an inline control
    Item {
      id: head
      width: parent.width
      height: Math.max(title.implicitHeight, tail.implicitHeight, root.inline ? slot.height : 0, Style.space(26))

      Text {
        id: title
        anchors.left: parent.left
        anchors.right: tail.left
        anchors.rightMargin: root.gap
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: root.label
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        wrapMode: Text.WordWrap
        maximumLineCount: 2
        elide: Text.ElideRight
      }

      Row {
        id: tail
        anchors.right: parent.right
        anchors.rightMargin: root.inline ? (slot.width > 0 ? slot.width + Style.space(12) : 0) : root.headReserve
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(8)

        AppliesBadge {
          id: badge
          anchors.verticalCenter: parent.verticalCenter
          text: root.appliesBadge
          emphasis: root.appliesEmphasis
          foreground: root.foreground
          fontFamily: root.fontFamily
          Accessible.name: root.appliesTip
          Accessible.description: root.appliesTip
        }

        Button {
          id: resetBtn
          visible: root.canReset
          anchors.verticalCenter: parent.verticalCenter
          text: Strings.t("settings.reset")
          iconText: Glyphs.ui("undo")
          tooltipText: root.resetTip
          focusable: true
          foreground: root.dim
          fontFamily: root.fontFamily
          fontSize: Style.font.caption
          verticalPadding: Style.space(2)
          horizontalPadding: Style.space(6)
          Accessible.name: Strings.t("settings.reset") + ": " + root.label
          Accessible.description: root.resetTip
          onClicked: root.resetClicked()
          FocusRing {}
        }

        ToggleSwitch {
          id: toggleSwitch
          visible: root.toggle
          anchors.verticalCenter: parent.verticalCenter
          checked: root.checked
          interactive: false
          // the keyboard focus rings the switch too, not only the row (#39)
          cursorRing: true
          cursorPad: Style.space(3)
          hasCursor: root.toggle && root.activeFocus
          foreground: root.foreground
          accent: Commons.Color.accent

          // On is the accent, in every theme: the theme's selected fill alone is a dark knob on a
          // grey track in light themes, too close to off (#39).
          Rectangle {
            visible: toggleSwitch.checked
            anchors.centerIn: parent
            width: toggleSwitch.trackWidth
            height: toggleSwitch.trackHeight
            radius: toggleSwitch.rounded ? height / 2 : 0
            color: Util.alpha(Commons.Color.accent, 0.22)
            border.width: Math.max(1, Style.space(1.5))
            border.color: Commons.Color.accent
          }
        }
      }
    }

    // ---- room for a control under the name (a slider)
    Item {
      id: slotSpace
      width: parent.width
      visible: !root.inline && !root.toggle
      height: visible ? slot.height : 0
    }

  // ---- ferryd's own sentence about the key
    Text {
      width: parent.width
      visible: text !== ""
      textFormat: Text.PlainText
      text: root.help
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.WordWrap
      lineHeight: 1.15
      maximumLineCount: 2
      elide: Text.ElideRight
    }

    // ---- ferryd said no: its words and its hint, with a glyph, so it is not colour alone
    Row {
      width: parent.width
      visible: root.error !== null
      spacing: Style.space(6)
      Accessible.role: Accessible.AlertMessage
      Accessible.name: root.error ? root.error.message + (root.error.hint ? ". " + root.error.hint : "") : ""

      Text {
        textFormat: Text.PlainText
        text: Glyphs.ui("alert")
        color: Commons.Color.urgent
        font.family: root.fontFamily
        font.pixelSize: Style.font.icon
      }

      Column {
        width: parent.width - Style.space(22)
        spacing: Style.space(1)

        Text {
          width: parent.width
          textFormat: Text.PlainText
          text: root.error ? root.error.message : ""
          color: Commons.Color.urgent
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          font.bold: true
          wrapMode: Text.WordWrap
        }

        Text {
          width: parent.width
          visible: text !== ""
          textFormat: Text.PlainText
          text: root.error ? root.error.hint : ""
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.WordWrap
        }
      }
    }

    // ---- not built yet
    Row {
      width: parent.width
      visible: root.unavailable
      spacing: Style.space(6)

      Text {
        textFormat: Text.PlainText
        text: Glyphs.ui("info")
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.icon
      }

      Text {
        width: parent.width - Style.space(22)
        textFormat: Text.PlainText
        text: Strings.t("unavailable.note")
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        wrapMode: Text.WordWrap
      }
    }

    // ---- what still has to happen
    Row {
      width: parent.width
      visible: root.note !== "" && !root.unavailable
      spacing: Style.space(6)

      Text {
        textFormat: Text.PlainText
        text: Glyphs.ui("refresh")
        color: Commons.Color.accent
        font.family: root.fontFamily
        font.pixelSize: Style.font.icon
      }

      Text {
        width: parent.width - Style.space(22)
        textFormat: Text.PlainText
        text: root.note
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        wrapMode: Text.WordWrap
      }
    }

    CommandLine {
      width: parent.width
      visible: root.note !== "" && root.noteCommand !== "" && !root.unavailable
      command: root.noteCommand
      foreground: root.foreground
      fontFamily: root.fontFamily
    }
  }

  // ---- the control: after the head in the focus chain, drawn on the name's line or under it
  Item {
    id: slot
    visible: !root.toggle
    x: root.inline ? column.x + column.width - width : column.x
    y: root.inline ? column.y + head.y + (head.height - height) / 2 : column.y + slotSpace.y
    width: root.inline ? childrenRect.width : column.width
    height: childrenRect.height
  }

  FocusRing { target: root; visible: root.toggle && root.activeFocus }
}
