//! `ferry setup` against a temp HOME, a fake Hyprland IPC socket and two scripts standing in
//! for `systemctl` and `sudo`. Nothing here touches the real Hyprland,
//! `~/.config`, systemd or the firewall.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

use std::io::Cursor;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod support;

use ferry_cli::setup::lua::{self, Block};
use ferry_cli::setup::{self, Env};
use support::script;

const SIG: &str = "fakesig";

/// What the user saves to `hyprland.lua` while Hyprland is loading the edit.
const EDITED_MEANWHILE: &str = "-- edited meanwhile\n";

/// An Omarchy-shaped `hyprland.lua`.
const HYPRLAND_LUA: &str = "-- Learn how to configure Hyprland\n\
    require(\"default.hypr.omarchy\")\n\
    require(\"hypr.monitors\")\n\
    require(\"hypr.bindings\")\n\
    \n\
    -- Add any other personal Hyprland configuration below.\n";

/// The shell's default `shell.json` (`config/omarchy/shell.json` of the omarchy package, cut to the
/// widgets that matter here), in the order the shell writes its keys.
const DEFAULT_SHELL_JSON: &str = r#"{
  "version": 1,
  "idle": {
    "screensaver": 150,
    "lock": 300
  },
  "bar": {
    "position": "top",
    "background": "opaque",
    "layout": {
      "left": [
        {
          "id": "omarchy.menu"
        },
        {
          "id": "omarchy.workspaces"
        }
      ],
      "centerBegin": [
        {
          "id": "omarchy.indicators"
        }
      ],
      "center": [
        {
          "id": "omarchy.clock",
          "format": "dddd HH:mm",
          "verticalFormat": "HH\n\u2014\nmm"
        }
      ],
      "centerEnd": [
        {
          "id": "omarchy.weather"
        }
      ],
      "right": [
        {
          "id": "omarchy.tray"
        },
        {
          "id": "omarchy.keyboard-layout"
        },
        {
          "id": "omarchy.audio"
        },
        {
          "id": "omarchy.power"
        }
      ]
    }
  },
  "dock": {
    "position": "bottom",
    "pinned": [
      "org.gnome.Nautilus",
      "chromium"
    ]
  },
  "plugins": []
}
"#;

/// The plugin's manifest, as the package ships it: setup reads `barWidget.defaultSection` from it.
const PLUGIN_MANIFEST: &str = include_str!("../../../plugin/manifest.json");

#[derive(Default)]
struct Snap {
    bind: bool,
    go: bool,
    option: bool,
    errors: String,
}

/// What the fake compositor does with `ferry.lua`: it "loads" it the way Hyprland would, from the
/// files, when `hyprland.lua` requires it.
struct Knobs {
    hypr_dir: PathBuf,
    enforce: AtomicBool,
    user_bind: AtomicBool,
    autoreload: AtomicBool,
    reject: AtomicBool,
    /// Once `ferry.lua` makes the fake report errors, the directory goes read-only: the rollback
    /// that follows cannot write.
    lock_dir_on_error: AtomicBool,
    /// Once, when it first reports errors, somebody saves a line to `hyprland.lua`.
    edit_hypr_on_error: AtomicBool,
    reloads: AtomicU32,
    snap: Mutex<Snap>,
}

impl Knobs {
    fn compute(&self) -> Snap {
        let hl = std::fs::read_to_string(self.hypr_dir.join("hyprland.lua")).unwrap_or_default();
        let ferry = std::fs::read_to_string(self.hypr_dir.join("ferry.lua")).ok();
        let loaded = lua::requires_ferry(&hl) && ferry.is_some();
        let has = |b| loaded && ferry.as_deref().is_some_and(|t| lua::has_block(t, b));
        let any_block = [
            Block::EscapeBind,
            Block::GoBinds,
            Block::ReleasePressed,
            Block::Permission,
        ]
        .into_iter()
        .any(has);
        Snap {
            bind: has(Block::EscapeBind),
            go: has(Block::GoBinds),
            option: has(Block::ReleasePressed),
            errors: if any_block && self.reject.load(Ordering::SeqCst) {
                "ferry.lua:2: the fake Hyprland dislikes this".into()
            } else {
                String::new()
            },
        }
    }

    fn refresh(&self) {
        *self.snap.lock().unwrap() = self.compute();
    }

