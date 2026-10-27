#!/usr/bin/env python3
"""Render Omarchy themes into what the shell reads, for the dev harness.

    render-themes.py OUT.json [THEME ...]

Each THEME is a theme name (looked up in ~/.config/omarchy/themes and /usr/share/omarchy/themes),
a directory that holds a colors.toml, or `current` for the theme that is applied right now. For
each one the output has the colors.toml text and a shell.toml, which is the stock template
(default/themed/shell.toml.tpl) filled in from the colors, as `omarchy theme set` does. The shell
applies a theme by loading those two texts (shell.qml, `applyTheme`), and so does the harness.

Reads only. Writes OUT.json. Python 3.11+ (tomllib), standard library only.
"""

import json
import os
import re
import sys
import tomllib

OMARCHY = os.environ.get("OMARCHY_PATH", "/usr/share/omarchy")
TEMPLATE = os.path.join(OMARCHY, "default/themed/shell.toml.tpl")
STATE = os.path.expanduser("~/.local/state/omarchy/current")
SEARCH = [os.path.expanduser("~/.config/omarchy/themes"), os.path.join(OMARCHY, "themes")]


def find(name):
    if name == "current":
        return os.path.join(STATE, "theme")
    if os.path.isdir(name):
        return name
    for base in SEARCH:
        path = os.path.join(base, name)
        if os.path.isfile(os.path.join(path, "colors.toml")):
            return path
    sys.exit(f"render-themes: no theme {name!r} in {', '.join(SEARCH)}")


def hex_to_rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def mix(a, b, pct):
    ra, rb = hex_to_rgb(a), hex_to_rgb(b)
    return "#" + "".join(f"{round(x * pct / 100 + y * (100 - pct) / 100):02x}" for x, y in zip(ra, rb))


def render(template, colors):
    def var(name):
        if name in colors:
            return colors[name]
        if name == "red":
            return colors.get("color1", "#a55555")
        sys.exit(f"render-themes: colors.toml has no {name!r}")

    def sub(m):
        parts = m.group(1).split()
        if parts[0] == "mix":
            return mix(var(parts[1]), var(parts[2]), int(parts[3].rstrip("%")))
        if parts[0] == "shell_gradient":
            return var(parts[2])  # a gradient is a solid colour here: the theme's own hyprland.lua is not read
        return var(parts[0])

    return re.sub(r"\{\{\s*([^}]*?)\s*\}\}", sub, template)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out, names = sys.argv[1], sys.argv[2:] or ["current"]
    template = open(TEMPLATE).read()
    themes = []
    for name in names:
        path = find(name)
        raw = open(os.path.join(path, "colors.toml")).read()
        colors = tomllib.loads(raw)
        shell_file = os.path.join(path, "shell.toml")
        shell = open(shell_file).read() if os.path.isfile(shell_file) else render(template, colors)
        themes.append({"name": os.path.basename(path.rstrip("/")) if name != "current" else "current",
                       "mode": colors.get("mode", "dark"), "colors": raw, "shell": shell})
    with open(out, "w") as f:
        json.dump({"themes": themes}, f)
    print(f"render-themes: {len(themes)} theme(s) -> {out}", file=sys.stderr)


if __name__ == "__main__":
    main()
