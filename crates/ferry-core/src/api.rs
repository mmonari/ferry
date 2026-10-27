//! Socket-API types: JSON lines on `$XDG_RUNTIME_DIR/ferry/ferryd.sock`.
//! These serde types are the schema; insta
//! snapshots pin their JSON.
//!
//! Versioning: the daemon's first line is `hello` with `api`. Within one
//! `api` version, fields are only added (clients ignore unknown fields and
//! the daemon ignores unknown params); a removal or a change of meaning bumps
//! `api`. Unknown methods answer `unknown_method`.

use serde::{Deserialize, Serialize};

use crate::geom::{Rect, Rel, Side};

/// The socket API version in `hello`.
pub const API_VERSION: u32 = 1;

/// The longest line either side of the socket reads: ferryd closes a client whose request line
/// is longer, and the CLI fails on a longer answer line.
pub const MAX_LINE: usize = 1 << 20;

/// One request line: `{"id":1,"method":"layout.place","params":{…}}`.
///
/// Parse it with [`RequestLine::parse`], which tells an unknown method
/// (`unknown_method`) from bad params (`bad_params`). Plain serde
/// deserialization fails the same way for both.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RequestLine {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// Why a request line was refused, with the id when one could be read (the
/// error response echoes it; without one, ferryd answers with id 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestError {
    pub id: Option<u64>,
    pub error: ApiError,
}

impl Request {
    /// Every method name, as it appears on the wire. `hello.methods` advertises exactly this list,
    /// so a frontend can feature-detect.
    pub const METHODS: &'static [&'static str] = &[
        "status",
        "release",
        "go",
        "id",
        "trust",
        "layout.get",
        "layout.place",
        "layout.set",
        "lock",
        "pair.accept",
        "pair.start",
        "pair.answer",
        "pair.cancel",
        "peers",
        "unpair",
        "doctor",
        "subscribe",
        "unsubscribe",
        "config.schema",
        "config.get",
        "config.set",
        "peer.rename",
    ];

    /// Methods that take no params; any `params` sent with them is ignored.
    const UNIT: &'static [&'static str] = &[
        "status",
        "release",
        "id",
        "layout.get",
        "pair.accept",
        "pair.cancel",
        "peers",
        "doctor",
        "unsubscribe",
        "config.schema",
    ];

    /// Methods whose params may be left out: no `params`, or `null`, reads as `{}`.
    const OPTIONAL: &'static [&'static str] = &["subscribe", "config.get"];

    /// The request cannot change what any snapshot shows. ferryd checks its subscriptions after
    /// every other request, so a method that is not listed here is safe by
    /// default.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Request::Status
                | Request::Id
                | Request::LayoutGet
                | Request::Peers
                | Request::Doctor
                | Request::Subscribe(_)
                | Request::Unsubscribe
                | Request::ConfigSchema
                | Request::ConfigGet(_)
        )
    }
}