    fn answer(&self, cmd: &str) -> String {
        if self.autoreload.load(Ordering::SeqCst) {
            self.refresh();
        }
        let snap = self.snap.lock().unwrap();
        match cmd.trim() {
            "j/binds" => {
                let mut binds = vec![
                    serde_json::json!({"key":"RETURN","modmask":64,"allow_input_capture":false,"dispatcher":"__lua"}),
                ];
                if snap.bind || self.user_bind.load(Ordering::SeqCst) {
                    binds.push(serde_json::json!({"key":"ESCAPE","modmask":68,"allow_input_capture":true,"dispatcher":"__lua"}));
                }
                if snap.go {
                    for side in ["left", "right", "up", "down"] {
                        binds.push(serde_json::json!({"key":side.to_uppercase(),"modmask":76,"allow_input_capture":false,"dispatcher":"__lua","description":format!("ferry: go {side}")}));
                    }
                }
                serde_json::Value::Array(binds).to_string()
            }
            "j/getoption input:virtualkeyboard:release_pressed_on_close" => {
                format!(r#"{{"option":"x","bool":{0},"set":{0}}}"#, snap.option)
            }
            "j/getoption ecosystem:enforce_permissions" => {
                let on = self.enforce.load(Ordering::SeqCst);
                format!(r#"{{"option":"x","bool":{on},"set":{on}}}"#)
            }
            "configerrors" => {
                if !snap.errors.is_empty() {
                    if self.edit_hypr_on_error.swap(false, Ordering::SeqCst) {
                        use std::io::Write;
                        let mut f = std::fs::OpenOptions::new()
                            .append(true)
                            .open(self.hypr_dir.join("hyprland.lua"))
                            .unwrap();
                        f.write_all(EDITED_MEANWHILE.as_bytes()).unwrap();
                    }
                    if self.lock_dir_on_error.load(Ordering::SeqCst) {
                        std::fs::set_permissions(
                            &self.hypr_dir,
                            std::fs::Permissions::from_mode(0o555),
                        )
                        .unwrap();
                    }
                }
                snap.errors.clone()
            }
            "reload" => {
                drop(snap);
                self.reloads.fetch_add(1, Ordering::SeqCst);
                self.refresh();
                "ok".into()
            }
            other => format!("unknown request {other}"),
        }
    }
}

struct Sandbox {
    tmp: tempfile::TempDir,
    home: PathBuf,
    env: Env,
    knobs: Arc<Knobs>,
    state: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        Self::build(true)
    }

    /// `hyprland: false` leaves no compositor socket behind.
    fn build(hyprland: bool) -> Self {
        let tmp = tempfile::Builder::new().prefix("fs").tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        let hypr_dir = home.join(".config/hypr");
        std::fs::create_dir_all(&hypr_dir).unwrap();
        std::fs::write(hypr_dir.join("hyprland.lua"), HYPRLAND_LUA).unwrap();
        let run = root.join("run");
        let sock_dir = run.join("hypr").join(SIG);
        std::fs::create_dir_all(&sock_dir).unwrap();
        let knobs = Arc::new(Knobs {
            hypr_dir: hypr_dir.clone(),
            enforce: AtomicBool::new(false),
            user_bind: AtomicBool::new(false),
            autoreload: AtomicBool::new(true),
            reject: AtomicBool::new(false),
            lock_dir_on_error: AtomicBool::new(false),
            edit_hypr_on_error: AtomicBool::new(false),
            reloads: AtomicU32::new(0),
            snap: Mutex::new(Snap::default()),
        });
        if hyprland {
            let listener = UnixListener::bind(sock_dir.join(".socket.sock")).unwrap();
            let k = knobs.clone();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                for conn in listener.incoming() {
                    let Ok(mut c) = conn else { continue };
                    let mut buf = [0u8; 512];
                    let n = c.read(&mut buf).unwrap_or(0);
                    let reply = k.answer(&String::from_utf8_lossy(&buf[..n]));
                    let _ = c.write_all(reply.as_bytes());
                }
            });
        }
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let state = root.join("sysstate");
        std::fs::create_dir_all(&state).unwrap();
        let s = state.display();
        script(
            &bin.join("systemctl"),
            &format!(
                "echo \"systemctl $*\" >> {s}/log\nshift\ncase \"$1\" in\n\
                 is-enabled) if [ -f {s}/enabled ]; then echo enabled; else echo disabled; exit 1; fi;;\n\
                 is-active) if [ -f {s}/active ]; then echo active; else echo inactive; exit 3; fi;;\n\
                 show) if [ -f {s}/enabled ]; then echo UnitFileState=enabled; else echo UnitFileState=disabled; fi; \
                 if [ -f {s}/active ]; then echo ActiveState=active; echo SubState=running; else echo ActiveState=inactive; echo SubState=dead; fi;;\n\
                 enable) touch {s}/enabled {s}/active;;\n\
                 disable) rm -f {s}/enabled {s}/active;;\n\
                 esac"
            ),
        );
        script(&bin.join("sudo"), &format!("echo \"sudo $*\" >> {s}/log"));
        script(&bin.join("ferryd"), "exit 0");
        // The package's plugin folder, Omarchy's default shell.json, and the user's copy of it.
        let plugin_source = root.join("usr/share/ferry/plugin");
        std::fs::create_dir_all(&plugin_source).unwrap();
        std::fs::write(plugin_source.join("manifest.json"), PLUGIN_MANIFEST).unwrap();
        let omarchy = root.join("omarchy");
        std::fs::create_dir_all(omarchy.join("config/omarchy")).unwrap();
        std::fs::write(
            omarchy.join("config/omarchy/shell.json"),
            DEFAULT_SHELL_JSON,
        )
        .unwrap();
        let omarchy_dir = home.join(".config/omarchy");
        std::fs::create_dir_all(&omarchy_dir).unwrap();
        std::fs::write(omarchy_dir.join("shell.json"), DEFAULT_SHELL_JSON).unwrap();
        // The shell's command line: it writes down what it was asked, and what the machine looked
        // like at that moment, so a test can tell the order of the link, the rescan and the layout.
        script(
            &bin.join("omarchy-shell"),
            &format!(
                "echo \"omarchy-shell $* [OMARCHY_PATH=$OMARCHY_PATH] link=$([ -L {link} ] && echo yes || echo no) widget=$(grep -c ferry.bar {json} 2>/dev/null)\" >> {s}/log",
                link = omarchy_dir.join("plugins/ferry.bar").display(),
                json = omarchy_dir.join("shell.json").display(),
            ),
        );
        let ufw = root.join("ufw.conf");
        std::fs::write(&ufw, "# ufw\nENABLED=yes\n").unwrap();
        let env = Env {
            home: home.clone(),
            config_home: home.join(".config"),
            runtime_dir: Some(run),
            hypr_signature: None,
            data_home: home.join(".local/share"),
            systemctl: bin.join("systemctl"),
            sudo: bin.join("sudo"),
            pacman: bin.join("pacman"),
            busctl: bin.join("busctl"),
            ufw_conf: ufw,
            ufw_profiles: vec![root.join("no-such-profile")],
            packaged_unit: root.join("no-such-packaged-unit"),
            plugin_source,
            omarchy_path: omarchy,
            omarchy_shell: bin.join("omarchy-shell"),
            ferryd: bin.join("ferryd"),
            socket: root.join("no-such-ferryd.sock"),
            interactive: true,
            hypr_wait: Duration::from_millis(300),
            unix_time: 1_800_000_000,
        };
        Self {
            tmp,
            home,
            env,
            knobs,
            state,
        }
    }

    fn hypr_dir(&self) -> PathBuf {
        self.home.join(".config/hypr")
    }

    fn hyprland_lua(&self) -> String {
        std::fs::read_to_string(self.hypr_dir().join("hyprland.lua")).unwrap()
    }

    fn ferry_lua(&self) -> Option<String> {
        std::fs::read_to_string(self.hypr_dir().join("ferry.lua")).ok()
    }

    fn unit_path(&self) -> PathBuf {
        self.home.join(".config/systemd/user/ferryd.service")
    }

    fn systemd_log(&self) -> String {
        std::fs::read_to_string(self.state.join("log")).unwrap_or_default()
    }

    fn omarchy_dir(&self) -> PathBuf {
        self.home.join(".config/omarchy")
    }

    /// `~/.config/omarchy/plugins/ferry.bar`.
    fn plugin_link(&self) -> PathBuf {
        self.omarchy_dir().join("plugins/ferry.bar")
    }

    fn shell_json_path(&self) -> PathBuf {
        self.omarchy_dir().join("shell.json")
    }

    fn shell_json(&self) -> String {
        std::fs::read_to_string(self.shell_json_path()).unwrap()
    }

    fn set_shell_json(&self, text: &str) {
        std::fs::write(self.shell_json_path(), text).unwrap();
    }

    /// What the plugin link is, `None` when there is none.
    fn link_target(&self) -> Option<PathBuf> {
        std::fs::read_link(self.plugin_link()).ok()
    }

    /// The lines of the shell command's log.
    fn shell_calls(&self) -> Vec<String> {
        self.systemd_log()
            .lines()
            .filter(|l| l.starts_with("omarchy-shell "))
            .map(str::to_string)
            .collect()
    }

    /// The `ferry.bar` backups of `shell.json` next to it.
    fn shell_json_backups(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.omarchy_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("shell.json.ferry-bak."))
            .collect();
        names.sort();
        names
    }

    /// Runs `ferry setup <args>`; `input` is what the user types.
    fn setup(&self, args: &[&str], input: &str) -> (u8, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let argv = ["ferry", "setup"].into_iter().chain(args.iter().copied());
        let code = ferry_cli::run_in(
            argv,
            Path::new("/nonexistent/ferryd.sock"),
            &self.env,
            &mut Cursor::new(input.to_string()),
            &mut out,
            &mut err,
        );
        assert!(err.is_empty(), "stderr: {}", String::from_utf8_lossy(&err));
        (code, self.normalise(&String::from_utf8(out).unwrap()))
    }

    /// Paths that depend on the temp dir, made stable for snapshots.
    fn normalise(&self, text: &str) -> String {
        text.replace(self.tmp.path().to_str().unwrap(), "<tmp>")
    }
}

/// The line that tallies the steps (the doctor's summary follows it).
fn summary(out: &str) -> &str {
    out.lines()
        .find(|l| l.contains(" done, ") && l.contains(" skipped, "))
        .unwrap()
}

/// What the first step printed, the bar plugin's.
fn plugin_step(out: &str) -> &str {
    let start = out.find("[1/7] bar plugin").unwrap();
    let end = out.find("\n[2/7]").unwrap();
    &out[start..end]
}

