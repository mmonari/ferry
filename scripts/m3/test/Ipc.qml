import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui
import "ferry" as Ferry

// The plugin's IPC target (`ferry`) in a standalone shell, offscreen: the real FerryWidget
// (which attaches the FerryUi singleton, which owns the IpcHandler) and the real FerryClient against
// the fake daemon. ipc.sh calls the target the way a keybind, a launcher entry and `ferry ui` do, and
// reads back what it did through the probe below, which exists only here.
ShellRoot {
  PluginBarApi {
    id: bar
    pluginId: "ferry.bar"
    moduleName: "ferry.bar"
    foreground: Commons.Color.foreground
    barForeground: Commons.Color.bar.text
    background: Commons.Color.bar.background
    urgent: Commons.Color.bar.active
    fontFamily: Style.font.family
    position: "top"
    vertical: false
    barSize: Style.bar.sizeHorizontal
  }

  FloatingWindow {
    visible: true
    implicitWidth: 400
    implicitHeight: 40
    Ferry.FerryWidget {
      bar: bar
      settings: ({})
      anchors.fill: parent
    }
  }

  IpcHandler {
    target: "ferry-probe"

    function ui(): string {
      return JSON.stringify({
        panelOpen: Ferry.FerryUi.panelOpen,
        view: Ferry.FerryUi.panelView,
        state: Ferry.FerryClient.state,
        locked: Ferry.FerryClient.locked,
        connected: Ferry.FerryClient.connected
      });
    }
  }
}