impl RequestLine {
    /// Parse in two steps: the envelope (`id`, `method`), then the params.
    ///
    /// - not JSON, not an object, or no numeric `id` → `bad_params`;
    /// - a method this build does not know → `unknown_method`;
    /// - params that do not fit the method → `bad_params`.
    ///
    /// Unknown fields inside params are ignored (API versioning), and so
    /// is a `params` value sent with a method that takes none.
    pub fn parse(line: &str) -> Result<RequestLine, RequestError> {
        let bad = |id, message: String| RequestError {
            id,
            error: ApiError {
                code: ErrorCode::BadParams,
                message,
                hint: None,
            },
        };
        let mut v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| bad(None, format!("not JSON: {e}")))?;
        let Some(obj) = v.as_object_mut() else {
            return Err(bad(None, "a request is a JSON object".into()));
        };
        let id = obj.get("id").and_then(serde_json::Value::as_u64);
        let Some(id) = id else {
            return Err(bad(None, "a request needs a numeric \"id\"".into()));
        };
        let method = match obj.get("method").and_then(serde_json::Value::as_str) {
            Some(m) => m.to_owned(),
            None => return Err(bad(Some(id), "a request needs a \"method\"".into())),
        };
        if !Request::METHODS.contains(&method.as_str()) {
            return Err(RequestError {
                id: Some(id),
                error: ApiError {
                    code: ErrorCode::UnknownMethod,
                    message: format!("unknown method {method:?}"),
                    hint: None,
                },
            });
        }
        if Request::UNIT.contains(&method.as_str()) {
            obj.remove("params");
        } else if Request::OPTIONAL.contains(&method.as_str())
            && obj.get("params").is_none_or(serde_json::Value::is_null)
        {
            obj.insert("params".into(), serde_json::json!({}));
        }
        serde_json::from_value(v).map_err(|e| bad(Some(id), format!("{method}: {e}")))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    #[serde(rename = "status")]
    Status,
    /// Force a release from any state.
    #[serde(rename = "release")]
    Release,
    /// Move the keyboard and mouse to the machine on `side`, or back home, without the pointer
    /// (`ferry go`). Answers `{}` at once; a side with no machine does nothing but log.
    #[serde(rename = "go")]
    Go(GoParams),
    /// This peer's fingerprint, name and port (`ferry id`).
    #[serde(rename = "id")]
    Id,
    /// Pin a fingerprint (`ferry trust`). Idempotent: an existing pin gets
    /// the new name and address.
    #[serde(rename = "trust")]
    Trust(TrustParams),
    #[serde(rename = "layout.get")]
    LayoutGet,
    #[serde(rename = "layout.place")]
    LayoutPlace(PlaceParams),
    /// Move any number of peer groups in one revision (`ferry layout set`, the Arrange panel's
    /// Apply; FR-CLI-06, FR-LAYOUT-05, M3 design 2.5 and 4.2).
    #[serde(rename = "layout.set")]
    LayoutSet(SetParams),
    /// Lock to this machine (-20): `{on}` sets it, `{}`
    /// toggles it. Persisted; answers [`LockResult`].
    #[serde(rename = "lock")]
    Lock(LockParams),
    /// Open the 120 s pairing window (`ferry pair --accept`). `pairing`
    /// events follow on the same connection; closing it cancels.
    #[serde(rename = "pair.accept")]
    PairAccept,
    /// Pair with the peer at `addr` (`ferry pair <host>`). Answers once the peer accepted the
    /// pairing connection; `pairing` events follow on the same connection. A dial that fails
    /// answers with the error, and every `pairing` subscriber then hears a `failed` event that
    /// says why (a connection that did not subscribe has the error alone, as in M1).
    #[serde(rename = "pair.start")]
    PairStart(PairStartParams),
    /// The local user's answer to "Same code?", from any connection while a code is shown. The
    /// first answer wins (m3 design M3-D8).
    #[serde(rename = "pair.answer")]
    PairAnswer(PairAnswerParams),
    /// Cancel the pairing that runs, from any connection (pair-polish, m3 design section 9): the
    /// connection that started it and every `pairing` subscriber hear the end.
    #[serde(rename = "pair.cancel")]
    PairCancel,
    /// The pinned peers (`ferry peers`).
    #[serde(rename = "peers")]
    Peers,
    /// Delete a peer's pin and close its connection with code 4 (`ferry unpair`).
    #[serde(rename = "unpair")]
    Unpair(UnpairParams),
    /// The checks only ferryd can make (`ferry doctor`).
    #[serde(rename = "doctor")]
    Doctor,
    /// Start events on this connection. The answer is [`SubscribeResult`]; one
    /// snapshot per topic follows at once, then one event per change. A second `subscribe`
    /// replaces the topic set and sends the snapshots again.
    #[serde(rename = "subscribe")]
    Subscribe(SubscribeParams),
    /// Stop every event on this connection. Answers `{}`.
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
    /// Every key of `config.toml`: type, range, default, when it applies.
    #[serde(rename = "config.schema")]
    ConfigSchema,
    /// The saved values of one key, or of every key. Answers [`ConfigValues`].
    #[serde(rename = "config.get")]
    ConfigGet(ConfigGetParams),
    /// Validate one value like the file loader does, save it to `config.toml` and apply it when
    /// the key is live. Answers [`ConfigSetResult`].
    #[serde(rename = "config.set")]
    ConfigSet(ConfigSetParams),
    /// Give a trusted peer another name (`ferry` panels; answers the peer, like `trust`).
    #[serde(rename = "peer.rename")]
    PeerRename(PeerRenameParams),
}

/// `go` params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoParams {
    pub side: GoSide,
}

/// The side a key crossing goes to, named like the arrow keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoSide {
    Left,
    Right,
    #[serde(alias = "top")]
    Up,
    #[serde(alias = "bottom")]
    Down,
}

