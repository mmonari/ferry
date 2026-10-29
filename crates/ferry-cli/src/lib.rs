//! The `ferry` command line: a client of ferryd's socket API.
//!
//! `run` takes the arguments, the socket path and two writers, and returns the exit code
//! (FR-CLI-04: 0 success, 1 failure, 2 usage error, 3 ferryd unreachable), so tests drive it
//! in process.

#![forbid(unsafe_code)]

pub mod cli;
pub mod client;
pub mod config;
pub mod doctor;
pub mod fingerprint;
pub mod pair;
pub mod render;
pub mod setup;
pub mod ui;

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

use clap::Parser;
use ferry_core::api::{
    AlignName, AlignParam, ConfigGetParams, ConfigSetParams, ConfigSetResult, ConfigValues,
    GoParams, IdResult, LayoutResult, LockParams, LockResult, OriginParam, PeersResult,
    PlaceParams, Request, SetParams, StatusResult, TrustParams, TrustResult,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use cli::{Cli, Command, LayoutCommand, exit};
use client::{Client, ClientError};
use setup::SetupError;

/// The port a peer listens on when `config.toml` does not say. The one source in the
/// CLI; a test holds it equal to `ferry_net::DEFAULT_PORT`, which ferryd binds.
pub const DEFAULT_PORT: u16 = 4243;

/// Runs one command. Output goes to `out`, errors to `err`; `ferry pair` and `ferry unpair`
/// ask on the process's terminal.
pub fn run<I, T>(args: I, socket: &Path, out: &mut dyn Write, err: &mut dyn Write) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let env = setup::Env::from_process();
    // Not `stdin().lock()`: `ferry pair` reads stdin on its own thread.
    let mut stdin = std::io::BufReader::new(std::io::stdin());
    run_inner(
        args,
        socket,
        env.as_ref(),
        &mut stdin,
        Box::new(pair::Stdin),
        out,
        err,
    )
}

/// [`run`], with the answers to `pair` and `unpair` read from `input` (tests script them).
pub fn run_with<I, T>(
    args: I,
    socket: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
    input: Box<dyn pair::Input>,
) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let env = setup::Env::from_process();
    run_inner(
        args,
        socket,
        env.as_ref(),
        &mut std::io::empty(),
        input,
        out,
        err,
    )
}

/// [`run`] with everything `setup` touches given, and the input its questions read.
pub fn run_in<I, T>(
    args: I,
    socket: &Path,
    env: &setup::Env,
    input: &mut dyn std::io::BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    run_inner(
        args,
        socket,
        Ok(env),
        input,
        Box::new(pair::Stdin),
        out,
        err,
    )
}

