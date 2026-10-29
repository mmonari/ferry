//! The command line (FR-CLI-01..06).

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use ferry_core::api::{AlignName, AlignParam, Topic, UiView};

use crate::fingerprint;

/// Exit codes.
pub mod exit {
    pub const OK: u8 = 0;
    pub const FAILURE: u8 = 1;
    /// What clap uses for a usage error.
    pub const USAGE: u8 = 2;
    pub const UNREACHABLE: u8 = 3;
}

#[derive(Debug, Parser)]
#[command(
    name = "ferry",
    version,
    about = "Share one keyboard and mouse between your machines",
    long_about = "Talks to ferryd over its socket ($XDG_RUNTIME_DIR/ferry/ferryd.sock).\n\
                  Exit codes: 0 success, 1 failure, 2 usage error, 3 ferryd unreachable.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Print the daemon's raw result as JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// This peer, each peer's link, and the current state.
    Status,
    /// Force a release from any state: input goes back to this machine.
    Release,
    /// Move the keyboard and mouse to the machine on that side, or back home, without the
    /// pointer. `ferry setup` binds it to the go chord (Super+Ctrl+Alt) and the arrow keys.
    Go {
        #[arg(value_enum)]
        side: GoTo,
    },
    /// Print this peer's fingerprint, and the line to paste on the other machine.
    Id,
    /// Pin a peer's fingerprint. Safe to repeat: it updates the name and address.
    Trust(TrustArgs),
    /// The shared layout.
    #[command(subcommand)]
    Layout(LayoutCommand),
    /// Lock to this machine: no crossing starts from here until it is off. It survives a
    /// restart, never moves the pointer, and never blocks the escape chord or `ferry release`.
    Lock {
        #[arg(value_enum)]
        to: LockTo,
    },
    /// Share the clipboard with the other machines, or stop. The same as `ferry config set
    /// clipboard.enabled`; it applies at once.
    Clipboard {
        #[arg(value_enum)]
        to: LockTo,
    },
    /// Pair with another machine: both show a 6-digit code, and you confirm on both.
    Pair(PairArgs),
    /// The pinned peers: how each was paired, where it is dialled, when it was last seen.
    Peers,
    /// Remove a peer's pin. Its connection closes at once, and it is refused from then on.
    Unpair(UnpairArgs),
    /// Check this machine: ferryd, its unit, Hyprland, the portal, the escape bind, the keys, the
    /// firewall and each peer. Prints PASS, WARN or FAIL with a fix for each; exits 1 on a FAIL.
    Doctor,
    /// Set this machine up: the bar plugin, the user unit, the escape bind and the other Hyprland
    /// settings, the firewall. Each step shows what it will change and asks once; a second run
    /// changes nothing.
    Setup(SetupArgs),
    /// Open a view of ferry in the Omarchy shell: the Arrange, Pair or Settings panel, or the
    /// popover.
    ///
    /// It asks the running shell, so it works from a launcher entry or a Hyprland bind as well as
    /// from a terminal; `omarchy-shell ferry arrange` does the same by hand. It needs the ferry
    /// plugin, which `ferry setup` links and enables.
    Ui(UiArgs),
    /// Read and change the settings in config.toml through ferryd, which checks every value.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Print ferryd's events as they happen, one JSON line each, until Ctrl+C. For people who
    /// write a frontend: the first line is ferryd's hello, then one snapshot of each topic, then
    /// one line per change.
    Events(EventsArgs),
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// The saved value of KEY, or of every key. A change that waits for a restart is marked.
    Get {
        /// A dotted key such as crossing.edge_resistance_px; `ferry config schema` lists them.
        key: Option<String>,
    },
    /// Check VALUE, save it to config.toml (comments and order stay) and apply it if the key
    /// applies at once. Exits 1 with the allowed values when VALUE is refused.
    Set {
        key: String,
        /// A number, true or false, or text, as the key's type wants.
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Every key: its type, allowed values, default, and when a change takes effect.
    Schema,
}

#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Only this topic (state, peers, layout, pairing, config, notice, ui). Repeat for several;
    /// without any, every topic.
    #[arg(long = "topic", short = 't', value_name = "TOPIC", value_parser = parse_topic)]
    pub topics: Vec<Topic>,
    /// Stop after this many events (the hello does not count). For scripts: `-t state -n 1` is
    /// one snapshot of the state.
    #[arg(long, short = 'n', value_name = "N")]
    pub count: Option<usize>,
}

