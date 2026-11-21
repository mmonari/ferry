import QtQuick
import qs.Commons
import qs.Ui
import "../views"
import "../Strings.js" as Strings
import ".."

// The one row of the Settings view that is not ferryd's: the bar widget's "show the peer's name" setting,
// which the shell keeps on the widget's shell.json entry (FerryUi.setShowPeerName). It is a SwitchRow that
// answers to the view the way a SettingItem does: it is a setting row (so Tab and reveal treat it as one),
// it has a key (so the view lists it), and it takes the keys as a toggle does.
SwitchRow {
  id: row

  // the key the view lists it under; not a ferryd key, and never sent to config.set
  property string key: "bar.show_peer_name"
  readonly property bool isSettingRow: true

  label: Strings.t("panel.bar.peer_name")
  description: Strings.t("panel.bar.peer_name.help")
  checked: FerryUi.showPeerName
  onClicked: FerryUi.setShowPeerName(!FerryUi.showPeerName)

  // For the dev harness: set a value as if the user had, and put the keys on the switch.
  function simulateSet(value) { FerryUi.setShowPeerName(value === true); }
  function focusControl() { row.forceActiveFocus(Qt.TabFocusReason); }
}
