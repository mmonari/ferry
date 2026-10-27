.pragma library

// Icon glyphs, all from the Material Design block of the Nerd Font that the shell's
// `monospace` alias resolves to (the same family every other bar widget draws with).
//
// They live in the Supplementary Private Use Area, above U+FFFF, so a QML string escape
// cannot spell them: a backslash-u escape takes four hex digits, so U+F0213 would come out as U+F021 followed by a 3. Build them from the code point,
// and check a new one against the real font before using it:
//   python3 -c "from fontTools.ttLib import TTFont; f=TTFont('/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf'); print(0xF0213 in f.getBestCmap())"

function cp(n) {
  return String.fromCodePoint(n);
}

// One glyph per widget state, and each silhouette differs from the next, so a greyscale
// screenshot still tells the states apart (NFR-A11Y-02).
var STATE = {
  off: cp(0xF06A6),         // md-power_plug_off: ferryd is not reachable
  idle: cp(0xF0031),        // md-anchor: docked, nobody to carry input to
  ready: cp(0xF0213),       // md-ferry: the mark, everything is fine
  away: cp(0xF0734),        // md-arrow_right_bold: input has left this machine
  controlled: cp(0xF0731),  // md-arrow_left_bold: input arrives from another machine
  locked: cp(0xF033E),      // md-lock
  attention: cp(0xF0026)    // md-alert
};

// Chrome glyphs used by the popover and the panels.
var UI = {
  ferry: cp(0xF0213),
  laptop: cp(0xF0322),
  desktop: cp(0xF01C5),
  monitor: cp(0xF0379),
  lock: cp(0xF033E),
  unlock: cp(0xF0FC6),
  alert: cp(0xF0026),
  alertCircle: cp(0xF05D6),
  check: cp(0xF012C),
  close: cp(0xF0156),
  closeCircle: cp(0xF0159),
  plus: cp(0xF0415),
  grid: cp(0xF02C1),
  cog: cp(0xF0493),
  copy: cp(0xF018F),
  clipboard: cp(0xF014C),
  eject: cp(0xF01EA),
  key: cp(0xF0306),
  link: cp(0xF0339),
  linkOff: cp(0xF0338),
  pulse: cp(0xF0430),
  refresh: cp(0xF0450),
  swap: cp(0xF04E1),
  undo: cp(0xF054C),
  trash: cp(0xF0A7A),
  chevronRight: cp(0xF0142),
  chevronDown: cp(0xF0140),
  keyboard: cp(0xF0313),
  mouse: cp(0xF037D),
  cursor: cp(0xF01C0),
  info: cp(0xF02FD),
  // the peer row, the pairing flow and the activity list
  pencil: cp(0xF03EB),
  dots: cp(0xF01D8),
  chevronUp: cp(0xF0143),
  loading: cp(0xF0772),
  checkCircle: cp(0xF05E0),
  timer: cp(0xF051F),
  fingerprint: cp(0xF0237),
  shield: cp(0xF0565),
  bell: cp(0xF009C),
  history: cp(0xF02DA),
  arrowRight: cp(0xF0054),
  arrowLeft: cp(0xF004D),
  plug: cp(0xF06A5),
  help: cp(0xF02D7),
  // the Arrange canvas
  zoomIn: cp(0xF034B),
  zoomOut: cp(0xF034A),
  fit: cp(0xF18F5),
  redo: cp(0xF044E)
};

// One glyph per kind of `notice` event (contract section 3): the activity list reads the kind from the
// glyph as well as from the words.
var NOTICE = {
  paired: "checkCircle",
  pair_failed: "alertCircle",
  pair_code: "key",
  peer_lost: "linkOff",
  peer_back: "link",
  unpaired: "linkOff",
  layout_replaced: "swap",
  attention: "alert"
};

function state(key) {
  return STATE[key] || STATE.off;
}

function notice(kind) {
  return UI[NOTICE[kind] || "info"];
}

function ui(key) {
  return UI[key] || "";
}
