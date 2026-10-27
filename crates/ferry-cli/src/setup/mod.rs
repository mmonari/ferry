//! `ferry setup [--yes] [--dry-run] [--undo]` (FR-SETUP-01..09).
//!
//! Each step checks the live state first, skips if it is already right, else shows exactly what
//! it will change and asks once. A second run reports every step as skipped. Hyprland config is
//! Lua and setup writes only its own file, `~/.config/hypr/ferry.lua`, plus one appended
//! `require("hypr.ferry")` line in `hyprland.lua`; it never edits a line it did not write. The
//! bar plugin is a link in `~/.config/omarchy/plugins` and one entry in the user's `shell.json`
//! (`plugin.rs`), added and taken out again as text.
//!
//! Everything outside the process is in [`Env`]: the home and config directories, the runtime
//! directory Hyprland's socket lives in, the `systemctl` and `sudo` programs, ufw's config and
//! the packaged unit. Tests point them at a temp dir, a fake Hyprland socket and two shell
//! scripts; nothing here reads the real ones unless [`Env::from_process`] built the [`Env`].

pub mod error;
pub mod fsx;
pub mod fw;
pub mod hypr;
pub mod jsonedit;
pub mod lua;
pub mod plugin;
pub mod unit;

use std::fmt::Display;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ferry_core::chord::{EscapeChord, GoChord};
use ferry_hypr_ipc::Instance;
use lua::{Block, Chords};

pub use error::SetupError;
use fsx::ReadError;

#[derive(Debug, Clone, Copy, Default)]
pub struct Args {
    /// Answer yes to every step except sudo.
    pub yes: bool,
    /// Show every diff and command; change nothing.
    pub dry_run: bool,
    /// Take back what setup did.
    pub undo: bool,
}

/// Everything setup reads or touches outside its own process.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME`, else `<home>/.config`.
    pub config_home: PathBuf,
    /// Where `hypr/<instance>/.socket.sock` lives.
    pub runtime_dir: Option<PathBuf>,
    pub hypr_signature: Option<String>,
    /// `$XDG_DATA_HOME`, else `<home>/.local/share`: `ferry/` in it holds the key.
    pub data_home: PathBuf,
    pub systemctl: PathBuf,
    pub sudo: PathBuf,
    /// For `ferry doctor`: which xdph is installed.
    pub pacman: PathBuf,
    /// For `ferry doctor`: whether a notification daemon owns its name on the session bus.
    pub busctl: PathBuf,
    pub ufw_conf: PathBuf,
    /// The packaged `ferry` application profile, wherever a package would put it.
    pub ufw_profiles: Vec<PathBuf>,
    pub packaged_unit: PathBuf,
    /// Where the package puts the omarchy-shell plugin: `~/.config/omarchy/plugins/ferry.bar`
    /// links here.
    pub plugin_source: PathBuf,
    /// `$OMARCHY_PATH`, else where the package puts Omarchy. Its `config/omarchy/shell.json` is
    /// what the shell runs with when the user has no `shell.json` of their own.
    pub omarchy_path: PathBuf,
    /// `omarchy-shell`: asked to rescan the plugins after the link, and by `ferry ui`.
    pub omarchy_shell: PathBuf,
    /// The `ferryd` beside this `ferry`.
    pub ferryd: PathBuf,
    /// ferryd's socket, for "has it accepted an inbound connection".
    pub socket: PathBuf,
    pub interactive: bool,
    /// How long to wait for Hyprland's auto-reload (once, then once more after asking for a reload).
    pub hypr_wait: Duration,
    pub unix_time: u64,
}

/// `$HOME`, `$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` as directories. The XDG variables count only
/// when absolute; without an absolute `HOME` there is no home, and the answer is ferryd's own
/// error rather than `/.config`.
pub fn home_dirs(
    home: Option<PathBuf>,
    config_home: Option<PathBuf>,
    data_home: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf, PathBuf), SetupError> {
    let home = home.filter(|p| p.is_absolute()).ok_or(SetupError::NoHome)?;
    let under = |var: Option<PathBuf>, rel: &str| {
        var.filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(rel))
    };
    let config_home = under(config_home, ".config");
    let data_home = under(data_home, ".local/share");
    Ok((home, config_home, data_home))
}

/// Where the package puts the plugin folder (`dist/` and `pkgbuild/PKGBUILD` agree).
pub const PLUGIN_SOURCE: &str = "/usr/share/ferry/plugin";
/// Where the package puts Omarchy, when `$OMARCHY_PATH` does not say.
pub const OMARCHY_PATH: &str = "/usr/share/omarchy";
/// The shell's command line tool.
pub const OMARCHY_SHELL: &str = "omarchy-shell";

/// `$OMARCHY_PATH` when it is an absolute path, else where the package puts Omarchy: the rule
/// Omarchy's own Hyprland config uses.
pub fn omarchy_path_from(var: Option<PathBuf>) -> PathBuf {
    var.filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(OMARCHY_PATH))
}

