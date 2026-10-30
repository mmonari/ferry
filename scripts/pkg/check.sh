#!/usr/bin/env bash
# What can be proven about the ferry package without root and without installing it
# (the plain-x86-64 rule). Run it after a build:
#
#   scripts/pkg/check.sh [PKGFILE]     default: the newest ferry-*.pkg.tar.* in $PKGDEST,
#                                      else in pkgbuild/
#   options: --no-emulate              skip the Nehalem run under qemu-user
#
# Checks: the file list is exactly NFR-PKG-03's (scripts/pkg/expected-files.sh: the binaries, the
# unit, the ufw profile, the plugin folder, the launcher entry, the licences); the launcher entry and
# the plugin are what the repository has (content.sh); the packaged unit is dist/systemd/ferryd.service
# and passes `systemd-analyze verify`; the binaries link no Qt and no libpython and need no
# library that is not found; pkgver and `--version` equal the workspace version; no target CPU is
# configured, and both binaries start under a Nehalem (x86-64-v2, no AVX) CPU; namcap, if it is
# installed. Exit 1 when any check fails. Installing, upgrading and removing the package is
# tested in a lab VM, never here (docs/packaging.md).
set -uo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
PKG='' EMULATE=1
while [ $# -gt 0 ]; do
  case $1 in
    --no-emulate) EMULATE=0 ;;
    -h | --help) sed -n '2,13p' "$0"; exit 0 ;;
    -*) echo "error: unknown option $1" >&2; exit 2 ;;
    *) PKG=$1 ;;
  esac
  shift
done

fails=0
ok() { printf '  ok    %s\n' "$*"; }
bad() { printf '  FAIL  %s\n' "$*"; fails=$((fails + 1)); }
skip() { printf '  skip  %s\n' "$*"; }
info() { printf '  info  %s\n' "$*"; }
# must <what it asserts> <command...>: ok when the command succeeds, FAIL otherwise.
must() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }

if [ -z "$PKG" ]; then
  # shellcheck disable=SC2012  # makepkg names are plain; newest first is the point
  PKG=$(ls -t "${PKGDEST:-$ROOT/pkgbuild}"/ferry-*.pkg.tar.{zst,xz} 2>/dev/null | head -1)
fi
[ -f "$PKG" ] || { echo "error: no ferry-*.pkg.tar.* (build one: cd pkgbuild && makepkg -f)" >&2; exit 2; }
echo "package: $PKG"

work=$(mktemp -d "${TMPDIR:-/tmp}/ferry-pkgcheck.XXXXXX") || exit 2
trap 'rm -rf "$work"' EXIT
bsdtar -xf "$PKG" -C "$work" || { echo "error: cannot extract $PKG" >&2; exit 2; }

echo "== file list (NFR-PKG-03)"
want=$("$ROOT/scripts/pkg/expected-files.sh")
have=$(bsdtar -tf "$PKG" | grep -v '/$' | grep -v '^\.' | LC_ALL=C sort)
if [ "$have" = "$want" ]; then
  ok "exactly the $(printf '%s\n' "$want" | wc -l) files of scripts/pkg/expected-files.sh"
else
  bad "file list differs (< wanted, > found)"
  diff <(echo "$want") <(echo "$have") | sed 's/^/        /'
fi
if echo "$have" | grep -q -E 'udev|modules-load'; then bad "a udev or module rule ships (A4)"; else ok "no udev rule, no module config"; fi
if echo "$have" | grep -v -E '^(usr/|etc/ufw/applications\.d/ferry$)' | grep -q .; then
  bad "a file outside /usr and the ufw profile"
else
  ok "files only under /usr, plus the ufw profile"
fi

echo "== contents"
must "the packaged unit is dist/systemd/ferryd.service" cmp -s "$work/usr/lib/systemd/user/ferryd.service" "$ROOT/dist/systemd/ferryd.service"
must "the ufw profile is dist/ufw/ferry" cmp -s "$work/etc/ufw/applications.d/ferry" "$ROOT/dist/ufw/ferry"
if grep -qx 'ports=4243/udp' "$work/etc/ufw/applications.d/ferry" && grep -qx '\[ferry\]' "$work/etc/ufw/applications.d/ferry"; then
  ok "profile [ferry] opens 4243/udp"
else
  bad "profile is not [ferry] with ports=4243/udp"
fi
for bin in usr/bin/ferry usr/bin/ferryd; do
  must "$bin is executable" test -x "$work/$bin"
done
must "the ufw profile is a backup file" grep -q '^backup = etc/ufw/applications.d/ferry$' "$work/.PKGINFO"

# shellcheck source=content.sh
. "$ROOT/scripts/pkg/content.sh"
echo "== plugin and launcher entry (FR-UI-18, FR-SETUP-02)"
check_plugin_and_desktop "$work"

echo "== unit shape (NFR-PKG-05)"
unit=$work/usr/lib/systemd/user/ferryd.service
for key in 'PartOf=graphical-session.target' 'WantedBy=graphical-session.target' 'Restart=on-failure' 'Type=notify'; do
  must "unit has $key" grep -qx "$key" "$unit"
