import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// A shell command the user can copy, in the look of the mockup's code line: a "$", the command
// in the font the shell uses, and a Copy button. Copying goes through wl-copy, which every
// Omarchy install has; if it cannot run, the command is still on screen to select and type.
Item {
  id: root

  property string command: ""
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property bool copied: false

  readonly property color dim: Qt.darker(foreground, 1.4)

  width: parent ? parent.width : implicitWidth
  implicitHeight: line.implicitHeight + (status.visible ? status.implicitHeight + Style.space(4) : 0)

  BorderSurface {
    id: line
    width: parent.width
    implicitHeight: Math.max(codeText.implicitHeight, copyButton.implicitHeight) + Style.space(8)
    height: implicitHeight
    color: Style.normalFill
    radius: Style.cornerRadius
    borderSpec: Border.controlSpec("normal", root.foreground, Commons.Color.accent)

    Text {
      id: codeText
      anchors.left: parent.left
      anchors.leftMargin: Style.space(10)
      anchors.right: copyButton.left
      anchors.rightMargin: Style.space(6)
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      text: "$ " + root.command
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      elide: Text.ElideRight
    }

    Button {
      id: copyButton
      anchors.right: parent.right
      anchors.rightMargin: Style.space(4)
      anchors.verticalCenter: parent.verticalCenter
      text: Strings.t("off.copy")
      iconText: Glyphs.ui("copy")
      bordered: true
      focusable: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      verticalPadding: Style.space(3)
      onClicked: root.copy()
      FocusRing {}
    }
  }

  Text {
    id: status
    anchors.top: line.bottom
    anchors.topMargin: Style.space(4)
    visible: root.copied
    textFormat: Text.PlainText
    text: Strings.t("off.copied")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }

  Timer {
    id: reset
    interval: 2500
    onTriggered: root.copied = false
  }

  function copy() {
    Quickshell.execDetached(["wl-copy", root.command]);
    root.copied = true;
    reset.restart();
  }
}