impl Env {
    pub fn from_process() -> Result<Self, SetupError> {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let (home, config_home, data_home) =
            home_dirs(var("HOME"), var("XDG_CONFIG_HOME"), var("XDG_DATA_HOME"))?;
        let ferryd = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("ferryd")))
            .unwrap_or_else(|| PathBuf::from("ferryd"));
        Ok(Self {
            home,
            config_home,
            runtime_dir: ferry_hypr_ipc::runtime_dir().ok(),
            hypr_signature: std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok(),
            data_home,
            systemctl: "systemctl".into(),
            sudo: "sudo".into(),
            pacman: "pacman".into(),
            busctl: "busctl".into(),
            ufw_conf: "/etc/ufw/ufw.conf".into(),
            ufw_profiles: vec![
                "/etc/ufw/applications.d/ferry".into(),
                "/usr/share/ufw/applications.d/ferry".into(),
            ],
            packaged_unit: "/usr/lib/systemd/user/ferryd.service".into(),
            plugin_source: PLUGIN_SOURCE.into(),
            omarchy_path: omarchy_path_from(var("OMARCHY_PATH")),
            omarchy_shell: OMARCHY_SHELL.into(),
            ferryd,
            socket: crate::client::socket_path(),
            interactive: std::io::stdin().is_terminal(),
            hypr_wait: Duration::from_secs(2),
            unix_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        })
    }

    fn hypr_dir(&self) -> PathBuf {
        self.config_home.join("hypr")
    }

    fn ferry_lua(&self) -> PathBuf {
        self.hypr_dir().join("ferry.lua")
    }

    fn hyprland_lua(&self) -> PathBuf {
        self.hypr_dir().join("hyprland.lua")
    }

    fn user_unit(&self) -> PathBuf {
        self.config_home.join("systemd/user/ferryd.service")
    }

    /// The shell's own directory. It is `~/.config/omarchy` whatever `XDG_CONFIG_HOME` says
    /// (`shell.qml` builds the path from the home directory).
    fn omarchy_dir(&self) -> PathBuf {
        self.home.join(".config/omarchy")
    }

    /// Where the plugin is linked to be found by the shell.
    pub(crate) fn plugin_link(&self) -> PathBuf {
        self.omarchy_dir().join("plugins/ferry.bar")
    }

    /// What lists the widgets on the bar.
    pub(crate) fn shell_json(&self) -> PathBuf {
        self.omarchy_dir().join("shell.json")
    }

    /// The running Hyprland, or why there is none.
    pub fn instance(&self) -> Result<Instance, SetupError> {
        let dir = self
            .runtime_dir
            .as_deref()
            .ok_or(SetupError::NoRuntimeDir)?;
        Ok(Instance::discover_in(dir, self.hypr_signature.as_deref())?)
    }

    /// The `[crossing] escape_chord` of `config.toml`, default `Super+Ctrl+Escape`.
    pub fn escape_chord(&self) -> Result<EscapeChord, SetupError> {
        let path = self.config_home.join("ferry/config.toml");
        let Some(text) = fsx::read_text(&path).map_err(|source| SetupError::Read {
            path: path.clone(),
            source,
        })?
        else {
            return Ok(EscapeChord::default());
        };
        let v: toml::Value = toml::from_str(&text).map_err(|source| SetupError::Toml {
            path: path.clone(),
            source,
        })?;
        match v.get("crossing").and_then(|c| c.get("escape_chord")) {
            None => Ok(EscapeChord::default()),
            Some(toml::Value::String(s)) => {
                EscapeChord::parse(s).map_err(|source| SetupError::EscapeChord { path, source })
            }
            Some(_) => Err(SetupError::EscapeChordNotText(path)),
        }
    }

    /// The `[crossing] go_chord` of `config.toml`, default `Super+Ctrl+Alt`.
    pub fn go_chord(&self) -> Result<GoChord, SetupError> {
        let path = self.config_home.join("ferry/config.toml");
        let Some(text) = fsx::read_text(&path).map_err(|source| SetupError::Read {
            path: path.clone(),
            source,
        })?
        else {
            return Ok(GoChord::default());
        };
        let v: toml::Value = toml::from_str(&text).map_err(|source| SetupError::Toml {
            path: path.clone(),
            source,
        })?;
        match v.get("crossing").and_then(|c| c.get("go_chord")) {
            None => Ok(GoChord::default()),
            Some(toml::Value::String(s)) => {
                GoChord::parse(s).map_err(|source| SetupError::GoChord { path, source })
            }
            Some(_) => Err(SetupError::GoChordNotText(path)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Ask each step on the terminal.
    Ask,
    /// `--yes`.
    Yes,
    /// `--dry-run`.
    Dry,
    /// No terminal and no `--yes`: a dry run that exits 1.
    NoTerminal,
}

#[derive(Debug)]
enum Out {
    Done(String),
    Skipped(String),
    /// The user said no.
    Declined,
    /// A dry run showed it.
    Pending,
    /// Nothing could be changed; the snippet was printed.
    Printed(String),
    Failed(String),
}

#[derive(Debug, Default)]
struct Tally {
    done: u32,
    skipped: u32,
    declined: u32,
    pending: u32,
    printed: u32,
    failed: u32,
}

struct Ui<'a> {
    out: &'a mut dyn Write,
    input: &'a mut dyn BufRead,
    mode: Mode,
    home: PathBuf,
    tally: Tally,
    step: u32,
    steps: u32,
}

impl Ui<'_> {
    fn say(&mut self, s: impl Display) {
        let _ = writeln!(self.out, "{s}");
    }

    /// A path with the home directory written as `~`.
    fn tilde(&self, p: &Path) -> String {
        match p.strip_prefix(&self.home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => p.display().to_string(),
        }
    }

    fn header(&mut self, title: &str) {
        self.step += 1;
        let (n, of) = (self.step, self.steps);
        self.say(format!("\n[{n}/{of}] {title}"));
    }

    /// `true` to go ahead. In a dry run nothing is asked and the answer is `None`.
    fn confirm(&mut self, question: &str, default_yes: bool) -> Option<bool> {
        match self.mode {
            Mode::Yes => {
                self.say(format!("  {question} yes (--yes)"));
                Some(true)
            }
            Mode::Dry | Mode::NoTerminal => None,
            Mode::Ask => {
                let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
                loop {
                    let _ = write!(self.out, "  {question} {hint} ");
                    let _ = self.out.flush();
                    let mut line = String::new();
                    match self.input.read_line(&mut line) {
                        // The terminal went away: that is a no, never a yes.
                        Ok(0) | Err(_) => {
                            let _ = writeln!(self.out);
                            return Some(false);
                        }
                        Ok(_) => {}
                    }
                    match line.trim().to_ascii_lowercase().as_str() {
                        "" => return Some(default_yes),
                        "y" | "yes" => return Some(true),
                        "n" | "no" => return Some(false),
                        _ => self.say("  please answer y or n"),
                    }
                }
            }
        }
    }

    fn finish(&mut self, out: Out) {
        match out {
            Out::Done(s) => {
                self.tally.done += 1;
                self.say(format!("  done: {s}"));
            }
            Out::Skipped(s) => {
                self.tally.skipped += 1;
                self.say(format!("  skipped: {s}"));
            }
            Out::Declined => {
                self.tally.declined += 1;
                self.say("  declined: nothing changed");
            }
            Out::Pending => {
                self.tally.pending += 1;
                let why = if self.mode == Mode::NoTerminal {
                    "not a terminal and no --yes"
                } else {
                    "dry run"
                };
                self.say(format!("  not applied ({why})"));
            }
            Out::Printed(s) => {
                self.tally.printed += 1;
                self.say(format!("  nothing changed: {s}"));
            }
            Out::Failed(s) => {
                self.tally.failed += 1;
                self.say(format!("  FAILED: {s}"));
            }
        }
    }

    fn diff(&mut self, label: &str, old: Option<&str>, new: Option<&str>) {
        self.say(format!("  --- {label}"));
        match (old, new) {
            (None, Some(n)) => n.lines().for_each(|l| self.say(format!("  + {l}"))),
            (Some(o), None) => o.lines().for_each(|l| self.say(format!("  - {l}"))),
            (Some(o), Some(n)) => {
                if let Some(tail) = n.strip_prefix(o) {
                    tail.lines().for_each(|l| self.say(format!("  + {l}")));
                } else {
                    let mut j = n.lines().peekable();
                    for l in o.lines() {
                        if j.peek() == Some(&l) {
                            j.next();
                        } else {
                            self.say(format!("  - {l}"));
                        }
                    }
                }
            }
            (None, None) => {}
        }
    }
}

