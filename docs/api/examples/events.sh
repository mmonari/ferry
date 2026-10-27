#!/bin/sh
# Print the events of the topics you name, or of every topic when you name none, one JSON line each,
# until you press Ctrl+C. Needs socat and jq. Usage: events.sh state layout
SOCK="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/ferry/ferryd.sock"
if [ "$#" -gt 0 ]; then
	TOPICS=$(printf '"%s",' "$@")
	PARAMS="{\"topics\":[${TOPICS%,}]}"
else
	PARAMS='{}'
fi
{
	printf '%s\n' "{\"id\":1,\"method\":\"subscribe\",\"params\":$PARAMS}"
	sleep infinity
} | socat - "UNIX-CONNECT:$SOCK" |
	jq --unbuffered -c 'select(.event != null)'
