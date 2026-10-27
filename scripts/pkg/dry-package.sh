#!/usr/bin/env bash
# package() of pkgbuild/PKGBUILD, run without a build: two empty executables stand in for ferry and
# ferryd, and the files it installs are compared with scripts/pkg/expected-files.sh and checked
# (launcher entry, plugin). Seconds, no cargo, no root, nothing installed anywhere but a temp dir.
# The full check of a built package is scripts/pkg/check.sh.
#
#   scripts/pkg/dry-package.sh
set -uo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
fails=0
ok() { printf '  ok    %s\n' "$*"; }
bad() { printf '  FAIL  %s\n' "$*"; fails=$((fails + 1)); }
skip() { printf '  skip  %s\n' "$*"; }
must() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }
# shellcheck source=content.sh
. "$ROOT/scripts/pkg/content.sh"

work=$(mktemp -d "${TMPDIR:-/tmp}/ferry-dry-package.XXXXXX") || exit 2
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src/target/release" "$work/pkg"
for b in ferry ferryd; do
  printf '#!/bin/sh\n' > "$work/src/target/release/$b"
  chmod 755 "$work/src/target/release/$b"
done

echo "== package()"
# a subshell: the PKGBUILD sets variables and functions that must not leak
if ! (
  set -e
  # shellcheck disable=SC2034  # read by the PKGBUILD
  startdir=$ROOT/pkgbuild
  # shellcheck disable=SC2034
  srcdir=$work/src
  # shellcheck disable=SC2034
  pkgdir=$work/pkg
  # shellcheck disable=SC1091
  . "$ROOT/pkgbuild/PKGBUILD"
  package
) > "$work/package.log" 2>&1; then
  cat "$work/package.log"
  bad "package() failed"
  exit 1
fi
ok "package() ran"

want=$("$ROOT/scripts/pkg/expected-files.sh")
have=$(cd "$work/pkg" && find . -type f | sed 's#^\./##' | LC_ALL=C sort)
n=$(printf '%s\n' "$want" | wc -l)
if [ "$have" = "$want" ]; then
  ok "it installs exactly the $n files of scripts/pkg/expected-files.sh"
else
  bad "the installed files differ from expected-files.sh (< wanted, > installed)"
  diff <(echo "$want") <(echo "$have") | sed 's/^/        /'
fi
must "nothing is installed outside /usr and the ufw profile" test -z "$(printf '%s\n' "$have" | grep -v -E '^(usr/|etc/ufw/applications\.d/ferry$)')"

echo "== plugin and launcher entry"
check_plugin_and_desktop "$work/pkg"

if [ "$fails" = 0 ]; then echo "all ok"; else echo "$fails check(s) failed"; fi
[ "$fails" = 0 ]