struct Cmd {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_cmd(program: &Path, args: &[&str]) -> Result<Cmd, SetupError> {
    let o = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|source| SetupError::Spawn {
            program: program.to_path_buf(),
            source,
        })?;
    Ok(Cmd {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    })
}

fn systemctl(env: &Env, args: &[&str]) -> Result<Cmd, SetupError> {
    let mut a = vec!["--user"];
    a.extend_from_slice(args);
    run_cmd(&env.systemctl, &a)
}

/// Runs the command line and exits with the code `ferry setup` returns: 0, or 1 when a step
/// failed or no terminal was there to ask.
pub fn run(args: &Args, env: &Env, input: &mut dyn BufRead, out: &mut dyn Write) -> u8 {
    let mode = if args.dry_run {
        Mode::Dry
    } else if args.yes {
        Mode::Yes
    } else if env.interactive {
        Mode::Ask
    } else {
        Mode::NoTerminal
    };
    let mut ui = Ui {
        out,
        input,
        mode,
        home: env.home.clone(),
        tally: Tally::default(),
        step: 0,
        steps: 7,
    };
    ui.say(if args.undo {
        "ferry setup --undo"
    } else {
        "ferry setup"
    });
    match mode {
        Mode::Dry => ui.say("dry run: every diff and command is shown, nothing is changed"),
        Mode::NoTerminal => ui.say(
            "this is not a terminal and --yes was not given: showing what would change, changing nothing",
        ),
        _ => {}
    }
    let chords = match (env.escape_chord(), env.go_chord()) {
        (Ok(escape), Ok(go)) => Chords {
            escape: lua::hypr_chord(&escape),
            go,
        },
        (Err(e), _) | (_, Err(e)) => {
            ui.say(format!("error: {e}"));
            return 1;
        }
    };
    let mut hypr = HyprCtx::new(env);

    if args.undo {
        plugin::undo(&mut ui, env);
        undo_unit(&mut ui, env);
        for b in [
            Block::Permission,
            Block::ReleasePressed,
            Block::GoBinds,
            Block::EscapeBind,
        ] {
            undo_block(&mut ui, env, &mut hypr, b, &chords);
        }
        undo_firewall(&mut ui, env);
    } else {
        plugin::step(&mut ui, env);
        step_unit(&mut ui, env);
        step_block(&mut ui, env, &mut hypr, Block::EscapeBind, &chords);
        step_block(&mut ui, env, &mut hypr, Block::GoBinds, &chords);
        step_block(&mut ui, env, &mut hypr, Block::ReleasePressed, &chords);
        step_block(&mut ui, env, &mut hypr, Block::Permission, &chords);
        step_firewall(&mut ui, env);
    }

    let t = &ui.tally;
    let summary = format!(
        "\n{} done, {} skipped, {} declined, {} not applied, {} printed only, {} failed",
        t.done, t.skipped, t.declined, t.pending, t.printed, t.failed
    );
    let failed = t.failed > 0 || mode == Mode::NoTerminal;
    ui.say(summary);
    u8::from(failed)
}

// --- the unit ----------------------------------------------------------------------------------

fn unit_state(env: &Env) -> Result<(bool, bool), SetupError> {
    let enabled = systemctl(env, &["is-enabled", "ferryd"])?.stdout == "enabled";
    let active = systemctl(env, &["is-active", "ferryd"])?.stdout == "active";
    Ok((enabled, active))
}

fn step_unit(ui: &mut Ui, env: &Env) {
    ui.header("ferryd.service");
    let out = unit_step(ui, env);
    ui.finish(out);
}

fn unit_step(ui: &mut Ui, env: &Env) -> Out {
    let (enabled, active) = match unit_state(env) {
        Ok(s) => s,
        Err(e) => return Out::Failed(format!("systemctl: {e}")),
    };
    let packaged = env.packaged_unit.is_file();
    let user_path = env.user_unit();
    let user_text = match fsx::read_text(&user_path) {
        Ok(t) => t,
        // not ours (setup writes UTF-8): the user's own, left alone like any other
        Err(ReadError::NotUtf8) => Some(String::new()),
        Err(e) => return Out::Failed(format!("{}: {e}", ui.tilde(&user_path))),
    };
    let desired = unit::render(&env.ferryd);
    let write_unit = !packaged
        && match &user_text {
            None => true,
            Some(t) => unit::is_ours(t) && *t != desired,
        };
    // A unit setup wrote for an unpackaged build still shadows the package's: the user's config
    // directory wins over /usr/lib. Setup retires it, then the package's unit takes over.
    let retire = packaged && user_text.as_deref().is_some_and(unit::is_ours);
    if enabled && active && !write_unit && !retire {
        let shadows = packaged && user_text.is_some();
        return Out::Skipped(if shadows {
            format!(
                "ferryd.service is enabled and active, from your own {}, which shadows the packaged unit (delete it to use the package's)",
                ui.tilde(&user_path)
            )
        } else {
            "ferryd.service is enabled and active".into()
        });
    }
    if !packaged && write_unit && !env.ferryd.is_file() {
        return Out::Failed(format!(
            "ferryd is not next to ferry ({}) and no packaged unit is installed",
            ui.tilde(&env.ferryd)
        ));
    }
    ui.say(format!(
        "  ferryd.service is {}, {}.",
        if enabled { "enabled" } else { "not enabled" },
        if active { "active" } else { "not active" }
    ));
    ui.say("  will do:");
    if write_unit {
        let label = format!(
            "{} ({})",
            ui.tilde(&user_path),
            if user_text.is_some() {
                "update"
            } else {
                "new file"
            }
        );
        ui.diff(&label, user_text.as_deref(), Some(&desired));
        ui.say("  $ systemctl --user daemon-reload");
    } else if retire {
        ui.say("  $ systemctl --user disable --now ferryd");
        ui.say(format!(
            "  delete {} (setup wrote it for a build without a package; it shadows the packaged unit)",
            ui.tilde(&user_path)
        ));
        ui.say("  $ systemctl --user daemon-reload");
    } else if let Some(t) = &user_text
        && !unit::is_ours(t)
    {
        ui.say(format!(
            "  (using your own {}; it is not touched{})",
            ui.tilde(&user_path),
            if packaged {
                " and it shadows the packaged unit: delete it to use the package's"
            } else {
                ""
            }
        ));
    } else if packaged {
        ui.say(format!(
            "  (using the packaged unit {})",
            ui.tilde(&env.packaged_unit)
        ));
    }
    ui.say("  $ systemctl --user enable --now ferryd");
    match ui.confirm("apply?", true) {
        None => return Out::Pending,
        Some(false) => return Out::Declined,
        Some(true) => {}
    }
    if retire {
        // Stop the old daemon and drop its wants/ link while the user copy still names the unit.
        if let Err(e) = checked(systemctl(env, &["disable", "--now", "ferryd"])) {
            return Out::Failed(format!("disable --now ferryd: {e}"));
        }
        if let Err(e) = std::fs::remove_file(&user_path) {
            return Out::Failed(format!("{}: {e}", ui.tilde(&user_path)));
        }
        if let Err(e) = checked(systemctl(env, &["daemon-reload"])) {
            return Out::Failed(format!("daemon-reload: {e}"));
        }
    }
    if write_unit {
        if let Err(e) = fsx::write_atomic(&user_path, desired.as_bytes()) {
            return Out::Failed(format!("{}: {e}", ui.tilde(&user_path)));
        }
        if let Err(e) = checked(systemctl(env, &["daemon-reload"])) {
            return Out::Failed(format!("daemon-reload: {e}"));
        }
    }
    if let Err(e) = checked(systemctl(env, &["enable", "--now", "ferryd"])) {
        return Out::Failed(format!("enable --now ferryd: {e}"));
    }
    Out::Done("ferryd.service is enabled and started".into())
}

