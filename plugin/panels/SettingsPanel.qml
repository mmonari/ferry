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
// Layout: the groups on the left, one per line, the chosen one marked; the
// chosen group's settings on the right as a flat hairline list; a strip at the bottom only while a
// saved change waits for a ferryd restart.
//
// Keyboard: Tab and Shift+Tab walk the controls, and past the last control of a group go on
// into the next group (the first, back into the one before); Up, Down, j and k do the same for what
// does not use them, and move between the groups while the group list has the keys; Esc closes the
// panel unless a control had a use for it. The view scrolls to whatever has the focus.
FocusScope {
  id: root

  readonly property color fg: Commons.Color.popups.text
  readonly property color dim: Qt.darker(fg, 1.4)
  readonly property string fontFamily: Style.font.family

  readonly property string phase: Model.settingsState(
    FerryClient.connected, FerryClient.hasMethod("config.schema"), FerryClient.configSchema, FerryClient.config)
  readonly property var groups: Model.settingsGroups(FerryClient.configSchema)
  readonly property var banner: Model.pendingBanner(FerryClient.configPending)
  // ferryd's groups, then the panel's own
  readonly property var navGroups: groups
  readonly property string current: {
    var want = FerryUi.settingsGroup;
    for (var i = 0; i < navGroups.length; i++) if (navGroups[i].id === want) return want;
    return navGroups.length ? navGroups[0].id : "";
  }
  readonly property int currentIndex: {
    for (var i = 0; i < navGroups.length; i++) if (navGroups[i].id === current) return i;
    return 0;
  }

  // key -> SettingItem, for the dev harness and tests.
  property var items: ({})

  function register(key, item) {
    items[key] = item;
    root.itemsChanged();
  }

  function setGroup(id) {
    if (id === root.current) return;
    FerryUi.settingsGroup = id;
    flick.contentY = 0;
  }

  function groupOf(key) {
    for (var i = 0; i < groups.length; i++)
      for (var j = 0; j < groups[i].keys.length; j++)
        if (groups[i].keys[j].key === key) return groups[i].id;
    return "";
  }

  // What a group's line in the list says on its right: the restart waits, or a refusal.
  function pendingIn(group) {
    var n = 0;
    for (var j = 0; j < group.keys.length; j++) if (Model.isPending(FerryClient.configPending, group.keys[j].key)) n++;
    return n;
  }
  function errorIn(group) {
    var all = root.items;
    for (var j = 0; j < group.keys.length; j++) {
      var it = all[group.keys[j].key];
      if (it && it.error !== null) return true;
    }
    return false;
  }

  // Set a value as the user would (the harness has no pointer), and put the focus where a user
  // would have it.
  function simulateSet(key, value) {
    var item = items[key];
    if (item) item.simulateSet(value);
  }

  function focusKey(key) {
    var item = items[key];
    if (!item) return;
    root.setGroup(root.groupOf(key));
    item.focusControl();
  }

  // Start recording a chord, with some modifiers shown as held (a picture of the recorder).
  function startRecording(key, held) {
    var item = items[key];
    if (!item) return;
    root.setGroup(root.groupOf(key));
    item.startRecording(held);
  }

  // Show a setting's group and scroll so that the setting is at the top of the view.
  function showKey(key) {
    var item = items[key];
    if (!item) return;
    root.setGroup(root.groupOf(key));
    scrollTo(item.mapToItem(flick.contentItem, 0, 0).y - Style.space(6));
  }

  function scrollTo(y) {
    flick.contentY = Math.max(0, Math.min(y, flick.contentHeight - flick.height));
  }

  focus: true

  // ---------------------------------------------------------------------------------- keys
  function inside(item, scope) {
    var top = scope || root;
    for (var p = item; p; p = p.parent)
      if (p === top) return true;
    return false;
  }

  function isEnd(it) { return it === headEnd || it === tailEnd; }

  function reason(forward) { return forward ? Qt.TabFocusReason : Qt.BacktabFocusReason; }

  // The first (or last) control of the group on show; false when it has none.
  function enterGroup(forward) {
    var it = forward ? headEnd.nextItemInFocusChain(true) : tailEnd.nextItemInFocusChain(false);
    // an end that does not take the focus is left out of the chain, and may hand back the other end
    for (var g = 0; it && (root.isEnd(it) || !root.inside(it, list)) && g < 400; g++) {
      if (!root.inside(it, list) && !root.inside(it, nav) && it !== root) break;
      it = it.nextItemInFocusChain(forward);
    }
    if (!it || root.isEnd(it) || !root.inside(it, list)) return false;
    it.forceActiveFocus(root.reason(forward));
    return true;
  }

  // Off the end of the group on show: into the next group (or the one before), if there is one with
  // a control.
  function crossGroup(forward) {
    for (var i = root.currentIndex + (forward ? 1 : -1); i >= 0 && i < navGroups.length; i += forward ? 1 : -1) {
      root.setGroup(navGroups[i].id);
      if (root.enterGroup(forward)) return true;
    }
    return false;
  }

  // The view holds the keys when it opens, with no ring on anything. The first Tab, Down or j goes
  // into the first setting of the group on show, Shift+Tab, Up or k into its last. (Qt's own chain
  // would start at the header, which is not what somebody who opened Settings is after.)
  function enter(forward) {
    root.enterGroup(forward);
  }

  function step(forward) {
    var current = root.Window.activeFocusItem;
    if (!current || current === root || !root.inside(current)) {
      root.enter(forward);
      return;
    }
    if (!root.inside(current, list)) return;
    var next = current.nextItemInFocusChain(forward);
    if (root.isEnd(next)) {
      var group = root.current;
      if (!root.crossGroup(forward)) root.setGroup(group);
      return;
    }
    if (next && root.inside(next, list)) next.forceActiveFocus(root.reason(forward));
  }

  // The focus before the last move, so an end of the list knows which way it was reached.
  property Item lastFocus: null

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
    function onActiveFocusItemChanged() {
      var it = root.Window.activeFocusItem;
      if (!root.isEnd(it)) root.lastFocus = it;
      root.reveal(it);
    }
  }

  readonly property real gutter: Style.space(16)

  // ------------------------------------------------------------------------------ the groups
  Item {
    id: nav
    visible: root.phase === "ready"
    anchors.top: parent.top
    anchors.bottom: strip.top
    anchors.left: parent.left
    width: Style.space(180)
    activeFocusOnTab: visible
    Accessible.role: Accessible.List
    Accessible.name: Strings.t("panel.settings.title")

    function move(by) {
      var i = Math.max(0, Math.min(root.navGroups.length - 1, root.currentIndex + by));
      root.setGroup(root.navGroups[i].id);
    }

    Keys.onPressed: function (event) {
      if (event.modifiers & (Qt.ControlModifier | Qt.AltModifier | Qt.MetaModifier)) return;
      if (event.key === Qt.Key_Down || event.text === "j") nav.move(1);
      else if (event.key === Qt.Key_Up || event.text === "k") nav.move(-1);
      else if (event.key === Qt.Key_Home) nav.move(-999);
      else if (event.key === Qt.Key_End) nav.move(999);
      else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space || event.key === Qt.Key_Right) root.enterGroup(true);
      else return;
      event.accepted = true;
    }

    Column {
      id: navList
      anchors.top: parent.top
      anchors.topMargin: Style.space(6)
      anchors.left: parent.left
      anchors.right: parent.right
      spacing: Style.space(2)

      Repeater {
        model: root.navGroups

        Item {
          id: line
          required property var modelData
          required property int index
          readonly property bool chosen: modelData.id === root.current
          readonly property int waits: root.pendingIn(modelData)
          readonly property bool refused: root.errorIn(modelData)
          width: navList.width
          height: name.implicitHeight + Style.space(12)

          Rectangle {
            visible: line.chosen
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            width: 2
            height: parent.height - Style.space(8)
            radius: Style.cornerRadius
            color: Commons.Color.accent
          }

          Text {
            id: name
            anchors.left: parent.left
            anchors.leftMargin: Style.space(12)
            anchors.right: mark.left
            anchors.rightMargin: Style.space(6)
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: line.modelData.title
            color: line.chosen || hover.containsMouse ? root.fg : root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.weight: line.chosen ? Font.DemiBold : Font.Normal
            elide: Text.ElideRight
          }

          // a refusal is a dot in the urgent colour, the restart waits a count in the accent
          Text {
            id: mark
            anchors.right: parent.right
            anchors.rightMargin: Style.space(10)
            anchors.verticalCenter: parent.verticalCenter
            visible: text !== ""
            textFormat: Text.PlainText
            text: line.refused ? "●" : (line.waits > 0 ? String(line.waits) : "")
            color: line.refused ? Commons.Color.urgent : Commons.Color.accent
            font.family: root.fontFamily
            font.pixelSize: line.refused ? Style.font.caption : Style.font.bodySmall
            font.weight: Font.DemiBold
          }

          MouseArea {
            id: hover
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.setGroup(line.modelData.id)
          }

          FocusRing { target: nav; visible: line.chosen && nav.activeFocus && keyboard }
        }
      }
    }

    // where the settings are saved: one dim line at the foot of the groups
    Text {
      anchors.left: parent.left
      anchors.leftMargin: Style.space(12)
      anchors.right: parent.right
      anchors.rightMargin: Style.space(8)
      anchors.bottom: parent.bottom
      anchors.bottomMargin: Style.space(4)
      textFormat: Text.PlainText
      text: Strings.t("settings.path")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.WordWrap
    }
  }

  // the hairline between the panes
  Rectangle {
    id: divider
    visible: nav.visible
    anchors.top: parent.top
    anchors.bottom: strip.top
    anchors.bottomMargin: Style.space(8)
    x: nav.width
    width: 1
    color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.12)
  }

  // ------------------------------------------------------------------- the chosen group's rows
  Flickable {
    id: flick
    anchors.top: parent.top
    anchors.bottom: strip.top
    anchors.bottomMargin: Style.space(8)
    anchors.left: divider.right
    anchors.leftMargin: root.gutter
    anchors.right: parent.right
    visible: root.phase === "ready"
    clip: true
    contentWidth: width
    contentHeight: list.implicitHeight + Style.space(6)
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

    Column {
      id: list
      x: 0
      width: flick.width - Style.space(12)

      // The two ends of the list. Reaching one from inside goes on into the next group (or the one
      // before); reaching one from outside goes into the group on show.
      Item {
        id: headEnd
        width: 0
        height: 0
        activeFocusOnTab: true
        onActiveFocusChanged: {
          if (!activeFocus) return;
          var group = root.current;
          if (root.inside(root.lastFocus, list)) {
            if (root.crossGroup(false)) return;
            root.setGroup(group);
            var out = headEnd.nextItemInFocusChain(false);
            for (var g = 0; out && (root.inside(out, list) || root.inside(root, out)) && g < 400; g++) out = out.nextItemInFocusChain(false);
            if (out) Qt.callLater(function () { out.forceActiveFocus(Qt.BacktabFocusReason); });
          } else if (!root.enterGroup(true)) {
            var on = tailEnd.nextItemInFocusChain(true);
            if (on) on.forceActiveFocus(Qt.TabFocusReason);
          }
        }
      }

      Repeater {
        model: root.navGroups

        Column {
          id: group
          required property var modelData
          width: list.width
          visible: modelData.id === root.current

          Repeater {
            model: group.modelData.keys

            Column {
              required property var modelData
              required property int index
              width: group.width

              PanelSeparator {
                visible: index > 0
                foreground: root.fg
              }

              SettingItem {
                spec: modelData
                foreground: root.fg
                fontFamily: root.fontFamily
                Component.onCompleted: root.register(modelData.key, this)
              }
            }
          }
        }
      }

      Item {
        id: tailEnd
        width: 0
        height: 0
        activeFocusOnTab: true
        onActiveFocusChanged: {
          if (!activeFocus) return;
          var group = root.current;
          if (root.inside(root.lastFocus, list)) {
            if (root.crossGroup(true)) return;
            root.setGroup(group);
            var out = tailEnd.nextItemInFocusChain(true);
            // (past the groups, and past a scope that holds this view: its focus would come straight back here)
            for (var g = 0; out && (root.inside(out, list) || root.inside(out, nav) || root.inside(root, out)) && g < 400; g++) out = out.nextItemInFocusChain(true);
            // out of the view's focus scope: after Qt has finished giving the focus here
            if (out) Qt.callLater(function () { out.forceActiveFocus(Qt.TabFocusReason); });
          } else if (!root.enterGroup(false)) {
            var on = headEnd.nextItemInFocusChain(false);
            if (on) on.forceActiveFocus(Qt.BacktabFocusReason);
          }
        }
      }
    }
  }

  // ------------------------------------------------------------- when there is nothing to show
  Column {
    id: notice
    anchors.top: parent.top
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

  // ------------------------------------------- the strip: saved changes that wait for a restart
  Item {
    id: strip
    readonly property bool shown: root.phase === "ready" && root.banner.count > 0
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: parent.bottom
    height: shown ? stripLine.height + Style.space(10) : 0
    visible: shown
    Accessible.role: Accessible.StaticText
    Accessible.name: stripText.text

    PanelSeparator {
      anchors.top: parent.top
      foreground: root.fg
    }

    Item {
      id: stripLine
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.bottom: parent.bottom
      height: Math.max(stripText.implicitHeight, copy.implicitHeight)

      Text {
        id: stripText
        anchors.left: parent.left
        anchors.leftMargin: Style.space(12)
        anchors.right: stripCmd.left
        anchors.rightMargin: Style.space(12)
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: Strings.t(root.banner.count === 1 ? "settings.strip.one" : "settings.strip.many", [root.banner.count])
        color: root.fg
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.weight: Font.DemiBold
        elide: Text.ElideRight
      }

      Text {
        id: stripCmd
        anchors.right: copy.left
        anchors.rightMargin: Style.space(12)
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: copier.copied ? Strings.t("off.copied") : root.banner.command
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
      }

      Button {
        id: copy
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: Strings.t("off.copy")
        iconText: Glyphs.ui("copy")
        bordered: true
        focusable: true
        foreground: root.fg
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        verticalPadding: Style.space(3)
        Accessible.name: Strings.t("off.copy") + ": " + root.banner.command
        onClicked: copier.copy()
        FocusRing {}
      }

      // the copy itself (wl-copy) and its "Copied" moment, borrowed from the command line
      CommandLine {
        id: copier
        visible: false
        width: 0
        command: root.banner.command
      }
    }
  }
}
