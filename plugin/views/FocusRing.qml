import QtQuick
import qs.Commons
import qs.Commons as Commons

// A ring around a control that has keyboard focus (NFR-A11Y-04). Drop it inside any
// focusable control. The shell's own focus state is a faint fill and a faint border by design
// (themes can strengthen it with the `focus-*` keys of [controls]); this ring is the plugin's own
// guarantee that the focused control is always visibly marked, in the theme's accent.
Rectangle {
  id: ring

  property Item target: parent
  // Shown for the keyboard, as a browser's :focus-visible does. The popover and the panel say whether
  // the user is on the keyboard (`keyboardNav`: a key was pressed since the last click), and a ring that
  // appears under a click, or because a screen moved the focus by itself, tells the user nothing. A
  // control outside both always shows it.
  readonly property bool keyboard: {
    for (var p = ring.parent; p; p = p.parent)
      if (p.keyboardNav !== undefined) return p.keyboardNav === true;
    return true;
  }

  anchors.fill: parent
  anchors.margins: -Style.space(2)
  z: 10
  visible: target !== null && target.activeFocus && keyboard
  color: "transparent"
  radius: Style.cornerRadius + Style.space(2)
  border.width: Math.max(1, Style.space(2))
  border.color: Util.alpha(Commons.Color.accent, 0.9)
}