impl GoSide {
    pub fn side(self) -> crate::geom::Side {
        use crate::geom::Side;
        match self {
            GoSide::Left => Side::Left,
            GoSide::Right => Side::Right,
            GoSide::Up => Side::Top,
            GoSide::Down => Side::Bottom,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            GoSide::Left => "left",
            GoSide::Right => "right",
            GoSide::Up => "up",
            GoSide::Down => "down",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
pub struct LockParams {
    /// `true` locks, `false` unlocks; absent toggles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
}

/// `lock` result: the state now in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockResult {
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PairStartParams {
    /// `host[:port]` (port 4243 by default).
    pub addr: String,
    /// The name to pin the peer under; default: the name in its `Hello`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PairAnswerParams {
    /// `true`: both machines show the same code.
    #[serde(rename = "match")]
    pub matches: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UnpairParams {
    /// A name, or at least 8 hex digits of the fingerprint.
    pub peer: String,
}

/// `pair.accept` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct PairAcceptResult {
    /// Until the window closes.
    pub expires_ms: u64,
}

/// Where a pairing stands (design m1-rest §2.11; `listening` and `closed` are the acceptor's
/// window between attempts and at its end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingStage {
    /// Acceptor: the window is open and no attempt runs.
    Listening,
    /// Dialer: dialling, and the exchange before the code.
    Connecting,
    /// The code is shown; the local user has not answered.
    Code,
    /// The local user said yes; waiting for the peer.
    WaitingPeer,
    /// Both said yes and this side pinned the peer.
    Paired,
    /// This attempt ended without a pin. On the acceptor the window may stay open
    /// (`listening` follows) or end (`closed` follows).
    Failed,
    /// Acceptor: the window ended without a pin.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingRole {
    /// `ferry pair <host>`.
    Dialer,
    /// `ferry pair --accept`.
    Acceptor,
}

/// The other machine in a pairing attempt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PairingPeer {
    pub name: String,
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
}

/// `{"event":"pairing","data":…}`: on the connection that started the pairing whether or not it
/// subscribed, and on every connection subscribed to `pairing` (m3 design M3-D8). `code` appears
/// only at stage `code`, and never anywhere else.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PairingEvent {
    pub stage: PairingStage,
    /// Which side this machine is in the pairing. Absent from a ferryd older than api 1's M3
    /// additions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<PairingRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PairingPeer>,
    /// Six digits, zero-padded (`"418207"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Until the window (acceptor) or the attempt (dialer) ends.
    pub expires_ms: u64,
    /// `failed`/`closed`: `rejected`, `cancelled`, `timeout`, `bad_reveal`, `protocol`,
    /// `version`, `lost`, `failures`, `save_failed`, and for a dial that did not reach the other
    /// peer `unreachable`, `not_accepting`, `own_key`, `connect_failed`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What to tell the user (`"cancelled: codes differ"`, `"pinned as laptop-2: …"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// `failed`/`closed` with reason `cancelled`: whose cancel it was. Absent for any other
    /// reason, and from a ferryd that does not say (then `cancelled` could be either).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_by: Option<CancelledBy>,
}

/// Whose cancel ended a pairing (`reason: "cancelled"`): a frontend words "you cancelled it
/// elsewhere" and "the other peer cancelled it" apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelledBy {
    /// Somebody on this machine: a `pair.cancel` from any connection, the connection that started
    /// the pairing closing, or ferryd stopping.
    Local,
    /// The other peer, which said so on the pairing connection.
    Peer,
    /// A value this build does not know (a newer ferryd).
    #[serde(other)]
    Unknown,
}

/// `status.pairing`, never with the code.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PairingStatus {
    pub role: PairingRole,
    pub stage: PairingStage,
    pub expires_ms: u64,
}

/// How a pin was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Paired {
    /// SAS pairing (`ferry pair`).
    Sas,
    /// `ferry trust` with a fingerprint copied by hand.
    Manual,
}

/// One pinned peer in `peers`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PeerEntry {
    pub name: String,
    pub fingerprint: String,
    /// Where this side dials it, if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// RFC 3339 UTC, the last time a connection was up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    pub paired: Paired,
    pub state: LinkState,
    /// The peer is locked to itself (lock to this machine; filled from its `PeerState`).
    #[serde(default)]
    pub locked: bool,
}

/// `peers` result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PeersResult {
    pub peers: Vec<PeerEntry>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TrustParams {
    /// 64 hex digits; `:`/space separators allowed.
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `host[:port]`; this peer dials it. Without it, this peer only accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PlaceParams {
    /// Peer name, or a fingerprint prefix of ≥ 8 hex digits.
    pub peer: String,
    pub rel: Rel,
    /// `<peer>`, `<monitor>` or `<peer>:<monitor>`. A bare monitor name is
    /// looked up on this peer first, then on the others; two matches answer
    /// `ambiguous_anchor`.
    pub anchor: String,
    /// Omitted when [`AlignParam::Default`] (serde cannot write that variant).
    #[serde(default, skip_serializing_if = "AlignParam::is_default")]
    pub align: AlignParam,
}

/// `layout.set` params: `{"origins":[{"peer":"lap","x":2560,"y":0}],"base_rev":{…}}`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SetParams {
    /// At least one. A peer may be named once. Peers not named keep their origins.
    pub origins: Vec<OriginParam>,
    /// The revision the caller's picture of the layout is from. When it is not the current
    /// one, nothing changes and the answer is `stale_layout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_rev: Option<RevisionView>,
}

