//! `~/.config/ferry/config.toml`.
//!
//! A missing file means defaults. An unknown key is a warning, not an error, so a config written
//! by a newer ferry does not stop an older ferryd. A wrong type or an out-of-range value is an
//! error that names the file, except `[safety] lock_check_budget_ms`, which is clamped into its
//! range with a warning.
//!
//! [`KEYS`] is the one list of the file's keys: what the loader knows (unknown keys warn), what
//! [`Config::validate`] checks, and what `config.schema`, `config.get` and `config.set` serve
//! (`config_api.rs`). A key added to a struct but not to the list fails a test.

use std::path::Path;

use ferry_core::api::Applies;
use ferry_core::chord::{ChordError, EscapeChord, GoChord};
use ferry_core::config::{CrossingConfig, SourceConfig};
use ferry_core::time::LockBudget;
use serde::{Deserialize, Serialize};

use crate::fsutil::{StoreError, read_optional};

/// `[peer]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerSection {
    /// This peer's name. Empty: the hostname.
    pub name: String,
    /// UDP port of the QUIC endpoint.
    pub port: u16,
}

impl Default for PeerSection {
    fn default() -> Self {
        Self {
            name: String::new(),
            port: ferry_net::DEFAULT_PORT,
        }
    }
}

/// `input.capture`. `off` is for target-only peers: no portal session is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureMode {
    #[default]
    Auto,
    Portal,
    Direct,
    LayerShell,
    Off,
}

impl CaptureMode {
    /// Every mode, in the order `config.schema` lists them.
    pub const ALL: [CaptureMode; 5] = [
        Self::Auto,
        Self::Portal,
        Self::Direct,
        Self::LayerShell,
        Self::Off,
    ];

    /// `portal` or `off`, the `backend` shown by `status`.
    pub fn backend_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            _ => "portal",
        }
    }

    /// The name in `config.toml`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Portal => "portal",
            Self::Direct => "direct",
            Self::LayerShell => "layer-shell",
            Self::Off => "off",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Emulation {
    #[default]
    Wayland,
}

impl Emulation {
    pub const ALL: [Emulation; 1] = [Self::Wayland];

    /// The name in `config.toml`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == name)
    }
}

/// `[input]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InputSection {
    pub capture: CaptureMode,
    pub emulation: Emulation,
    pub scroll_invert: bool,
}

/// `[clipboard]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardSection {
    pub enabled: bool,
    pub max_bytes: u64,
}

impl Default for ClipboardSection {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: 32 << 20,
        }
    }
}

/// `[discovery]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoverySection {
    pub mdns: bool,
    pub advertise: bool,
}

impl Default for DiscoverySection {
    fn default() -> Self {
        Self {
            mdns: true,
            advertise: true,
        }
    }
}

/// `[safety]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetySection {
    /// The lock check's budget, 50–100 ms. A slow machine may need more than 50; the
    /// active lock poll shortens and the Pending timeout grows with it ([`LockBudget`]).
    pub lock_check_budget_ms: u32,
}

impl Default for SafetySection {
    fn default() -> Self {
        Self {
            lock_check_budget_ms: LockBudget::DEFAULT.millis(),
        }
    }
}

/// `[notify]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifySection {
    /// Send the toasts to the desktop's notification daemon. `false` turns off only that: every
    /// toast is still decided and still reaches the socket as a `notice` event, so a
    /// frontend that draws its own toasts keeps working.
    pub enabled: bool,
}

impl Default for NotifySection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub peer: PeerSection,
    pub crossing: CrossingConfig,
    pub input: InputSection,
    pub clipboard: ClipboardSection,
    pub discovery: DiscoverySection,
    pub safety: SafetySection,
    pub notify: NotifySection,
}

/// The rule lives in ferry-core, which applies it to a peer's `Hello.name`.
pub use ferry_core::peer::validate_peer_name;

