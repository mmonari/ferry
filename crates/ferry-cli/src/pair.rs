//! `ferry pair` and `ferry unpair`: the commands that ask the user.
//!
//! The pairing runs on one socket connection: `pair.start` or `pair.accept`, then the
//! `pairing` events of that connection, and `pair.answer` with the user's answer. Closing the
//! connection (Ctrl+C) cancels it. The question needs a terminal and defaults to No.

use std::io::{IsTerminal, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ferry_core::PeerId;
use ferry_core::api::{
    CancelledBy, ErrorCode, IdResult, LinkState, PairAcceptResult, PairAnswerParams,
    PairStartParams, PairingEvent, PairingStage, Request, StatusResult, UnpairParams, UnpairResult,
};

use crate::cli::{PairArgs, UnpairArgs};
use crate::client::{Client, ClientError};
use crate::render::plain;
use crate::{DEFAULT_PORT, typed};

/// Where the answers to the questions come from.
pub trait Input: Send {
    /// A person can answer (stdin is a terminal).
    fn is_terminal(&self) -> bool;
    /// The terminal shows what the person types, so the answer is not written out again.
    fn echoes(&self) -> bool {
        self.is_terminal()
    }
    /// One line without its newline; `None` at the end of input.
    fn read_line(&mut self) -> Option<String>;
}

/// The process's stdin.
pub struct Stdin;

impl Input for Stdin {
    fn is_terminal(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn read_line(&mut self) -> Option<String> {
        let mut s = String::new();
        match std::io::stdin().read_line(&mut s) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(s.trim_end_matches(['\n', '\r']).to_string()),
        }
    }
}

/// Lines from `input`, read on their own thread so a question can wait for the answer and for
/// ferryd's events at once. Started at the first question.
struct Answers {
    input: Option<Box<dyn Input>>,
    rx: Option<mpsc::Receiver<Option<String>>>,
    /// A terminal echoes what the user types; anything else gets the answer written out, so
    /// the output reads as the same transcript.
    echoes: bool,
}

impl Answers {
    fn new(input: Box<dyn Input>) -> Self {
        Self {
            echoes: input.echoes(),
            input: Some(input),
            rx: None,
        }
    }

    fn rx(&mut self) -> &mpsc::Receiver<Option<String>> {
        if self.rx.is_none() {
            let (tx, rx) = mpsc::channel();
            let mut input = self.input.take().expect("started once");
            std::thread::spawn(move || {
                loop {
                    let line = input.read_line();
                    let end = line.is_none();
                    if tx.send(line).is_err() || end {
                        return;
                    }
                }
            });
            self.rx = Some(rx);
        }
        self.rx.as_ref().expect("just set")
    }

    /// Lines typed before the question are not its answer.
    fn drain(&mut self) {
        while let Ok(Some(_)) = self.rx().try_recv() {}
    }

    /// The answer, if it came: `Some(true)` for y or yes; anything else, or the end of input,
    /// is No. What ends the prompt's line goes to `out`.
    fn poll(&mut self, out: &mut dyn Write) -> std::io::Result<Option<bool>> {
        let echoes = self.echoes;
        Ok(match self.rx().try_recv() {
            Ok(Some(line)) => {
                if !echoes {
                    writeln!(out, "{line}")?;
                }
                Some(matches!(
                    line.trim().to_ascii_lowercase().as_str(),
                    "y" | "yes"
                ))
            }
            Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                writeln!(out)?;
                Some(false)
            }
            Err(mpsc::TryRecvError::Empty) => None,
        })
    }
}

/// `9c2e:41aa:0b7d:5512`.
fn short(fp: &str) -> String {
    PeerId::parse(fp).map_or_else(|_| fp.chars().take(16).collect(), |p| p.short())
}

