# Packaging and install

Ferry ships as one Arch package, `ferry`, built from this checkout with `pkgbuild/PKGBUILD`.

| File | What |
|------|------|
| `pkgbuild/PKGBUILD` | Builds `ferryd` and `ferry` from the checkout (no source tarball). |
| `pkgbuild/ferry.install` | The messages pacman prints after install, upgrade and removal. |
| `dist/systemd/ferryd.service` | The user unit, installed to `/usr/lib/systemd/user/`. |
| `dist/ufw/ferry` | The ufw application profile `ferry` (4243/udp), installed to `/etc/ufw/applications.d/`. |
| `dist/ferry.desktop` | The launcher entry "Ferry", installed to `/usr/share/applications/`; it runs `ferry ui arrange`. |
| `plugin/` | The omarchy-shell plugin `ferry.bar`, installed to `/usr/share/ferry/plugin/`; `ferry setup` links it. |
| `bin/install` | `makepkg -fsi` in `pkgbuild/`. |
| `scripts/pkg/check.sh` | What can be proven about a built package without root. |
| `scripts/pkg/dry-package.sh` | `package()` without a build: the file list and the plugin checks in seconds. |
| `scripts/pkg/expected-files.sh` | The list of files the package holds, which both scripts above compare with. |

## What the package holds

Exactly this (`scripts/pkg/expected-files.sh` is the list the checks use):

```
/usr/bin/ferry
/usr/bin/ferryd
/usr/lib/systemd/user/ferryd.service
/usr/share/ferry/plugin/                 the omarchy-shell plugin: manifest.json and its QML
/usr/share/applications/ferry.desktop    the launcher entry (opens the Arrange panel)
/etc/ufw/applications.d/ferry            (a backup file: pacman keeps your edits)
/usr/share/licenses/ferry/LICENSE
/usr/share/licenses/ferry/NOTICE
```

The plugin folder is `plugin/` of the checkout without its developer README, root-owned and with no
symlink inside it (`omarchy plugin validate` refuses one). A package cannot touch `$HOME`, so
`ferry setup` links the folder into `~/.config/omarchy/plugins/ferry.bar` and puts the widget on the
bar (see "The plugin step" below). No udev rule and no module config ship (decision brief A4).
The workspace also holds the M0 spikes; they are not built into the package.

`depends` are `glibc`, `gcc-libs`, `libxkbcommon` (the binaries link it) and `systemd`: the binaries
are Qt-free Rust. The plugin needs the Omarchy shell it is made for, which
every Omarchy machine has; it is not a `depends`, like `ufw` and `xdg-desktop-portal-hyprland`,
which are `optdepends` because Omarchy already has both.

## Build and install

```bash
bin/install              # makepkg -fsi in pkgbuild/; pacman asks for sudo
ferry setup              # as your user, not root; asks before each step
```

`bin/install` builds in `pkgbuild/src/` (git-ignored), so a second run is an incremental build.
`makepkg` without `-s` and `-i` builds the package and installs nothing:

```bash
cd pkgbuild && makepkg -f          # writes pkgbuild/ferry-<ver>-<rel>-<arch>.pkg.tar.zst (.xz where PKGEXT says so)
BUILDDIR=/tmp/fb PKGDEST=/tmp/fp makepkg -f   # same, with nothing landing in the checkout
scripts/pkg/check.sh [PKG]         # file list, unit, linkage, version, baseline CPU, namcap
```

`check()` runs `cargo test --frozen --release --workspace` so the tests must not need a
compositor, a network or a lab VM. To skip the tests on a rebuild, `makepkg -f --nocheck`.

`scripts/pkg/dry-package.sh` runs `package()` with two empty executables for the binaries and compares
what it installs with the expected list, in a few seconds and with no build: use it after any edit
of the PKGBUILD or of `plugin/`. It also validates `ferry.desktop` (`desktop-file-validate`) and the
plugin (`omarchy plugin validate`), and checks that the plugin's id is `ferry.bar` and its version is
the workspace version.

**Plain x86-64.** The build never sets a target CPU: no `target-cpu=native`, no `.cargo/config.toml`.
makepkg's own `RUSTFLAGS` (`/etc/makepkg.conf.d/rust.conf`) is only `-C force-frame-pointers=yes`.
The same binary must run on x86-64-v2 CPUs such as the Celeron N3060. `scripts/pkg/check.sh`
checks the configuration and starts both binaries under a Nehalem CPU (`qemu-x86_64-static`).
That proves startup only: `ring` picks its AVX code paths at run time from `cpuid`.
On aarch64 the package is built the same way (`arch=('x86_64' 'aarch64')`) and the Nehalem run is
skipped; a package runs only on the arch it was built on.

