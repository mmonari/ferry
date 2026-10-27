//! `docs/api/socket-api.md` against the code.
//!
//! Every transcript on the page (a `jsonl` block) parses with the API types and writes back to the
//! same JSON, so a misspelled or removed field fails here. The tables name every method, topic,
//! error code, notice kind and config key, and the numbers in the prose (limits, windows, counts)
//! are the ones in the code. A change to the API that leaves the page behind fails this test.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

use std::collections::BTreeSet;
use std::time::Duration;

use ferry_core::api::{
    API_VERSION, ApiError, Applies, AttentionCode, ConfigSchema, ConfigSetResult, ConfigType,
    ConfigValues, EmptyResult, ErrorCode, EventLine, HelloData, IdResult, LayoutResult, LinkState,
    LockResult, MAX_LINE, NoticeKind, PairAcceptResult, Paired, PairingRole, PairingStage,
    PeersResult, RenameResult, Request, RequestError, RequestLine, Role, StatusResult,
    SubscribeResult, SummaryState, Topic, TrustResult, UiView, UnpairResult, Urgency,
};
use ferry_core::doctor::DoctorResult;
use ferry_core::pair::PairReason;
use ferry_core::wire::{LeaveReason, caps};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn doc() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/socket-api.md");
    std::fs::read_to_string(path).unwrap()
}

/// The wire spelling of a unit variant.
fn word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// The text from the heading `title` (with its `#` marks) to the next heading of the same level
/// or a higher one. Lines inside a code block are not headings.
fn section<'a>(doc: &'a str, title: &str) -> &'a str {
    let level = title.chars().take_while(|c| *c == '#').count();
    let (mut start, mut offset, mut fenced) = (None, 0, false);
    for line in doc.split_inclusive('\n') {
        let text = line.trim_end_matches('\n');
        if text.starts_with("```") {
            fenced = !fenced;
        }
        if !fenced {
            let marks = text.chars().take_while(|c| *c == '#').count();
            let heading = marks > 0 && text[marks..].starts_with(' ');
            match start {
                None if text == title => start = Some(offset),
                Some(from) if heading && marks <= level => return &doc[from..offset],
                _ => {}
            }
        }
        offset += line.len();
    }
    match start {
        Some(from) => &doc[from..],
        None => panic!("the page has no heading {title:?}"),
    }
}

/// The cells of a table row: `line` without its outer bars, split at the bars.
fn cells(line: &str) -> Vec<&str> {
    line.trim_matches('|').split('|').map(str::trim).collect()
}

/// The cells of the row of `section` whose first cell is `` `key` ``.
fn row<'a>(section: &'a str, key: &str) -> Vec<&'a str> {
    let start = format!("| `{key}` |");
    let line = section
        .lines()
        .find(|l| l.starts_with(&start))
        .unwrap_or_else(|| panic!("no table row for `{key}`"));
    cells(line)
}

/// The cells of the row whose first cell is a link, `` [`key`](#anchor) ``.
fn linked_row<'a>(section: &'a str, key: &str) -> Vec<&'a str> {
    let start = format!("| [`{key}`](#");
    let line = section
        .lines()
        .find(|l| l.starts_with(&start))
        .unwrap_or_else(|| panic!("no table row for [`{key}`]"));
    cells(line)
}

// --- the transcripts -----------------------------------------------------------------------------

enum Line {
    Sent(String),
    Received(String),
}

fn transcripts(doc: &str) -> Vec<Vec<Line>> {
    let mut blocks = Vec::new();
    let mut open: Option<Vec<Line>> = None;
    for line in doc.lines() {
        match (&mut open, line) {
            (None, "```jsonl") => open = Some(Vec::new()),
            (Some(_), "```") => blocks.push(open.take().unwrap()),
            (Some(block), text) => {
                if let Some(sent) = text.strip_prefix("> ") {
                    block.push(Line::Sent(sent.to_string()));
                } else if let Some(received) = text.strip_prefix("< ") {
                    block.push(Line::Received(received.to_string()));
                } else {
                    panic!("a transcript line starts with `> ` or `< `: {text:?}");
                }
            }
            _ => {}
        }
    }
    assert!(open.is_none(), "a jsonl block is not closed");
    blocks
}

