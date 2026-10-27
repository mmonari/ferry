import QtQuick
import Quickshell
import Quickshell.Io

// Prints the state of ferryd each time it changes. Run it with: quickshell -p Status.qml
ShellRoot {
	Socket {
		id: ferry
		path: Quickshell.env("XDG_RUNTIME_DIR") + "/ferry/ferryd.sock"
		connected: true

		function send(request) {
			write(JSON.stringify(request) + "\n");
			flush();
		}

		parser: SplitParser {
			splitMarker: "\n"
			onRead: function (line) {
				const message = JSON.parse(line);
				if (message.event === "hello")
					ferry.send({ id: 1, method: "subscribe", params: { topics: ["state"] } });
				else if (message.event === "state")
					console.log("ferry state: " + message.data.summary.state);
			}
		}
	}
}