fn parse_topic(s: &str) -> Result<Topic, String> {
    s.parse::<Topic>().map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GoTo {
    Left,
    Right,
    Up,
    Down,
}

impl GoTo {
    pub fn side(self) -> ferry_core::api::GoSide {
        use ferry_core::api::GoSide;
        match self {
            GoTo::Left => GoSide::Left,
            GoTo::Right => GoSide::Right,
            GoTo::Up => GoSide::Up,
            GoTo::Down => GoSide::Down,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LockTo {
    On,
    Off,
    Toggle,
}

impl LockTo {
    /// The `lock` method's `on`: absent toggles.
    pub fn param(self) -> Option<bool> {
        match self {
            LockTo::On => Some(true),
            LockTo::Off => Some(false),
            LockTo::Toggle => None,
        }
    }
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("who").required(true).multiple(false).args(["host", "accept"])))]
pub struct PairArgs {
    /// The other machine: HOST or HOST:PORT (port 4243 by default). Run `ferry pair --accept`
    /// there first.
    #[arg(value_name = "HOST[:PORT]")]
    pub host: Option<String>,
    /// Accept one pairing request on this machine, for 120 s.
    #[arg(long)]
    pub accept: bool,
    /// The name to pin the other machine under (default: the name it gives).
    #[arg(long, conflicts_with = "accept")]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct UnpairArgs {
    /// The peer: its name, or at least 8 digits of its fingerprint.
    pub peer: String,
    /// Do not ask first.
    #[arg(long, short)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Answer yes to every step except the ones that need sudo.
    #[arg(long)]
    pub yes: bool,
    /// Show every diff and command and change nothing.
    #[arg(long, conflicts_with = "undo")]
    pub dry_run: bool,
    /// Take back what setup did: the plugin link and its entry on the bar, the unit it wrote, its
    /// blocks in ferry.lua, the require line.
    #[arg(long)]
    pub undo: bool,
}

#[derive(Debug, Args)]
pub struct UiArgs {
    /// arrange, pair, settings or popover.
    #[arg(value_parser = parse_view, value_name = "VIEW")]
    pub view: UiView,
}

fn parse_view(s: &str) -> Result<UiView, String> {
    s.parse::<UiView>()
}

#[derive(Debug, Args)]
pub struct TrustArgs {
    /// The peer's fingerprint, as `ferry id` prints it there (64 hex digits; any case; ':', '-'
    /// or spaces between digits and a `SHA256:` prefix are fine).
    #[arg(value_parser = parse_fingerprint)]
    pub fingerprint: String,
    /// A name for the peer, used by `ferry layout` (default: peer-<first 8 digits>).
    #[arg(long)]
    pub name: Option<String>,
    /// Where to reach it, IP[:PORT] (port 4243 by default). Without it this machine only
    /// accepts the peer's connection.
    #[arg(long, value_name = "IP[:PORT]")]
    pub addr: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum LayoutCommand {
    /// Print the layout: peers, their monitors on the plane, and the links between them.
    #[command(alias = "get")]
    Show,
    /// Put a peer's monitors next to an anchor.
    Place(PlaceArgs),
    /// Put a peer's monitors at an origin on the plane.
    ///
    /// X and Y are in logical pixels, as `ferry layout show` prints an origin. The change is
    /// refused when the monitors would cover another peer's.
    Set(SetArgs),
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// The peer to move: its name, or at least 8 digits of its fingerprint.
    pub peer: String,
    /// The origin's x, as `ferry layout show` prints it. May be negative.
    #[arg(allow_negative_numbers = true)]
    pub x: i32,
    /// The origin's y. May be negative.
    #[arg(allow_negative_numbers = true)]
    pub y: i32,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("side").required(true).multiple(false).args(["below", "above", "left_of", "right_of"])))]
pub struct PlaceArgs {
    /// The peer to place: its name, or at least 8 digits of its fingerprint.
    pub peer: String,
    /// Put the peer below the anchor.
    #[arg(long, value_name = "ANCHOR")]
    pub below: Option<String>,
    /// Put the peer above the anchor.
    #[arg(long, value_name = "ANCHOR")]
    pub above: Option<String>,
    /// Put the peer left of the anchor.
    #[arg(long = "left-of", value_name = "ANCHOR")]
    pub left_of: Option<String>,
    /// Put the peer right of the anchor.
    #[arg(long = "right-of", value_name = "ANCHOR")]
    pub right_of: Option<String>,
    /// Where along the shared edge: start, center, end, or a pixel offset from the start.
    #[arg(long, value_parser = parse_align, allow_negative_numbers = true, value_name = "start|center|end|PX")]
    pub align: Option<AlignParam>,
}

impl PlaceArgs {
    /// The side flag that was given, and its anchor (a peer, a monitor, or `<peer>:<monitor>`).
    pub fn side(&self) -> (ferry_core::geom::Rel, &str) {
        use ferry_core::geom::Rel;
        match (&self.below, &self.above, &self.left_of, &self.right_of) {
            (Some(a), _, _, _) => (Rel::Below, a),
            (_, Some(a), _, _) => (Rel::Above, a),
            (_, _, Some(a), _) => (Rel::LeftOf, a),
            (_, _, _, Some(a)) => (Rel::RightOf, a),
            // clap's required group makes this unreachable
            _ => unreachable!("clap requires one of --below, --above, --left-of, --right-of"),
        }
    }
}

fn parse_fingerprint(s: &str) -> Result<String, String> {
    fingerprint::normalize(s).map_err(|e| e.to_string())
}

fn parse_align(s: &str) -> Result<AlignParam, String> {
    match s {
        "start" => Ok(AlignParam::Named(AlignName::Start)),
        "center" => Ok(AlignParam::Named(AlignName::Center)),
        "end" => Ok(AlignParam::Named(AlignName::End)),
        px => px
            .parse::<i32>()
            .map(AlignParam::Px)
            .map_err(|_| format!("expected start, center, end or a pixel offset, got {s:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_core::geom::Rel;

    const FP: &str = "4b1f0a9c2e7d3b5a6c8e1f0a9c2e7d3b5a6c8e1f0a9c2e7d3b5a6c8e1f0a9c2e";

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("ferry").chain(args.iter().copied()))
    }

    #[test]
    fn the_command_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn commands_without_arguments_parse_and_layout_get_aliases_show() {
        assert!(matches!(
            parse(&["status"]).unwrap().command,
            Command::Status
        ));
        assert!(matches!(
            parse(&["release"]).unwrap().command,
            Command::Release
        ));
        assert!(matches!(parse(&["id"]).unwrap().command, Command::Id));
        assert!(matches!(
            parse(&["go", "up"]).unwrap().command,
            Command::Go { side: GoTo::Up }
        ));
        assert!(parse(&["go", "north"]).is_err());
        assert!(matches!(
            parse(&["layout", "show"]).unwrap().command,
            Command::Layout(LayoutCommand::Show)
        ));
        assert!(matches!(
            parse(&["layout", "get"]).unwrap().command,
            Command::Layout(LayoutCommand::Show)
        ));
    }

    #[test]
    fn setup_has_no_flags_by_default_and_refuses_undo_with_dry_run() {
        let Command::Setup(a) = parse(&["setup"]).unwrap().command else {
            panic!()
        };
        assert!(!a.yes && !a.dry_run && !a.undo);
        let Command::Setup(a) = parse(&["setup", "--yes", "--dry-run"]).unwrap().command else {
            panic!()
        };
        assert!(a.yes && a.dry_run);
        assert!(parse(&["setup", "--undo", "--dry-run"]).is_err());
    }

    #[test]
    fn ui_takes_one_of_the_four_views() {
        for (word, view) in [
            ("arrange", UiView::Arrange),
            ("pair", UiView::Pair),
            ("settings", UiView::Settings),
            ("popover", UiView::Popover),
        ] {
            let Command::Ui(u) = parse(&["ui", word]).unwrap().command else {
                panic!()
            };
            assert_eq!(u.view, view, "{word}");
        }
        for bad in [&["ui"][..], &["ui", "arrange", "pair"], &["ui", "close"]] {
            assert_eq!(
                parse(bad).unwrap_err().exit_code(),
                i32::from(exit::USAGE),
                "{bad:?}"
            );
        }
        let e = parse(&["ui", "wallpaper"]).unwrap_err().to_string();
        for view in ["arrange", "pair", "settings", "popover"] {
            assert!(e.contains(view), "{e}");
        }
        assert!(parse(&["--json", "ui", "pair"]).unwrap().json);
    }

    #[test]
    fn json_is_global() {
        assert!(parse(&["--json", "status"]).unwrap().json);
        assert!(parse(&["status", "--json"]).unwrap().json);
        assert!(parse(&["layout", "show", "--json"]).unwrap().json);
        assert!(!parse(&["status"]).unwrap().json);
    }

    #[test]
    fn trust_takes_a_fingerprint_name_and_addr() {
        let Command::Trust(t) = parse(&["trust", FP, "--name", "laptop", "--addr", "192.0.2.11"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(t.fingerprint, FP);
        assert_eq!(t.name.as_deref(), Some("laptop"));
        assert_eq!(t.addr.as_deref(), Some("192.0.2.11"));
        let Command::Trust(t) = parse(&["trust", FP]).unwrap().command else {
            panic!()
        };
        assert_eq!((t.name, t.addr), (None, None));
    }

    #[test]
    fn trust_normalises_a_pasted_fingerprint() {
        let pasted = format!("SHA256:{}", FP.to_uppercase());
        let Command::Trust(t) = parse(&["trust", &pasted]).unwrap().command else {
            panic!()
        };
        assert_eq!(t.fingerprint, FP);
    }

    #[test]
    fn trust_rejects_a_bad_fingerprint_as_a_usage_error() {
        let e = parse(&["trust", "abc"]).unwrap_err();
        assert_eq!(e.exit_code(), i32::from(exit::USAGE));
        assert!(e.to_string().contains("64 hex digits"), "{e}");
        assert!(parse(&["trust"]).is_err());
    }

    #[test]
    fn place_takes_exactly_one_side() {
        for (flag, rel) in [
            ("--below", Rel::Below),
            ("--above", Rel::Above),
            ("--left-of", Rel::LeftOf),
            ("--right-of", Rel::RightOf),
        ] {
            let Command::Layout(LayoutCommand::Place(p)) =
                parse(&["layout", "place", "laptop", flag, "HDMI-A-1"])
                    .unwrap()
                    .command
            else {
                panic!()
            };
            assert_eq!(p.peer, "laptop");
            assert_eq!(p.side(), (rel, "HDMI-A-1"));
            assert_eq!(p.align, None);
        }
        assert!(parse(&["layout", "place", "laptop"]).is_err());
        assert!(parse(&["layout", "place", "laptop", "--below", "a", "--above", "b"]).is_err());
        assert!(parse(&["layout", "place", "--below", "a"]).is_err());
    }

    #[test]
    fn place_anchors_may_be_peers_or_monitors() {
        let Command::Layout(LayoutCommand::Place(p)) =
            parse(&["layout", "place", "lap", "--right-of", "desk:DVI-D-1"])
                .unwrap()
                .command
        else {
            panic!()
        };
        assert_eq!(p.side(), (Rel::RightOf, "desk:DVI-D-1"));
    }

    #[test]
    fn set_takes_a_peer_and_two_signed_pixels() {
        let set = |args: &[&str]| {
            let Command::Layout(LayoutCommand::Set(s)) = parse(args).unwrap().command else {
                panic!()
            };
            (s.peer, s.x, s.y)
        };
        assert_eq!(
            set(&["layout", "set", "lap", "2560", "0"]),
            ("lap".to_string(), 2560, 0)
        );
        assert_eq!(
            set(&["layout", "set", "lap", "-1366", "-20"]),
            ("lap".to_string(), -1366, -20)
        );
        assert_eq!(
            set(&["layout", "set", "4b1f0a9c", "0", "1080", "--json"]),
            ("4b1f0a9c".to_string(), 0, 1080)
        );
        assert!(parse(&["layout", "set", "lap", "1", "2", "3"]).is_err());
        assert!(parse(&["layout", "set", "lap", "2147483648", "0"]).is_err());
    }

    #[test]
    fn layout_place_align_takes_a_name_or_signed_pixels_and_refuses_others() {
        let align = |a: &str| {
            let Command::Layout(LayoutCommand::Place(p)) =
                parse(&["layout", "place", "m", "--below", "x", "--align", a])
                    .unwrap()
                    .command
            else {
                panic!()
            };
            p.align.unwrap()
        };
        assert_eq!(align("start"), AlignParam::Named(AlignName::Start));
        assert_eq!(align("center"), AlignParam::Named(AlignName::Center));
        assert_eq!(align("end"), AlignParam::Named(AlignName::End));
        assert_eq!(align("120"), AlignParam::Px(120));
        assert_eq!(align("-40"), AlignParam::Px(-40));
        assert!(parse(&["layout", "place", "m", "--below", "x", "--align", "middle"]).is_err());
    }

    #[test]
    fn pair_takes_a_host_or_accept() {
        let Command::Pair(p) = parse(&["pair", "192.0.2.11", "--name", "laptop"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(p.host.as_deref(), Some("192.0.2.11"));
        assert_eq!(p.name.as_deref(), Some("laptop"));
        assert!(!p.accept);
        let Command::Pair(p) = parse(&["pair", "--accept"]).unwrap().command else {
            panic!()
        };
        assert!(p.accept && p.host.is_none());
        for bad in [
            &["pair"][..],
            &["pair", "h", "--accept"],
            &["pair", "--accept", "--name", "x"],
        ] {
            assert_eq!(
                parse(bad).unwrap_err().exit_code(),
                i32::from(exit::USAGE),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn peers_and_unpair() {
        assert!(matches!(parse(&["peers"]).unwrap().command, Command::Peers));
        let Command::Unpair(u) = parse(&["unpair", "laptop", "--yes"]).unwrap().command else {
            panic!()
        };
        assert_eq!((u.peer.as_str(), u.yes), ("laptop", true));
        let Command::Unpair(u) = parse(&["unpair", "4b1f0c22"]).unwrap().command else {
            panic!()
        };
        assert!(!u.yes);
        assert!(parse(&["unpair"]).is_err());
    }

    #[test]
    fn config_and_events_parse() {
        let Command::Config(ConfigCommand::Get { key }) =
            parse(&["config", "get"]).unwrap().command
        else {
            panic!()
        };
        assert_eq!(key, None);
        let Command::Config(ConfigCommand::Get { key }) =
            parse(&["config", "get", "peer.name"]).unwrap().command
        else {
            panic!()
        };
        assert_eq!(key.as_deref(), Some("peer.name"));
        let Command::Config(ConfigCommand::Set { key, value }) =
            parse(&["config", "set", "crossing.edge_resistance_px", "40"])
                .unwrap()
                .command
        else {
            panic!()
        };
        assert_eq!(
            (key.as_str(), value.as_str()),
            ("crossing.edge_resistance_px", "40")
        );
        // a value may start with a dash
        assert!(parse(&["config", "set", "crossing.edge_dwell_ms", "-5"]).is_ok());
        assert!(matches!(
            parse(&["config", "schema"]).unwrap().command,
            Command::Config(ConfigCommand::Schema)
        ));
        assert!(parse(&["config", "set", "peer.name"]).is_err());
        assert!(parse(&["config"]).is_err());

        let Command::Events(e) = parse(&["events"]).unwrap().command else {
            panic!()
        };
        assert!(e.topics.is_empty() && e.count.is_none());
        let Command::Events(e) = parse(&["events", "-t", "state", "--topic", "ui", "-n", "3"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(e.topics, [Topic::State, Topic::Ui]);
        assert_eq!(e.count, Some(3));
        let err = parse(&["events", "-t", "fax"]).unwrap_err();
        assert_eq!(err.exit_code(), i32::from(exit::USAGE));
        assert!(err.to_string().contains("pairing"), "{err}");
    }

    #[test]
    fn unknown_things_are_usage_errors() {
        for args in [
            &["frobnicate"][..],
            &["status", "--wat"],
            &["layout"],
            &["layout", "set", "a", "1"],
            &["layout", "set", "a", "1", "two"],
            &["layout", "set", "a", "1.5", "2"],
        ] {
            let e = parse(args).unwrap_err();
            assert_eq!(e.exit_code(), i32::from(exit::USAGE), "{args:?}");
        }
    }
}
