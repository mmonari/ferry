import QtQuick
import qs.Commons
import qs.Commons as Commons

// The accent ring of views/FocusRing, for a control whose focused item is one of its children (the
// shell's Dropdown focuses its trigger, not itself). It shows while the window's focused item is
// `scope` or lives inside it.
Rectangle {
  id: ring

  property Item scope: parent
  readonly property Item focused: scope && scope.Window.window ? scope.Window.window.activeFocusItem : null
  readonly property bool within: {
    var f = ring.focused;
    while (f) {
      if (f === ring.scope) return true;
      f = f.parent;
    }
    return false;
  }

  anchors.fill: parent
  anchors.margins: -Style.space(2)
  z: 10
  visible: ring.within
  color: "transparent"
  radius: Style.cornerRadius + Style.space(2)
  border.width: Math.max(1, Style.space(2))
  border.color: Util.alpha(Commons.Color.accent, 0.9)
}
