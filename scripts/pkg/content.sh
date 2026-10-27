#!/usr/bin/env bash
# Checks of what the ferry package holds beyond its file list: the launcher entry and the plugin.
# Sourced by check.sh and dry-package.sh, which define ok, bad,
# skip and must (and ROOT). `check_plugin_and_desktop <dir>`: <dir> is the root of the files, an
# extracted package or a pkgdir.

check_plugin_and_desktop() {
  local plugin=$1/usr/share/ferry/plugin desktop=$1/usr/share/applications/ferry.desktop
  must "ferry.desktop is dist/ferry.desktop" cmp -s "$desktop" "$ROOT/dist/ferry.desktop"
  if command -v desktop-file-validate > /dev/null; then
    must "ferry.desktop passes desktop-file-validate (no error, no hint)" test -z "$(desktop-file-validate "$desktop" 2>&1)"
  else
    skip "desktop-file-validate is not installed"
  fi
  must "the launcher entry opens the Arrange panel through ferry ui" grep -qx 'Exec=ferry ui arrange' "$desktop"
  must "the launcher entry's icon is input-mouse" grep -qx 'Icon=input-mouse' "$desktop"
  must "the packaged plugin is plugin/, file for file, but for the developer README" diff -rq --exclude=README.md "$ROOT/plugin" "$plugin"
  must "the plugin folder holds no symlink (omarchy plugin validate refuses one)" test -z "$(find "$plugin" -type l)"
  if command -v omarchy-plugin-validate > /dev/null; then
    must "omarchy plugin validate passes on the packaged plugin" omarchy-plugin-validate "$plugin"
  else
    skip "omarchy-plugin-validate is not installed"
  fi
  local ws mv id
  ws=$(awk '/^\[/ { in_ws = ($0 == "[workspace.package]") } in_ws && /^version = / { gsub(/"/, "", $3); print $3; exit }' "$ROOT/Cargo.toml")
  mv=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' "$plugin/manifest.json")
  id=$(sed -n 's/^  "id": "\(.*\)",$/\1/p' "$plugin/manifest.json")
  must "the plugin's id is ferry.bar, the name ferry setup links it under (got '$id')" test "$id" = ferry.bar
  must "the plugin's version is the workspace version $ws (got '$mv')" test "$mv" = "$ws"
}