/// `[crossing] edge_resistance_px` and `corner_dead_zone_px` are 0 to this many logical pixels
/// (-08).
pub const CROSSING_PX_MAX: i64 = 200;

/// How a key's value is read and written, in `Config`.
#[derive(Clone, Copy, Debug)]
pub enum Field {
    Bool {
        get: fn(&Config) -> bool,
        set: fn(&mut Config, bool),
    },
    Int {
        min: i64,
        max: i64,
        /// The file loader clamps an out-of-range value with a warning instead of refusing it.
        /// `config.set` refuses it all the same.
        clamped_by_loader: bool,
        /// The top of the range a person usually wants, when it is far below `max`: a frontend
        /// may offer a slider up to here beside the exact field (`config.schema` `slider_max`).
        slider_max: Option<i64>,
        get: fn(&Config) -> i64,
        set: fn(&mut Config, i64),
    },
    /// A peer name; empty means the hostname.
    Name {
        get: fn(&Config) -> &str,
        set: fn(&mut Config, String),
    },
    Chord {
        kind: ChordKind,
        get: fn(&Config) -> &str,
        set: fn(&mut Config, String),
    },
    Enum {
        choices: &'static [&'static str],
        get: fn(&Config) -> &'static str,
        /// `false`: not one of `choices`.
        set: fn(&mut Config, &str) -> bool,
    },
}

/// What a [`Field::Chord`] holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordKind {
    /// Modifiers and one key ([`EscapeChord`]).
    Escape,
    /// Modifiers only; the arrow keys complete it ([`GoChord`]).
    Go,
}

impl ChordKind {
    /// `text` parsed as this kind, in its canonical spelling.
    pub fn canonical(self, text: &str) -> Result<String, ChordError> {
        match self {
            ChordKind::Escape => EscapeChord::parse(text).map(|c| c.to_string()),
            ChordKind::Go => GoChord::parse(text).map(|c| c.to_string()),
        }
    }
}

/// One key of `config.toml`.
#[derive(Clone, Copy, Debug)]
pub struct Key {
    /// `section.name`.
    pub key: &'static str,
    pub field: Field,
    pub applies: Applies,
    /// False while no code in this build acts on the key (the feature arrives in a later
    /// version). `config.schema` says so, so a frontend does not offer a switch that does nothing;
    /// the file loader and `config.set` still take the value, so a config written now keeps working.
    pub available: bool,
    pub unit: Option<&'static str>,
    pub doc: &'static str,
}

impl Key {
    /// `(section, name)` of `crossing.escape_chord`.
    pub fn parts(&self) -> (&'static str, &'static str) {
        self.key.split_once('.').unwrap_or(("", self.key))
    }

    /// The value in `config`, as JSON.
    pub fn value(&self, config: &Config) -> serde_json::Value {
        use serde_json::Value;
        match self.field {
            Field::Bool { get, .. } => Value::Bool(get(config)),
            Field::Int { get, .. } => Value::from(get(config)),
            Field::Name { get, .. } | Field::Chord { get, .. } => Value::from(get(config)),
            Field::Enum { get, .. } => Value::from(get(config)),
        }
    }

    /// Why the loader refuses `config`'s value of this key, if it does: a clause that reads after
    /// the key's name ("must be 0..=200").
    pub fn refusal(&self, config: &Config) -> Option<String> {
        match self.field {
            Field::Int {
                min,
                max,
                clamped_by_loader: false,
                get,
                ..
            } => {
                let v = get(config);
                (v < min || v > max).then(|| format!("must be {min}..={max}"))
            }
            Field::Name { get, .. } => {
                let v = get(config);
                if v.is_empty() {
                    return None;
                }
                validate_peer_name(v).err()
            }
            Field::Chord { kind, get, .. } => {
                let v = get(config);
                kind.canonical(v).err().map(|e| format!("{v:?}: {e}"))
            }
            Field::Int { .. } | Field::Bool { .. } | Field::Enum { .. } => None,
        }
    }
}

