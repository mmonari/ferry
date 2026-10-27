//! A client of ferryd's socket: connect, check the `hello`, send requests, read the
//! matching response. Synchronous: the CLI does one thing and exits.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ferry_core::api::{
    API_VERSION, ApiError, EventLine, HelloData, MAX_LINE, PairingEvent, Request, RequestLine,
    SubscribeParams, Topic,
};
use serde_json::Value;

use crate::cli::exit;

/// Waiting for ferryd: a `trust` answers only after the swap is live, which is quick.
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No socket, or nothing answers it.
    #[error("ferryd is not running (no socket at {})\n  start it with: systemctl --user start ferryd", .0.display())]
    NotRunning(PathBuf),
    #[error("ferryd is not running (the socket {} is stale: nothing answers it)\n  start it with: systemctl --user start ferryd", .0.display())]
    Stale(PathBuf),
    #[error("cannot reach ferryd at {}: {source}", path.display())]
    Unreachable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The peer on the socket did not speak the protocol.
    #[error("{0}")]
    Protocol(String),
    #[error(
        "ferryd speaks socket API {theirs} (version {version}), this ferry speaks {ours}\n  update the older of the two"
    )]
    ApiVersion {
        ours: u32,
        theirs: u32,
        version: String,
    },
    /// ferryd answered with an error.
    #[error("{}", .0.message)]
    Api(ApiError),
    /// The command cannot run this way (`ferry pair` without a terminal): exit 2.
    #[error("{message}")]
    Usage { message: String, hint: String },
    /// The command ran and did not succeed (a pairing cancelled, an unpair declined): exit 1.
    /// `code` is what `--json` reports.
    #[error("{message}")]
    Failed { code: String, message: String },
    /// ferryd did not answer in time: it is running (the socket took the connection) and busy or
    /// stuck, which is not a lost connection.
    #[error(
        "ferryd did not answer within {}\n  see what it is doing with: journalctl --user -u ferryd",
        show_duration(*.0)
    )]
    Timeout(Duration),
    /// Writing the command's own output failed (`ferry status | head -1` closes the pipe). Not a
    /// ferryd error: a broken pipe ends the command quietly ([`ClientError::is_broken_pipe`]).
    /// This is what `?` on an `io::Error` becomes, so every socket call below maps its errors
    /// with [`socket_error`] instead of `?`.
    #[error("cannot write the output: {0}")]
    Output(#[from] io::Error),
    /// The socket failed.
    #[error("lost the connection to ferryd: {0}")]
    Io(#[source] io::Error),
}

/// `10 s`, or `200 ms` for a timeout under a second or not a whole number of seconds.
fn show_duration(d: Duration) -> String {
    if d.subsec_millis() == 0 && d.as_secs() > 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{} ms", d.as_millis())
    }
}

/// A socket error as the CLI reports it: the read or write timeout expiring is `Timeout`.
fn socket_error(timeout: Duration) -> impl Fn(io::Error) -> ClientError {
    move |e| match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => ClientError::Timeout(timeout),
        _ => ClientError::Io(e),
    }
}

impl ClientError {
    /// The reader of the command's output went away: the command is done, as `cat` is.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Output(e) if e.kind() == io::ErrorKind::BrokenPipe)
    }

    /// FR-CLI-04.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::NotRunning(_) | Self::Stale(_) | Self::Unreachable { .. } => exit::UNREACHABLE,
            Self::Usage { .. } => exit::USAGE,
            _ => exit::FAILURE,
        }
    }
}

/// `$XDG_RUNTIME_DIR/ferry/ferryd.sock`, or under `/run/user/<uid>` without the variable
/// (the same rule as ferryd's).
pub fn socket_path_from(runtime_dir: Option<PathBuf>) -> PathBuf {
    let dir = runtime_dir.filter(|p| p.is_absolute()).unwrap_or_else(|| {
        PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
    });
    dir.join("ferry/ferryd.sock")
}