/// One command: `setup` reads `setup_input`, `pair` and `unpair` read `pair_input`. `env` is an
/// error when the process has no home directory: only `setup` and `doctor` need one, and they
/// say so.
fn run_inner<I, T>(
    args: I,
    socket: &Path,
    env: Result<&setup::Env, &SetupError>,
    setup_input: &mut dyn std::io::BufRead,
    pair_input: Box<dyn pair::Input>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let text = e.render().to_string();
            let code = e.exit_code();
            if e.use_stderr() {
                let _ = write!(err, "{text}");
            } else {
                let _ = write!(out, "{text}");
            }
            return u8::try_from(code).unwrap_or(exit::USAGE);
        }
    };
    // `setup` and `doctor` read and write under the home directory; the others never do.
    let local_env = |err: &mut dyn Write| {
        env.map_err(|e| {
            if cli.json {
                let body = serde_json::json!({ "code": "no_home", "message": e.to_string() });
                let _ = writeln!(err, "{}", serde_json::json!({ "error": body }));
            } else {
                let _ = writeln!(err, "error: {e}");
            }
            exit::FAILURE
        })
    };
    if let Command::Doctor = &cli.command {
        let env = match local_env(err) {
            Ok(env) => env,
            Err(code) => return code,
        };
        // Works with ferryd down: the CLI's own checks still run.
        return doctor::run(socket, env, cli.json, out);
    }
    if let Command::Ui(a) = &cli.command {
        // Local like `setup`, but it asks the shell, not ferryd, and has no use for a home
        // directory: a launcher entry may run without one.
        let shell = match env {
            Ok(env) => ui::Shell::from_env(env),
            Err(_) => ui::Shell::from_process(),
        };
        return match ui::open(a.view, &shell) {
            Ok(()) => exit::OK,
            Err(e) => {
                report(err, cli.json, e.code(), &e.to_string(), e.hint());
                exit::FAILURE
            }
        };
    }
    if let Command::Setup(a) = &cli.command {
        let env = match local_env(err) {
            Ok(env) => env,
            Err(code) => return code,
        };
        // Local work: ferryd need not be running (setup is what starts it).
        let args = setup::Args {
            yes: a.yes,
            dry_run: a.dry_run,
            undo: a.undo,
        };
        let code = setup::run(&args, env, setup_input, out);
        // Setup ends with the doctor's summary. A dry run, an undo and a run that
        // had nobody to ask changed nothing, so there is nothing new to check.
        if !a.dry_run && !a.undo && (a.yes || env.interactive) {
            let _ = writeln!(out, "\n[doctor]");
            doctor::run(socket, env, false, out);
        }
        return code;
    }
    match execute(&cli, socket, out, pair_input) {
        Ok(()) => exit::OK,
        // the reader of our output went away (`ferry status | head -1`): done, as `cat` is
        Err(e) if e.is_broken_pipe() => exit::OK,
        Err(e) if cli.json => {
            // FR-CLI-03/04 under --json: one JSON line on stderr, `{"error":{code, message,
            // hint?}}`, the same shape as the socket API's errors; the exit code is unchanged.
            let (code, hint) = match &e {
                ClientError::Api(api) => (
                    serde_json::to_value(api.code)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "internal".into()),
                    api.hint.clone(),
                ),
                ClientError::NotRunning(_)
                | ClientError::Stale(_)
                | ClientError::Unreachable { .. } => ("ferryd_unreachable".into(), None),
                ClientError::ApiVersion { .. } => ("api_version".into(), None),
                ClientError::Protocol(_) | ClientError::Io(_) => ("protocol".into(), None),
                ClientError::Timeout(_) => ("timeout".into(), None),
                ClientError::Output(_) => ("output".into(), None),
                ClientError::Usage { hint, .. } => ("usage".into(), Some(hint.clone())),
                ClientError::Failed { code, .. } => (code.clone(), None),
            };
            let mut body = serde_json::json!({ "code": code, "message": e.to_string() });
            if let Some(h) = hint {
                body["hint"] = Value::String(h);
            }
            let _ = writeln!(err, "{}", serde_json::json!({ "error": body }));
            e.exit_code()
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            if let ClientError::Api(api) = &e
                && let Some(hint) = &api.hint
            {
                let _ = writeln!(err, "  hint: {hint}");
            }
            if let ClientError::Usage { hint, .. } = &e {
                let _ = writeln!(err, "  hint: {hint}");
            }
            e.exit_code()
        }
    }
}

/// A failure that is not ferryd's, on `err`: `error: …` and an indented `hint: …`, or under
/// `--json` the one line `{"error":{code, message, hint?}}` the socket API's errors have.
fn report(err: &mut dyn Write, json: bool, code: &str, message: &str, hint: Option<&str>) {
    if json {
        let mut body = serde_json::json!({ "code": code, "message": message });
        if let Some(h) = hint {
            body["hint"] = Value::String(h.to_string());
        }
        let _ = writeln!(err, "{}", serde_json::json!({ "error": body }));
    } else {
        let _ = writeln!(err, "error: {message}");
        if let Some(h) = hint {
            let _ = writeln!(err, "  hint: {h}");
        }
    }
}

pub(crate) fn typed<T: DeserializeOwned>(v: Value) -> Result<T, ClientError> {
    serde_json::from_value(v)
        .map_err(|e| ClientError::Protocol(format!("ferryd's answer has an unexpected shape: {e}")))
}

