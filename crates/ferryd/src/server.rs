//! The socket: binding it safely, and serving JSON lines on it.
//!
//! - `$XDG_RUNTIME_DIR/ferry/` is 0700 and the socket 0600.
//! - Every client's uid is read with `SO_PEERCRED`; any other uid is dropped without a hello.
//! - The first line is the `hello` event. Then one response per request line, in order.
//! - Event lines may come between responses: `pairing` events on the connection that started
//!   the pairing, and the topics of a `subscribe`. A writer task owns the write
//!   half. Responses queue to it in a bounded queue and are never dropped; events wait in the
//!   connection's [`EventQueue`], which keeps only the newest snapshot of a topic, so a reader
//!   that stops reading costs a few kilobytes and never stalls ferryd.

use std::cell::RefCell;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener as StdListener, UnixStream as StdStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ferry_core::api::{
    ApiError, ErrorCode, EventLine, HelloData, RequestError, RequestLine, ResponseBody,
    ResponseLine, Topic,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::api;
use crate::ctl::Ctl;
use crate::error::StartError;
use crate::events::EventQueue;
use crate::paths::Paths;

/// One request line may not exceed this; a longer one closes the connection.
pub use ferry_core::api::MAX_LINE;

/// Removes the socket file when ferryd stops.
#[derive(Debug)]
pub struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Creates the socket directory (0700) and binds the socket (0600).
///
/// A second ferryd must not start while the first answers: a socket that accepts a connection
/// is live. A socket file nothing answers is a leftover of a crash and is replaced.
pub fn bind(paths: &Paths) -> Result<(StdListener, SocketGuard), StartError> {
    let dir = paths.socket_dir();
    let path = paths.socket_path();
    let sock_err = |path: &Path| {
        let path = path.to_path_buf();
        move |source| StartError::Socket { path, source }
    };

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(sock_err(&dir))?;
    let meta = std::fs::metadata(&dir).map_err(sock_err(&dir))?;
    if meta.uid() != rustix::process::getuid().as_raw() {
        return Err(StartError::Socket {
            path: dir,
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the directory belongs to another user",
            ),
        });
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(sock_err(&dir))?;
    }

    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_socket() => match StdStream::connect(&path) {
            Ok(_) => return Err(StartError::AlreadyRunning(path)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                tracing::info!(path = %path.display(), "removing a stale socket");
                std::fs::remove_file(&path).map_err(sock_err(&path))?;
            }
            Err(e) => return Err(sock_err(&path)(e)),
        },
        Ok(_) => {
            return Err(StartError::Socket {
                path,
                source: io::Error::new(io::ErrorKind::AlreadyExists, "exists and is not a socket"),
            });
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(sock_err(&path)(e)),
    }

    let listener = match StdListener::bind(&path) {
        Ok(l) => l,
        // Two ferryds starting together: the other bound first.
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            return Err(StartError::AlreadyRunning(path));
        }
        Err(e) => return Err(sock_err(&path)(e)),
    };
    let guard = SocketGuard { path: path.clone() };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(sock_err(&path))?;
    listener.set_nonblocking(true).map_err(sock_err(&path))?;
    Ok((listener, guard))
}

/// `SO_PEERCRED`: is the client the user ferryd runs as?
pub fn uid_allowed(peer_uid: u32, my_uid: u32) -> bool {
    peer_uid == my_uid
}

fn response(id: u64, body: Result<Value, ApiError>) -> Vec<u8> {
    let body = match body {
        Ok(v) => ResponseBody::Result(v),
        Err(e) => ResponseBody::Error(e),
    };
    let mut line = serde_json::to_vec(&ResponseLine { id, body }).expect("a response serialises");
    line.push(b'\n');
    line
}

fn bad(message: impl Into<String>) -> ApiError {
    ApiError {
        code: ErrorCode::BadParams,
        message: message.into(),
        hint: None,
    }
}

/// Lines a connection may have queued for its writer. A client that stops reading fills it and
/// is closed, so ferryd's memory per connection is bounded at about this many replies
/// (`status` is about 2 KB).
pub const OUT_QUEUE: usize = 256;

/// The queue of lines to one client, bounded. [`LineSink::send`] never waits: a full queue means
/// the client is not reading, and the connection is closed.
#[derive(Clone)]
pub struct LineSink {
    tx: mpsc::Sender<Vec<u8>>,
    overflow: Rc<tokio::sync::Notify>,
}

impl LineSink {
    fn new(cap: usize) -> (Self, mpsc::Receiver<Vec<u8>>, Rc<tokio::sync::Notify>) {
        let (tx, rx) = mpsc::channel(cap);
        let overflow = Rc::new(tokio::sync::Notify::new());
        (
            Self {
                tx,
                overflow: overflow.clone(),
            },
            rx,
            overflow,
        )
    }

    /// Queues one line. `false`: the connection is closed, or this call closed it because the
    /// queue was full.
    pub fn send(&self, line: Vec<u8>) -> bool {
        match self.tx.try_send(line) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.overflow.notify_one();
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// What a connection does once the response to a request is queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum After {
    /// Start events on these topics: the snapshots must follow the `subscribe` response, never
    /// precede it.
    Subscribe(Vec<Topic>),
}

/// One socket connection, as request handlers see it.
pub struct ConnCtx {
    /// Unique for the daemon's life.
    pub id: u64,
    /// Lines to the client: responses, and the `pairing` events of a pairing it started, in order.
    pub out: LineSink,
    /// The events of its subscription, for its writer.
    pub events: Rc<EventQueue>,
    /// Set by a request whose effect must follow its response.
    pub after: RefCell<Option<After>>,
}

/// Ends the connection's subscription when the connection ends, whichever way it does.
struct Subscription<'a> {
    ctl: &'a Ctl,
    conn: u64,
}