/// One group's new origin: the layout's value, which `layout.get` lists as `origin`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct OriginParam {
    /// Peer name, or a fingerprint prefix of ≥ 8 hex digits.
    pub peer: String,
    pub x: i32,
    pub y: i32,
}

/// `"start" | "center" | "end" | <px>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum AlignParam {
    #[default]
    #[serde(skip)]
    Default,
    Named(AlignName),
    Px(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignName {
    Start,
    Center,
    End,
}

impl AlignParam {
    pub fn is_default(&self) -> bool {
        matches!(self, AlignParam::Default)
    }

    /// The placement alignment; an absent `align` is `start`.
    pub fn to_align(self) -> crate::geom::Align {
        use crate::geom::Align;
        match self {
            AlignParam::Default | AlignParam::Named(AlignName::Start) => Align::Start,
            AlignParam::Named(AlignName::Center) => Align::Center,
            AlignParam::Named(AlignName::End) => Align::End,
            AlignParam::Px(px) => Align::Offset(px),
        }
    }
}

/// `{"id":n,"result":…}` or `{"id":n,"error":{…}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseLine<T> {
    pub id: u64,
    #[serde(flatten)]
    pub body: ResponseBody<T>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseBody<T> {
    Result(T),
    Error(ApiError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnknownMethod,
    BadParams,
    BadFingerprint,
    SelfFingerprint,
    UnknownPeer,
    AmbiguousPeer,
    UnknownAnchor,
    AmbiguousAnchor,
    /// The peer's monitors were never received.
    PeerMonitorsUnknown,
    Overlap,
    /// `layout.set` named a `base_rev` that is not the current revision.
    StaleLayout,
    Internal,
    /// The peer has no pairing window open, is busy, or this address is over its limit.
    PairingNotAccepting,
    /// A pairing already runs on this machine.
    PairingBusy,
    /// The window or the attempt ended before the answer.
    PairingExpired,
    /// No pairing runs on this connection.
    NoPairing,
    /// The peer did not answer the pairing dial.
    PeerUnreachable,
    /// `config.get` or `config.set` named a key `config.toml` does not have.
    UnknownKey,
    /// A `config.set` value of the wrong type, out of range, or refused by the file loader's
    /// rules.
    BadValue,
    /// `config.toml` cannot be written (a read-only file or directory).
    ReadOnly,
    /// An error code this build does not know (a newer ferryd); `message` still tells the user.
    #[serde(other)]
    Unknown,
}

/// `{"event":"hello","data":…}`, the daemon's first line, and the events that follow.
/// The `data` of `state`, `peers`, `layout` and `config` is a full snapshot,
/// never a delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "snake_case",
    try_from = "RawEvent"
)]
pub enum EventLine {
    Hello(HelloData),
    /// Pairing progress, on the connection that started it and on every `pairing` subscriber.
    Pairing(PairingEvent),
    /// The `status` result.
    State(Box<StatusResult>),
    /// The `peers` result.
    Peers(PeersResult),
    /// The `layout.get` result.
    Layout(LayoutResult),
    /// The saved configuration.
    Config(ConfigValues),
    /// A toast ferryd shows (or would show), for frontends with toasts of their own.
    Notice(Notice),
    /// A request that a frontend open one of its views.
    Ui(UiEvent),
    /// A crossing over an edge of this peer.
    Edge(EdgeEvent),
    /// An event this build does not know (a newer ferryd). Its data is skipped. It reads, but is
    /// never written.
    #[serde(skip)]
    Unknown,
}

/// An event line before its `data` is typed. serde's `#[serde(other)]` cannot skip the data of an
/// adjacently tagged variant, so [`EventLine`] reads the name first and types the data by it
/// (RG-CFG-04: an event of a newer ferryd must not fail the line).
#[derive(Deserialize)]
struct RawEvent {
    event: String,
    #[serde(default)]
    data: serde_json::Value,
}

impl TryFrom<RawEvent> for EventLine {
    type Error = serde_json::Error;