/// `value` is a `T` and writes back to the same JSON: no field is unknown or misshapen.
fn typed<T: DeserializeOwned + Serialize>(value: &Value) {
    let t: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|e| panic!("{}: {e}: {value}", std::any::type_name::<T>()));
    assert_eq!(
        &serde_json::to_value(&t).unwrap(),
        value,
        "{} does not write back to the page's JSON",
        std::any::type_name::<T>()
    );
}

/// The type of the `result` that answers `request`. There is no wildcard: a new method fails to
/// compile here, which is the reminder to document it.
fn check_result(request: &Request, result: &Value) {
    match request {
        Request::Status => typed::<StatusResult>(result),
        Request::Release
        | Request::Go(_)
        | Request::PairStart(_)
        | Request::PairAnswer(_)
        | Request::PairCancel
        | Request::Unsubscribe => typed::<EmptyResult>(result),
        Request::Id => typed::<IdResult>(result),
        Request::Trust(_) => typed::<TrustResult>(result),
        Request::Unpair(_) => typed::<UnpairResult>(result),
        Request::PeerRename(_) => typed::<RenameResult>(result),
        Request::LayoutGet | Request::LayoutPlace(_) | Request::LayoutSet(_) => {
            typed::<LayoutResult>(result);
        }
        Request::Lock(_) => typed::<LockResult>(result),
        Request::PairAccept => typed::<PairAcceptResult>(result),
        Request::Peers => typed::<PeersResult>(result),
        Request::Doctor => typed::<DoctorResult>(result),
        Request::Subscribe(_) => typed::<SubscribeResult>(result),
        Request::ConfigSchema => typed::<ConfigSchema>(result),
        Request::ConfigGet(_) => typed::<ConfigValues>(result),
        Request::ConfigSet(_) => typed::<ConfigSetResult>(result),
    }
}

fn check_event(text: &str) {
    let value: Value = serde_json::from_str(text).unwrap();
    let event: EventLine = serde_json::from_str(text).unwrap();
    assert!(
        !matches!(event, EventLine::Unknown),
        "the page shows an event that ferryd does not send: {text}"
    );
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        value,
        "an event does not write back to the page's JSON: {text}"
    );
    if let EventLine::Hello(hello) = event {
        let now = HelloData::new(hello.version, hello.peer, hello.fingerprint);
        assert_eq!(hello.api, API_VERSION);
        assert_eq!(hello.methods, now.methods, "the hello lists the methods");
        assert_eq!(hello.topics, now.topics, "the hello lists the topics");
    }
}

/// The requests of a block that no response has answered yet.
type Asked = Vec<Result<RequestLine, RequestError>>;

fn answered_id(request: &Result<RequestLine, RequestError>) -> Option<u64> {
    match request {
        Ok(line) => Some(line.id),
        Err(refused) => refused.id,
    }
}

fn check_response(value: &Value, asked: &mut Asked) {
    let id = value["id"].as_u64().unwrap();
    // ferryd answers a request line whose id it could not read with id 0
    let at = asked
        .iter()
        .position(|r| answered_id(r) == Some(id) || (id == 0 && answered_id(r).is_none()))
        .unwrap_or_else(|| panic!("a response answers no request of its block: {value}"));
    let parsed = asked.remove(at);
    if let Some(error) = value.get("error") {
        let api: ApiError = serde_json::from_value(error.clone()).unwrap();
        assert_ne!(api.code, ErrorCode::Unknown, "{value}");
        assert_eq!(&serde_json::to_value(&api).unwrap(), error, "{value}");
        if let Err(refused) = parsed {
            assert_eq!(
                refused.error.code, api.code,
                "ferryd refuses this request line with another code: {value}"
            );
        }
    } else {
        let request = parsed.unwrap_or_else(|e| {
            panic!("the page answers a request that ferryd refuses ({e:?}): {value}")
        });
        check_result(&request.request, &value["result"]);
    }
}

