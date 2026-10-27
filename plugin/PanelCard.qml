import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "."
import "Glyphs.js" as Glyphs
import "Strings.js" as Strings
import "views"

// What is inside the setup panel's window: a header with the title, the three tabs and a close
// button, and below it the view the tab names. It owns no window (PanelHost.qml does), so the dev
// harness can put it in a plain card and photograph it.
//
// Each view is its own file under panels/ and fills the area below the header, footer included.
// A view that needs Escape for something of its own (cancel a pairing step, stop recording a
// chord) handles it and accepts the key; anything it leaves alone closes the panel.
FocusScope {
  id: root

  property string view: FerryUi.panelView
  // The view that is showing, for a harness or a test to reach.
  readonly property var viewItem: body.item
  signal closeRequested()
  signal viewRequested(string view)

  readonly property color fg: Commons.Color.popups.text
  readonly property string fontFamily: Style.font.family
  readonly property var tabs: [
    { value: "arrange", label: Strings.t("panel.tab.arrange"), icon: Glyphs.ui("grid") },
    { value: "pair", label: Strings.t("panel.tab.pair"), icon: Glyphs.ui("plus") },
    { value: "settings", label: Strings.t("panel.tab.settings"), icon: Glyphs.ui("cog") }
  ]
  readonly property var sources: ({
    arrange: "panels/ArrangePanel.qml",
    pair: "panels/PairPanel.qml",
    settings: "panels/SettingsPanel.qml"
  })

  focus: true
  Keys.onEscapePressed: root.closeRequested()

  Item {
    id: header
    anchors.top: parent.top
    anchors.left: parent.left
    anchors.right: parent.right
    height: Math.max(tabsRow.implicitHeight, closeButton.implicitHeight, titleRow.implicitHeight)

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
        font.pixelSize: Style.font.subtitle
        font.bold: true
      }
    }

    ButtonGroup {
      id: tabsRow
      anchors.right: closeButton.left
      anchors.rightMargin: Style.space(10)
      anchors.verticalCenter: parent.verticalCenter
      options: root.tabs
      value: root.view
      foreground: root.fg
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      onChanged: function (value) { root.viewRequested(value); }
    }

    // The active tab carries the accent underline the bar gives its active widget (#18): a lighter
    // fill alone is too weak in light themes.
    Rectangle {
      id: tabMark
      readonly property Item chip: {
        var unused = root.view + tabsRow.width;
        for (var i = 0; i < tabsRow.children.length; i++)
          if (tabsRow.children[i].selected === true) return tabsRow.children[i];
        return null;
      }
      visible: chip !== null
      x: tabsRow.x + (chip ? chip.x + Style.space(6) : 0)
      y: tabsRow.y + tabsRow.height + Style.space(3)
      width: chip ? Math.max(0, chip.width - Style.space(12)) : 0
      height: Math.max(2, Style.space(2))
      radius: height / 2
      color: Commons.Color.accent
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

  PanelSeparator {
    id: rule
    anchors.top: header.bottom
    anchors.topMargin: Style.space(10)
    foreground: root.fg
  }

  Loader {
    id: body
    // A Loader is a focus scope: with `focus` the card's own forceActiveFocus() (PanelHost asks for it
    // when the window maps) lands in the view, so Tab starts inside it and the arrow keys reach it.
    // Without it the card keeps the focus, and Tab first walks the tabs and the close button.
    focus: true
    anchors.top: rule.bottom
    anchors.topMargin: Style.space(14)
    anchors.bottom: parent.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    source: root.sources[root.view] || root.sources.arrange
    // The view takes the keyboard when it appears, so Tab starts inside it.
    onLoaded: if (item && item.forceActiveFocus) item.forceActiveFocus()
  }

  // A view can send the user to another tab (Arrange with nothing to arrange offers Pair): it says
  // `viewRequested("pair")` and the host does the rest.
  Connections {
    target: body.item
    ignoreUnknownSignals: true
    function onViewRequested(view) { root.viewRequested(view); }
  }
}