done
must "unit sets WatchdogSec" grep -q '^WatchdogSec=' "$unit"
# verify wants ExecStart to be an executable: point it at the extracted ferryd
v=$work/ferryd-verify.service
sed "s|^ExecStart=/usr/bin/ferryd|ExecStart=$work/usr/bin/ferryd|" "$unit" >"$v"
out=$(systemd-analyze --user verify "$v" 2>&1)
if [ -z "$out" ]; then ok "systemd-analyze --user verify prints nothing"; else bad "systemd-analyze verify: $out"; fi

echo "== linkage (NFR-PKG-01)"
libs=$(ldd "$work/usr/bin/ferry" "$work/usr/bin/ferryd" 2>&1)
if echo "$libs" | grep -q -i -E 'libQt|libpython'; then bad "links Qt or libpython"; else ok "no libQt*, no libpython"; fi
if echo "$libs" | grep -q 'not found'; then bad "a library is not found"; else ok "every library resolves"; fi
needed=$(readelf -d "$work/usr/bin/ferry" "$work/usr/bin/ferryd" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p' | sort -u | tr '\n' ' ')
info "needed: $needed"
# every library's owning package must be a depend, or one of a depend's own (gcc-libs pulls in
# libgcc): namcap's dependency check, without namcap
if command -v pacman >/dev/null; then
  declared=$(sed -n 's/^depend = \([^<>=]*\).*/\1/p' "$work/.PKGINFO")
  # shellcheck disable=SC2086  # one word per package
  declared="$declared
$(pacman -Qi $declared 2>/dev/null | sed -n 's/^Depends On *: //p' | tr -s ' ' '\n' | sed 's/[<>=].*//')"
  missing=''
  for lib in $needed; do
    path=$(ldd "$work/usr/bin/ferryd" "$work/usr/bin/ferry" | awk -v l="$lib" '$1 == l { print $3; exit }')
    [ -n "$path" ] || continue
    owner=$(pacman -Qoq "$path" 2>/dev/null)
    [ -n "$owner" ] || continue
    echo "$declared" | grep -qx "$owner" || missing="$missing $owner"
  done
  must "every linked library's package is in depends (missing:${missing:- none})" test -z "$missing"
else
  skip "pacman is not available: no depends check"
fi
glibc=$(objdump -T "$work/usr/bin/ferry" "$work/usr/bin/ferryd" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
info "newest glibc symbol version: ${glibc:-none} (the fast ring needs no newer than rc/stable)"

echo "== version (NFR-PKG-06)"
ws=$(awk '/^\[/ { in_ws = ($0 == "[workspace.package]") } in_ws && /^version = / { gsub(/"/, "", $3); print $3; exit }' "$ROOT/Cargo.toml")
pv=$(sed -n 's/^pkgver = //p' "$work/.PKGINFO")
case $pv in "$ws"-*) ok "pkgver ${pv} starts with the workspace version $ws" ;; *) bad "pkgver $pv, workspace version $ws" ;; esac
for b in ferry ferryd; do
  got=$("$work/usr/bin/$b" --version 2>&1)
  must "$b --version is '$b $ws' (got '$got')" test "$got" = "$b $ws"
done
for c in "$ROOT"/crates/*/Cargo.toml; do
  grep -q '^version.workspace = true$' "$c" || bad "$(basename "$(dirname "$c")") sets its own version"
done

echo "== no target CPU (on x86-64, the netbook is a Celeron N3060)"
cfg=0
for f in "$ROOT/.cargo/config.toml" "$ROOT/.cargo/config" "$HOME/.cargo/config.toml" /etc/makepkg.conf /etc/makepkg.conf.d/rust.conf; do
  if [ -f "$f" ] && grep -v '^[[:space:]]*#' "$f" | grep -q -E 'target-cpu|target_cpu|march=native'; then
    bad "$f sets a target CPU"; cfg=1
  fi
done
if printf '%s %s' "${RUSTFLAGS:-}" "${CARGO_ENCODED_RUSTFLAGS:-}" | grep -q 'target-cpu'; then bad "RUSTFLAGS sets a target CPU"; cfg=1; fi
[ $cfg = 0 ] && ok "no target-cpu in .cargo, makepkg.conf, rust.conf or RUSTFLAGS"
info "makepkg RUSTFLAGS: $(grep -h '^RUSTFLAGS=' /etc/makepkg.conf.d/rust.conf 2>/dev/null | tail -1)"
if [ $EMULATE = 1 ] && [ "$(uname -m)" != x86_64 ]; then
  skip "not an x86-64 host: no Nehalem run"
elif [ $EMULATE = 1 ]; then
  if command -v qemu-x86_64-static >/dev/null; then
    for b in ferry ferryd; do
      got=$(qemu-x86_64-static -cpu Nehalem -L / "$work/usr/bin/$b" --version 2>&1)
      must "$b starts under a Nehalem CPU, no AVX (got '$got')" test "$got" = "$b $ws"
    done
  else
    skip "qemu-x86_64-static is not installed: no Nehalem run"
  fi
fi

echo "== namcap"
if command -v namcap >/dev/null; then
  nc=$(namcap "$PKG" 2>&1)
  if echo "$nc" | grep -q ' E: '; then bad "namcap errors"; else ok "namcap reports no errors"; fi
  printf '%s\n' "${nc//$'\n'/$'\n        '}"
else
  skip "namcap is not installed on this host (run it where it is: namcap $PKG)"
fi

echo
if [ "$fails" -eq 0 ]; then echo "package checks passed"; else echo "$fails package check(s) FAILED"; fi
[ "$fails" -eq 0 ]
