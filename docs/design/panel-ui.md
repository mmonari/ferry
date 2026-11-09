# The Ferry panel: layout and style rules

> Decided on 2026-11-09, after a look at Station (github.com/Yakumy/Station), whose panel is the
> model for ease of use. It replaces the earlier popover and separate setup window.

## The frame

- **One panel.** The bar icon opens it; there is no popover any more. It is the widget's popout
  (`PanelHost.qml`, the shell's `KeyboardPanel`), so a click outside, `shell toggle ferry.bar`,
  SUPER+CTRL+<n> and Tab between popouts all work.
- **It opens centred** on the monitor, as a dialog. There is no option to open it beside the icon: one
  way to open it, nothing to configure.
- **One size for every tab** (`Style.space(920)` by `Style.space(600)`, fitted to the screen). A tab
  that has more than fits scrolls inside the frame.
- **Header:** the mark and "Ferry" on the left; *Lock to this machine* and the close button on the
  right, on every tab. **Tabs** under it: `Screens · Pair · Settings`, plain text, the shown one in
  full colour, DemiBold, with a 2 px `Commons.Color.accent` underline; the others dim. A hairline
  (`PanelSeparator`) under the tabs. Alt+1..3 and Ctrl+PgUp/PgDn switch tabs.

## The tabs

| Tab | Left | Right |
|-----|------|-------|
| **Screens** (home) | the layout canvas (drag to arrange), its status line and edge links | this machine at rest (where the pointer is, what needs you, the trusted machines with rename and unpair, clipboard, Release, the escape chord, recent activity); a machine clicked on the canvas opens its row |
| **Pair** | pair with a machine: nearby machines as a hairline list, or an address | let a machine pair with this one: the address and the pairing-window button |
| **Settings** | the groups, one per line, the chosen one marked | the chosen group's settings, as a hairline list |

A running pairing takes the whole Pair tab (step bar, code, Match). Trusted machines are managed on
Screens, not on Pair.

## Style rules (from Station's design system, adapted)

1. **Nothing boxed.** A section is a small eyebrow label (`PanelSectionHeader`) over rows split by
   `PanelSeparator` hairlines. A row has no border and no fill, except a row that is in error
   (a `Commons.Color.urgent` border) — the one state worth decorating.
2. **Row anatomy.** The label on the left (`Style.font.body`, `Commons.Color.popups.text`), the value or
   the control on the right (a value is `Font.DemiBold`). A slider runs full width under its label
   line, with its value at the right end of that line. Help is one dim line (`Style.font.caption`)
   only when it says something the label does not; anything longer goes in a tooltip.
3. **One filled action per screen**: the one that commits (Apply, Match, Pair). Its partner that
   cancels or defers is bordered and neutral. Everything else is a flat text button.
4. **Badges only for the exception.** Saving at once is the default and carries no badge; *Restart*
   and *Setup* keep theirs.
5. **Bars appear only when needed**: an unapplied layout, a setting that waits for a restart, a
   problem. They sit at the bottom of the tab above a hairline, words on the left, actions on the right.
6. **Theme tokens only**: `Commons.Color.*`, `Style.*`, `Style.cornerRadius` for every radius; dim text is
   `Qt.darker(<token>, 1.4)`, never alpha; no shadow, glow or gradient. A light theme (Catppuccin
   Latte) is checked with every change.
7. **Two panes**: what you work on left, the details of what is selected right, with a vertical
   hairline between them and the same padding on both.
8. **The keyboard keeps working** as before: Tab and the arrows walk the controls, Enter and Space
   press, Esc backs out one step and then closes the panel.
