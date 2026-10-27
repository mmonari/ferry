pragma Singleton
import QtQuick
import qs.Commons

// Two shell tokens that Omarchy 4.5 added and older shells lack: the animation duration that
// follows "animations off" (Style.duration), and the flag itself (Style.reduceMotion). A plugin
// that calls Style.duration() on an older shell throws; this answers the plain number instead.
QtObject {
  function ms(n) {
    return typeof Style.duration === "function" ? Style.duration(n) : n;
  }

  readonly property bool still: Style.reduceMotion === true
}