#[test]
fn a_fresh_run_sets_everything_up_and_a_second_run_changes_nothing() {
    let sb = Sandbox::new();
    let before = sb.hyprland_lua();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("fresh_run", out);

    // the unit
    let unit = std::fs::read_to_string(sb.unit_path()).unwrap();
    assert!(
        unit.contains(&format!(
            "ExecStart={}/bin/ferryd\n",
            sb.tmp.path().display()
        )),
        "{unit}"
    );
    let log = sb.systemd_log();
    assert!(log.contains("systemctl --user daemon-reload"), "{log}");
    assert!(
        log.contains("systemctl --user enable --now ferryd"),
        "{log}"
    );
    // the two Hyprland blocks, and the one appended line
    let ferry = sb.ferry_lua().unwrap();
    assert!(lua::is_ours(&ferry));
    assert!(lua::has_block(&ferry, Block::EscapeBind));
    assert!(lua::has_block(&ferry, Block::ReleasePressed));
    assert!(
        !lua::has_block(&ferry, Block::Permission),
        "enforce_permissions is off"
    );
    let hl = sb.hyprland_lua();
    assert!(hl.starts_with(&before), "no line of the user's changed");
    assert!(hl.ends_with("require(\"hypr.ferry\")\n"));
    assert_eq!(lua::strip_require(&hl).as_deref(), Some(before.as_str()));
    // sudo is never run by --yes
    assert!(!sb.systemd_log().contains("sudo"), "{log}");
    // the plugin: linked to the package, the widget on the bar right after the tray, and the
    // shell asked to look again after the link and before the layout names the plugin
    assert_eq!(
        sb.link_target().as_deref(),
        Some(sb.env.plugin_source.as_path())
    );
    // one entry added to the file, right after the tray, and not a byte more
    assert_eq!(
        sb.shell_json(),
        DEFAULT_SHELL_JSON.replacen(
            "        {\n          \"id\": \"omarchy.keyboard-layout\"",
            "        {\n          \"id\": \"ferry.bar\"\n        },\n        {\n          \"id\": \"omarchy.keyboard-layout\"",
            1
        )
    );
    let calls = sb.shell_calls();
    assert_eq!(calls.len(), 1, "{log}");
    assert!(
        calls[0].contains("-q shell rescanPlugins")
            && calls[0].contains(&format!("OMARCHY_PATH={}/omarchy", sb.tmp.path().display()))
            && calls[0].ends_with("link=yes widget=0"),
        "{}",
        calls[0]
    );
    assert_eq!(
        sb.shell_json_backups(),
        ["shell.json.ferry-bak.1800000000"],
        "the user's file is kept"
    );
    assert_eq!(
        std::fs::read_to_string(sb.omarchy_dir().join("shell.json.ferry-bak.1800000000")).unwrap(),
        DEFAULT_SHELL_JSON
    );

    // the second run: every step skipped, no file touched
    let snapshot = (sb.hyprland_lua(), sb.ferry_lua(), unit);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("second_run", out);
    assert!(
        summary(&out).starts_with("0 done, 7 skipped"),
        "{}",
        summary(&out)
    );
    let unit2 = std::fs::read_to_string(sb.unit_path()).unwrap();
    assert_eq!(snapshot, (sb.hyprland_lua(), sb.ferry_lua(), unit2));
    // the plugin step ran no command the second time, wrote nothing, and made no second backup
    assert_eq!(sb.shell_calls().len(), 1, "{}", sb.systemd_log());
    assert_eq!(sb.shell_json_backups().len(), 1);
}

#[test]
fn a_declined_run_leaves_every_file_as_it_was() {
    let sb = Sandbox::new();
    let before = sb.hyprland_lua();
    // plugin n, unit n, bind n, go binds n, option n, firewall n
    let (code, out) = sb.setup(&[], "n\nn\nn\nn\nn\nn\n");
    assert_eq!(code, 0, "{out}");
    assert_eq!(sb.hyprland_lua(), before);
    assert!(sb.ferry_lua().is_none());
    assert!(!sb.unit_path().exists());
    assert!(
        !sb.systemd_log().contains("enable --now"),
        "{}",
        sb.systemd_log()
    );
    assert!(summary(&out).contains("6 declined"), "{}", summary(&out));
    assert!(sb.link_target().is_none() && !sb.plugin_link().exists());
    assert_eq!(sb.shell_json(), DEFAULT_SHELL_JSON);
    assert!(sb.shell_json_backups().is_empty());
    assert!(sb.shell_calls().is_empty(), "{}", sb.systemd_log());
    assert!(
        out.contains("(to do it yourself: link the plugin folder to ~/.config/omarchy/plugins/ferry.bar, then run `omarchy plugin enable ferry.bar --section right`)"),
        "{out}"
    );
    assert!(
        !sb.hypr_dir()
            .join("hyprland.lua.ferry-bak.1800000000")
            .exists()
    );
}

#[test]
fn the_questions_default_to_yes_except_sudo() {
    let sb = Sandbox::new();
    // Enter for the plugin, the unit, the binds and the option; Enter for the firewall is a no.
    let (code, out) = sb.setup(&[], "\n\n\n\n\n\n");
    assert_eq!(code, 0, "{out}");
    assert!(sb.unit_path().exists());
    assert!(sb.link_target().is_some() && sb.shell_json().contains("ferry.bar"));
    assert!(lua::has_block(&sb.ferry_lua().unwrap(), Block::EscapeBind));
    assert!(!sb.systemd_log().contains("sudo"));
    assert!(out.contains("run it? [y/N]"), "{out}");
}

#[test]
fn the_firewall_rule_runs_only_when_the_user_says_yes() {
    let sb = Sandbox::new();
    // answers: plugin n, unit n, bind n, go binds n, option n, firewall y
    let (code, out) = sb.setup(&[], "n\nn\nn\nn\nn\ny\n");
    assert_eq!(code, 0, "{out}");
    assert!(
        sb.systemd_log()
            .contains("sudo ufw allow 4243/udp comment ferry"),
        "{}",
        sb.systemd_log()
    );
}

#[test]
fn undo_puts_every_file_back() {
    let sb = Sandbox::new();
    let before = sb.hyprland_lua();
    sb.setup(&["--yes"], "");
    assert!(sb.ferry_lua().is_some() && sb.unit_path().exists());
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("undo", out);
    assert_eq!(
        sb.hyprland_lua(),
        before,
        "hyprland.lua is byte for byte as it was"
    );
    assert!(sb.ferry_lua().is_none(), "ferry.lua is gone");
    assert!(!sb.unit_path().exists(), "the unit setup wrote is gone");
    assert!(
        sb.systemd_log()
            .contains("systemctl --user disable --now ferryd")
    );
    assert!(!sb.knobs.compute().bind);
    // the plugin: the widget is off the bar and the file is the user's again, byte for byte; the
    // link is gone; the shell was asked to look again once after each of the two runs' changes
    assert_eq!(sb.shell_json(), DEFAULT_SHELL_JSON);
    assert!(sb.link_target().is_none() && !sb.plugin_link().exists());
    let calls = sb.shell_calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[1].ends_with("link=no widget=0"), "{}", calls[1]);
    // a second undo has nothing to do
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        plugin_step(&out).contains("skipped: ferry.bar is not on the bar and setup left no link"),
        "{out}"
    );
    assert_eq!(sb.shell_calls().len(), 2);
}