fn execute(
    cli: &Cli,
    socket: &Path,
    out: &mut dyn Write,
    input: Box<dyn pair::Input>,
) -> Result<(), ClientError> {
    // the questions need a terminal: a usage error before anything is asked of ferryd
    match &cli.command {
        Command::Pair(_) if !cli.json && !input.is_terminal() => {
            return Err(ClientError::Usage {
                message: "ferry pair asks you to compare codes and needs a terminal".into(),
                hint: "scripts use the socket methods pair.start or pair.accept, then pair.answer"
                    .into(),
            });
        }
        Command::Unpair(u) if !u.yes && !input.is_terminal() => {
            return Err(ClientError::Usage {
                message: "ferry unpair asks first and needs a terminal".into(),
                hint: "pass --yes to unpair without asking".into(),
            });
        }
        _ => {}
    }
    let mut client = Client::connect(socket)?;
    match &cli.command {
        Command::Pair(p) => return pair::pair(&mut client, p, cli.json, out, input),
        Command::Unpair(u) => return pair::unpair(&mut client, u, cli.json, out, input),
        Command::Config(c) => return config::run(&mut client, c, cli.json, out),
        // raw event lines are JSON whatever the flag says
        Command::Events(e) => return client.watch(&e.topics, e.count, out),
        _ => {}
    }
    let mut show =
        |raw: &Value, text: &dyn Fn() -> Result<String, ClientError>| -> Result<(), ClientError> {
            if cli.json {
                writeln!(out, "{raw}")?;
            } else {
                write!(out, "{}", text()?)?;
            }
            Ok(())
        };
    match &cli.command {
        Command::Status => {
            let raw = client.call(Request::Status)?;
            show(&raw, &|| {
                Ok(render::status(&typed::<StatusResult>(raw.clone())?))
            })
        }
        Command::Release => {
            let raw = client.call(Request::Release)?;
            show(&raw, &|| {
                Ok("Released: input is back on this machine.\n".to_string())
            })
        }
        Command::Id => {
            let raw = client.call(Request::Id)?;
            show(&raw, &|| Ok(render::id(&typed::<IdResult>(raw.clone())?)))
        }
        Command::Trust(t) => {
            let raw = client.call(Request::Trust(TrustParams {
                fingerprint: t.fingerprint.clone(),
                name: t.name.clone(),
                addr: t.addr.clone(),
            }))?;
            show(&raw, &|| {
                let TrustResult { peer } = typed(raw.clone())?;
                Ok(render::trusted(
                    &peer.name,
                    &peer.fingerprint,
                    t.addr.as_deref(),
                ))
            })
        }
        Command::Layout(LayoutCommand::Show) => {
            let raw = client.call(Request::LayoutGet)?;
            show(&raw, &|| {
                Ok(render::layout(&typed::<LayoutResult>(raw.clone())?))
            })
        }
        Command::Setup(_) | Command::Doctor | Command::Ui(_) => {
            unreachable!("handled before the connection")
        }
        Command::Layout(LayoutCommand::Place(p)) => {
            let (rel, anchor) = p.side();
            let raw = client.call(Request::LayoutPlace(PlaceParams {
                peer: p.peer.clone(),
                rel,
                anchor: anchor.to_string(),
                // `AlignParam::Default` cannot be serialised (see the report): say `start`,
                // which is what the daemon takes an omitted align to mean.
                align: p.align.unwrap_or(AlignParam::Named(AlignName::Start)),
            }))?;
            show(&raw, &|| {
                Ok(render::layout(&typed::<LayoutResult>(raw.clone())?))
            })
        }
        Command::Layout(LayoutCommand::Set(a)) => {
            // One origin and no `base_rev`: this is a person typing, not a panel with a draft.
            let raw = client.call(Request::LayoutSet(SetParams {
                origins: vec![OriginParam {
                    peer: a.peer.clone(),
                    x: a.x,
                    y: a.y,
                }],
                base_rev: None,
            }))?;
            show(&raw, &|| {
                Ok(render::layout(&typed::<LayoutResult>(raw.clone())?))
            })
        }
        Command::Go { side } => {
            // Answers at once: a side with no machine does nothing, and ferryd logs why.
            let raw = client.call(Request::Go(GoParams { side: side.side() }))?;
            show(&raw, &|| Ok(String::new()))
        }
        Command::Lock { to } => {
            let raw = client.call(Request::Lock(LockParams { on: to.param() }))?;
            show(&raw, &|| {
                Ok(render::lock(&typed::<LockResult>(raw.clone())?))
            })
        }
        Command::Clipboard { to } => {
            const KEY: &str = "clipboard.enabled";
            let on = match to.param() {
                Some(on) => on,
                None => {
                    let raw = client.call(Request::ConfigGet(ConfigGetParams {
                        key: Some(KEY.into()),
                    }))?;
                    typed::<ConfigValues>(raw)?.values.get(KEY) != Some(&Value::Bool(true))
                }
            };
            let raw = client.call(Request::ConfigSet(ConfigSetParams {
                key: KEY.into(),
                value: Value::Bool(on),
            }))?;
            show(&raw, &|| {
                Ok(render::config_set(&typed::<ConfigSetResult>(raw.clone())?))
            })
        }
        Command::Peers => {
            let raw = client.call(Request::Peers)?;
            show(&raw, &|| {
                Ok(render::peers(&typed::<PeersResult>(raw.clone())?))
            })
        }
        Command::Pair(_) | Command::Unpair(_) | Command::Config(_) | Command::Events(_) => {
            unreachable!("handled above")
        }
    }
}
