import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "."
import "Glyphs.js" as Glyphs
import "Strings.js" as Strings
import "views"

// What is inside the panel's window: the header (the mark, the lock, the close button), the tabs, and
// below them the view the tab names. It owns no window (PanelHost.qml does), so the dev harness can
// put it in a plain card and photograph it.
//
// The frame follows docs/design/panel-ui.md: text tabs with the accent underline on the one that is
// showing, one size for every tab, and nothing boxed. Each view is its own file under panels/ and
// fills the area below the tabs, footer included. A view that needs Escape for something of its own
// (cancel a pairing step, stop recording a chord) handles it and accepts the key; anything it leaves
// alone closes the panel.
FocusScope {
  id: root

  // False while the panel is closed, so a closed panel costs no timers.
  property bool shown: true
  property string view: FerryUi.panelView
  // The view that is showing, for a harness or a test to reach.
  readonly property var viewItem: body.item
  signal closeRequested()
  signal viewRequested(string view)

  readonly property color fg: Commons.Color.popups.text
  readonly property color dim: Qt.darker(fg, 1.4)
  readonly property string fontFamily: Style.font.family
  readonly property var tabs: [
    { value: "screens", label: Strings.t("panel.tab.screens") },
    { value: "pair", label: Strings.t("panel.tab.pair") },
    { value: "settings", label: Strings.t("panel.tab.settings") }
  ]
  readonly property var sources: ({
    screens: "panels/ArrangePanel.qml",
    pair: "panels/PairPanel.qml",
    settings: "panels/SettingsPanel.qml"
  })
  readonly property string current: sources[view] !== undefined ? view : "screens"
  // the lock switch is there when there is something to lock to
  readonly property bool canLock: FerryClient.connected && !FerryClient.firstRun

  focus: true
  Keys.onEscapePressed: root.closeRequested()
  // Alt+1..3 picks a tab, Ctrl+PgUp and Ctrl+PgDn walk them, from anywhere in the panel
  Keys.onPressed: function (event) {
    var i = -1;
    for (var t = 0; t < tabs.length; t++) if (tabs[t].value === root.current) i = t;
    if ((event.modifiers & Qt.AltModifier) && event.key >= Qt.Key_1 && event.key < Qt.Key_1 + tabs.length) {
      root.viewRequested(tabs[event.key - Qt.Key_1].value);
      event.accepted = true;
    } else if ((event.modifiers & Qt.ControlModifier) && (event.key === Qt.Key_PageDown || event.key === Qt.Key_PageUp)) {
      var n = (i + (event.key === Qt.Key_PageDown ? 1 : tabs.length - 1)) % tabs.length;
      root.viewRequested(tabs[n].value);
      event.accepted = true;
    }
  }

  // ---------------------------------------------------------------------------------- header
  Item {
    id: header
    anchors.top: parent.top
    anchors.left: parent.left
    anchors.right: parent.right
    height: Math.max(titleRow.implicitHeight, lockRow.implicitHeight, closeButton.implicitHeight)

    Row {
      id: titleRow
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(8)

      Text {
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: Glyphs.ui("ferry")
        color: root.fg
        font.family: root.fontFamily
        font.pixelSize: Style.font.iconLarge
      }

      Text {
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: Strings.t("panel.title")
        color: root.fg
        font.family: root.fontFamily
        font.pixelSize: Style.font.title
        font.weight: Font.DemiBold
      }
    }

    // The lock is the one switch people reach for in a hurry, so it is on every tab.
    Item {
      id: lockRow
      visible: root.canLock
      anchors.right: closeButton.left
      anchors.rightMargin: Style.space(12)
      anchors.verticalCenter: parent.verticalCenter
      implicitWidth: lockLabel.implicitWidth + Style.space(10) + lockSwitch.width
      implicitHeight: Math.max(lockLabel.implicitHeight, lockSwitch.height)
      width: implicitWidth
      height: implicitHeight
      activeFocusOnTab: true
      Accessible.role: Accessible.CheckBox
      Accessible.name: Strings.t("lock.label")
      Accessible.checked: FerryClient.locked
      Keys.onSpacePressed: FerryClient.setLock(!FerryClient.locked)
      Keys.onReturnPressed: FerryClient.setLock(!FerryClient.locked)

      Text {
        id: lockLabel
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: Strings.t("lock.label")
        color: FerryClient.locked ? root.fg : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
      }

      ToggleSwitch {
        id: lockSwitch
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        checked: FerryClient.locked
        interactive: false
        cursorRing: false
        foreground: root.fg
      }

      MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onClicked: FerryClient.setLock(!FerryClient.locked)
      }

      FocusRing {}
    }

    PanelActionButton {
      id: closeButton
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      iconText: Glyphs.ui("close")
      tooltipText: Strings.t("panel.close")
      foreground: root.fg
      fontFamily: root.fontFamily
      focusable: true
      Accessible.name: Strings.t("panel.close")
      onClicked: root.closeRequested()

      FocusRing {}
    }
  }

  // ------------------------------------------------------------------------------------ tabs
  Item {
    id: tabBar
    anchors.top: header.bottom
    anchors.topMargin: Style.space(10)
    anchors.left: parent.left
    anchors.right: parent.right
    height: tabRow.implicitHeight

    Row {
      id: tabRow
      anchors.left: parent.left
      // the first tab's text lines up with the mark above it
      spacing: Style.space(4)

      Repeater {
        model: root.tabs

        Item {
          id: tab
          required property var modelData
          readonly property bool selected: modelData.value === root.current
          implicitWidth: tabText.implicitWidth + Style.space(16)
          implicitHeight: tabText.implicitHeight + Style.space(12) + underline.height
          activeFocusOnTab: true
          Accessible.role: Accessible.PageTab
          Accessible.name: modelData.label
          Accessible.selected: selected
          Keys.onSpacePressed: root.viewRequested(modelData.value)
          Keys.onReturnPressed: root.viewRequested(modelData.value)
          Keys.onLeftPressed: tab.nextItemInFocusChain(false).forceActiveFocus()
          Keys.onRightPressed: tab.nextItemInFocusChain(true).forceActiveFocus()

          Text {
            id: tabText
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: parent.top
            anchors.topMargin: Style.space(4)
            textFormat: Text.PlainText
            text: tab.modelData.label
            color: tab.selected || tabMouse.containsMouse ? root.fg : root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.weight: tab.selected ? Font.DemiBold : Font.Normal
          }

          Rectangle {
            id: underline
            anchors.bottom: parent.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            height: Math.max(2, Style.space(2))
            color: Commons.Color.accent
            opacity: tab.selected ? 1 : 0
            Behavior on opacity { NumberAnimation { duration: Fx.ms(120) } }
          }

          MouseArea {
            id: tabMouse
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.viewRequested(tab.modelData.value)
          }

          FocusRing {}
        }
      }
    }

    Text {
      anchors.right: parent.right
      anchors.verticalCenter: tabRow.verticalCenter
      visible: FerryClient.connected && FerryClient.version !== ""
      textFormat: Text.PlainText
      text: Strings.t("footer.version", [FerryClient.version])
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }

  PanelSeparator {
    id: rule
    anchors.top: tabBar.bottom
    foreground: root.fg
  }

  // ------------------------------------------------------------------------------------ body
  Loader {
    id: body
    // A Loader is a focus scope: with `focus` the card's own forceActiveFocus() lands in the view,
    // so Tab starts inside it and the arrow keys reach it.
    focus: true
    anchors.top: rule.bottom
    anchors.topMargin: Style.space(14)
    anchors.bottom: parent.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    source: root.sources[root.current]
    // The view takes the keyboard when it appears, so Tab starts inside it.
    onLoaded: if (item && item.forceActiveFocus) item.forceActiveFocus()
  }

  // a view that keeps timers running stops them while the panel is closed
  Binding {
    target: body.item
    property: "shown"
    value: root.shown
    when: body.item !== null && body.item.shown !== undefined
  }

  // A view can send the user to another tab (Arrange with nothing to arrange offers Pair): it says
  // `viewRequested("pair")` and the host does the rest.
  Connections {
    target: body.item
    ignoreUnknownSignals: true
    function onViewRequested(view) { root.viewRequested(view); }
  }
}