    fn try_from(raw: RawEvent) -> Result<Self, Self::Error> {
        let d = raw.data;
        Ok(match raw.event.as_str() {
            "hello" => EventLine::Hello(serde_json::from_value(d)?),
            "pairing" => EventLine::Pairing(serde_json::from_value(d)?),
            "state" => EventLine::State(Box::new(serde_json::from_value(d)?)),
            "peers" => EventLine::Peers(serde_json::from_value(d)?),
            "layout" => EventLine::Layout(serde_json::from_value(d)?),
            "config" => EventLine::Config(serde_json::from_value(d)?),
            "notice" => EventLine::Notice(serde_json::from_value(d)?),
            "ui" => EventLine::Ui(serde_json::from_value(d)?),
            "edge" => EventLine::Edge(serde_json::from_value(d)?),
            _ => EventLine::Unknown,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloData {
    pub api: u32,
    pub version: String,
    pub peer: String,
    pub fingerprint: String,
    /// Every method this ferryd answers ([`Request::METHODS`]); lets a frontend feature-detect.
    #[serde(default)]
    pub methods: Vec<String>,
    /// Every topic `subscribe` takes ([`Topic::ALL`]).
    #[serde(default)]
    pub topics: Vec<String>,
}

impl HelloData {
    /// The hello of this build: `api`, and the methods and topics it has.
    pub fn new(
        version: impl Into<String>,
        peer: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            api: API_VERSION,
            version: version.into(),
            peer: peer.into(),
            fingerprint: fingerprint.into(),
            methods: Request::METHODS.iter().map(|m| (*m).to_string()).collect(),
            topics: Topic::ALL.iter().map(|t| t.as_str().to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdResult {
    pub name: String,
    pub fingerprint: String,
    pub port: u16,
    /// Non-loopback addresses, best effort, for the `ferry trust` line.
    pub addrs: Vec<String>,
}

/// `trust` result: `{"peer":{"name","fingerprint"}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustResult {
    pub peer: PeerRef,
}

/// `unpair` result: the peer that was unpinned, `{"peer":{"name","fingerprint"}}` (the shape of
/// [`TrustResult`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnpairResult {
    pub peer: PeerRef,
}

/// `release` result: `{}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EmptyResult {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRef {
    pub name: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Idle,
    Pending,
    Arming,
    Source,
    Target,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Connected,
    Connecting,
    Offline,
    /// Refused: unknown fingerprint, changed fingerprint, version mismatch.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerStatus {
    pub name: String,
    pub fingerprint: String,
    pub state: LinkState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u32>,
    /// `"1.0"` from its `Hello`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<String>,
    /// Why it is `refused` (`"version mismatch: 1.0 vs 2.0"`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// RFC 3339 UTC: the last time a connection to it was up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    /// Released for silence and not heard from since: this
    /// peer's links are walls, so a crossing to it bounces, until a Ping or
    /// Pong arrives (at most 2 s after the link works again). Always
    /// `false` with capture off.
    #[serde(default)]
    pub suspect: bool,
    /// The peer is locked to itself (its `PeerState`): no crossing starts
    /// from it, so while this peer drives it its links are walls.
    #[serde(default)]
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureStatus {
    /// `portal` or `off` (capture disabled on this peer).
    pub backend: String,
    /// `armed | idle | locked | suspended | rezoning | barriers_failed | off`.
    pub gate: String,
    pub barriers: u32,
    pub failed_barriers: Vec<u32>,
    /// Why capture is off on purpose: `controlled` (another peer drives this
    /// one) or `locked_here` (lock to this machine). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_reason: Option<String>,
    /// A condition worth a line in `ferry status`: input capture may be denied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastRelease {
    /// [`crate::wire::LeaveReason::as_str`].
    pub reason: String,
    pub ago_ms: u64,
    /// When it happened, on the daemon's `CLOCK_MONOTONIC`, in µs (for drills that line it up
    /// with other events on the same machine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono_us: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusResult {
    pub api: u32,
    pub me: PeerRef,
    pub port: u16,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_peer: Option<PeerRef>,
    pub capture: CaptureStatus,
    /// `bool?` in §8: `null` when the last lock check was unknown (IPC down,
    /// or before the first poll). Always present.
    pub screen_locked: Option<bool>,
    /// Lock to this machine is on (persisted).
    #[serde(default)]
    pub locked: bool,
    pub peers: Vec<PeerStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_release: Option<LastRelease>,
    /// Flat counter map (`bounces`, `silence_releases`, …).
    pub counters: std::collections::BTreeMap<String, u64>,
    /// A pairing that runs on this machine (never the code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing: Option<PairingStatus>,
    /// The one word a bar widget shows, computed by ferryd so every frontend shows
    /// the same state for the same facts (m3 design M3-D2). Absent from an older ferryd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    /// Problems the user must act on; empty when there are none. A frontend shows them as they
    /// are.
    #[serde(default)]
    pub attention: Vec<Attention>,
    /// The escape chord in force, as `Super+Ctrl+Escape`, for a tooltip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escape_chord: Option<String>,
}

impl StatusResult {
    /// The snapshot without the fields that change while nothing happens: every counter, the
    /// clock of the last release, a peer's round trip and the pairing countdown. Two snapshots
    /// that are equal in this form are the same news, so ferryd sends an event only when they
    /// differ; the noise goes out in the 2 s refresh.
    pub fn without_noise(&self) -> StatusResult {
        let mut s = self.clone();
        s.counters.clear();
        if let Some(r) = &mut s.last_release {
            r.ago_ms = 0;
            r.mono_us = None;
        }
        for p in &mut s.peers {
            p.rtt_us = None;
        }
        if let Some(p) = &mut s.pairing {
            p.expires_ms = 0;
        }
        s
    }
}

/// What a bar widget shows: one of [`SummaryState`], and the other peer's name while
/// away or controlled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub state: SummaryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
}

/// Highest priority first: `attention`, `away`, `controlled`, `locked`, `ready`, `idle`. The
/// widget's `off` (ferryd unreachable) is its own and never comes from ferryd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryState {
    /// No trusted peer is connected.
    Idle,
    /// At least one trusted peer is connected and nothing else is going on.
    Ready,
    /// This peer is the source and drives `peer`.
    Away,
    /// `peer` drives this peer.
    Controlled,
    /// Lock to this machine is on.
    Locked,
    /// `attention` is not empty.
    Attention,
    /// A state this build does not know (a newer ferryd).
    #[serde(other)]
    Unknown,
}

/// The facts [`Summary::derive`] reads.
#[derive(Debug, Clone, Copy)]
pub struct SummaryFacts<'a> {
    pub role: &'a Role,
    /// The name of the peer on the other end of a crossing.
    pub active_peer: Option<&'a str>,
    /// Lock to this machine.
    pub locked: bool,
    /// A trusted peer's link is up.
    pub connected: bool,
    /// `attention` is not empty.
    pub attention: bool,
}

impl Summary {
    /// The priority rule of m3 design §2.4. Only a crossing in progress is `away` or
    /// `controlled`; the moments before it (`pending`, `arming`) read as `ready`, so the widget
    /// does not flicker through them.
    pub fn derive(f: SummaryFacts<'_>) -> Summary {
        let with_peer = |state| Summary {
            state,
            peer: f.active_peer.map(str::to_string),
        };
        let alone = |state| Summary { state, peer: None };
        if f.attention {
            alone(SummaryState::Attention)
        } else if *f.role == Role::Source {
            with_peer(SummaryState::Away)
        } else if *f.role == Role::Target {
            with_peer(SummaryState::Controlled)
        } else if f.locked {
            alone(SummaryState::Locked)
        } else if f.connected {
            alone(SummaryState::Ready)
        } else {
            alone(SummaryState::Idle)
        }
    }
}

/// A problem the user must act on (`status.attention`). A frontend shows `message` and `hint`
/// as they are and may use `code` to pick a glyph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    pub code: AttentionCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// The peer it is about, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionCode {
    /// A trusted peer's link is refused: its key changed, it does not trust this machine, or the
    /// versions differ.
    PeerRefused,
    /// The pointer barriers could not be set on some edge.
    BarriersFailed,
    /// Input capture is not permitted, or Hyprland will ask at the first crossing.
    CaptureDenied,
    /// A trusted peer with known monitors has no place in the layout.
    LayoutUnplaced,
    /// `config.toml` has a key ferryd does not know, or a value it had to clamp.
    ConfigInvalid,
    /// A code this build does not know (a newer ferryd); `message` still tells the user.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutResult {
    pub revision: RevisionView,
    pub peers: Vec<LayoutPeer>,
    pub links: Vec<LinkView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionView {
    pub counter: u64,
    /// The fingerprint of the peer that made the edit: 64 lowercase hex
    /// digits ([`crate::PeerId`]'s `Display`).
    pub peer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutPeer {
    pub name: String,
    pub fingerprint: String,
    /// The revisioned origin; `None`: known but not placed. A monitor sits on the plane at its
    /// local position plus `shift` plus this.
    pub origin: Option<(i32, i32)>,
    pub monitors: Vec<MonitorView>,
    pub online: bool,
    /// Directly paired with this peer (this peer counts as paired with itself). A peer that is
    /// not has no live edge link to this one. Absent from an older ferryd, hence true.
    #[serde(default = "yes")]
    pub trusted: bool,
    /// This is the peer that answered.
    #[serde(default, rename = "self")]
    pub is_self: bool,
    /// The group's bounding box: on the plane when placed, else local. `None` without monitors.
    #[serde(default)]
    pub bounds: Option<Rect>,
    /// How far the peer moved its group to keep its monitors in place after Hyprland moved them
    /// (M3 design 4.1). Left out when zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<(i32, i32)>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorView {
    pub name: String,
    /// Plane rectangle when placed, else local.
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub scale: f32,
    /// Hyprland's description (make, model, serial); what FR-LAYOUT-03 recognises the monitor by.
    #[serde(default)]
    pub description: String,
    /// `wl_output` transform, 0–7.
    #[serde(default)]
    pub transform: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkView {
    /// `peer:monitor`.
    pub from: String,
    pub side: Side,
    pub to: String,
    pub span: (i32, i32),
    /// Live = both peers trusted and connected.
    pub live: bool,
}

// --- events, subscriptions and config (m3 design §2.2–2.7) ------------------------------------

/// What a frontend can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// The `status` result, on a change that matters (see [`StatusResult::without_noise`]).
    State,
    /// The `peers` result.
    Peers,
    /// The `layout.get` result.
    Layout,
    /// Every stage change of a pairing on this machine. Not a snapshot: a pairing that runs is
    /// sent once on subscribe, with its code if it is at that stage.
    Pairing,
    /// The saved configuration, after a `config.set` that changed a value.
    Config,
    /// Every toast. An event, not a snapshot.
    Notice,
    /// A request that a view open. An event, not a snapshot.
    Ui,
    /// A crossing started or came home over an edge of this peer. An event, not a
    /// snapshot.
    Edge,
}

impl Topic {
    /// Every topic, in the order `hello.topics` lists them.
    pub const ALL: [Topic; 8] = [
        Topic::State,
        Topic::Peers,
        Topic::Layout,
        Topic::Pairing,
        Topic::Config,
        Topic::Notice,
        Topic::Ui,
        Topic::Edge,
    ];

    /// The name on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Topic::State => "state",
            Topic::Peers => "peers",
            Topic::Layout => "layout",
            Topic::Pairing => "pairing",
            Topic::Config => "config",
            Topic::Notice => "notice",
            Topic::Ui => "ui",
            Topic::Edge => "edge",
        }
    }

    /// The event carries the whole current value of something, so a dropped one can always be
    /// replaced by a fresh one. The other topics carry things that happened.
    pub const fn is_snapshot(self) -> bool {
        matches!(
            self,
            Topic::State | Topic::Peers | Topic::Layout | Topic::Config
        )
    }
}

impl std::fmt::Display for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A topic name that is not one of [`Topic::ALL`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown topic {0:?}; the topics are state, peers, layout, pairing, config, notice, ui, edge"
)]
pub struct UnknownTopic(pub String);