/// `418 207`. Anything but six ASCII characters is shown as it came (C-14: six bytes of
/// two-byte characters have no boundary at 3).
fn spaced(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

/// The address without its port, for "Request from desktop at 192.0.2.111".
fn host_of(addr: &str) -> String {
    match addr.parse::<std::net::SocketAddr>() {
        Ok(a) => a.ip().to_string(),
        Err(_) => addr.to_string(),
    }
}

/// `host[:port]` with the default port written out, as it is dialled.
fn with_port(addr: &str) -> String {
    let bare_v6 = addr.matches(':').count() > 1 && !addr.starts_with('[');
    if bare_v6 {
        format!("[{addr}]:{DEFAULT_PORT}")
    } else if addr.starts_with('[') && addr.ends_with(']') || !addr.contains(':') {
        format!("{addr}:{DEFAULT_PORT}")
    } else {
        addr.to_string()
    }
}

/// The line for an attempt that ended without a pin. The `reason` decides the wording, never the
/// text of the message: a rewording in ferryd must not turn a cancel into a
/// failure. The reasons that are somebody's decision or a clock have their line here; for any
/// other the daemon's message is the detail.
fn failure_line(ev: &PairingEvent) -> String {
    match ev.reason.as_deref() {
        Some("rejected") => "Pairing cancelled: codes differ.".into(),
        Some("cancelled" | "other") => "Pairing cancelled.".into(),
        Some("timeout") => "Pairing timed out.".into(),
        Some("bad_reveal") => {
            "Pairing cancelled: the peer broke the protocol (its reveal does not match its commitment)."
                .into()
        }
        Some("protocol") => "Pairing cancelled: the peer broke the protocol.".into(),
        _ => format!(
            "Pairing failed: {}.",
            plain(ev.message.as_deref().unwrap_or("failed"))
        ),
    }
}

/// A cancel on this machine that `ferry pair` did not send (it cancels only by closing its
/// connection, which it then no longer reads): the panel or another terminal stopped it. That is
/// the user's decision, not a failure, so the command ends cleanly.
fn cancelled_elsewhere(ev: &PairingEvent) -> bool {
    ev.reason.as_deref() == Some("cancelled") && ev.cancelled_by == Some(CancelledBy::Local)
}

fn failed(ev: &PairingEvent) -> ClientError {
    ClientError::Failed {
        code: format!("pairing_{}", ev.reason.as_deref().unwrap_or("failed")),
        message: failure_line(ev),
    }
}

/// `ferry pair <host>` and `ferry pair --accept`.
pub fn pair(
    client: &mut Client,
    args: &PairArgs,
    json: bool,
    out: &mut dyn Write,
    input: Box<dyn Input>,
) -> Result<(), ClientError> {
    let mut answers = Answers::new(input);
    let acceptor = args.accept;
    if acceptor {
        let port = typed::<IdResult>(client.call(Request::Id)?)?.port;
        let window = typed::<PairAcceptResult>(client.call(Request::PairAccept)?)?;
        if !json {
            writeln!(
                out,
                "Accepting one pairing request for {} s on UDP {port}. Ctrl+C stops.",
                window.expires_ms / 1000
            )?;
        }
    } else {
        let host = args.host.as_deref().unwrap_or_default();
        if !json {
            writeln!(out, "Asking {}...", with_port(host))?;
            out.flush()?;
        }
        client.call(Request::PairStart(PairStartParams {
            addr: host.to_string(),
            name: args.name.clone(),
        }))?;
    }

    let mut asking = false;
    loop {
        // the user's answer, while a question is open
        if asking && let Some(yes) = answers.poll(out)? {
            asking = false;
            match client.call(Request::PairAnswer(PairAnswerParams { matches: yes })) {
                Ok(_) => {}
                // The attempt ended while the user typed (the peer said no, cancelled or timed
                // out): its `failed` event is on its way, or already read, and says why.
                Err(ClientError::Api(e))
                    if matches!(e.code, ErrorCode::NoPairing | ErrorCode::PairingExpired) => {}
                Err(e) => return Err(e),
            }
        }
        let Some(ev) = client.next_event(Duration::from_millis(100))? else {
            continue;
        };
        if json {
            writeln!(out, "{}", serde_json::to_string(&ev).unwrap_or_default())?;
        }
        // ferryd already replaced a peer's name with control characters; escaping again
        // here keeps the terminal safe whatever the daemon sends.
        let peer_name = ev.peer.as_ref().map_or_else(
            || "the other machine".to_string(),
            |p| plain(&p.name).into_owned(),
        );
        match ev.stage {
            PairingStage::Connecting | PairingStage::Listening if json => {}
            PairingStage::Connecting => {}
            PairingStage::Listening => {
                writeln!(
                    out,
                    "Still accepting for {} s. Ctrl+C stops.",
                    ev.expires_ms / 1000
                )?;
            }
            PairingStage::Code => {
                let (Some(peer), Some(code)) = (&ev.peer, &ev.code) else {
                    continue;
                };
                if !json {
                    if acceptor {
                        writeln!(
                            out,
                            "Request from {} at {}, key {}",
                            peer_name,
                            peer.addr.as_deref().map_or_else(|| "?".into(), host_of),
                            short(&peer.fingerprint)
                        )?;
                    } else {
                        writeln!(
                            out,
                            "{} answered, key {}",
                            peer_name,
                            short(&peer.fingerprint)
                        )?;
                    }
                    writeln!(out, "\n    Pairing code   {}\n", spaced(code))?;
                    write!(
                        out,
                        "Confirm only if {peer_name} shows this same code. Same code? [y/N] "
                    )?;
                    out.flush()?;
                }
                answers.drain();
                asking = true;
            }
            // The question is moot once this side has answered, and that may have happened in the
            // Pair panel or another terminal (FR-PAIR-13: the first answer counts).
            PairingStage::WaitingPeer if !json => {
                if std::mem::take(&mut asking) {
                    writeln!(out)?;
                }
                writeln!(out, "Waiting for {peer_name}...")?;
            }
            PairingStage::WaitingPeer => asking = false,
            PairingStage::Paired => {
                let fp = ev
                    .peer
                    .as_ref()
                    .map(|p| short(&p.fingerprint))
                    .unwrap_or_default();
                if json {
                    return Ok(());
                }
                if let Some(m) = &ev.message {
                    writeln!(out, "Note: {}.", plain(m))?;
                }
                if acceptor {
                    writeln!(out, "Paired with {peer_name} ({fp}).")?;
                    return Ok(());
                }
                write!(out, "Paired with {peer_name} ({fp}). Connecting...")?;
                out.flush()?;
                let full = ev.peer.as_ref().map(|p| p.fingerprint.clone());
                let until = Instant::now() + Duration::from_secs(5);
                let connected = loop {
                    let st = typed::<StatusResult>(client.call(Request::Status)?)?;
                    let up = st.peers.iter().any(|p| {
                        Some(p.fingerprint.as_str()) == full.as_deref()
                            && p.state == LinkState::Connected
                    });
                    if up || Instant::now() >= until {
                        break up;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                };
                if connected {
                    writeln!(out, " connected.")?;
                } else {
                    writeln!(out, " not yet: `ferry status` shows the link.")?;
                }
                return Ok(());
            }
            PairingStage::Failed | PairingStage::Closed if cancelled_elsewhere(&ev) => {
                if std::mem::take(&mut asking) && !json {
                    writeln!(out)?;
                }
                if !json {
                    writeln!(out, "Pairing cancelled elsewhere.")?;
                }
                return Ok(());
            }
            PairingStage::Failed => {
                if std::mem::take(&mut asking) && !json {
                    // the question ends unanswered
                    writeln!(out)?;
                }
                if !acceptor {
                    return Err(failed(&ev));
                }
                if !json {
                    writeln!(out, "{}", failure_line(&ev))?;
                }
            }
            PairingStage::Closed => {
                if std::mem::take(&mut asking) && !json {
                    writeln!(out)?;
                }
                return Err(ClientError::Failed {
                    code: format!("pairing_{}", ev.reason.as_deref().unwrap_or("closed")),
                    message: ev.message.as_deref().map_or_else(
                        || "the pairing window closed".into(),
                        |m| plain(m).into_owned(),
                    ),
                });
            }
        }
    }
}

/// `ferry unpair <peer> [--yes]`.
pub fn unpair(
    client: &mut Client,
    args: &UnpairArgs,
    json: bool,
    out: &mut dyn Write,
    input: Box<dyn Input>,
) -> Result<(), ClientError> {
    if !args.yes {
        write!(
            out,
            "Unpair {}? It can no longer connect, and any crossing with it ends at once. [y/N] ",
            args.peer
        )?;
        out.flush()?;
        let mut answers = Answers::new(input);
        let yes = loop {
            if let Some(yes) = answers.poll(out)? {
                break yes;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !yes {
            return Err(ClientError::Failed {
                code: "declined".into(),
                message: format!("{} is still paired", args.peer),
            });
        }
    }
    let raw = client.call(Request::Unpair(UnpairParams {
        peer: args.peer.clone(),
    }))?;
    if json {
        writeln!(out, "{raw}")?;
    } else {
        let UnpairResult { peer } = typed(raw)?;
        writeln!(
            out,
            "Unpaired {} ({}). It is refused from now on; `ferry pair` pairs it again.",
            peer.name,
            short(&peer.fingerprint)
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_host_and_port_helpers_format_as_displayed() {
        assert_eq!(spaced("418207"), "418 207");
        assert_eq!(spaced("007003"), "007 003");
        assert_eq!(host_of("192.0.2.111:4243"), "192.0.2.111");
        assert_eq!(host_of("[::1]:4243"), "::1");
        assert_eq!(with_port("192.0.2.11"), "192.0.2.11:4243");
        assert_eq!(with_port("laptop:5000"), "laptop:5000");
        assert_eq!(with_port("fe80::1"), "[fe80::1]:4243");
        assert_eq!(with_port("[fe80::1]"), "[fe80::1]:4243");
        assert_eq!(with_port("[fe80::1]:5000"), "[fe80::1]:5000");
        assert_eq!(short(&"9c2e41aa".repeat(8)), "9c2e:41aa:9c2e:41aa");
    }

    /// Six bytes that are not six ASCII characters used to panic on the slice.
    #[test]
    fn spaced_keeps_a_code_that_is_not_six_ascii_characters() {
        assert_eq!(spaced("ééé"), "ééé");
        assert_eq!(spaced("12345"), "12345");
    }
}
