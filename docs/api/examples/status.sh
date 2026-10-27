#!/bin/sh
# Print the status result of ferryd as one JSON line. Needs socat and jq.
SOCK="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/ferry/ferryd.sock"
printf '%s\n' '{"id":1,"method":"status"}' |
	socat - "UNIX-CONNECT:$SOCK" |
	jq -c 'select(.id == 1) | .result'