#[test]
fn every_transcript_parses_with_the_api_types_and_writes_back() {
    let doc = doc();
    let blocks = transcripts(&doc);
    assert!(blocks.len() > 30, "only {} transcripts", blocks.len());
    let mut lines = 0;
    for block in &blocks {
        let mut asked: Asked = Vec::new();
        for line in block {
            lines += 1;
            match line {
                Line::Sent(text) => {
                    let parsed = RequestLine::parse(text);
                    if let Ok(request) = &parsed {
                        // the params of the request are the ones the type knows
                        assert_eq!(
                            serde_json::to_value(request).unwrap(),
                            serde_json::from_str::<Value>(text).unwrap(),
                            "a request does not write back to the page's JSON: {text}"
                        );
                    }
                    asked.push(parsed);
                }
                Line::Received(text) => {
                    let value: Value = serde_json::from_str(text)
                        .unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
                    if value.get("event").is_some() {
                        check_event(text);
                    } else {
                        check_response(&value, &mut asked);
                    }
                }
            }
        }
    }
    assert!(lines > 100, "only {lines} transcript lines");
}

/// The names of the objects in `value`, leaving out the keys of maps whose keys are data.
fn field_names(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                out.insert(name.clone());
                if !matches!(name.as_str(), "counters" | "values") {
                    field_names(child, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| field_names(v, out)),
        _ => {}
    }
}

#[test]
fn every_field_that_a_transcript_shows_has_a_table_row() {
    let doc = doc();
    let mut names = BTreeSet::new();
    for block in transcripts(&doc) {
        for line in block {
            let (Line::Sent(text) | Line::Received(text)) = line;
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                field_names(&value, &mut names);
            }
        }
    }
    // the envelope, and the wrappers that the prose of their method describes
    let envelope = [
        "id", "method", "params", "result", "error", "event", "data", "code", "message", "hint",
        "keys", "checks",
    ];
    let first_cells: Vec<&str> = doc
        .lines()
        .filter(|l| l.starts_with("| "))
        .filter_map(|l| cells(l).first().copied())
        .collect();
    for name in names {
        if envelope.contains(&name.as_str()) {
            continue;
        }
        let quoted = format!("`{name}`");
        assert!(
            first_cells.iter().any(|c| c.contains(&quoted)),
            "no table row documents the field {quoted}"
        );
    }
}

// --- the tables ----------------------------------------------------------------------------------

#[test]
fn every_method_has_its_section_and_its_row() {
    let doc = doc();
    for method in Request::METHODS {
        let heading = format!("### `{method}`");
        assert!(
            doc.lines().any(|l| l == heading),
            "the page has no section {heading}"
        );
    }
    let methods = section(&doc, "## Methods");
    assert!(
        methods.contains(&format!("answers {} methods", Request::METHODS.len())),
        "the count of methods"
    );
    for method in Request::METHODS {
        linked_row(methods, method);
    }
}

#[test]
fn every_topic_has_its_section_and_its_row() {
    let doc = doc();
    let topics = section(&doc, "## Topics and events");
    for topic in Topic::ALL {
        let name = topic.as_str();
        let heading = format!("### The `{name}` topic");
        assert!(
            doc.lines().any(|l| l == heading),
            "the page has no section {heading}"
        );
        let kind = if topic.is_snapshot() {
            "snapshot"
        } else {
            "stream"
        };
        assert_eq!(linked_row(topics, name)[2], kind, "the kind of {name}");
    }
}

macro_rules! variants {
    ($ty:ident: $($variant:ident),+ $(,)?) => {{
        // no wildcard: a new variant fails to compile here, and is the reminder to document it
        fn guard(x: $ty) {
            match x {
                $($ty::$variant => {})+
                $ty::Unknown => {}
            }
        }
        let _ = guard;
        vec![$($ty::$variant),+]
    }};
}