/// Every key of `config.toml`, in the order of the file.
pub static KEYS: &[Key] = &[
    Key {
        key: "peer.name",
        field: Field::Name {
            get: |c| &c.peer.name,
            set: |c, v| c.peer.name = v,
        },
        applies: Applies::Restart,
        available: true,
        unit: None,
        doc: "This machine's name on the other machines and in the layout. Empty uses the hostname",
    },
    Key {
        key: "peer.port",
        field: Field::Int {
            min: 0,
            max: 65535,
            clamped_by_loader: false,
            slider_max: None,
            get: |c| i64::from(c.peer.port),
            set: |c, v| c.peer.port = u16::try_from(v).unwrap_or_default(),
        },
        applies: Applies::Restart,
        available: true,
        unit: None,
        doc: "The UDP port the other machines connect to; open it in the firewall. 0 lets the kernel pick one",
    },
    Key {
        key: "crossing.edge_resistance_px",
        field: Field::Int {
            min: 0,
            max: CROSSING_PX_MAX,
            clamped_by_loader: false,
            slider_max: None,
            get: |c| i64::from(c.crossing.edge_resistance_px),
            set: |c, v| c.crossing.edge_resistance_px = u16::try_from(v).unwrap_or_default(),
        },
        applies: Applies::Live,
        available: true,
        unit: Some("px"),
        doc: "How far to push into an edge before the pointer crosses. 0 crosses on contact",
    },
    Key {
        key: "crossing.edge_dwell_ms",
        field: Field::Int {
            min: 0,
            max: 65535,
            clamped_by_loader: false,
            slider_max: Some(1000),
            get: |c| i64::from(c.crossing.edge_dwell_ms),
            set: |c, v| c.crossing.edge_dwell_ms = u16::try_from(v).unwrap_or_default(),
        },
        applies: Applies::Live,
        available: true,
        unit: Some("ms"),
        doc: "How long to push into an edge before the pointer crosses. 0 turns the delay off",
    },
    Key {
        key: "crossing.corner_dead_zone_px",
        field: Field::Int {
            min: 0,
            max: CROSSING_PX_MAX,
            clamped_by_loader: false,
            slider_max: None,
            get: |c| i64::from(c.crossing.corner_dead_zone_px),
            set: |c, v| c.crossing.corner_dead_zone_px = u16::try_from(v).unwrap_or_default(),
        },
        applies: Applies::Live,
        available: true,
        unit: Some("px"),
        doc: "The stretch at each end of an edge link that acts as a wall",
    },
    Key {
        key: "crossing.block_while_button_held",
        field: Field::Bool {
            get: |c| c.crossing.block_while_button_held,
            set: |c, v| c.crossing.block_while_button_held = v,
        },
        applies: Applies::Live,
        available: true,
        unit: None,
        doc: "Do not cross back while a mouse button is held on the other machine, so a drag stays where it began",
    },
    Key {
        key: "crossing.escape_chord",
        field: Field::Chord {
            kind: ChordKind::Escape,
            get: |c| &c.crossing.escape_chord,
            set: |c, v| c.crossing.escape_chord = v,
        },
        // ferryd matches the new chord at once; the bind in Hyprland's config, which works even
        // when ferryd is stuck, follows `ferry setup`.
        applies: Applies::Setup,
        available: true,
        unit: None,
        doc: "The keys that always give input back: at least two modifiers and one key. Run `ferry setup` to update the Hyprland bind",
    },
    Key {
        key: "crossing.go_chord",
        field: Field::Chord {
            kind: ChordKind::Go,
            get: |c| &c.crossing.go_chord,
            set: |c, v| c.crossing.go_chord = v,
        },
        // ferryd matches it at once while on another machine; the binds that start a crossing
        // from here are Hyprland's, and follow `ferry setup`.
        applies: Applies::Setup,
        available: true,
        unit: None,
        doc: "These modifiers with an arrow key move the keyboard and mouse to the machine on that side, or back. Run `ferry setup` to update the Hyprland binds",
    },
    Key {
        key: "crossing.edge_glow",
        field: Field::Bool {
            get: |c| c.crossing.edge_glow,
            set: |c, v| c.crossing.edge_glow = v,
        },
        // ctl reads it at each crossing
        applies: Applies::Live,
        available: true,
        unit: None,
        doc: "Light the edge on both machines for a moment when the pointer crosses it, in each machine's theme colour",
    },
    Key {
        key: "input.capture",
        field: Field::Enum {
            choices: &["auto", "portal", "direct", "layer-shell", "off"],
            get: |c| c.input.capture.as_str(),
            set: |c, v| {
                CaptureMode::from_name(v)
                    .map(|m| c.input.capture = m)
                    .is_some()
            },
        },
        applies: Applies::Restart,
        available: true,
        unit: None,
        doc: "How input is captured on this machine. off makes it a target only",
    },
    Key {
        key: "input.emulation",
        field: Field::Enum {
            choices: &["wayland"],
            get: |c| c.input.emulation.as_str(),
            set: |c, v| {
                Emulation::from_name(v)
                    .map(|m| c.input.emulation = m)
                    .is_some()
            },
        },
        applies: Applies::Restart,
        available: true,
        unit: None,
        doc: "How input is injected when this machine is a target",
    },
    // The scroll direction and discovery are read from the file and written back, and nothing
    // else uses them yet. `available: false` tells a frontend not to offer them as controls that
    // do nothing.
    Key {
        key: "input.scroll_invert",
        field: Field::Bool {
            get: |c| c.input.scroll_invert,
            set: |c, v| c.input.scroll_invert = v,
        },
        applies: Applies::Restart,
        available: false,
        unit: None,
        doc: "Invert the scroll direction on targets",
    },
    Key {
        key: "clipboard.enabled",
        field: Field::Bool {
            get: |c| c.clipboard.enabled,
            set: |c, v| c.clipboard.enabled = v,
        },
        // the net thread starts or stops the clipboard bridge at once
        applies: Applies::Live,
        available: true,
        unit: None,
        doc: "Share the clipboard between the machines",
    },
    Key {
        key: "clipboard.max_bytes",
        field: Field::Int {
            min: 0,
            // the largest integer a TOML file holds, which is what the loader has always accepted
            max: i64::MAX,
            clamped_by_loader: false,
            slider_max: None,
            get: |c| i64::try_from(c.clipboard.max_bytes).unwrap_or(i64::MAX),
            set: |c, v| c.clipboard.max_bytes = u64::try_from(v).unwrap_or_default(),
        },
        applies: Applies::Live,
        available: true,
        unit: Some("bytes"),
        doc: "The largest clipboard item that is shared",
    },
    Key {
        key: "discovery.mdns",
        field: Field::Bool {
            get: |c| c.discovery.mdns,
            set: |c, v| c.discovery.mdns = v,
        },
        applies: Applies::Restart,
        available: false,
        unit: None,
        doc: "Look for other machines on the network",
    },
    Key {
        key: "discovery.advertise",
        field: Field::Bool {
            get: |c| c.discovery.advertise,
            set: |c, v| c.discovery.advertise = v,
        },
        applies: Applies::Restart,
        available: false,
        unit: None,
        doc: "Let other machines on the network find this one",
    },
    Key {
        key: "safety.lock_check_budget_ms",
        field: Field::Int {
            min: LockBudget::MIN_MS as i64,
            max: LockBudget::MAX_MS as i64,
            clamped_by_loader: true,
            slider_max: None,
            get: |c| i64::from(c.safety.lock_check_budget_ms),
            set: |c, v| c.safety.lock_check_budget_ms = u32::try_from(v).unwrap_or_default(),
        },
        applies: Applies::Restart,
        available: true,
        unit: Some("ms"),
        doc: "How long the screen-lock check may take before a crossing counts the screen as locked. A slow machine may need more",
    },
    Key {
        key: "notify.enabled",
        field: Field::Bool {
            get: |c| c.notify.enabled,
            set: |c, v| c.notify.enabled = v,
        },
        // `notify::set_enabled` switches the desktop delivery on the running daemon
        applies: Applies::Live,
        available: true,
        unit: None,
        doc: "Show Ferry's notifications on the desktop: the pairing code, a machine lost or back, a replaced layout, a problem that needs you. Off, they still reach the bar plugin",
    },
];