impl std::str::FromStr for Topic {
    type Err = UnknownTopic;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Topic::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| UnknownTopic(s.to_string()))
    }
}

/// `subscribe` params: `{"topics":["state","layout"]}`, or none for every topic.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct SubscribeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<Topic>>,
}

/// `subscribe` result: the topics now subscribed, in [`Topic::ALL`] order and without repeats.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SubscribeResult {
    pub topics: Vec<Topic>,
}

/// Why a toast is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// A pairing reached stage `code`.
    PairCode,
    Paired,
    PairFailed,
    PeerLost,
    PeerBack,
    /// The other side revoked this machine.
    Unpaired,
    /// This machine's own layout edit lost to a higher revision.
    LayoutReplaced,
    /// A new `attention` code appeared.
    Attention,
    /// A kind this build does not know (a newer ferryd).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    Critical,
    /// An urgency this build does not know (a newer ferryd).
    #[serde(other)]
    Unknown,
}

/// `{"event":"notice","data":…}`: every toast ferryd shows, also when no notification daemon
/// answers, so a frontend without one (or with toasts of its own) still sees it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub kind: NoticeKind,
    pub title: String,
    /// What the toast says under its title. A `pair_code` notice has the pairing code in its
    /// title ("Pairing code 418 207"), so never log a notice's title or body;
    /// `Debug` leaves both out.
    #[serde(default)]
    pub body: String,
    /// The peer it is about, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    pub urgency: Urgency,
}