#[test]
fn a_symlinked_hyprland_lua_stays_a_symlink() {
    let sb = Sandbox::new();
    let dotfiles = sb.tmp.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let real = dotfiles.join("hyprland.lua");
    let link = sb.hypr_dir().join("hyprland.lua");
    std::fs::rename(&link, &real).unwrap();
    symlink(&real, &link).unwrap();
    let before = std::fs::read_to_string(&real).unwrap();

    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("the link points here, the link itself stays"),
        "{out}"
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link was replaced by a file"
    );
    let after = std::fs::read_to_string(&real).unwrap();
    assert!(after.ends_with("require(\"hypr.ferry\")\n") && after.starts_with(&before));

    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), before);
}

#[test]
fn a_foreign_ferry_lua_stops_the_hyprland_steps_and_prints_the_snippet() {
    let sb = Sandbox::new();
    let mine = "-- somebody else's file\nhl.config({})\n";
    std::fs::write(sb.hypr_dir().join("ferry.lua"), mine).unwrap();
    let before = sb.hyprland_lua();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(sb.ferry_lua().as_deref(), Some(mine));
    assert_eq!(sb.hyprland_lua(), before);
    assert!(
        out.contains("did not write it, so it is left alone"),
        "{out}"
    );
    assert!(out.contains("allow_input_capture = true"), "{out}");
    // undo does not touch it either
    sb.setup(&["--undo", "--yes"], "");
    assert_eq!(sb.ferry_lua().as_deref(), Some(mine));
}

#[test]
fn a_bind_the_user_already_has_is_a_skip() {
    let sb = Sandbox::new();
    sb.knobs.user_bind.store(true, Ordering::SeqCst);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let ferry = sb.ferry_lua().unwrap();
    assert!(!lua::has_block(&ferry, Block::EscapeBind), "{ferry}");
    assert!(lua::has_block(&ferry, Block::ReleasePressed));
    assert!(
        out.contains("skipped: Hyprland has a bind with allow_input_capture"),
        "{out}"
    );
}

#[test]
fn enforced_permissions_get_the_permission_line() {
    let sb = Sandbox::new();
    sb.knobs.enforce.store(true, Ordering::SeqCst);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let ferry = sb.ferry_lua().unwrap();
    assert!(ferry.contains(
        r#"hl.permission({ binary = "/usr/lib/xdg-desktop-portal-hyprland", type = "input-capture", mode = "allow" })"#
    ));
    // Hyprland adds hl.permission rules only when it starts: setup says to restart
    assert!(
        out.contains(
            "done: written; Hyprland reads hl.permission only when it starts: log out and back in"
        ),
        "{out}"
    );
    // a second run does not add it again, and says the same of the restart
    let before = ferry;
    let (_, out) = sb.setup(&["--yes"], "");
    assert_eq!(sb.ferry_lua().unwrap(), before);
    assert!(!out.contains("done:"), "{out}");
    assert!(
        out.contains("has the block already; Hyprland reads it only when it starts"),
        "{out}"
    );
    // undo: the rule stays in Hyprland until it restarts
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("done: removed; Hyprland keeps allowing xdph until it restarts"),
        "{out}"
    );
}

#[test]
fn the_bind_follows_the_chord_in_config_toml() {
    let sb = Sandbox::new();
    let cfg = sb.home.join(".config/ferry");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "[crossing]\nescape_chord = \"Ctrl+Alt+Delete\"\n",
    )
    .unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        sb.ferry_lua()
            .unwrap()
            .contains(r#"hl.bind("CTRL + ALT + DELETE","#),
        "{}",
        sb.ferry_lua().unwrap()
    );
    std::fs::write(
        cfg.join("config.toml"),
        "[crossing]\nescape_chord = \"Escape\"\n",
    )
    .unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 1);
    assert!(out.contains("at least 2 modifiers"), "{out}");
}

#[test]
fn a_config_error_from_hyprland_rolls_the_edit_back() {
    let sb = Sandbox::new();
    sb.knobs.reject.store(true, Ordering::SeqCst);
    let before = sb.hyprland_lua();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("FAILED: Hyprland reports a config error"),
        "{out}"
    );
    assert!(out.contains("the fake Hyprland dislikes this"), "{out}");
    assert_eq!(sb.hyprland_lua(), before, "hyprland.lua restored");
    assert!(sb.ferry_lua().is_none(), "ferry.lua removed");
    assert!(sb.knobs.compute().errors.is_empty());
}

/// C-06 (1): a restore that fails is not "restored". The output names the file, and the backup
/// that holds the user's original.
#[test]
fn a_rollback_that_cannot_restore_names_the_backup_and_does_not_claim_it_did() {
    if rustix::process::geteuid().is_root() {
        return; // root writes into a read-only directory
    }
    let sb = Sandbox::new();
    sb.knobs.reject.store(true, Ordering::SeqCst);
    sb.knobs.lock_dir_on_error.store(true, Ordering::SeqCst);
    let (code, out) = sb.setup(&["--yes"], "");
    // let the temp dir be removed
    std::fs::set_permissions(sb.hypr_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("could not restore ~/.config/hypr/hyprland.lua; your original is ~/.config/hypr/hyprland.lua.ferry-bak.1800000000"),
        "{out}"
    );
    assert!(
        out.contains("could not remove ~/.config/hypr/ferry.lua, which this step created"),
        "{out}"
    );
    assert!(!out.contains("restored the files"), "{out}");
    assert!(
        out.contains("FAILED: Hyprland reports a config error and the change could not be undone"),
        "{out}"
    );
    // the backup is the user's original, byte for byte
    let backup = sb.hypr_dir().join("hyprland.lua.ferry-bak.1800000000");
    assert_eq!(std::fs::read_to_string(backup).unwrap(), HYPRLAND_LUA);
}

/// C-06 (2): a file the step did not change is not rewritten, so what somebody saved to it
/// during the wait survives.
#[test]
fn a_rollback_leaves_a_file_this_step_did_not_change() {
    let sb = Sandbox::new();
    // the user's hyprland.lua already loads ferry.lua: the step writes only ferry.lua
    let loaded = format!("{HYPRLAND_LUA}{}\n", lua::REQUIRE);
    std::fs::write(sb.hypr_dir().join("hyprland.lua"), &loaded).unwrap();
    sb.knobs.reject.store(true, Ordering::SeqCst);
    sb.knobs.edit_hypr_on_error.store(true, Ordering::SeqCst);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("restored the files from before this step"),
        "{out}"
    );
    assert!(sb.ferry_lua().is_none(), "ferry.lua removed");
    assert_eq!(
        sb.hyprland_lua(),
        format!("{loaded}{EDITED_MEANWHILE}"),
        "the line saved meanwhile is still there"
    );
}

/// C-06 (3): after a rollback Hyprland is asked to reload, and the step waits until it shows the
/// restored files. Without auto-reload nothing else would make it drop the failed config.
#[test]
fn after_a_rollback_hyprland_is_asked_to_reload_and_the_step_waits_for_it() {
    let sb = Sandbox::new();
    sb.knobs.autoreload.store(false, Ordering::SeqCst);
    sb.knobs.reject.store(true, Ordering::SeqCst);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 1, "{out}");
    assert!(
        sb.knobs.snap.lock().unwrap().errors.is_empty(),
        "Hyprland still runs the config it failed on"
    );
    // each failed step: the reload that showed the error, and the one after the rollback
    assert!(sb.knobs.reloads.load(Ordering::SeqCst) >= 4);
    assert!(
        !out.contains("has not loaded the restored files yet"),
        "{out}"
    );
}

