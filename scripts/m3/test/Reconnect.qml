import QtQuick
import Quickshell
import "ferry" as Ferry

// The widget side of FR-UI-03, with nothing else in the way: print every change of the
// connection with the time, so reconnect.sh can measure how long a restart takes to show.
ShellRoot {
  Connections {
    target: Ferry.FerryClient

    function onConnectedChanged() {
      console.log("RECONNECT connected=" + Ferry.FerryClient.connected + " state=" + Ferry.FerryClient.state + " t=" + Date.now());
    }
    function onStateChanged() {
      console.log("RECONNECT state=" + Ferry.FerryClient.state + " t=" + Date.now());
    }
  }

  Component.onCompleted: console.log("RECONNECT ready state=" + Ferry.FerryClient.state + " t=" + Date.now())
}