macro_rules! plain_variants {
    ($ty:ident: $($variant:ident),+ $(,)?) => {{
        fn guard(x: $ty) {
            match x {
                $($ty::$variant => {})+
            }
        }
        let _ = guard;
        vec![$($ty::$variant),+]
    }};
}

fn error_codes() -> Vec<ErrorCode> {
    variants!(
        ErrorCode: UnknownMethod, BadParams, BadFingerprint, SelfFingerprint, UnknownPeer,
        AmbiguousPeer, UnknownAnchor, AmbiguousAnchor, PeerMonitorsUnknown, Overlap, StaleLayout,
        Internal, PairingNotAccepting, PairingBusy, PairingExpired, NoPairing, PeerUnreachable,
        UnknownKey, BadValue, ReadOnly
    )
}

fn notice_kinds() -> Vec<NoticeKind> {
    variants!(
        NoticeKind: PairCode, Paired, PairFailed, PeerLost, PeerBack, Unpaired, LayoutReplaced,
        Attention
    )
}

fn ui_views() -> Vec<UiView> {
    variants!(UiView: Pair, Arrange, Popover, Settings)
}

#[test]
fn every_error_code_has_a_row_and_the_page_lists_no_other() {
    let doc = doc();
    let errors = section(&doc, "## Errors");
    let codes = error_codes();
    for code in &codes {
        row(errors, &word(code));
    }
    let listed: Vec<String> = errors
        .lines()
        .filter(|l| l.starts_with("| `"))
        .map(|l| cells(l)[0].trim_matches('`').to_string())
        .collect();
    assert_eq!(listed.len(), codes.len(), "the table lists {listed:?}");
    for code in listed {
        let parsed: ErrorCode = serde_json::from_value(Value::String(code.clone())).unwrap();
        assert_ne!(parsed, ErrorCode::Unknown, "`{code}` is not an error code");
    }
}

#[test]
fn every_notice_kind_has_its_two_rows() {
    let doc = doc();
    let notice = section(&doc, "### The `notice` topic");
    for kind in notice_kinds() {
        let start = format!("| `{}` |", word(&kind));
        let rows = notice.lines().filter(|l| l.starts_with(&start)).count();
        assert_eq!(rows, 2, "{start}: one row says when, one says what");
    }
    // the urgency of each kind is the one in the code
    let urgency = |kind: &str| row(notice, kind)[2].trim_matches('`').to_string();
    for kind in ["pair_code", "attention"] {
        assert_eq!(urgency(kind), "critical", "{kind}");
    }
    for kind in [
        "paired",
        "pair_failed",
        "peer_lost",
        "peer_back",
        "unpaired",
        "layout_replaced",
    ] {
        assert_eq!(urgency(kind), "normal", "{kind}");
    }
}

#[test]
fn every_view_of_the_ui_topic_has_a_row() {
    let doc = doc();
    let ui = section(&doc, "### The `ui` topic");
    for view in ui_views() {
        row(ui, &word(&view));
    }
}