/// A hyprland.lua that is not UTF-8 is the user's, byte for byte: setup prints the snippet.
#[test]
fn a_non_utf8_hyprland_lua_is_left_byte_for_byte() {
    let sb = Sandbox::new();
    let mut bytes = b"-- caf\xe9\n".to_vec();
    bytes.extend_from_slice(HYPRLAND_LUA.as_bytes());
    let path = sb.hypr_dir().join("hyprland.lua");
    std::fs::write(&path, &bytes).unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(sb.ferry_lua().is_none());
    assert!(
        std::fs::read_dir(sb.hypr_dir()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("ferry-bak")),
        "no backup made"
    );
    assert!(out.contains("is not valid UTF-8"), "{out}");
    assert!(out.contains("allow_input_capture = true"), "{out}");
    assert!(
        summary(&out).contains("3 printed only"),
        "{}",
        summary(&out)
    );
    assert!(summary(&out).ends_with("0 failed"), "{}", summary(&out));
    // undo leaves it alone as well
    sb.setup(&["--undo", "--yes"], "");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn without_hyprland_nothing_is_changed_and_the_snippet_is_printed() {
    let sb = Sandbox::build(false);
    let before = sb.hyprland_lua();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(sb.hyprland_lua(), before);
    assert!(sb.ferry_lua().is_none());
    assert!(out.contains("no running Hyprland"), "{out}");
    assert!(out.contains("hl.bind(\"SUPER + CTRL + ESCAPE\""), "{out}");
    // the unit step does not need Hyprland
    assert!(sb.unit_path().exists());
}

#[test]
fn hyprland_that_does_not_auto_reload_is_asked_to() {
    let sb = Sandbox::new();
    sb.knobs.autoreload.store(false, Ordering::SeqCst);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(sb.knobs.reloads.load(Ordering::SeqCst) >= 1);
    assert!(out.contains("done: Hyprland loaded it"), "{out}");
}

#[test]
fn dry_run_shows_the_diffs_and_changes_nothing() {
    let sb = Sandbox::new();
    let before = sb.hyprland_lua();
    let (code, out) = sb.setup(&["--dry-run"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("dry_run", out);
    assert_eq!(sb.hyprland_lua(), before);
    assert!(sb.ferry_lua().is_none() && !sb.unit_path().exists());
    assert!(!sb.systemd_log().contains("enable --now"));
    // the plugin: shown, not done
    assert!(sb.link_target().is_none() && !sb.plugin_link().exists());
    assert_eq!(sb.shell_json(), DEFAULT_SHELL_JSON);
    assert!(sb.shell_json_backups().is_empty());
    assert!(sb.shell_calls().is_empty());
}

#[test]
fn without_a_terminal_and_without_yes_it_is_a_dry_run_that_exits_1() {
    let mut sb = Sandbox::new();
    sb.env.interactive = false;
    let before = sb.hyprland_lua();
    let (code, out) = sb.setup(&[], "");
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("not a terminal and --yes was not given"),
        "{out}"
    );
    assert_eq!(sb.hyprland_lua(), before);
    assert!(sb.ferry_lua().is_none() && !sb.unit_path().exists());
    assert!(sb.link_target().is_none() && sb.shell_json() == DEFAULT_SHELL_JSON);
    // with --yes the same environment runs
    let (code, _) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0);
    assert!(sb.link_target().is_some());
}

#[test]
fn a_unit_the_user_wrote_is_enabled_but_never_overwritten() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.unit_path().parent().unwrap()).unwrap();
    std::fs::write(sb.unit_path(), "[Service]\nExecStart=/opt/mine/ferryd\n").unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        std::fs::read_to_string(sb.unit_path()).unwrap(),
        "[Service]\nExecStart=/opt/mine/ferryd\n"
    );
    assert!(sb.systemd_log().contains("enable --now ferryd"));
    sb.setup(&["--undo", "--yes"], "");
    assert!(sb.unit_path().exists(), "a unit setup did not write stays");
}

#[test]
fn a_packaged_unit_is_only_enabled() {
    let mut sb = Sandbox::new();
    let packaged = sb.tmp.path().join("packaged.service");
    std::fs::write(&packaged, "[Service]\nExecStart=/usr/bin/ferryd\n").unwrap();
    sb.env.packaged_unit = packaged;
    sb.env.ferryd = sb.tmp.path().join("no-ferryd-here");
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(!sb.unit_path().exists());
    assert!(!sb.systemd_log().contains("daemon-reload"));
    assert!(sb.systemd_log().contains("enable --now ferryd"));
}

/// A package installed over a build run from a directory: the unit `ferry setup` wrote for that
/// build sits in the user's config directory and would shadow the packaged one.
#[test]
fn installing_the_package_retires_the_unit_setup_wrote_for_an_unpackaged_build() {
    let mut sb = Sandbox::new();
    sb.setup(&["--yes"], "");
    assert!(sb.unit_path().exists(), "the unpackaged run wrote a unit");
    let packaged = sb.tmp.path().join("packaged.service");
    std::fs::write(&packaged, "[Service]\nExecStart=/usr/bin/ferryd\n").unwrap();
    sb.env.packaged_unit = packaged;
    sb.env.ferryd = sb.tmp.path().join("no-ferryd-here");
    std::fs::write(sb.state.join("log"), "").unwrap();

    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("retire_user_unit", out);
    assert!(!sb.unit_path().exists(), "the shadowing copy is gone");
    // stop the old one while the copy still names the unit, drop the copy, then start the packaged
    let log = sb.systemd_log();
    let at = |needle: &str| {
        log.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {log}"))
    };
    assert!(at("disable --now ferryd") < at("daemon-reload"), "{log}");
    assert!(at("daemon-reload") < at("enable --now ferryd"), "{log}");

    // the next run has nothing left to do, and undo has no copy to delete
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("[2/7] ferryd.service\n  skipped: ferryd.service is enabled and active"),
        "{out}"
    );
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("delete ~"), "{out}");
}

#[test]
fn a_dry_run_leaves_the_old_unit_in_place() {
    let mut sb = Sandbox::new();
    sb.setup(&["--yes"], "");
    let before = std::fs::read_to_string(sb.unit_path()).unwrap();
    let packaged = sb.tmp.path().join("packaged.service");
    std::fs::write(&packaged, "[Service]\nExecStart=/usr/bin/ferryd\n").unwrap();
    sb.env.packaged_unit = packaged;
    std::fs::write(sb.state.join("log"), "").unwrap();
    let (_, out) = sb.setup(&["--dry-run"], "");
    assert!(out.contains("it shadows the packaged unit"), "{out}");
    assert_eq!(std::fs::read_to_string(sb.unit_path()).unwrap(), before);
    assert!(!sb.systemd_log().contains("daemon-reload"));
}

#[test]
fn a_unit_the_user_wrote_is_reported_as_shadowing_the_package() {
    let mut sb = Sandbox::new();
    std::fs::create_dir_all(sb.unit_path().parent().unwrap()).unwrap();
    std::fs::write(sb.unit_path(), "[Service]\nExecStart=/opt/mine/ferryd\n").unwrap();
    let packaged = sb.tmp.path().join("packaged.service");
    std::fs::write(&packaged, "[Service]\nExecStart=/usr/bin/ferryd\n").unwrap();
    sb.env.packaged_unit = packaged;
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("it shadows the packaged unit"), "{out}");
    assert!(
        sb.unit_path().exists(),
        "a unit setup did not write is never deleted"
    );
    assert!(!sb.systemd_log().contains("daemon-reload"));
    // the unit runs now: the second run still says whose unit it is
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("skipped: ferryd.service is enabled and active, from your own ~/.config/systemd/user/ferryd.service, which shadows the packaged unit"),
        "{out}"
    );
}