/// The key `crossing.escape_chord`, if `key` is one.
pub fn find_key(key: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.key == key)
}

impl Config {
    /// Parses `text`. `source` names the file in errors. Returns the config and one warning per
    /// unknown key.
    pub fn parse(text: &str, source: &Path) -> Result<(Self, Vec<String>), StoreError> {
        // `Display` of a TOML error quotes the offending line, so the message names the key.
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| StoreError::corrupt(source, e))?;
        let mut warnings = unknown_keys(&table);
        let mut config: Config =
            toml::from_str(text).map_err(|e| StoreError::corrupt(source, e))?;
        config
            .validate()
            .map_err(|m| StoreError::corrupt(source, m))?;
        let given = config.safety.lock_check_budget_ms;
        let (budget, clamped) = LockBudget::clamped(given);
        if clamped {
            warnings.push(format!(
                "[safety] lock_check_budget_ms = {given} is outside {}..={}: using {}",
                LockBudget::MIN_MS,
                LockBudget::MAX_MS,
                budget.millis()
            ));
            config.safety.lock_check_budget_ms = budget.millis();
        }
        Ok((config, warnings))
    }

    /// Loads `path`. A missing file is the defaults.
    pub fn load(path: &Path) -> Result<(Self, Vec<String>), StoreError> {
        match read_optional(path)? {
            Some(text) => Self::parse(&text, path),
            None => Ok((Self::default(), Vec::new())),
        }
    }

    /// The first key the loader refuses, and why ([`KEYS`]). `config.set` asks the same question
    /// of the config it would save, so the file and the API never disagree.
    pub fn refusal(&self) -> Option<(&'static Key, String)> {
        KEYS.iter()
            .find_map(|k| k.refusal(self).map(|why| (k, why)))
    }

    /// [`Config::refusal`], as the message of the loader: `[crossing] edge_resistance_px: must be
    /// 0..=200`.
    pub fn validate(&self) -> Result<(), String> {
        match self.refusal() {
            Some((k, why)) => {
                let (section, name) = k.parts();
                Err(format!("[{section}] {name}: {why}"))
            }
            None => Ok(()),
        }
    }

    /// What the SourceMachine runs with: `[crossing]`, the escape chord parsed, and the repeat
    /// hold ctl derived from Hyprland's `input:repeat_delay`.
    pub fn source_config(&self, repeat_hold_after_ms: u32) -> Result<SourceConfig, ChordError> {
        let c = &self.crossing;
        Ok(SourceConfig {
            edge_resistance_px: c.edge_resistance_px,
            edge_dwell_ms: c.edge_dwell_ms,
            corner_dead_zone_px: c.corner_dead_zone_px,
            block_while_button_held: c.block_while_button_held,
            escape_chord: EscapeChord::parse(&c.escape_chord)?,
            go_chord: GoChord::parse(&c.go_chord)?,
            repeat_hold_after_ms,
            lock_check_budget: self.lock_check_budget(),
        })
    }

    /// `[safety] lock_check_budget_ms`, clamped (`parse` already clamped what a file said).
    pub fn lock_check_budget(&self) -> LockBudget {
        LockBudget::clamped(self.safety.lock_check_budget_ms).0
    }
}