**One version.** `[workspace.package] version` in the top `Cargo.toml` is the only version
. Every crate takes it with `version.workspace = true`; `pkgver` is read from it; the
build fails if `ferry --version` or `ferryd --version` says anything else. To release, change
that one line (and `pkgrel=1`), and commit.

## The plugin step

`ferry setup` has one step for the bar plugin, the first of six. It makes two changes and asks once:

- **The link** `~/.config/omarchy/plugins/ferry.bar` -> `/usr/share/ferry/plugin`, the way
  `jankeesvw.meeting-recorder` is linked on an Omarchy machine. A link that points elsewhere (a
  checkout under development) or a real directory of that name is yours: setup says so, leaves it
  alone and still does the second change. Then it asks the shell to look at its plugin directory
  again (`omarchy-shell -q shell rescanPlugins`, best effort).
- **The entry** `{ "id": "ferry.bar" }` in `bar.layout.right` of `~/.config/omarchy/shell.json`,
  right after the tray (or after the plugin that took the tray's place, such as `my.tray`), as the
  shell's own `omarchy plugin enable` would put it. Only that element is inserted into the text: every
  other byte of the file stays, and a backup `shell.json.ferry-bak.<time>` is made first. A cloned bar
  (`bar.id` is `my.bar`) changes nothing: every bar reads `bar.layout`.
  A file that the shell would not read (not JSON, no `"version": 1`, no `bar.layout`) is not touched:
  setup prints `omarchy plugin enable ferry.bar --section right` instead. With no `shell.json` at all,
  setup starts one from the shell's default (`$OMARCHY_PATH/config/omarchy/shell.json`), because a valid
  user file replaces the default whole and a file with only Ferry in it would empty the bar.

A second run reports the step as skipped. `ferry setup --undo` takes the entry out (every section) and
removes the link if it is the one setup made. `--dry-run` shows both changes and writes nothing.

## Which unit runs

A unit in `~/.config/systemd/user/` wins over `/usr/lib/systemd/user/`. `ferry setup` therefore
treats the two cases apart:

- **Package installed, no user copy:** setup runs `systemctl --user enable --now ferryd` and writes
  no unit. It says `(using the packaged unit /usr/lib/systemd/user/ferryd.service)`.
- **Package installed, and a user copy that `ferry setup` wrote earlier** (it starts with the
  `# Written by `ferry setup`` line, and its `ExecStart=` names the old build): the copy would keep
  starting the old `ferryd`. Setup shows the plan and asks once: `systemctl --user disable --now
  ferryd`, delete the copy, `daemon-reload`, `enable --now ferryd`. The next `ferry setup` run
  skips the step.
- **Package installed, and a unit you wrote yourself** at that path: setup never touches it. It says
  that your file shadows the packaged unit and that you can delete it. `ferry doctor` shows the
  same as a `unit` warning (`... shadows the packaged /usr/lib/systemd/user/ferryd.service`).
- **No package:** as before, setup writes the user copy with `ExecStart=` set to the `ferryd` next
  to the `ferry` that ran it.

## Upgrade

```bash
git pull                           # or switch branch
bin/install                        # pkgrel stays 1; -f overwrites the package file
systemctl --user restart ferryd    # the running ferryd is still the old binary
ferry doctor
```

pacman prints the restart line from `post_upgrade`. A package cannot restart a user's daemon:
it runs as root and cannot reach the user manager. `ferry doctor` warns on the `daemon` row when
the running ferryd's version differs from `ferry --version`, with the restart line as the fix.
A rebuild of the same `pkgver` (a new `pkgrel`, or new code under the same version) looks
identical to doctor, so restart it by hand then. Upgrade both machines to the same build: mixed versions
are not tested yet.


## Uninstall

```bash
ferry setup --undo                 # as your user: takes the widget off the bar, unlinks the plugin,
                                   # stops and disables ferryd, prints the Hyprland and ufw lines to remove
sudo pacman -R ferry
```

Add `--purge` to `ferry setup --undo` to revoke trusted peers and delete keys, config and state.
Take the printed lines out of `~/.config/hypr/` yourself, and run `sudo ufw delete allow ferry`
if you opened the port. If you removed the package first, `ferry` is gone: pacman's `post_remove`
message lists the steps by hand (`systemctl --user disable --now ferryd`, `daemon-reload`, `rm` of the
plugin link, the `ferry.bar` entry in `shell.json`, the `ferry.lua` require line, the ufw rule). The
shell cannot disable a plugin it no longer knows, so that entry is taken out of `shell.json` by hand.
Removal does not touch your home directory, and `pacman -R` keeps a
changed `/etc/ufw/applications.d/ferry` as `.pacsave`.