#[test]
fn a_missing_ferryd_fails_the_unit_step_and_the_rest_goes_on() {
    let mut sb = Sandbox::new();
    sb.env.ferryd = sb.tmp.path().join("no-ferryd-here");
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("ferryd is not next to ferry"), "{out}");
    assert!(lua::has_block(&sb.ferry_lua().unwrap(), Block::EscapeBind));
}

#[test]
fn ufw_off_skips_the_firewall() {
    let sb = Sandbox::new();
    std::fs::write(&sb.env.ufw_conf, "ENABLED=no\n").unwrap();
    let (_, out) = sb.setup(&["--yes"], "");
    assert!(out.contains("skipped: ufw is not enabled"), "{out}");
}

// --- the bar plugin -----------------------------------------------------

/// A `shell.json` the way a machine with a cloned bar and a cloned tray has it: `bar.id` names the
/// clone, the layout still sits in `bar.layout`, and the tray is `my.tray`.
const CLONED: &str = r#"{
  "bar": {
    "background": "glassy",
    "id": "my.bar",
    "layout": {
      "center": [],
      "centerBegin": [],
      "centerEnd": [],
      "left": [
        {
          "id": "omarchy.menu"
        }
      ],
      "right": [
        {
          "id": "omarchy.keyboard-layout"
        },
        {
          "id": "my.tray"
        },
        {
          "id": "my.artisan"
        }
      ]
    },
    "position": "top"
  },
  "disabledPlugins": [
    "my.lock",
    "omarchy.lock"
  ],
  "plugins": [
    {
      "id": "my.rulers"
    }
  ],
  "version": 1
}
"#;

/// Keys in an order of the user's own, settings on widgets, strings among objects, one-line objects.
const BUSY: &str = r#"{
  "version": 1,
  "plugins": [{ "id": "my.rulers" }],
  "bar": {
    "layout": {
      "right": [
        { "id": "omarchy.tray", "iconSize": 14 },
        { "id": "io.hubdev.buddy", "view": "dense" },
        "omarchy.audio"
      ],
      "left": [], "center": [], "centerEnd": [], "centerBegin": []
    },
    "id": "my.bar"
  }
}
"#;

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn right_of(json: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v["bar"]["layout"]["right"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().or_else(|| e["id"].as_str()).unwrap().to_string())
        .collect()
}

fn clone_manifest(sb: &Sandbox, id: &str, of: &str) {
    let dir = sb.omarchy_dir().join("plugins").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        format!(r#"{{"id":"{id}","omarchy":{{"clonedFrom":"{of}"}}}}"#),
    )
    .unwrap();
}

#[test]
fn the_plugin_step_alone_from_a_fresh_machine_to_undo() {
    let sb = Sandbox::new();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("plugin_fresh_run", plugin_step(&out));
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("plugin_undo", plugin_step(&out));
}

#[test]
fn a_cloned_bar_and_a_cloned_tray_are_looked_through() {
    let sb = Sandbox::new();
    sb.set_shell_json(CLONED);
    std::fs::set_permissions(sb.shell_json_path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    clone_manifest(&sb, "my.tray", "omarchy.tray");
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("plugin_cloned", plugin_step(&out));
    let json = sb.shell_json();
    assert_eq!(
        right_of(&json),
        [
            "omarchy.keyboard-layout",
            "my.tray",
            "ferry.bar",
            "my.artisan"
        ],
        "right after the clone that took the tray's place"
    );
    // the cloned bar is the user's choice and stays; so does every other byte
    assert_eq!(
        json,
        CLONED.replacen(
            "        {\n          \"id\": \"my.artisan\"",
            "        {\n          \"id\": \"ferry.bar\"\n        },\n        {\n          \"id\": \"my.artisan\"",
            1
        )
    );
    assert_eq!(
        mode(&sb.shell_json_path()),
        0o600,
        "the file keeps its mode"
    );

    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(sb.shell_json(), CLONED, "back to the byte");
}

#[test]
fn a_tray_that_is_nobodys_clone_is_not_an_anchor() {
    let sb = Sandbox::new();
    sb.set_shell_json(CLONED);
    // my.tray has no manifest here: setup cannot tell what it is, so the widget goes last
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        right_of(&sb.shell_json()),
        [
            "omarchy.keyboard-layout",
            "my.tray",
            "my.artisan",
            "ferry.bar"
        ]
    );
    assert!(plugin_step(&out).contains("(right, at the end)"), "{out}");
    // a manifest that says it is a clone of something else does not help either
    let sb = Sandbox::new();
    sb.set_shell_json(CLONED);
    clone_manifest(&sb, "my.tray", "omarchy.network");
    sb.setup(&["--yes"], "");
    assert_eq!(
        right_of(&sb.shell_json()).last().map(String::as_str),
        Some("ferry.bar")
    );
}

#[test]
fn the_file_is_edited_as_text_and_undo_gives_it_back_whatever_its_order() {
    let sb = Sandbox::new();
    sb.set_shell_json(BUSY);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let json = sb.shell_json();
    assert_eq!(
        right_of(&json),
        [
            "omarchy.tray",
            "ferry.bar",
            "io.hubdev.buddy",
            "omarchy.audio"
        ]
    );
    // the keys are where the user had them, and the other widgets keep their settings: one entry
    // is added, and not a byte more
    assert_eq!(
        json,
        BUSY.replacen(
            "        { \"id\": \"io.hubdev.buddy\"",
            "        {\n          \"id\": \"ferry.bar\"\n        },\n        { \"id\": \"io.hubdev.buddy\"",
            1
        )
    );
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(sb.shell_json(), BUSY, "the file the user wrote");
}

#[test]
fn without_a_shell_json_of_the_users_the_new_one_starts_from_the_shells_default() {
    let sb = Sandbox::new();
    std::fs::remove_file(sb.shell_json_path()).unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("plugin_seeded", plugin_step(&out));
    let json = sb.shell_json();
    // the whole of the default is there, and the widget; a file with only ferry in it would have
    // replaced the user's bar with nothing
    assert_eq!(
        json,
        DEFAULT_SHELL_JSON.replacen(
            "        {\n          \"id\": \"omarchy.keyboard-layout\"",
            "        {\n          \"id\": \"ferry.bar\"\n        },\n        {\n          \"id\": \"omarchy.keyboard-layout\"",
            1
        )
    );
    assert_eq!(
        right_of(&json),
        [
            "omarchy.tray",
            "ferry.bar",
            "omarchy.keyboard-layout",
            "omarchy.audio",
            "omarchy.power"
        ]
    );
    assert!(
        sb.shell_json_backups().is_empty(),
        "there was nothing to keep"
    );
    assert_eq!(mode(&sb.shell_json_path()), 0o644);

    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        plugin_step(&out).contains("skipped: ferry.bar is linked and on the bar"),
        "{out}"
    );

    // undo takes the widget off and leaves the file, which is now the user's
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(sb.shell_json(), DEFAULT_SHELL_JSON);
}