fn checked(r: Result<Cmd, SetupError>) -> Result<(), SetupError> {
    let c = r?;
    if c.code == 0 {
        Ok(())
    } else {
        Err(SetupError::CommandFailed {
            code: c.code,
            stderr: c.stderr,
        })
    }
}

fn undo_unit(ui: &mut Ui, env: &Env) {
    ui.header("ferryd.service");
    let out = undo_unit_step(ui, env);
    ui.finish(out);
}

fn undo_unit_step(ui: &mut Ui, env: &Env) -> Out {
    let (enabled, active) = match unit_state(env) {
        Ok(s) => s,
        Err(e) => return Out::Failed(format!("systemctl: {e}")),
    };
    let user_path = env.user_unit();
    let ours = match fsx::read_text(&user_path) {
        Ok(t) => t.filter(|t| unit::is_ours(t)),
        // setup writes UTF-8: a unit that is not is not setup's
        Err(ReadError::NotUtf8) => None,
        Err(e) => return Out::Failed(e.to_string()),
    };
    if !enabled && !active && ours.is_none() {
        return Out::Skipped("ferryd.service is not enabled and setup wrote no unit".into());
    }
    ui.say("  will do:");
    ui.say("  $ systemctl --user disable --now ferryd");
    if ours.is_some() {
        ui.say(format!(
            "  delete {} (setup wrote it)",
            ui.tilde(&user_path)
        ));
        ui.say("  $ systemctl --user daemon-reload");
    }
    match ui.confirm("apply?", true) {
        None => return Out::Pending,
        Some(false) => return Out::Declined,
        Some(true) => {}
    }
    if let Err(e) = checked(systemctl(env, &["disable", "--now", "ferryd"])) {
        return Out::Failed(format!("disable --now ferryd: {e}"));
    }
    if ours.is_some() {
        if let Err(e) = std::fs::remove_file(&user_path) {
            return Out::Failed(format!("{}: {e}", user_path.display()));
        }
        if let Err(e) = checked(systemctl(env, &["daemon-reload"])) {
            return Out::Failed(format!("daemon-reload: {e}"));
        }
    }
    Out::Done("ferryd.service is disabled and stopped".into())
}

// --- Hyprland: ferry.lua and the require line ------------------------------------------------------

/// What the three Hyprland steps share: the running instance (or why there is none), and a
/// `ferry.lua` that is not ours, which stops every one of them.
struct HyprCtx {
    inst: Result<Instance, SetupError>,
}

impl HyprCtx {
    fn new(env: &Env) -> Self {
        Self {
            inst: env.instance(),
        }
    }
}

/// What makes a `hl.permission` change take effect.
const RESTART_HYPRLAND: &str = "log out and back in (or restart Hyprland) once";

fn title(b: Block) -> &'static str {
    match b {
        Block::EscapeBind => "escape bind (SUPER + CTRL + ESCAPE returns input from any capture)",
        Block::GoBinds => {
            "go binds (the go chord and an arrow key move to the machine on that side)"
        }
        Block::ReleasePressed => {
            "release_pressed_on_close (clients see key-ups when a virtual keyboard dies)"
        }
        Block::Permission => {
            "input-capture permission (only when ecosystem:enforce_permissions is on)"
        }
    }
}

/// Is the block's effect live in the running Hyprland? `None` when no live state tells.
fn live(inst: &Instance, b: Block, chords: &Chords) -> Result<Option<bool>, SetupError> {
    match b {
        Block::EscapeBind => hypr::has_capture_bind(inst).map(Some),
        Block::GoBinds => hypr::has_go_binds(inst, &chords.go).map(Some),
        Block::ReleasePressed => {
            hypr::option_on(inst, "input:virtualkeyboard:release_pressed_on_close").map(Some)
        }
        Block::Permission => {
            // Not enforced means there is nothing to allow: "live" in the sense of done.
            hypr::option_on(inst, "ecosystem:enforce_permissions")
                .map(|on| if on { None } else { Some(true) })
        }
    }
}

fn snippet(b: Block, chord: &Chords) -> String {
    b.text(chord)
}

/// The block's Lua, indented under the line that explained why it is only printed.
fn say_snippet(ui: &mut Ui, b: Block, chord: &Chords) {
    for l in snippet(b, chord).lines() {
        ui.say(format!("    {l}"));
    }
}

fn step_block(ui: &mut Ui, env: &Env, ctx: &mut HyprCtx, b: Block, chord: &Chords) {
    ui.header(title(b));
    let out = block_step(ui, env, ctx, b, chord);
    ui.finish(out);
}

struct Edit<'a> {
    hypr_path: &'a Path,
    ferry_path: &'a Path,
    /// ferry.lua goes first (so a `require` never points at nothing); else it goes last.
    creating: bool,
    /// `ferry.lua` before and after; `None` is "absent".
    ferry_before: Option<String>,
    ferry_after: Option<String>,
    /// `hyprland.lua` before and after (the same when it is not edited).
    hypr_before: String,
    hypr_after: String,
}