pub fn socket_path() -> PathBuf {
    socket_path_from(std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
}

pub struct Client {
    /// How long a read or write on the socket may take (a field so a test can shorten it).
    timeout: Duration,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    pub hello: HelloData,
    /// The hello as ferryd wrote it, newline included (`ferry events` prints it).
    hello_line: Vec<u8>,
    /// `pairing` events that came while a response was awaited.
    events: VecDeque<PairingEvent>,
    /// The start of a line that a timed-out read left behind.
    partial: Vec<u8>,
}

impl Client {
    pub fn connect(path: &Path) -> Result<Self, ClientError> {
        Self::connect_with(path, TIMEOUT)
    }

    /// [`connect`](Self::connect) with another limit on how long ferryd may take to answer.
    pub fn connect_with(path: &Path, timeout: Duration) -> Result<Self, ClientError> {
        let io = socket_error(timeout);
        let stream = UnixStream::connect(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => ClientError::NotRunning(path.to_path_buf()),
            io::ErrorKind::ConnectionRefused => ClientError::Stale(path.to_path_buf()),
            _ => ClientError::Unreachable {
                path: path.to_path_buf(),
                source: e,
            },
        })?;
        stream.set_read_timeout(Some(timeout)).map_err(&io)?;
        stream.set_write_timeout(Some(timeout)).map_err(&io)?;
        let writer = stream.try_clone().map_err(&io)?;
        let mut reader = BufReader::new(stream);

        let mut line = Vec::new();
        if read_line(&mut reader, &mut line, timeout)? == 0 {
            return Err(ClientError::Protocol(
                "ferryd closed the connection without a hello (is this user allowed to use the socket?)".into(),
            ));
        }
        let hello = match serde_json::from_slice::<EventLine>(&line) {
            Ok(EventLine::Hello(h)) => h,
            Ok(_) | Err(_) => {
                return Err(ClientError::Protocol(format!(
                    "{} did not start with a ferryd hello",
                    path.display()
                )));
            }
        };
        if hello.api != API_VERSION {
            return Err(ClientError::ApiVersion {
                ours: API_VERSION,
                theirs: hello.api,
                version: hello.version,
            });
        }
        Ok(Self {
            timeout,
            reader,
            writer,
            next_id: 1,
            hello,
            hello_line: line,
            events: VecDeque::new(),
            partial: Vec::new(),
        })
    }

    /// `ferry events`: writes ferryd's hello to `out`, subscribes to `topics` (every topic when
    /// there are none), and writes every line ferryd sends from then on, as it came, until the
    /// connection ends or `limit` events have been written. Each line is flushed.
    pub fn watch(
        &mut self,
        topics: &[Topic],
        limit: Option<usize>,
        out: &mut dyn Write,
    ) -> Result<(), ClientError> {
        out.write_all(&self.hello_line)?;
        out.flush()?;
        self.call(Request::Subscribe(SubscribeParams {
            topics: (!topics.is_empty()).then(|| topics.to_vec()),
        }))?;
        // the events come when they come: no limit on the wait
        self.reader
            .get_ref()
            .set_read_timeout(None)
            .map_err(socket_error(self.timeout))?;
        let mut line = std::mem::take(&mut self.partial);
        let mut written = 0usize;
        while limit.is_none_or(|n| written < n) {
            if read_line(&mut self.reader, &mut line, self.timeout)? == 0 {
                return Err(ClientError::Protocol("ferryd closed the connection".into()));
            }
            if line.last() != Some(&b'\n') {
                continue; // a line cut short; the next read completes it
            }
            out.write_all(&line)?;
            out.flush()?;
            line.clear();
            written += 1;
        }
        Ok(())
    }

    /// The next `pairing` event, waiting up to `timeout`; `Ok(None)` when none came. A
    /// pairing's events come on the connection that started it, between its responses.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<PairingEvent>, ClientError> {
        if let Some(ev) = self.events.pop_front() {
            return Ok(Some(ev));
        }
        let io = socket_error(self.timeout);
        let wait = timeout.max(Duration::from_millis(1));
        self.reader
            .get_ref()
            .set_read_timeout(Some(wait))
            .map_err(&io)?;
        // a read cut by the timeout keeps what it got in `partial` for the next call
        let read = read_line(&mut self.reader, &mut self.partial, wait);
        self.reader
            .get_ref()
            .set_read_timeout(Some(self.timeout))
            .map_err(&io)?;
        match read {
            Ok(_) if self.partial.last() == Some(&b'\n') => {
                let line = std::mem::take(&mut self.partial);
                match serde_json::from_slice::<EventLine>(&line) {
                    Ok(EventLine::Pairing(ev)) => Ok(Some(ev)),
                    _ => Ok(None),
                }
            }
            Ok(_) => Err(ClientError::Protocol(
                "ferryd closed the connection during the pairing".into(),
            )),
            // the wait this call asked for ran out: no event yet
            Err(ClientError::Timeout(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Sends `request` and returns its `result`, or the daemon's error.
    pub fn call(&mut self, request: Request) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_vec(&RequestLine { id, request })
            .map_err(|e| ClientError::Protocol(format!("cannot encode the request: {e}")))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .map_err(socket_error(self.timeout))?;

        // the start of a line a timed-out `next_event` left behind comes first
        let mut text = std::mem::take(&mut self.partial);
        loop {
            if read_line(&mut self.reader, &mut text, self.timeout)? == 0
                && text.last() != Some(&b'\n')
            {
                return Err(ClientError::Protocol(
                    "ferryd closed the connection before it answered".into(),
                ));
            }
            let line = std::mem::take(&mut text);
            let mut value: Value = serde_json::from_slice(&line).map_err(|e| {
                ClientError::Protocol(format!("ferryd sent a line that is not JSON: {e}"))
            })?;
            // events carry no id: keep a pairing's for `next_event`
            if value.get("event").is_some() {
                if let Ok(EventLine::Pairing(ev)) = serde_json::from_value::<EventLine>(value) {
                    self.events.push_back(ev);
                }
                continue;
            }
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get_mut("error") {
                let error = serde_json::from_value(error.take()).map_err(|e| {
                    ClientError::Protocol(format!("ferryd sent an unreadable error: {e}"))
                })?;
                return Err(ClientError::Api(error));
            }
            return value.get_mut("result").map(Value::take).ok_or_else(|| {
                ClientError::Protocol("ferryd's answer has neither result nor error".into())
            });
        }
    }
}

/// `read_until(b'\n')` that appends at most up to [`MAX_LINE`] bytes in `buf` (which may hold
/// the start of the line already) and fails past it, so a socket that streams bytes without a
/// newline cannot grow the CLI's memory without bound. A read that the socket's timeout
/// cuts is [`ClientError::Timeout`] with `timeout`.
fn read_line(
    reader: &mut impl BufRead,
    buf: &mut Vec<u8>,
    timeout: Duration,
) -> Result<usize, ClientError> {
    let room = (MAX_LINE + 1).saturating_sub(buf.len());
    let n = reader
        .by_ref()
        .take(room as u64)
        .read_until(b'\n', buf)
        .map_err(socket_error(timeout))?;
    if buf.len() > MAX_LINE && buf.last() != Some(&b'\n') {
        return Err(ClientError::Protocol(
            "ferryd sent a line over 1 MiB".into(),
        ));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket that sends 2 MiB without a newline fails after 1 MiB, quickly.
    #[test]
    fn a_line_over_1_mib_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ferryd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..32 {
                if s.write_all(&chunk).is_err() {
                    return;
                }
            }
        });
        let started = std::time::Instant::now();
        let e = Client::connect(&path).err().unwrap();
        assert!(
            matches!(&e, ClientError::Protocol(m) if m == "ferryd sent a line over 1 MiB"),
            "{e}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        server.join().unwrap();

        let mut long = std::io::Cursor::new(vec![b'y'; MAX_LINE + 10]);
        let mut buf = b"{".to_vec();
        assert!(read_line(&mut long, &mut buf, TIMEOUT).is_err());
        assert_eq!(buf.len(), MAX_LINE + 1);
        let mut ok = std::io::Cursor::new(b"{}\nnext".to_vec());
        let mut buf = Vec::new();
        assert_eq!(read_line(&mut ok, &mut buf, TIMEOUT).unwrap(), 3);
        assert_eq!(buf, b"{}\n");
    }

    #[test]
    fn socket_path_rules() {
        assert_eq!(
            socket_path_from(Some("/run/x".into())),
            PathBuf::from("/run/x/ferry/ferryd.sock")
        );
        let uid = rustix::process::getuid().as_raw();
        let fallback = PathBuf::from(format!("/run/user/{uid}/ferry/ferryd.sock"));
        assert_eq!(socket_path_from(None), fallback);
        assert_eq!(socket_path_from(Some("relative".into())), fallback);
    }

    #[test]
    fn a_missing_socket_is_exit_3() {
        // a directory that exists and holds no socket, so the name cannot collide with a real one
        let dir = tempfile::tempdir().unwrap();
        let e = Client::connect(&dir.path().join("ferryd.sock"))
            .err()
            .unwrap();
        assert!(matches!(e, ClientError::NotRunning(_)));
        assert_eq!(e.exit_code(), 3);
        assert!(e.to_string().contains("systemctl --user start ferryd"));
    }

    /// A ferryd that takes the connection and then says nothing is a timeout with a next
    /// step, not "lost the connection". The limit is a field, so the test does not wait 10 s.
    #[test]
    fn a_ferryd_that_never_answers_is_a_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ferryd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            // one that never says hello, then one that says it and never answers a request
            let (_silent, _) = listener.accept().unwrap();
            let (mut s, _) = listener.accept().unwrap();
            let hello = serde_json::json!({"event": "hello", "data": {"api": API_VERSION,
                "version": "0.1.0", "peer": "desk", "fingerprint": "00"}});
            writeln!(s, "{hello}").unwrap();
            let _ = held.recv();
        });
        let limit = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let e = Client::connect_with(&path, limit).err().unwrap();
        assert!(matches!(e, ClientError::Timeout(d) if d == limit), "{e}");
        assert_ne!(e.exit_code(), exit::UNREACHABLE);

        let mut client = Client::connect_with(&path, limit).unwrap();
        let e = client.call(Request::Status).unwrap_err();
        assert!(matches!(e, ClientError::Timeout(d) if d == limit), "{e}");
        let text = e.to_string();
        assert!(text.contains("did not answer within 200 ms"), "{text}");
        assert!(text.contains("journalctl --user -u ferryd"), "{text}");
        assert!(!text.contains("lost the connection"), "{text}");
        assert!(started.elapsed() < Duration::from_secs(5));
        // an event wait that runs out is `None`, not an error
        assert!(
            client
                .next_event(Duration::from_millis(20))
                .unwrap()
                .is_none()
        );
        release.send(()).unwrap();
        server.join().unwrap();
    }

    /// Only a failed write to the output is `Output`, and only a closed reader is quiet.
    #[test]
    fn a_write_error_is_an_output_error_and_a_broken_pipe_is_the_quiet_one() {
        let pipe = ClientError::from(io::Error::from(io::ErrorKind::BrokenPipe));
        assert!(pipe.is_broken_pipe(), "{pipe}");
        let full = ClientError::from(io::Error::from(io::ErrorKind::StorageFull));
        assert!(!full.is_broken_pipe());
        assert!(full.to_string().starts_with("cannot write the output"));
        let lost = socket_error(TIMEOUT)(io::Error::from(io::ErrorKind::ConnectionReset));
        assert!(matches!(lost, ClientError::Io(_)), "{lost}");
        assert!(!lost.is_broken_pipe());
    }

    use proptest::prelude::*;

    /// Serves one connection: the hello line, then `reply`, then closes.
    fn serve(path: &Path, hello: bool, reply: Vec<u8>) -> std::thread::JoinHandle<()> {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            if hello {
                let hello = serde_json::json!({"event": "hello", "data": {"api": API_VERSION,
                    "version": "0.1.0", "peer": "desk", "fingerprint": "00"}});
                let _ = writeln!(s, "{hello}");
            }
            let _ = s.write_all(&reply);
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// `read_line` takes any bytes: the line it returns ends at the first newline, and it
        /// leaves the rest unread.
        #[test]
        fn read_line_never_panics_and_stops_at_the_first_newline(
            bytes in proptest::collection::vec(any::<u8>(), 0..300),
            start in proptest::collection::vec(any::<u8>(), 0..8),
        ) {
            let mut reader = std::io::Cursor::new(bytes.clone());
            let mut buf = start.clone();
            let n = read_line(&mut reader, &mut buf, TIMEOUT).unwrap();
            prop_assert_eq!(&buf[..start.len()], &start[..]);
            prop_assert_eq!(buf.len() - start.len(), n);
            let first = bytes.iter().position(|&b| b == b'\n').map_or(bytes.len(), |i| i + 1);
            prop_assert_eq!(n, first);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// ferryd's socket is trusted but not infallible: a first line that is not a hello, and
        /// a reply that is not an answer, are errors, never panics.
        #[test]
        fn the_client_line_decoders_never_panic(
            first in proptest::collection::vec(any::<u8>(), 0..200),
            reply in proptest::collection::vec(any::<u8>(), 0..200),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("ferryd.sock");
            let server = serve(&path, false, first);
            let _ = Client::connect_with(&path, Duration::from_millis(500));
            server.join().unwrap();

            let path = dir.path().join("ferryd2.sock");
            let server = serve(&path, true, reply);
            let mut client = Client::connect_with(&path, Duration::from_millis(500)).unwrap();
            let _ = client.next_event(Duration::from_millis(50));
            let _ = client.call(Request::Status);
            server.join().unwrap();
        }
    }
}