#[test]
fn with_no_default_to_start_from_a_missing_file_is_not_invented() {
    let sb = Sandbox::new();
    std::fs::remove_file(sb.shell_json_path()).unwrap();
    std::fs::remove_file(sb.env.omarchy_path.join("config/omarchy/shell.json")).unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let step = plugin_step(&out);
    assert!(
        step.contains("~/.config/omarchy/shell.json does not exist, and the shell's default <tmp>/omarchy/config/omarchy/shell.json is not there to start it from; setup leaves it alone."),
        "{step}"
    );
    assert!(
        step.contains("$ ln -s <tmp>/usr/share/ferry/plugin ~/.config/omarchy/plugins/ferry.bar"),
        "{step}"
    );
    assert!(
        step.contains("$ omarchy plugin enable ferry.bar --section right"),
        "{step}"
    );
    assert!(!sb.shell_json_path().exists() && sb.link_target().is_none());
    assert!(
        summary(&out).contains("1 printed only"),
        "{}",
        summary(&out)
    );
}

#[test]
fn a_widget_that_is_on_the_bar_already_is_left_where_it_is() {
    for entry in [
        "\"ferry.bar\"",
        "{ \"id\": \"ferry.bar\", \"showPeerName\": false }",
    ] {
        let sb = Sandbox::new();
        let text =
            DEFAULT_SHELL_JSON.replace("{\n          \"id\": \"omarchy.audio\"\n        }", entry);
        sb.set_shell_json(&text);
        let (code, out) = sb.setup(&["--yes"], "");
        assert_eq!(code, 0, "{out}");
        // the link was missing, and that is all there was to do
        assert!(sb.link_target().is_some());
        assert_eq!(sb.shell_json(), text, "{entry}");
        assert!(sb.shell_json_backups().is_empty());
        let step = plugin_step(&out);
        assert!(
            step.contains("$ ln -s ") && !step.contains("--- "),
            "{step}"
        );
        assert!(step.contains("done: linked ferry.bar\n"), "{step}");
        let (_, out) = sb.setup(&["--yes"], "");
        assert!(
            plugin_step(&out).contains("skipped: ferry.bar is linked and on the bar"),
            "{out}"
        );
        // undo takes the entry, settings and all
        let (_, out) = sb.setup(&["--undo", "--yes"], "");
        assert!(!sb.shell_json().contains("ferry.bar"), "{out}");
        assert!(sb.link_target().is_none());
    }
}

#[test]
fn a_link_that_is_already_ours_is_not_made_again() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.plugin_link().parent().unwrap()).unwrap();
    symlink(&sb.env.plugin_source, sb.plugin_link()).unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let step = plugin_step(&out);
    assert!(
        !step.contains("$ ln -s") && !step.contains("rescanPlugins"),
        "{step}"
    );
    assert!(
        step.contains("done: put the widget on the bar (right, after omarchy.tray)"),
        "{step}"
    );
    assert!(
        sb.shell_calls().is_empty(),
        "the shell already knows the plugin"
    );
    assert!(sb.shell_json().contains("\"id\": \"ferry.bar\""));
    // and a link written as a relative path to the same place is the same link
    sb.setup(&["--undo", "--yes"], "");
    assert!(sb.link_target().is_none());
    let relative = Path::new("../../../../usr/share/ferry/plugin");
    symlink(relative, sb.plugin_link()).unwrap();
    assert_eq!(
        sb.plugin_link().canonicalize().unwrap(),
        sb.env.plugin_source.canonicalize().unwrap()
    );
    let (_, out) = sb.setup(&["--yes"], "");
    assert!(!plugin_step(&out).contains("is linked to"), "{out}");
    assert_eq!(
        sb.link_target().as_deref(),
        Some(relative),
        "setup did not replace it"
    );
}

#[test]
fn a_link_that_is_not_ours_is_left_alone_and_the_widget_still_goes_on_the_bar() {
    let sb = Sandbox::new();
    let checkout = sb.tmp.path().join("checkout/plugin");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("manifest.json"), PLUGIN_MANIFEST).unwrap();
    std::fs::create_dir_all(sb.plugin_link().parent().unwrap()).unwrap();
    symlink(&checkout, sb.plugin_link()).unwrap();

    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let step = plugin_step(&out);
    assert!(
        step.contains("ferry.bar is linked to <tmp>/checkout/plugin (yours), left alone."),
        "{step}"
    );
    assert!(!step.contains("$ ln -s"), "{step}");
    assert_eq!(sb.link_target(), Some(checkout.clone()));
    assert!(sb.shell_json().contains("\"id\": \"ferry.bar\""));
    assert!(
        sb.shell_calls().is_empty(),
        "no link was made, so no rescan"
    );

    let (_, out) = sb.setup(&["--yes"], "");
    assert!(
        plugin_step(&out).contains("skipped: ferry.bar is on the bar, from a plugin folder that is linked to <tmp>/checkout/plugin (yours)"),
        "{out}"
    );

    // undo takes the widget off and leaves the user's link
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(sb.shell_json(), DEFAULT_SHELL_JSON);
    assert_eq!(sb.link_target(), Some(checkout));
    assert!(sb.shell_calls().is_empty());
    let (_, out) = sb.setup(&["--undo", "--yes"], "");
    assert!(
        plugin_step(&out).contains("skipped: ferry.bar is not on the bar, and the plugin folder is linked to <tmp>/checkout/plugin (yours)"),
        "{out}"
    );
}

#[test]
fn a_directory_where_the_link_goes_is_the_users() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.plugin_link()).unwrap();
    std::fs::write(sb.plugin_link().join("manifest.json"), "{}").unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        plugin_step(&out).contains("ferry.bar is a directory (yours), left alone."),
        "{out}"
    );
    assert!(sb.plugin_link().join("manifest.json").is_file() && sb.link_target().is_none());
    assert!(sb.shell_json().contains("ferry.bar"));
    let (code, _) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0);
    assert!(
        sb.plugin_link().join("manifest.json").is_file(),
        "undo leaves it too"
    );
    assert_eq!(sb.shell_json(), DEFAULT_SHELL_JSON);
}