impl std::fmt::Debug for Notice {
    // Not the title or the body: a `pair_code` notice holds the pairing code, and no log line at
    // any level may.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notice")
            .field("kind", &self.kind)
            .field("peer", &self.peer)
            .field("urgency", &self.urgency)
            .finish_non_exhaustive()
    }
}

/// The views a frontend can be asked to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiView {
    Pair,
    Arrange,
    Popover,
    Settings,
    /// A view this build does not know (a newer ferryd).
    #[serde(other)]
    Unknown,
}

impl UiView {
    pub const fn as_str(self) -> &'static str {
        match self {
            UiView::Pair => "pair",
            UiView::Arrange => "arrange",
            UiView::Popover => "popover",
            UiView::Settings => "settings",
            UiView::Unknown => "unknown",
        }
    }
}

impl std::str::FromStr for UiView {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [
            UiView::Pair,
            UiView::Arrange,
            UiView::Popover,
            UiView::Settings,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
        .ok_or_else(|| {
            format!("unknown view {s:?}; the views are pair, arrange, popover, settings")
        })
    }
}

/// `{"event":"ui","data":{"open":"pair"}}`: a notification action asked a frontend to open a
/// view. ferryd never knows which shell runs or how to open it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiEvent {
    pub open: UiView,
}

/// `{"event":"edge","data":{...}}`: the pointer crossed a link at an edge of one of this peer's
/// monitors, out (`role` `source`, or `target` with `home` false) or back home. A
/// frontend lights the stretch `[start, end)` of that edge for a moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeEvent {
    /// `source`: this peer's input left over the edge. `target`: another peer's input came in.
    pub role: EdgeRole,
    /// The crossing ended: the pointer came back over the same edge to the source.
    pub home: bool,
    /// The name of the other peer.
    pub peer: String,
    /// The monitor of this peer that the edge belongs to, by its Hyprland name.
    pub monitor: String,
    pub side: crate::geom::Side,
    /// The linked stretch along the edge, in logical px from the monitor's left (top and bottom
    /// edges) or top (left and right edges), dead corners left out. `start < end`.
    pub start: i32,
    pub end: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRole {
    Source,
    Target,
}