fn block_step(ui: &mut Ui, env: &Env, ctx: &mut HyprCtx, b: Block, chord: &Chords) -> Out {
    let inst = match &ctx.inst {
        Ok(i) => i.clone(),
        Err(e) => {
            ui.say(format!("  no running Hyprland ({e}). Put this in ~/.config/hypr/ferry.lua, loaded from hyprland.lua with {}:", lua::REQUIRE));
            say_snippet(ui, b, chord);
            return Out::Printed("no running Hyprland".into());
        }
    };
    let state = match live(&inst, b, chord) {
        Ok(s) => s,
        Err(e) => return Out::Failed(format!("Hyprland IPC: {e}")),
    };
    if state == Some(true) {
        let why = match b {
            Block::EscapeBind => "Hyprland has a bind with allow_input_capture",
            Block::GoBinds => "Hyprland has the four go binds",
            Block::ReleasePressed => "release_pressed_on_close is on",
            Block::Permission => "ecosystem:enforce_permissions is off",
        };
        return Out::Skipped(why.into());
    }
    let (ferry_path, hypr_path) = (env.ferry_lua(), fsx::resolve(&env.hyprland_lua()));
    // A file that is not UTF-8 is never ours (setup writes UTF-8) and is never rewritten: setup
    // would write the text back with the user's bytes replaced.
    let ferry_text = match fsx::read_text(&ferry_path) {
        Ok(t) => t,
        Err(ReadError::NotUtf8) => {
            ui.say(format!(
                "  {} is not valid UTF-8 and `ferry setup` did not write it, so it is left alone. Add this to it:",
                ui.tilde(&ferry_path)
            ));
            say_snippet(ui, b, chord);
            return Out::Printed("a foreign ferry.lua".into());
        }
        Err(e) => return Out::Failed(format!("{}: {e}", ui.tilde(&ferry_path))),
    };
    if let Some(t) = &ferry_text
        && !lua::is_ours(t)
    {
        ui.say(format!(
            "  {} exists and `ferry setup` did not write it, so it is left alone. Add this to it:",
            ui.tilde(&ferry_path)
        ));
        say_snippet(ui, b, chord);
        return Out::Printed("a foreign ferry.lua".into());
    }
    let hypr_text = match fsx::read_text(&hypr_path) {
        Ok(Some(t)) => t,
        Ok(None) => {
            ui.say(format!(
                "  {} does not exist, so there is nothing to load ferry.lua from. Put this in a file Hyprland loads:",
                ui.tilde(&env.hyprland_lua())
            ));
            say_snippet(ui, b, chord);
            return Out::Printed("no hyprland.lua".into());
        }
        Err(ReadError::NotUtf8) => {
            ui.say(format!(
                "  {} is not valid UTF-8, so it is left byte for byte as it is. Put this in ~/.config/hypr/ferry.lua, loaded from it with {}:",
                ui.tilde(&hypr_path),
                lua::REQUIRE
            ));
            say_snippet(ui, b, chord);
            return Out::Printed("hyprland.lua is not UTF-8".into());
        }
        Err(e) => return Out::Failed(format!("{}: {e}", ui.tilde(&hypr_path))),
    };
    // A go-binds block written for another go chord is replaced.
    let stale = ferry_text.as_deref().is_some_and(|t| {
        b == Block::GoBinds && lua::has_block(t, b) && !t.contains(&b.text(chord))
    });
    let base = if stale {
        ferry_text.as_deref().map(|t| lua::remove_block(t, b))
    } else {
        ferry_text.clone()
    };
    let in_file = base.as_deref().is_some_and(|t| lua::has_block(t, b));
    let needs_require = !lua::requires_ferry(&hypr_text);
    if in_file && !needs_require {
        return Out::Skipped(if b == Block::Permission {
            format!(
                "{} has the block already; Hyprland reads it only when it starts, so if crossings do not start: {RESTART_HYPRLAND}",
                ui.tilde(&ferry_path)
            )
        } else {
            format!(
                "{} has the block already; Hyprland does not show it yet (try `hyprctl reload`)",
                ui.tilde(&ferry_path)
            )
        });
    }
    let ferry_after = if in_file {
        ferry_text.clone()
    } else {
        Some(match &base {
            Some(t) => lua::add_block(t, b, chord),
            None => lua::compose(&[b], chord),
        })
    };
    let (hypr_after, _) = if needs_require {
        lua::append_require(&hypr_text)
    } else {
        (hypr_text.clone(), String::new())
    };
    ui.say("  will do:");
    if !in_file {
        let label = format!(
            "{} ({})",
            ui.tilde(&ferry_path),
            if stale {
                "replace the go binds"
            } else if ferry_text.is_some() {
                "append"
            } else {
                "new file"
            }
        );
        ui.diff(&label, ferry_text.as_deref(), ferry_after.as_deref());
    }
    if needs_require {
        let linked = if hypr_path != env.hyprland_lua() {
            "; the link points here, the link itself stays".to_string()
        } else {
            String::new()
        };
        let label = format!("{} (append{linked})", ui.tilde(&hypr_path));
        ui.diff(&label, Some(&hypr_text), Some(&hypr_after));
    }
    match ui.confirm("apply?", true) {
        None => return Out::Pending,
        Some(false) => {
            ui.say("  (to add it yourself, put the lines above in ~/.config/hypr/ferry.lua)");
            return Out::Declined;
        }
        Some(true) => {}
    }
    let edit = Edit {
        hypr_path: &hypr_path,
        ferry_path: &ferry_path,
        creating: true,
        ferry_before: ferry_text,
        ferry_after,
        hypr_before: hypr_text,
        hypr_after,
    };
    let want_live = |inst: &Instance| live(inst, b, chord).map(|s| s == Some(true));
    apply_edit(ui, env, &inst, &edit, b, want_live)
}

/// The files an edit has started to write; only those are restored.
#[derive(Default, Clone, Copy)]
struct Touched {
    hypr: bool,
    ferry: bool,
}