#[test]
fn a_shell_json_the_shell_would_not_read_is_not_touched() {
    let cases: [(&str, &[u8], &str); 7] = [
        ("garbage", b"{ not json", "is not valid JSON"),
        ("an array", b"[]", "is not a JSON object"),
        (
            "no version",
            br#"{"bar":{"layout":{"right":[]}}}"#,
            "has no \"version\": 1",
        ),
        (
            "version 2",
            br#"{"version":2,"bar":{"layout":{"right":[]}}}"#,
            "has no \"version\": 1",
        ),
        (
            "no layout",
            br#"{"version":1,"bar":{"position":"top"}}"#,
            "has no bar.layout",
        ),
        (
            "no right section",
            br#"{"version":1,"bar":{"layout":{"left":[]}}}"#,
            "has no bar.layout.right",
        ),
        (
            "not utf-8",
            b"{\"version\": 1, \"x\": \"caf\xe9\"}",
            "is not valid UTF-8",
        ),
    ];
    for (name, bytes, why) in cases {
        let sb = Sandbox::new();
        std::fs::write(sb.shell_json_path(), bytes).unwrap();
        let (code, out) = sb.setup(&["--yes"], "");
        assert_eq!(code, 0, "{name}: {out}");
        assert_eq!(
            std::fs::read(sb.shell_json_path()).unwrap(),
            bytes,
            "{name}"
        );
        assert!(sb.link_target().is_none(), "{name}: nothing is half done");
        assert!(
            sb.shell_json_backups().is_empty() && sb.shell_calls().is_empty(),
            "{name}"
        );
        let step = plugin_step(&out);
        assert!(
            step.contains(why)
                && step.contains("; setup leaves it alone. Put the widget on the bar yourself:"),
            "{name}: {step}"
        );
        assert!(
            step.contains("$ ln -s ")
                && step.contains("$ omarchy plugin enable ferry.bar --section right"),
            "{name}: {step}"
        );
        assert!(
            step.contains("nothing changed: shell.json is not one setup edits"),
            "{name}: {step}"
        );
        assert!(
            summary(&out).contains("1 printed only"),
            "{name}: {}",
            summary(&out)
        );

        // undo: a file that never says the id has nothing of ours in it; one that does is shown
        // to the user to deal with
        let (code, out) = sb.setup(&["--undo", "--yes"], "");
        assert_eq!(code, 0, "{name}: {out}");
        assert_eq!(
            std::fs::read(sb.shell_json_path()).unwrap(),
            bytes,
            "{name}"
        );
        assert!(
            plugin_step(&out)
                .contains("skipped: ferry.bar is not on the bar and setup left no link"),
            "{name}: {out}"
        );
    }
    // an unreadable file that does name the plugin
    let sb = Sandbox::new();
    let bytes = b"{ \"ferry.bar\": ";
    std::fs::write(sb.shell_json_path(), bytes).unwrap();
    std::fs::create_dir_all(sb.plugin_link().parent().unwrap()).unwrap();
    symlink(&sb.env.plugin_source, sb.plugin_link()).unwrap();
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    let step = plugin_step(&out);
    assert!(
        step.contains("is not valid JSON")
            && step.contains("Take the widget off the bar yourself:"),
        "{step}"
    );
    assert!(
        step.contains("$ omarchy plugin disable ferry.bar"),
        "{step}"
    );
    assert_eq!(std::fs::read(sb.shell_json_path()).unwrap(), bytes);
}

#[test]
fn a_symlinked_shell_json_stays_a_symlink() {
    let sb = Sandbox::new();
    let dotfiles = sb.tmp.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let real = dotfiles.join("shell.json");
    std::fs::rename(sb.shell_json_path(), &real).unwrap();
    symlink(&real, sb.shell_json_path()).unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        plugin_step(&out).contains("; the link points here, the link itself stays)"),
        "{out}"
    );
    assert!(
        std::fs::symlink_metadata(sb.shell_json_path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::read_to_string(&real)
            .unwrap()
            .contains("ferry.bar")
    );
    assert_eq!(
        std::fs::read_to_string(dotfiles.join("shell.json.ferry-bak.1800000000")).unwrap(),
        DEFAULT_SHELL_JSON,
        "the backup is next to the file the link points at"
    );
    sb.setup(&["--undo", "--yes"], "");
    assert!(
        std::fs::symlink_metadata(sb.shell_json_path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), DEFAULT_SHELL_JSON);
}

#[test]
fn ferry_in_disabled_plugins_is_taken_out_as_the_shell_does() {
    let sb = Sandbox::new();
    let text = DEFAULT_SHELL_JSON.replace(
        "  \"plugins\": []",
        "  \"disabledPlugins\": [\n    \"ferry.bar\",\n    \"x\"\n  ],\n  \"plugins\": []",
    );
    sb.set_shell_json(&text);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let step = plugin_step(&out);
    assert!(step.contains("(add the widget to the bar: right, after omarchy.tray; take ferry.bar out of disabledPlugins)"), "{step}");
    let v: serde_json::Value = serde_json::from_str(&sb.shell_json()).unwrap();
    assert_eq!(v["disabledPlugins"], serde_json::json!(["x"]));
    assert!(right_of(&sb.shell_json()).contains(&"ferry.bar".to_string()));

    // on the bar but disabled: the shell treats the plugin as off, so only the list changes
    let sb = Sandbox::new();
    let on_bar = text.replace("\"omarchy.audio\"", "\"ferry.bar\"");
    sb.set_shell_json(&on_bar);
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&sb.shell_json()).unwrap();
    assert_eq!(v["disabledPlugins"], serde_json::json!(["x"]));
    assert_eq!(right_of(&sb.shell_json()), right_of(&on_bar));
    assert!(
        plugin_step(&out).contains("took ferry.bar out of disabledPlugins"),
        "{out}"
    );
}

#[test]
fn without_the_package_there_is_no_plugin_to_link() {
    let mut sb = Sandbox::new();
    sb.env.plugin_source = sb.tmp.path().join("nowhere");
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    let step = plugin_step(&out);
    assert!(
        step.contains("<tmp>/nowhere is not there: the ferry package is not installed"),
        "{step}"
    );
    assert!(
        step.contains("$ ln -s <checkout>/plugin ~/.config/omarchy/plugins/ferry.bar"),
        "{step}"
    );
    assert!(
        step.contains("$ omarchy plugin enable ferry.bar --section right"),
        "{step}"
    );
    assert!(
        step.contains("nothing changed: no packaged plugin to link"),
        "{step}"
    );
    assert_eq!(sb.shell_json(), DEFAULT_SHELL_JSON);
    assert!(sb.link_target().is_none() && !sb.plugin_link().exists());
    // the rest of setup goes on
    assert!(sb.unit_path().exists());
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(
        plugin_step(&out).contains("skipped: ferry.bar is not on the bar and setup left no link"),
        "{out}"
    );
}

#[test]
fn a_shell_that_cannot_be_asked_to_rescan_does_not_stop_setup() {
    // not there at all
    let mut sb = Sandbox::new();
    sb.env.omarchy_shell = sb.tmp.path().join("no-such-omarchy-shell");
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(sb.link_target().is_some() && sb.shell_json().contains("ferry.bar"));
    assert!(
        plugin_step(&out).contains(
            "done: linked ferry.bar and put the widget on the bar (right, after omarchy.tray)"
        ),
        "{out}"
    );
    // there, and failing
    let sb = Sandbox::new();
    script(
        &sb.tmp.path().join("bin/omarchy-shell"),
        "echo 'omarchy-shell is not running' >&2\nexit 1",
    );
    let (code, out) = sb.setup(&["--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(sb.link_target().is_some() && sb.shell_json().contains("ferry.bar"));
    assert!(
        !plugin_step(&out).contains("is not running"),
        "its words are not setup's"
    );
    let (code, out) = sb.setup(&["--undo", "--yes"], "");
    assert_eq!(code, 0, "{out}");
    assert!(sb.link_target().is_none());
}

#[test]
fn a_link_that_cannot_be_made_is_a_failed_step_and_the_layout_is_not_touched() {
    if rustix::process::geteuid().is_root() {
        return; // root writes into a read-only directory
    }
    let sb = Sandbox::new();
    // the plugin directory exists and cannot be written to
    let plugins = sb.omarchy_dir().join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::set_permissions(&plugins, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (code, out) = sb.setup(&["--yes"], "");
    std::fs::set_permissions(&plugins, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code, 1, "{out}");
    assert!(
        plugin_step(&out).contains("FAILED: ~/.config/omarchy/plugins/ferry.bar:"),
        "{out}"
    );
    assert_eq!(
        sb.shell_json(),
        DEFAULT_SHELL_JSON,
        "the layout waits for the link"
    );
    assert!(sb.shell_calls().is_empty());
    // the other steps went on
    assert!(sb.unit_path().exists());
}

// Guard against the doc drifting from the code: the setup module is the only writer of these.
#[test]
fn the_module_is_reachable_for_other_crates() {
    let _ = setup::Args::default();
}