/// How a config value is shown and checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigType {
    Bool,
    /// A whole number between `min` and `max`.
    Int,
    String,
    /// One of `choices`.
    Enum,
    /// A key combination such as `Super+Ctrl+Escape` (at least two modifiers and one key).
    Chord,
    /// A peer name: no whitespace, control characters or `:`, at most 64 characters.
    Name,
    /// A type this build does not know (a newer ferryd); show the value as text.
    #[serde(other)]
    Unknown,
}

/// When a saved value is in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    /// At once.
    Live,
    /// After `systemctl --user restart ferryd`.
    Restart,
    /// After `ferry setup`: the value is also written into Hyprland's config.
    Setup,
    /// A moment this build does not know (a newer ferryd).
    #[serde(other)]
    Unknown,
}

/// One key of `config.toml` (`config.schema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigKey {
    /// The dotted path: `[crossing] edge_resistance_px` is `crossing.edge_resistance_px`.
    pub key: String,
    #[serde(rename = "type")]
    pub kind: ConfigType,
    pub default: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// For an `int` whose `max` is far above what a person usually sets: the top of the usual
    /// range. A frontend may offer a slider up to here beside the exact field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slider_max: Option<i64>,
    /// The allowed values of an `enum`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
    pub applies: Applies,
    pub doc: String,
    /// False while no code in this build acts on the key: a feature that arrives in a later version.
    /// A frontend shows the control disabled, with a note, and not as a switch that does nothing;
    /// `config.set` still saves the value. Left out when true, so an older frontend sees no change.
    #[serde(default = "key_available", skip_serializing_if = "key_is_available")]
    pub available: bool,
}

fn key_available() -> bool {
    true
}

fn key_is_available(available: &bool) -> bool {
    *available
}

/// `config.schema` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSchema {
    pub keys: Vec<ConfigKey>,
}

/// `config.get` params: one key, or none for every key.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct ConfigGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// `config.get` result and the `config` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigValues {
    /// The saved values, by dotted key.
    pub values: std::collections::BTreeMap<String, serde_json::Value>,
    /// Keys whose saved value is not the one the running ferryd uses: they apply after
    /// `systemctl --user restart ferryd`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ConfigSetParams {
    pub key: String,
    pub value: serde_json::Value,
}

/// `config.set` result: the value as saved, and when it applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSetResult {
    pub key: String,
    pub value: serde_json::Value,
    pub applies: Applies,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PeerRenameParams {
    /// A name, or at least 8 hex digits of the fingerprint.
    pub peer: String,
    /// The new name; the rule of `peer.name`.
    pub name: String,
}

/// `peer.rename` result: the peer under its new name, the shape of [`TrustResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameResult {
    pub peer: PeerRef,
}

#[cfg(test)]
mod tests;