impl Drop for Subscription<'_> {
    fn drop(&mut self) {
        self.ctl.hub.unsubscribe(self.conn);
    }
}

static NEXT_CONN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Serves one client until it closes.
pub async fn serve_connection(stream: UnixStream, ctl: Rc<Ctl>) {
    match stream.peer_cred() {
        Ok(cred) if uid_allowed(cred.uid(), ctl.uid) => {}
        Ok(cred) => {
            tracing::warn!(uid = cred.uid(), "rejected a socket client of another user");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "rejected a socket client: no SO_PEERCRED");
            return;
        }
    }
    if let Err(e) = serve(stream, &ctl).await {
        tracing::debug!(error = %e, "socket client ended");
    }
}

async fn serve(stream: UnixStream, ctl: &Ctl) -> io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);

    let hello = {
        let s = ctl.state.borrow();
        EventLine::Hello(HelloData::new(
            env!("CARGO_PKG_VERSION"),
            s.me.name.clone(),
            s.me.fp.to_string(),
        ))
    };
    let mut line = serde_json::to_vec(&hello).expect("hello serialises");
    line.push(b'\n');
    write.write_all(&line).await?;

    // The writer owns the write half from here on: responses and events queue to it, in order.
    let (out, mut lines, overflow) = LineSink::new(OUT_QUEUE);
    let conn = ConnCtx {
        id: NEXT_CONN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        out,
        events: Rc::new(EventQueue::default()),
        after: RefCell::new(None),
    };
    let _subscription = Subscription { ctl, conn: conn.id };
    let (reader_done, mut done) = tokio::sync::oneshot::channel::<()>();
    let writer = async {
        loop {
            tokio::select! {
                biased;
                line = lines.recv() => match line {
                    Some(line) => write.write_all(&line).await?,
                    None => return Ok(()),
                },
                // After the responses: an event never waits for one, and one never overtakes it.
                line = conn.events.next() => write.write_all(&line).await?,
                _ = &mut done => {
                    // the client stopped sending: flush what is queued, then stop
                    while let Ok(line) = lines.try_recv() {
                        write.write_all(&line).await?;
                    }
                    return Ok::<(), io::Error>(());
                }
            }
        }
    };
    let reader = async {
        let mut buf = Vec::new();
        let result = loop {
            buf.clear();
            let n = match (&mut read)
                .take(MAX_LINE as u64 + 1)
                .read_until(b'\n', &mut buf)
                .await
            {
                Ok(n) => n,
                Err(e) => break Err(e),
            };
            if n == 0 {
                break Ok(());
            }
            if buf.last() != Some(&b'\n') && buf.len() > MAX_LINE {
                conn.out
                    .send(response(0, Err(bad("request line too long"))));
                break Ok(());
            }
            let trimmed = buf.trim_ascii();
            if trimmed.is_empty() {
                continue;
            }
            let line = handle_line(ctl, &conn, trimmed).await;
            if !conn.out.send(line) {
                break Ok(());
            }
            let after = conn.after.borrow_mut().take();
            if let Some(After::Subscribe(topics)) = after {
                ctl.hub.subscribe(
                    ctl,
                    conn.id,
                    &conn.events,
                    &topics,
                    std::time::Instant::now(),
                );
            }
        };
        // A pairing this connection started is cancelled.
        crate::pairing::socket_closed(ctl, conn.id).await;
        let _ = reader_done.send(());
        result
    };
    let both = async {
        let (read_result, write_result) = tokio::join!(reader, writer);
        read_result.and(write_result)
    };
    tokio::select! {
        result = both => result,
        // The client does not read what it asked for: drop both halves, which closes the
        // socket, and cancel what it started.
        () = overflow.notified() => {
            tracing::warn!(
                conn = conn.id,
                queued = OUT_QUEUE,
                "closing a socket client that does not read its replies"
            );
            crate::pairing::socket_closed(ctl, conn.id).await;
            Err(io::Error::new(io::ErrorKind::TimedOut, "the client does not read its replies"))
        }
    }
}

async fn handle_line(ctl: &Ctl, conn: &ConnCtx, line: &[u8]) -> Vec<u8> {
    let Ok(line) = std::str::from_utf8(line) else {
        return response(0, Err(bad("a request line is UTF-8 JSON")));
    };
    match RequestLine::parse(line) {
        Ok(req) => {
            let read_only = req.request.is_read_only();
            let result = api::dispatch_conn(ctl, conn, req.request).await;
            if !read_only {
                // the request may have changed what a snapshot shows, or what
                // `attention` says
                ctl.hub.poke();
                ctl.notify.poke();
            }
            response(req.id, result)
        }
        Err(RequestError { id, mut error }) => {
            if error.code == ErrorCode::UnknownMethod && error.hint.is_none() {
                error.hint = Some(
                    "this ferryd may be older than your client; compare `api` in the hello line"
                        .into(),
                );
            }
            response(id.unwrap_or(0), Err(error))
        }
    }
}