fn unknown_keys(table: &toml::Table) -> Vec<String> {
    let mut out = Vec::new();
    for (section, value) in table {
        if !KEYS.iter().any(|k| k.parts().0 == section) {
            out.push(format!("unknown section [{section}]"));
            continue;
        }
        if let toml::Value::Table(t) = value {
            for key in t
                .keys()
                .filter(|k| find_key(&format!("{section}.{k}")).is_none())
            {
                out.push(format!("unknown key {section}.{key}"));
            }
        }
    }
    out
}

/// The machine's hostname up to the first dot, for the default peer name.
pub fn hostname() -> String {
    let uname = rustix::system::uname();
    let full = uname.nodename().to_string_lossy().into_owned();
    let short = full.split('.').next().unwrap_or("").to_string();
    if validate_peer_name(&short).is_ok() {
        short
    } else {
        "ferry".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<(Config, Vec<String>), StoreError> {
        Config::parse(text, Path::new("/x/config.toml"))
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        let (c, w) = parse("").unwrap();
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
        assert_eq!(c.peer.port, 4243);
        assert_eq!(c.input.capture, CaptureMode::Auto);
    }

    #[test]
    fn toasts_are_on_unless_the_notify_section_turns_them_off() {
        let (c, w) = parse("").unwrap();
        assert!(c.notify.enabled && w.is_empty());
        let (c, w) = parse("[notify]\nenabled = false\n").unwrap();
        assert!(!c.notify.enabled && w.is_empty());
        assert!(parse("[notify]\nenabled = \"no\"\n").is_err());
        let (_, w) = parse("[notify]\nsound = true\n").unwrap();
        assert_eq!(w, vec!["unknown key notify.sound"]);
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let (c, w) = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
    }

    #[test]
    fn reads_values_and_capture_off() {
        let (c, w) = parse(
            "[peer]\nname = \"laptop\"\nport = 5000\n[input]\ncapture = \"off\"\n\
             [crossing]\nedge_resistance_px = 12\n",
        )
        .unwrap();
        assert!(w.is_empty());
        assert_eq!(c.peer.name, "laptop");
        assert_eq!(c.peer.port, 5000);
        assert_eq!(c.input.capture, CaptureMode::Off);
        assert_eq!(c.input.capture.backend_name(), "off");
        assert_eq!(c.crossing.edge_resistance_px, 12);
        // keys left out keep their defaults
        assert_eq!(c.crossing.corner_dead_zone_px, 8);
    }

    #[test]
    fn unknown_keys_and_sections_warn() {
        let (c, w) =
            parse("[peer]\nnmae = \"x\"\n[future]\na = 1\n[input]\ncapture = \"portal\"\n")
                .unwrap();
        assert_eq!(c.input.capture, CaptureMode::Portal);
        assert_eq!(w, vec!["unknown section [future]", "unknown key peer.nmae"]);
    }

    #[test]
    fn the_lock_budget_is_read_and_clamped_with_a_warning() {
        let (c, w) = parse("").unwrap();
        assert_eq!(c.lock_check_budget(), LockBudget::DEFAULT);
        assert!(w.is_empty());
        let (c, w) = parse("[safety]\nlock_check_budget_ms = 100\n").unwrap();
        assert_eq!(c.lock_check_budget().millis(), 100);
        assert!(w.is_empty());
        assert_eq!(
            c.source_config(0).unwrap().lock_check_budget,
            c.lock_check_budget()
        );
        let (c, w) = parse("[safety]\nlock_check_budget_ms = 250\n").unwrap();
        assert_eq!(c.safety.lock_check_budget_ms, 100);
        assert_eq!(
            w,
            vec!["[safety] lock_check_budget_ms = 250 is outside 50..=100: using 100"]
        );
        let (c, w) = parse("[safety]\nlock_check_budget_ms = 10\n").unwrap();
        assert_eq!(c.lock_check_budget(), LockBudget::DEFAULT);
        assert_eq!(w.len(), 1);
        // A wrong type is still an error.
        assert!(parse("[safety]\nlock_check_budget_ms = \"x\"\n").is_err());
    }

    /// The key list is the structs' fields, no more and no fewer, and each key reads its own
    /// field: a field added to a struct without a key (or the reverse) fails here.
    #[test]
    fn the_key_list_matches_the_structs() {
        let json = serde_json::to_value(Config::default()).unwrap();
        let mut fields = Vec::new();
        for (section, t) in json.as_object().unwrap() {
            for name in t.as_object().unwrap().keys() {
                fields.push(format!("{section}.{name}"));
            }
        }
        let mut listed: Vec<String> = KEYS.iter().map(|k| k.key.to_string()).collect();
        fields.sort_unstable();
        listed.sort_unstable();
        assert_eq!(listed, fields);
        for k in KEYS {
            let (section, name) = k.parts();
            assert!(!section.is_empty() && !name.is_empty(), "{}", k.key);
            assert_eq!(
                k.value(&Config::default()),
                json[section][name],
                "{} reads another field",
                k.key
            );
        }
        let mut names: Vec<_> = KEYS.iter().map(|k| k.key).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), KEYS.len(), "a key is listed twice");
    }

    /// The schema's ranges are the loader's: the edges of each range load, the first value
    /// outside does not (the clamped budget loads, clamped, and warns).
    #[test]
    fn the_ranges_in_the_list_are_the_ranges_the_loader_enforces() {
        for k in KEYS {
            let Field::Int {
                min,
                max,
                clamped_by_loader,
                ..
            } = k.field
            else {
                continue;
            };
            let (section, name) = k.parts();
            let load = |v: i64| parse(&format!("[{section}]\n{name} = {v}\n"));
            assert!(load(min).is_ok() && load(max).is_ok(), "{}", k.key);
            for outside in [min.checked_sub(1), max.checked_add(1)]
                .into_iter()
                .flatten()
            {
                let r = load(outside);
                if clamped_by_loader {
                    let (_, warnings) = r.unwrap_or_else(|e| panic!("{}: {e}", k.key));
                    assert_eq!(warnings.len(), 1, "{}", k.key);
                } else {
                    // a value the field's own type cannot hold is refused by the parser, one
                    // inside the type but outside the range by `validate`: both name the key
                    let e = r.expect_err(k.key).to_string();
                    assert!(e.contains(name), "{}: {e}", k.key);
                }
            }
        }
    }

    #[test]
    fn bad_values_name_the_file() {
        for (text, needle) in [
            ("[peer]\nport = \"x\"\n", "port"),
            ("[peer]\nport = 70000\n", "port"),
            ("[input]\ncapture = \"telepathy\"\n", "telepathy"),
            ("[peer]\nname = \"a b\"\n", "[peer] name"),
            (
                "[crossing]\nedge_resistance_px = 201\n",
                "edge_resistance_px",
            ),
            ("[crossing\n", "TOML"),
        ] {
            let e = parse(text).unwrap_err().to_string();
            assert!(e.contains("/x/config.toml"), "{e}");
            assert!(e.contains(needle) || needle == "TOML", "{needle}: {e}");
        }
    }

    #[test]
    fn the_escape_chord_is_parsed_and_checked() {
        let (c, _) = parse("").unwrap();
        let sc = c.source_config(550).unwrap();
        assert_eq!(sc.escape_chord, EscapeChord::default());
        assert_eq!(sc.repeat_hold_after_ms, 550);
        assert_eq!(sc.corner_dead_zone_px, 8);
        let (c, _) = parse("[crossing]\nescape_chord = \"Super+Alt+F12\"\n").unwrap();
        assert_eq!(
            c.source_config(0).unwrap().escape_chord.to_string(),
            "Super+Alt+F12"
        );
        let e = parse("[crossing]\nescape_chord = \"Ctrl+Escape\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("escape_chord") && e.contains("2 modifiers"),
            "{e}"
        );
    }

    #[test]
    fn peer_names() {
        assert!(validate_peer_name("desk").is_ok());
        assert!(validate_peer_name("net-book_2").is_ok());
        for bad in ["", "a b", "a:b", "a\tb", &"x".repeat(65)] {
            assert!(validate_peer_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn hostname_is_a_valid_name() {
        assert!(validate_peer_name(&hostname()).is_ok());
    }

    use proptest::prelude::*;

    /// A valid file with a few characters replaced and maybe a cut tail: the damage a disk, an
    /// editor or a half-finished write does.
    fn mangled(valid: String) -> impl Strategy<Value = String> {
        let n = valid.chars().count().max(1);
        (
            proptest::collection::vec((0..n, any::<char>()), 0..6),
            proptest::option::of(0..=n),
        )
            .prop_map(move |(edits, cut)| {
                let mut chars: Vec<char> = valid.chars().collect();
                for (i, c) in edits {
                    if let Some(slot) = chars.get_mut(i) {
                        *slot = c;
                    }
                }
                if let Some(cut) = cut {
                    chars.truncate(cut);
                }
                chars.into_iter().collect()
            })
    }

    fn arb_config() -> impl Strategy<Value = Config> {
        (
            "[a-zA-Z0-9._-]{0,20}",
            any::<u16>(),
            (0u16..=200, any::<u16>(), 0u16..=200, any::<bool>()),
            (
                proptest::sample::select(vec![
                    CaptureMode::Auto,
                    CaptureMode::Portal,
                    CaptureMode::Direct,
                    CaptureMode::LayerShell,
                    CaptureMode::Off,
                ]),
                any::<bool>(),
            ),
            (
                any::<bool>(),
                0..=u64::try_from(i64::MAX).unwrap(),
                any::<bool>(),
                any::<bool>(),
            ),
            LockBudget::MIN_MS..=LockBudget::MAX_MS,
            any::<bool>(),
        )
            .prop_map(
                |(
                    name,
                    port,
                    (res, dwell, dead, block),
                    (capture, invert),
                    (cb, max, mdns, adv),
                    budget,
                    notify,
                )| {
                    Config {
                        peer: PeerSection { name, port },
                        crossing: CrossingConfig {
                            edge_resistance_px: res,
                            edge_dwell_ms: dwell,
                            corner_dead_zone_px: dead,
                            block_while_button_held: block,
                            ..CrossingConfig::default()
                        },
                        input: InputSection {
                            capture,
                            scroll_invert: invert,
                            ..InputSection::default()
                        },
                        clipboard: ClipboardSection {
                            enabled: cb,
                            max_bytes: max,
                        },
                        discovery: DiscoverySection {
                            mdns,
                            advertise: adv,
                        },
                        safety: SafetySection {
                            lock_check_budget_ms: budget,
                        },
                        notify: NotifySection { enabled: notify },
                    }
                },
            )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// A config the daemon could have written reads back as written, with no warning.
        #[test]
        fn a_valid_config_parses_back_without_warnings(c in arb_config()) {
            let text = toml::to_string(&c).unwrap();
            let (back, warnings) = parse(&text).unwrap();
            prop_assert_eq!(back, c);
            prop_assert!(warnings.is_empty(), "{warnings:?}");
        }

        /// `config.toml` is edited by hand: whatever it holds, parse answers `Ok` with a config
        /// that passes `validate`, or an `Err`.
        #[test]
        fn config_parse_never_panics_and_only_returns_valid_configs(
            raw in "\\PC{0,300}",
            damaged in arb_config().prop_flat_map(|c| mangled(toml::to_string(&c).unwrap())),
        ) {
            for text in [raw, damaged] {
                if let Ok((c, _)) = parse(&text) {
                    prop_assert!(c.validate().is_ok());
                    prop_assert!(c.source_config(500).is_ok());
                    let ms = c.safety.lock_check_budget_ms;
                    prop_assert!((LockBudget::MIN_MS..=LockBudget::MAX_MS).contains(&ms));
                }
            }
        }
    }
}
