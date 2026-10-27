//! Hyprland's event socket (`.socket2.sock`): one `name>>data` line per event.
//!
//! The stream is non-blocking and exposes its fd, so a caller polls it next to
//! its own fds (calloop, tokio's `AsyncFd`, `poll`) and then drains it with
//! [`EventStream::try_next_event`].

use std::io::Read;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;

use crate::{Error, Instance, Result};

/// One line of the event socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    pub data: String,
}

impl Event {
    /// Parse `name>>data`. A line without the separator has no data.
    pub fn parse(line: &str) -> Self {
        let line = line.trim_end_matches(['\r', '\n']);
        match line.split_once(">>") {
            Some((name, data)) => Self {
                name: name.to_owned(),
                data: data.to_owned(),
            },
            None => Self {
                name: line.to_owned(),
                data: String::new(),
            },
        }
    }

    /// True for the events after which the monitor layout has to be read
    /// again: a monitor added or removed, or the config reloaded.
    ///
    /// Hyprland 0.56.2 emits **nothing** for a mode, scale or position change
    /// made through `hl.monitor` (checked on 2026-10-16); only the shell's
    /// layers churn. A caller that must catch those too also needs another
    /// signal, such as `wl_output.done`.
    pub fn affects_monitor_layout(&self) -> bool {
        matches!(
            self.name.as_str(),
            "monitoradded"
                | "monitoraddedv2"
                | "monitorremoved"
                | "monitorremovedv2"
                | "configreloaded"
        )
    }
}

/// Splits a byte stream into lines. Partial lines wait for the next read.
#[derive(Debug, Default)]
struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    fn next_line(&mut self) -> Option<String> {
        let end = self.pending.iter().position(|&b| b == b'\n')?;
        let line: Vec<u8> = self.pending.drain(..=end).collect();
        Some(String::from_utf8_lossy(&line[..end]).into_owned())
    }
}

/// A connection to the event socket of one instance.
#[derive(Debug)]
pub struct EventStream {
    stream: UnixStream,
    lines: LineBuffer,
}

impl EventStream {
    pub(crate) fn connect(instance: &Instance) -> Result<Self> {
        let stream = UnixStream::connect(instance.events_socket_path())?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            lines: LineBuffer::default(),
        })
    }

    /// The next complete event, or `Ok(None)` when none is available yet.
    /// A closed socket (the compositor is gone) is an error.
    pub fn try_next_event(&mut self) -> Result<Option<Event>> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(line) = self.lines.next_line() {
                return Ok(Some(Event::parse(&line)));
            }
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Hyprland closed the event socket",
                    )));
                }
                Ok(n) => self.lines.feed(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl AsFd for EventStream {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_events() {
        assert_eq!(
            Event::parse("monitoraddedv2>>1,HEADLESS-1,desc\n"),
            Event {
                name: "monitoraddedv2".into(),
                data: "1,HEADLESS-1,desc".into()
            }
        );
        // Data may itself contain the separator.
        assert_eq!(Event::parse("x>>a>>b").data, "a>>b");
        assert_eq!(Event::parse("configreloaded>>").data, "");
        assert_eq!(Event::parse("bare").name, "bare");
    }

    #[test]
    fn layout_events_are_the_monitor_ones() {
        let hit = |n: &str| Event::parse(&format!("{n}>>x")).affects_monitor_layout();
        for n in [
            "monitoradded",
            "monitoraddedv2",
            "monitorremoved",
            "monitorremovedv2",
            "configreloaded",
        ] {
            assert!(hit(n), "{n}");
        }
        for n in ["openlayer", "focusedmon", "workspace", "createworkspace"] {
            assert!(!hit(n), "{n}");
        }
    }

    #[test]
    fn reads_whole_lines_across_partial_writes() {
        let (a, mut b) = UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        let mut stream = EventStream {
            stream: a,
            lines: LineBuffer::default(),
        };
        assert_eq!(stream.try_next_event().unwrap(), None);
        b.write_all(b"monitoradded>>HEAD").unwrap();
        assert_eq!(stream.try_next_event().unwrap(), None);
        b.write_all(b"LESS-1\nconfigreloaded>>\nlayer").unwrap();
        let first = stream.try_next_event().unwrap().unwrap();
        assert_eq!(
            (first.name.as_str(), first.data.as_str()),
            ("monitoradded", "HEADLESS-1")
        );
        assert_eq!(
            stream.try_next_event().unwrap().unwrap().name,
            "configreloaded"
        );
        assert_eq!(stream.try_next_event().unwrap(), None);
        drop(b);
        assert!(stream.try_next_event().is_err());
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// Any line the compositor (or a bug in it) writes parses into a name and data without
        /// panicking, and `name>>data` round-trips when the name holds no separator.
        #[test]
        fn event_parse_never_panics(line in "\\PC{0,64}[\r\n]{0,2}") {
            let _ = Event::parse(&line);
        }

        #[test]
        fn an_event_parses_back_from_name_and_data(name in "[a-z0-9]{1,16}", data in "[^\r\n]{0,40}") {
            let e = Event::parse(&format!("{name}>>{data}\n"));
            prop_assert_eq!(e, Event { name, data });
        }

        /// Arbitrary bytes, cut into arbitrary chunks: no panic, one line per newline, and the
        /// bytes that follow the last newline stay pending.
        #[test]
        fn line_buffer_never_panics_and_yields_one_line_per_newline(
            bytes in proptest::collection::vec(any::<u8>(), 0..512),
            cuts in proptest::collection::vec(1usize..64, 1..16),
        ) {
            let mut buf = LineBuffer::default();
            let mut lines = 0;
            let mut rest = &bytes[..];
            for cut in cuts.iter().cycle() {
                if rest.is_empty() {
                    break;
                }
                let (head, tail) = rest.split_at((*cut).min(rest.len()));
                buf.feed(head);
                while buf.next_line().is_some() {
                    lines += 1;
                }
                rest = tail;
            }
            prop_assert_eq!(lines, bytes.iter().filter(|&&b| b == b'\n').count());
            let tail = bytes.iter().rposition(|&b| b == b'\n').map_or(bytes.len(), |i| bytes.len() - i - 1);
            prop_assert_eq!(buf.pending.len(), tail);
        }

        /// Valid lines survive any chunking unchanged.
        #[test]
        fn line_buffer_returns_the_lines_that_were_fed(
            lines in proptest::collection::vec("[^\n]{0,24}", 0..8),
            cut in 1usize..16,
        ) {
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            let mut buf = LineBuffer::default();
            let mut got = Vec::new();
            for chunk in text.as_bytes().chunks(cut) {
                buf.feed(chunk);
                while let Some(l) = buf.next_line() {
                    got.push(l);
                }
            }
            prop_assert_eq!(got, lines);
        }
    }
}
