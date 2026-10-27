#!/usr/bin/env bash
# The files the ferry package holds, one path per line without a leading slash,
# sorted the C way. The plugin folder is every file of plugin/ but the developer README; the rest is
# a fixed list. `scripts/pkg/check.sh` (a built package) and `scripts/pkg/dry-package.sh` (package()
# without a build) both compare against this, so the PKGBUILD cannot drift from the list unnoticed.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
{
  cat <<'LIST'
etc/ufw/applications.d/ferry
usr/bin/ferry
usr/bin/ferryd
usr/lib/systemd/user/ferryd.service
usr/share/applications/ferry.desktop
usr/share/licenses/ferry/LICENSE
usr/share/licenses/ferry/NOTICE
LIST
  (cd "$ROOT/plugin" && find . -type f ! -name README.md | sed 's#^\./#usr/share/ferry/plugin/#')
} | LC_ALL=C sort
