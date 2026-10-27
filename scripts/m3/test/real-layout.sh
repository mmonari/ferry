#!/usr/bin/env bash
# The fake daemon against the real ferryd for `layout.set`, and plugin/Geometry.js against it
# (real-layout.mjs says what is compared). Starts a fresh pair of `ferryd --no-input` on scratch dirs
# (scripts/m3/dev-ferryd.sh: UDP 4251 and 4252, the host's monitors read-only), a fake with the same
# monitors, runs the comparison, and stops them. Never the user's own ferryd.
#
#   scripts/m3/test/real-layout.sh          needs target/debug/ferryd (scripts/m3/dev-ferryd.sh build)
#
# Not part of check.sh: it needs a built ferryd and the host's Hyprland.
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

# short on purpose: the Hyprland socket is reached through a symlink in this dir, and a path over 108
# bytes does not fit a unix socket address (ferryd then finds no monitors and says so in its log)
DEV=/tmp/fm3r
[ -x "$ROOT/target/debug/ferryd" ] || die "no $ROOT/target/debug/ferryd: scripts/m3/dev-ferryd.sh build"
"$M3/dev-ferryd.sh" start --pair --monitors host --dir "$DEV" >&2 || die "the dev pair did not start"
trap '"$M3/dev-ferryd.sh" stop --dir "$DEV" >&2; m3_cleanup' EXIT
m3_scratch
trap '"$M3/dev-ferryd.sh" stop --dir "$DEV" >&2; m3_cleanup' EXIT
m3_fake fake --name dev-a --fixture pair
timeout 120 node "$M3/test/real-layout.mjs" "$DEV/a/run/ferry/ferryd.sock" "$SCR/run/ferry/ferryd.sock"