/// Writes the edit, waits for Hyprland to load it, and rolls it back if Hyprland reports errors.
/// `edit.creating`: ferry.lua goes first (so a `require` never points at nothing); else it goes last.
fn apply_edit(
    ui: &mut Ui,
    env: &Env,
    inst: &Instance,
    edit: &Edit,
    b: Block,
    is_live: impl Fn(&Instance) -> Result<bool, SetupError>,
) -> Out {
    let Edit {
        hypr_path,
        ferry_path,
        creating,
        ..
    } = *edit;
    let errors_before = hypr::config_errors(inst).unwrap_or_default();
    // what Hyprland shows now: a rollback is done when it shows this again
    let live_before = is_live(inst).ok();
    let hypr_changed = edit.hypr_after != edit.hypr_before;
    let ferry_changed = edit.ferry_before != edit.ferry_after;
    let mut hypr_backup = None;
    let mut ferry_backup = None;
    if hypr_changed {
        match fsx::backup(hypr_path, env.unix_time) {
            Ok(p) => {
                ui.say(format!("  backup: {}", ui.tilde(&p)));
                hypr_backup = Some(p);
            }
            Err(e) => return Out::Failed(format!("backup of {}: {e}", ui.tilde(hypr_path))),
        }
    }
    if edit.ferry_before.is_some() && ferry_changed {
        match fsx::backup(ferry_path, env.unix_time) {
            Ok(p) => {
                ui.say(format!("  backup: {}", ui.tilde(&p)));
                ferry_backup = Some(p);
            }
            Err(e) => return Out::Failed(format!("backup of {}: {e}", ui.tilde(ferry_path))),
        }
    }
    // `None` is "absent": the file goes
    let write_ferry = |text: &Option<String>| -> std::io::Result<()> {
        match text {
            Some(t) => fsx::write_atomic(ferry_path, t.as_bytes()),
            None => match std::fs::remove_file(ferry_path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        }
    };
    let write_hypr = |t: &str| fsx::write_atomic(hypr_path, t.as_bytes());
    let mut touched = Touched::default();
    let result = (|| {
        if creating {
            if ferry_changed {
                touched.ferry = true;
                write_ferry(&edit.ferry_after)?;
            }
            if hypr_changed {
                touched.hypr = true;
                write_hypr(&edit.hypr_after)?;
            }
        } else {
            if hypr_changed {
                touched.hypr = true;
                write_hypr(&edit.hypr_after)?;
            }
            if ferry_changed {
                touched.ferry = true;
                write_ferry(&edit.ferry_after)?;
            }
        }
        Ok::<(), std::io::Error>(())
    })();

    // Puts back the files the writes touched, last written first, and says which could not be.
    // The Hyprland reload that follows is `settle`'s.
    let rollback = |ui: &mut Ui| -> bool {
        let restore_ferry = |ui: &mut Ui| match write_ferry(&edit.ferry_before) {
            Ok(()) => true,
            Err(e) => {
                let path = ui.tilde(ferry_path);
                ui.say(match &ferry_backup {
                    Some(p) => format!(
                        "  could not restore {path}; your original is {} ({e})",
                        ui.tilde(p)
                    ),
                    None => format!(
                        "  could not remove {path}, which this step created ({e}); delete it yourself"
                    ),
                });
                false
            }
        };
        let restore_hypr = |ui: &mut Ui| match write_hypr(&edit.hypr_before) {
            Ok(()) => true,
            Err(e) => {
                let path = ui.tilde(hypr_path);
                let original = hypr_backup.as_deref().map(|p| ui.tilde(p));
                ui.say(format!(
                    "  could not restore {path}; your original is {} ({e})",
                    original.unwrap_or_else(|| "in your own copy".into())
                ));
                false
            }
        };
        let mut all = true;
        if creating {
            if touched.hypr {
                all &= restore_hypr(ui);
            }
            if touched.ferry {
                all &= restore_ferry(ui);
            }
        } else {
            if touched.ferry {
                all &= restore_ferry(ui);
            }
            if touched.hypr {
                all &= restore_hypr(ui);
            }
        }
        if all {
            ui.say("  restored the files from before this step");
        }
        all
    };
    // After a rollback Hyprland still runs the config it failed on: ask it to reload the old
    // files and wait until it shows the old state.
    let settle = |ui: &mut Ui, restored: bool| {
        if !restored {
            return;
        }
        let back = || {
            if b == Block::Permission {
                return Ok(true);
            }
            is_live(inst).map(|on| live_before.is_none_or(|before| on == before))
        };
        if !hypr::reload_restored(inst, &errors_before, env.hypr_wait, back) {
            ui.say("  Hyprland has not loaded the restored files yet; `hyprctl reload` does");
        }
    };
    if let Err(e) = result {
        let restored = rollback(ui);
        settle(ui, restored);
        return Out::Failed(format!("write: {e}"));
    }
    let started = std::time::Instant::now();
    let settled = env.hypr_wait.min(Duration::from_millis(800));
    let verdict = hypr::wait_for_reload(inst, &errors_before, env.hypr_wait, || {
        live_or_settled(inst, b, &is_live, started, settled, creating)
    });
    match verdict {
        // Hyprland adds `hl.permission` rules only on its first config parse and never drops them
        // on a reload, so the permission step is live only after Hyprland restarts.
        hypr::Reload::Live if b == Block::Permission => Out::Done(if creating {
            format!("written; Hyprland reads hl.permission only when it starts: {RESTART_HYPRLAND}")
        } else {
            format!("removed; Hyprland keeps allowing xdph until it restarts: {RESTART_HYPRLAND}")
        }),
        hypr::Reload::Live => Out::Done(if creating {
            "Hyprland loaded it".into()
        } else {
            "Hyprland no longer has it".into()
        }),
        hypr::Reload::NotSeen => Out::Done(
            "written; Hyprland loaded the config without errors but does not show the change yet (try `hyprctl reload`)".into(),
        ),
        hypr::Reload::Errors(e) => {
            let restored = rollback(ui);
            settle(ui, restored);
            Out::Failed(if restored {
                format!("Hyprland reports a config error, so the change was undone:\n{e}")
            } else {
                format!("Hyprland reports a config error and the change could not be undone:\n{e}")
            })
        }
    }
}

/// `is_live` for blocks that have a live signal; for the permission block, which has none, "live"
/// is "no error for a moment".
fn live_or_settled(
    inst: &Instance,
    b: Block,
    is_live: &impl Fn(&Instance) -> Result<bool, SetupError>,
    started: std::time::Instant,
    settle: Duration,
    creating: bool,
) -> Result<bool, SetupError> {
    if b == Block::Permission {
        return Ok(started.elapsed() >= settle);
    }
    let on = is_live(inst)?;
    Ok(if creating { on } else { !on })
}

fn undo_block(ui: &mut Ui, env: &Env, ctx: &mut HyprCtx, b: Block, chords: &Chords) {
    ui.header(title(b));
    let out = undo_block_inner(ui, env, ctx, b, chords);
    ui.finish(out);
}

fn undo_block_inner(ui: &mut Ui, env: &Env, ctx: &mut HyprCtx, b: Block, chords: &Chords) -> Out {
    let (ferry_path, hypr_path) = (env.ferry_lua(), fsx::resolve(&env.hyprland_lua()));
    let ferry_text = match fsx::read_text(&ferry_path) {
        Ok(Some(t)) if lua::is_ours(&t) => t,
        // setup writes UTF-8, so a file that is not UTF-8 is not setup's
        Ok(Some(_)) | Err(ReadError::NotUtf8) => {
            return Out::Skipped("ferry.lua is not setup's".into());
        }
        Ok(None) => return Out::Skipped("setup wrote no ferry.lua".into()),
        Err(e) => return Out::Failed(format!("{}: {e}", ui.tilde(&ferry_path))),
    };
    if !lua::has_block(&ferry_text, b) {
        return Out::Skipped("not in ferry.lua".into());
    }
    let after = lua::remove_block(&ferry_text, b);
    let last = lua::is_empty(&after);
    let hypr_text = match fsx::read_text(&hypr_path) {
        Ok(t) => t.unwrap_or_default(),
        Err(ReadError::NotUtf8) => {
            // the line that loads ferry.lua cannot be taken out without rewriting the file, and
            // ferry.lua must not go while the line stays
            ui.say(format!(
                "  {} is not valid UTF-8, so setup leaves it as it is. Remove the {} line yourself, then delete {}",
                ui.tilde(&hypr_path),
                lua::REQUIRE,
                ui.tilde(&ferry_path)
            ));
            return Out::Printed("hyprland.lua is not UTF-8".into());
        }
        Err(e) => return Out::Failed(format!("{}: {e}", ui.tilde(&hypr_path))),
    };
    // The file goes only when the line that loads it can go too.
    let hypr_after = if last && lua::requires_ferry(&hypr_text) {
        lua::strip_require(&hypr_text)
    } else {
        Some(hypr_text.clone())
    };
    let delete_file = last && hypr_after.is_some();
    if last && hypr_after.is_none() {
        ui.say("  the line that loads ferry.lua in hyprland.lua is not the one setup added; remove it yourself, then delete ferry.lua");
    }
    ui.say("  will do:");
    let label = format!(
        "{} ({})",
        ui.tilde(&ferry_path),
        if delete_file {
            "delete"
        } else {
            "remove the block"
        }
    );
    ui.diff(
        &label,
        Some(&ferry_text),
        if delete_file { None } else { Some(&after) },
    );
    let hypr_after = hypr_after.unwrap_or_else(|| hypr_text.clone());
    if hypr_after != hypr_text {
        let label = format!("{} (remove the line setup added)", ui.tilde(&hypr_path));
        ui.diff(&label, Some(&hypr_text), Some(&hypr_after));
    }
    match ui.confirm("apply?", true) {
        None => return Out::Pending,
        Some(false) => return Out::Declined,
        Some(true) => {}
    }
    let edit = Edit {
        hypr_path: &hypr_path,
        ferry_path: &ferry_path,
        creating: false,
        ferry_before: Some(ferry_text),
        ferry_after: if delete_file { None } else { Some(after) },
        hypr_before: hypr_text,
        hypr_after,
    };
    match &ctx.inst {
        Ok(inst) => {
            let inst = inst.clone();
            apply_edit(ui, env, &inst, &edit, b, |i| {
                live(i, b, chords).map(|s| s == Some(true))
            })
        }
        Err(_) => {
            // No Hyprland to ask: write the files and say so.
            let w = (|| {
                if edit.hypr_after != edit.hypr_before {
                    fsx::backup(&hypr_path, env.unix_time)?;
                    fsx::write_atomic(&hypr_path, edit.hypr_after.as_bytes())?;
                }
                fsx::backup(&ferry_path, env.unix_time)?;
                match &edit.ferry_after {
                    Some(t) => fsx::write_atomic(&ferry_path, t.as_bytes()),
                    None => std::fs::remove_file(&ferry_path),
                }
            })();
            match w {
                Ok(()) => Out::Done("removed (no running Hyprland to reload)".into()),
                Err(e) => Out::Failed(e.to_string()),
            }
        }
    }
}

// --- the firewall ------------------------------------------------------------------------------

/// The port ferryd listens on: `[peer] port` of `config.toml`, else the default. The one reader
/// of it in the CLI, so `setup` and `doctor` name the same port.
pub fn config_port(env: &Env) -> u16 {
    fsx::read_text(&env.config_home.join("ferry/config.toml"))
        .ok()
        .flatten()
        .and_then(|t| toml::from_str::<toml::Value>(&t).ok())
        .and_then(|v| v.get("peer")?.get("port")?.as_integer())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(crate::DEFAULT_PORT)
}

/// ferryd has taken an inbound connection since it started (`status` counter).
fn inbound_proven(env: &Env) -> bool {
    use ferry_core::api::{Request, StatusResult};
    let Ok(mut c) = crate::client::Client::connect(&env.socket) else {
        return false;
    };
    c.call(Request::Status)
        .ok()
        .and_then(|v| serde_json::from_value::<StatusResult>(v).ok())
        .is_some_and(|s| s.counters.get("inbound_accepted").copied().unwrap_or(0) > 0)
}

fn step_firewall(ui: &mut Ui, env: &Env) {
    ui.header("firewall (ufw)");
    let out = firewall_step(ui, env);
    ui.finish(out);
}

fn firewall_step(ui: &mut Ui, env: &Env) -> Out {
    let conf = fsx::read_lossy(&env.ufw_conf)
        .ok()
        .flatten()
        .unwrap_or_default();
    if !fw::ufw_enabled(&conf) {
        return Out::Skipped("ufw is not enabled".into());
    }
    if inbound_proven(env) {
        return Out::Skipped("ferryd has accepted an inbound connection since it started".into());
    }
    let port = config_port(env);
    let profiles: Vec<&Path> = env.ufw_profiles.iter().map(PathBuf::as_path).collect();
    let cmd = fw::allow_command(&profiles, port);
    ui.say("  ufw is on and no peer has connected in yet.");
    ui.say("  will do (needs sudo; only if a peer must dial this machine):");
    ui.say(format!("  $ sudo {}", cmd.join(" ")));
    if ui.mode == Mode::Yes {
        ui.say("  sudo is never run by --yes");
        return Out::Skipped(format!(
            "not run; run `sudo {}` yourself if a peer must reach this machine",
            cmd.join(" ")
        ));
    }
    match ui.confirm("run it?", false) {
        None => return Out::Pending,
        Some(false) => return Out::Declined,
        Some(true) => {}
    }
    let words: Vec<&str> = cmd.iter().map(String::as_str).collect();
    match checked(run_cmd(&env.sudo, &words)) {
        Ok(()) => Out::Done(format!("sudo {}", cmd.join(" "))),
        Err(e) => Out::Failed(format!("sudo {}: {e}", cmd.join(" "))),
    }
}

fn undo_firewall(ui: &mut Ui, env: &Env) {
    ui.header("firewall (ufw)");
    let conf = fsx::read_lossy(&env.ufw_conf)
        .ok()
        .flatten()
        .unwrap_or_default();
    let out = if fw::ufw_enabled(&conf) {
        let profiles: Vec<&Path> = env.ufw_profiles.iter().map(PathBuf::as_path).collect();
        let cmd = fw::delete_command(&profiles, config_port(env));
        ui.say(format!(
            "  if setup's rule is there, remove it with: sudo {}",
            cmd.join(" ")
        ));
        Out::Printed("setup never runs sudo to undo".into())
    } else {
        Out::Skipped("ufw is not enabled".into())
    };
    ui.finish(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_at(root: &Path) -> Env {
        Env {
            home: root.to_path_buf(),
            config_home: root.join(".config"),
            runtime_dir: None,
            hypr_signature: None,
            data_home: root.join(".local/share"),
            systemctl: "systemctl".into(),
            sudo: "sudo".into(),
            pacman: "pacman".into(),
            busctl: "busctl".into(),
            ufw_conf: root.join("ufw.conf"),
            ufw_profiles: vec![],
            packaged_unit: root.join("none"),
            plugin_source: root.join("none-plugin"),
            omarchy_path: root.join("none-omarchy"),
            omarchy_shell: root.join("none-omarchy-shell"),
            ferryd: root.join("ferryd"),
            socket: root.join("ferryd.sock"),
            interactive: false,
            hypr_wait: Duration::from_millis(10),
            unix_time: 0,
        }
    }

    /// "no Hyprland running" and "Hyprland answered garbage" are different variants.
    #[test]
    fn a_missing_hyprland_is_a_variant_of_its_own() {
        let d = tempfile::tempdir().unwrap();
        let mut env = env_at(d.path());
        assert!(matches!(env.instance(), Err(SetupError::NoRuntimeDir)));
        env.runtime_dir = Some(d.path().to_path_buf());
        assert!(matches!(
            env.instance(),
            Err(SetupError::Hyprland(ferry_hypr_ipc::Error::NoInstance(_)))
        ));
        env.hypr_signature = Some("nope".into());
        assert!(matches!(
            env.instance(),
            Err(SetupError::Hyprland(
                ferry_hypr_ipc::Error::UnknownInstance(_)
            ))
        ));
    }

    /// Each way `config.toml` can be wrong has its own variant.
    #[test]
    fn a_config_toml_that_is_wrong_says_how() {
        let d = tempfile::tempdir().unwrap();
        let env = env_at(d.path());
        let dir = env.config_home.join("ferry");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        let chord = |text: &[u8]| {
            std::fs::write(&file, text).unwrap();
            env.escape_chord()
        };
        assert!(chord(b"[crossing]\nescape_chord = \"Ctrl+Alt+Delete\"\n").is_ok());
        assert!(matches!(
            chord(b"[crossing\n"),
            Err(SetupError::Toml { .. })
        ));
        assert!(matches!(
            chord(b"[crossing]\nescape_chord = \"Escape\"\n"),
            Err(SetupError::EscapeChord { .. })
        ));
        assert!(matches!(
            chord(b"[crossing]\nescape_chord = 7\n"),
            Err(SetupError::EscapeChordNotText(_))
        ));
        assert!(matches!(
            chord(b"# caf\xe9\n"),
            Err(SetupError::Read {
                source: ReadError::NotUtf8,
                ..
            })
        ));
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(matches!(
            env.escape_chord(),
            Err(SetupError::Read {
                source: ReadError::Io(_),
                ..
            })
        ));
    }

    /// A program that cannot start and one that fails are different variants.
    #[test]
    fn a_command_that_cannot_start_is_not_one_that_failed() {
        let nowhere = Path::new("/nonexistent/ferry-test-program");
        assert!(matches!(
            run_cmd(nowhere, &[]),
            Err(SetupError::Spawn { .. })
        ));
        let failed = checked(run_cmd(Path::new("false"), &[]));
        assert!(matches!(
            failed,
            Err(SetupError::CommandFailed { code: 1, .. })
        ));
        assert!(checked(run_cmd(Path::new("true"), &[])).is_ok());
    }

    /// `$OMARCHY_PATH` counts when it is an absolute path, as in Omarchy's own Hyprland config.
    #[test]
    fn omarchy_path_is_the_variable_or_the_package_location() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(
            omarchy_path_from(p("/home/me/omarchy")),
            PathBuf::from("/home/me/omarchy")
        );
        for unset in [None, p(""), p("omarchy")] {
            assert_eq!(
                omarchy_path_from(unset),
                PathBuf::from("/usr/share/omarchy")
            );
        }
    }

    /// The shell hard-codes `~/.config/omarchy`, and so does the plugin step.
    #[test]
    fn the_shells_files_are_under_the_home_not_the_xdg_directory() {
        let d = tempfile::tempdir().unwrap();
        let mut env = env_at(d.path());
        env.config_home = d.path().join("elsewhere");
        assert_eq!(
            env.shell_json(),
            d.path().join(".config/omarchy/shell.json")
        );
        assert_eq!(
            env.plugin_link(),
            d.path().join(".config/omarchy/plugins/ferry.bar")
        );
    }

    /// No home is ferryd's error, whatever the XDG variables say; the XDG ones count only
    /// when absolute.
    #[test]
    fn the_home_rules_are_ferryds() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert!(matches!(
            home_dirs(None, p("/x/config"), p("/x/data")),
            Err(SetupError::NoHome)
        ));
        assert!(matches!(
            home_dirs(p("relative"), None, None),
            Err(SetupError::NoHome)
        ));
        let (home, config, data) = home_dirs(p("/h"), None, None).unwrap();
        assert_eq!(
            (home, config, data),
            ("/h".into(), "/h/.config".into(), "/h/.local/share".into())
        );
        let (_, config, data) = home_dirs(p("/h"), p("/x/config"), p("relative")).unwrap();
        assert_eq!(
            (config, data),
            ("/x/config".into(), "/h/.local/share".into())
        );
    }
}