#[test]
fn every_word_of_a_closed_set_is_on_the_page() {
    let doc = doc();
    let mut words: Vec<String> = Vec::new();
    words.extend(error_codes().iter().map(word));
    words.extend(notice_kinds().iter().map(word));
    words.extend(ui_views().iter().map(word));
    words.extend(variants!(Urgency: Low, Normal, Critical).iter().map(word));
    words.extend(variants!(Applies: Live, Restart, Setup).iter().map(word));
    words.extend(
        variants!(ConfigType: Bool, Int, String, Enum, Chord, Name)
            .iter()
            .map(word),
    );
    words.extend(
        variants!(SummaryState: Idle, Ready, Away, Controlled, Locked, Attention)
            .iter()
            .map(word),
    );
    words.extend(
        variants!(
            AttentionCode: PeerRefused, BarriersFailed, CaptureDenied, LayoutUnplaced,
            ConfigInvalid
        )
        .iter()
        .map(word),
    );
    words.extend(
        plain_variants!(
            PairingStage: Listening, Connecting, Code, WaitingPeer, Paired, Failed, Closed
        )
        .iter()
        .map(word),
    );
    words.extend(
        plain_variants!(PairingRole: Dialer, Acceptor)
            .iter()
            .map(word),
    );
    words.extend(
        plain_variants!(LinkState: Connected, Connecting, Offline, Refused)
            .iter()
            .map(word),
    );
    words.extend(
        plain_variants!(Role: Idle, Pending, Arming, Source, Target)
            .iter()
            .map(word),
    );
    words.extend(plain_variants!(Paired: Sas, Manual).iter().map(word));
    words.extend(LeaveReason::ALL.iter().map(|r| r.as_str().to_string()));
    words.extend(PairReason::ALL.iter().map(|r| r.as_str().to_string()));
    words.extend(Topic::ALL.iter().map(|t| t.as_str().to_string()));
    words.extend(Request::METHODS.iter().map(|m| (*m).to_string()));
    for word in words {
        assert!(
            doc.contains(&format!("`{word}`")),
            "the page never names `{word}`"
        );
    }
}

#[test]
fn the_config_table_is_the_schema() {
    let doc = doc();
    let table = section(&doc, "## Config keys");
    let schema = ferryd::config_api::schema();
    assert!(
        table.contains(&format!("has {} keys", schema.keys.len())),
        "the count of keys"
    );
    for key in &schema.keys {
        let c = row(table, &key.key);
        assert_eq!(c[1], word(&key.kind), "the type of {}", key.key);
        assert_eq!(
            c[2],
            format!("`{}`", key.default),
            "the default of {}",
            key.key
        );
        assert_eq!(c[4], word(&key.applies), "when {} applies", key.key);
        if let (Some(min), Some(max)) = (key.min, key.max) {
            assert!(
                c[3].starts_with(&format!("{min} to {max}")),
                "the range of {}: {}",
                key.key,
                c[3]
            );
        }
        for choice in key.choices.iter().flatten() {
            assert!(
                c[3].contains(&format!("`{choice}`")),
                "the choices of {}",
                key.key
            );
        }
    }
    let rows = table.lines().filter(|l| l.starts_with("| `")).count();
    assert_eq!(
        rows,
        schema.keys.len(),
        "the table lists a key that does not exist"
    );
}

#[test]
fn the_numbers_in_the_prose_are_the_codes() {
    let doc = doc();
    let says = |text: &str| assert!(doc.contains(text), "the page does not say {text:?}");
    says(&format!("ferryd {}", env!("CARGO_PKG_VERSION")));
    says(&format!("API version {API_VERSION}"));
    assert_eq!(MAX_LINE, 1_048_576);
    says("1 MiB (1,048,576 bytes)");
    assert_eq!(ferryd::server::OUT_QUEUE, 256);
    says("once 256 lines wait");
    assert_eq!(ferryd::events::COALESCE, Duration::from_millis(50));
    says("once every 50 milliseconds");
    assert_eq!(ferryd::events::REFRESH, Duration::from_secs(2));
    says("every 2 seconds");
    assert_eq!(ferryd::events::EVENT_BACKLOG, 32);
    says("up to 32 waiting events");
    assert_eq!(ferryd::pairing::WINDOW, Duration::from_secs(120));
    says("for 120 seconds");
    assert_eq!(ferryd::pairing::MAX_FAILURES, 3);
    says("After 3 failed attempts");
    assert_eq!(ferryd::notify::policy::CONNECTION_WINDOW.0, 30_000_000);
    says("in 30 seconds");
    assert_eq!(ferryd::notify::policy::ATTENTION_WINDOW.0, 300_000_000);
    says("in 5 minutes");
    assert_eq!(caps::LAYOUT_PEERS, 4);
    says("more than 4 peers");
    assert_eq!(caps::COORD, 1 << 24);
    says("at most 2^24");
}
