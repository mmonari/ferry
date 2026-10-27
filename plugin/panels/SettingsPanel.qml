import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../settings"
import "../views"
import "../Glyphs.js" as Glyphs
import "../Model.js" as Model
import "../Strings.js" as Strings

// The Settings view. It has no list of keys: it draws what ferryd's `config.schema`
// describes, grouped by the dotted prefix, one control per key chosen from the key's type,
// range and choices (Model.controlFor), with ferryd's own sentence as the help. A key ferryd adds
// later appears here with no change.
//
//   * Every change is a `config.set` (FerryClient.setConfig), so ferryd checks it with the code its
//     file loader uses; its refusal is shown under the control with its hint.
//   * What the key does after saving is said next to it: Live (in force at once), Restart (listed in
//     `pending` until ferryd restarts, with the command to copy), Setup (run `ferry setup`).
//   * The `config` event keeps every value current, so `ferry config set` in a terminal shows here.
//
// Keyboard: Tab and Shift+Tab walk the controls; Up, Down, j and k do the same for what
// does not use them; Esc closes the panel unless a control had a use for it. The view scrolls to
// whatever has the focus.
FocusScope {
  id: root

  readonly property color fg: Commons.Color.popups.text
  readonly property color dim: Qt.darker(fg, 1.4)
  readonly property string fontFamily: Style.font.family

  readonly property string phase: Model.settingsState(
    FerryClient.connected, FerryClient.hasMethod("config.schema"), FerryClient.configSchema, FerryClient.config)
  readonly property var groups: Model.settingsGroups(FerryClient.configSchema)
  readonly property var banner: Model.pendingBanner(FerryClient.configPending)
  readonly property int columnCount: width >= Style.space(640) ? 2 : 1
  readonly property var columns: Model.settingsColumns(groups, columnCount)

  // key -> SettingItem, for the dev harness and tests.
  property var items: ({})

  function register(key, item) { items[key] = item; }

  // Set a value as the user would (the harness has no pointer), and put the focus where a user
  // would have it.
  function simulateSet(key, value) {
    var item = items[key];
    if (item) item.simulateSet(value);
  }

  function focusKey(key) {
    var item = items[key];
    if (item) item.focusControl();
  }

  // Start recording a chord, with some modifiers shown as held (a picture of the recorder).
  function startRecording(key, held) {
    var item = items[key];
    if (item) item.startRecording(held);
  }

  // Scroll so that a setting is at the top of the view.
  function showKey(key) {
    var item = items[key];
    if (item) scrollTo(item.mapToItem(flick.contentItem, 0, 0).y - Style.space(6));
  }

  function scrollTo(y) {
    flick.contentY = Math.max(0, Math.min(y, flick.contentHeight - flick.height));
  }

  focus: true

  // ---------------------------------------------------------------------------------- keys
  function inside(item) {
    for (var p = item; p; p = p.parent)
      if (p === root) return true;
    return false;
  }

  // The view holds the keys when it opens, with no ring on anything. The first Tab, Down or j goes
  // into the first setting, Shift+Tab, Up or k into the last. (Qt's own chain would start at the
  // header, which is not what somebody who opened Settings is after.)
  function enter(forward) {
    var it = root.nextItemInFocusChain(forward);
    for (var guard = 0; it && !root.inside(it) && guard < 400; guard++) it = it.nextItemInFocusChain(forward);
    if (it && it !== root && root.inside(it)) it.forceActiveFocus(forward ? Qt.TabFocusReason : Qt.BacktabFocusReason);
  }

  function step(forward) {
    var current = root.Window.activeFocusItem;
    if (!current || current === root || !root.inside(current)) {
      root.enter(forward);
      return;
    }
    var next = current.nextItemInFocusChain(forward);
    if (next && next !== root && root.inside(next))
      next.forceActiveFocus(forward ? Qt.TabFocusReason : Qt.BacktabFocusReason);
  }

  Keys.onTabPressed: function (event) {
    if (root.Window.activeFocusItem !== root) return;
    root.enter(true);
    event.accepted = true;
  }
  Keys.onBacktabPressed: function (event) {
    if (root.Window.activeFocusItem !== root) return;
    root.enter(false);
    event.accepted = true;
  }

  Keys.onPressed: function (event) {
    if (event.modifiers & (Qt.ControlModifier | Qt.AltModifier | Qt.MetaModifier)) return;
    if (event.key === Qt.Key_Down || event.text === "j") { root.step(true); event.accepted = true; }
    else if (event.key === Qt.Key_Up || event.text === "k") { root.step(false); event.accepted = true; }
  }

  // Whatever takes the focus is brought into view: the whole setting if it fits, else the control.
  function reveal(item) {
    if (!item || !root.inside(item)) return;
    var inFlick = false;
    var row = null;
    for (var p = item; p; p = p.parent) {
      if (p === flick.contentItem) { inFlick = true; break; }
      if (row === null && p.isSettingRow === true) row = p;
    }
    if (!inFlick) return;
    var target = row !== null && row.height <= flick.height ? row : item;
    var top = target.mapToItem(flick.contentItem, 0, 0).y - Style.space(6);
    var bottom = top + target.height + Style.space(12);
    if (top < flick.contentY) scrollAnim.go(Math.max(0, top));
    else if (bottom > flick.contentY + flick.height) scrollAnim.go(Math.min(flick.contentHeight - flick.height, bottom - flick.height));
  }

  Connections {
    target: root.Window.window
    function onActiveFocusItemChanged() { root.reveal(root.Window.activeFocusItem); }
  }

  // ---------------------------------------------------------------------------------- header
  Column {
    id: header
    anchors.top: parent.top
    anchors.left: parent.left
    anchors.right: parent.right
    spacing: Style.space(12)

    Hero {
      glyph: Glyphs.ui("cog")
      title: Strings.t("panel.settings.title")
      meta: Strings.t("panel.settings.meta")
      detail: root.phase === "ready" ? root.banner.pill : ""
      detailTone: "accent"
      foreground: root.fg
      fontFamily: root.fontFamily
    }

    // Saved changes that ferryd has not picked up yet, and the one command that does it.
    BorderSurface {
      id: bannerBox
      visible: root.phase === "ready" && root.banner.count > 0
      width: parent.width
      implicitHeight: bannerColumn.implicitHeight + Style.space(20)
      height: visible ? implicitHeight : 0
      color: Util.alpha(Commons.Color.accent, 0.08)
      radius: Style.cornerRadius
      borderSpec: Border.flat(Util.alpha(Commons.Color.accent, 0.6), 1)
      Accessible.role: Accessible.StaticText
      Accessible.name: root.banner.text

      Column {
        id: bannerColumn
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.margins: Style.space(10)
        spacing: Style.space(8)

        Row {
          width: parent.width
          spacing: Style.space(8)

          Text {
            textFormat: Text.PlainText
            text: Glyphs.ui("refresh")
            color: Commons.Color.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.icon
          }

          Text {
            width: parent.width - Style.space(26)
            textFormat: Text.PlainText
            text: root.banner.text
            color: root.fg
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            wrapMode: Text.WordWrap
          }
        }

        CommandLine {
          width: Math.min(parent.width, Style.space(440))
          command: root.banner.command
          foreground: root.fg
          fontFamily: root.fontFamily
        }
      }
    }
  }

  // ----------------------------------------------------------------------------------- body
  Flickable {
    id: flick
    anchors.top: header.bottom
    anchors.topMargin: Style.space(14)
    anchors.bottom: footer.top
    anchors.bottomMargin: Style.space(10)
    anchors.left: parent.left
    anchors.right: parent.right
    visible: root.phase === "ready"
    clip: true
    contentWidth: width
    contentHeight: body.implicitHeight + Style.space(6)
    boundsBehavior: Flickable.StopAtBounds
    flickableDirection: Flickable.VerticalFlick
    interactive: contentHeight > height

    // a list cut off by the window says so: the bar is there for as long as there is more below
    ScrollBar.vertical: PanelScrollBar { flickable: flick }

    NumberAnimation {
      id: scrollAnim
      target: flick
      property: "contentY"
      duration: Fx.ms(140)
      easing.type: Easing.OutCubic
      function go(y) {
        stop();
        to = y;
        if (Fx.still) flick.contentY = y;
        else start();
      }
    }

    Row {
      id: body
      x: 0
      width: flick.width - Style.space(12)
      spacing: Style.space(16)

      Repeater {
        model: root.columns

        Column {
          required property var modelData
          width: (body.width - (root.columns.length - 1) * body.spacing) / Math.max(1, root.columns.length)
          spacing: Style.space(16)

          Repeater {
            model: modelData

            Column {
              required property var modelData
              width: parent.width
              spacing: Style.space(8)

              PanelSectionHeader {
                text: modelData.title
                foreground: root.fg
                fontFamily: root.fontFamily
              }

              Repeater {
                model: modelData.keys

                SettingItem {
                  required property var modelData
                  spec: modelData
                  foreground: root.fg
                  fontFamily: root.fontFamily
                  Component.onCompleted: root.register(modelData.key, this)
                }
              }
            }
          }
        }
      }
    }

    // Where the view is, when it is longer than the window. No handle: the wheel and the focus move it.
    Rectangle {
      parent: flick
      visible: flick.contentHeight > flick.height
      anchors.right: parent.right
      width: Math.max(2, Style.space(3))
      radius: width / 2
      color: Util.alpha(root.fg, 0.35)
      y: flick.height * (flick.contentY / flick.contentHeight)
      height: Math.max(Style.space(24), flick.height * (flick.height / flick.contentHeight))
    }
  }

  // ------------------------------------------------------------- when there is nothing to show
  Column {
    id: notice
    anchors.top: header.bottom
    anchors.topMargin: Style.space(14)
    anchors.left: parent.left
    anchors.right: parent.right
    spacing: Style.space(10)
    visible: root.phase !== "ready"

    Text {
      width: parent.width
      textFormat: Text.PlainText
      text: root.phase === "off" ? Strings.t("settings.state.off.title")
        : (root.phase === "unsupported" ? Strings.t("settings.state.unsupported.title") : Strings.t("settings.state.loading"))
      color: root.phase === "loading" ? root.dim : root.fg
      font.family: root.fontFamily
      font.pixelSize: root.phase === "loading" ? Style.font.bodySmall : Style.font.subtitle
      font.bold: root.phase !== "loading"
      font.italic: root.phase === "loading"
      wrapMode: Text.WordWrap
    }

    Text {
      width: parent.width
      visible: root.phase !== "loading"
      textFormat: Text.PlainText
      text: root.phase === "off" ? Strings.t("settings.state.off.body") : Strings.t("settings.state.unsupported.body")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      wrapMode: Text.WordWrap
    }

    CommandLine {
      width: parent.width
      visible: root.phase === "off"
      command: Strings.t("off.cmd")
      foreground: root.fg
      fontFamily: root.fontFamily
    }
  }

  // ---------------------------------------------------------------------------------- footer
  Text {
    id: footer
    anchors.bottom: parent.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    visible: root.phase === "ready"
    textFormat: Text.PlainText
    text: Strings.t("settings.footer")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    wrapMode: Text.WordWrap
  }
}
