//! The toasts: what ferryd tells the user, and in which words.
//!
//! Pure data and text. A [`Toast`] is also what the `notice` event is built from, so it
//! carries what a frontend needs and nothing of the desktop's wire format. The words follow the
//! FRD: short, sentence case, `CONTEXT.md`'s terms.
//!
//! **The pairing code is in a pair-code toast's title.** Its `Debug` hides the texts, as
//! `PairCode`'s does: a `{:?}` in a log never leaks it.

use std::fmt;

use ferry_core::api::{Notice, NoticeKind, UiView};

/// The pairing window: a code toast is no use after it.
const WINDOW_MS: u64 = 120_000;
/// A notification whose lifetime is `0` never expires, so a short one is rounded up.
const MIN_EXPIRE_MS: u64 = 1_000;
/// Longest title and body a toast carries. Text that comes from a peer's name, an error or a
/// frontend's attention list is cut here, so no toast can fill the screen.
const TITLE_MAX: usize = 120;
const BODY_MAX: usize = 400;

/// What a toast is about. [`Kind::as_str`] is the `kind` of the `notice` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    PairCode,
    Paired,
    PairFailed,
    PeerLost,
    PeerBack,
    Unpaired,
    LayoutReplaced,
    Attention,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PairCode => "pair_code",
            Self::Paired => "paired",
            Self::PairFailed => "pair_failed",
            Self::PeerLost => "peer_lost",
            Self::PeerBack => "peer_back",
            Self::Unpaired => "unpaired",
            Self::LayoutReplaced => "layout_replaced",
            Self::Attention => "attention",
        }
    }

    /// The pairing kinds share one notification on the desktop: the result replaces the code.
    pub const fn shares_pairing_toast(self) -> bool {
        matches!(self, Self::PairCode | Self::Paired | Self::PairFailed)
    }

    const fn notice(self) -> NoticeKind {
        match self {
            Self::PairCode => NoticeKind::PairCode,
            Self::Paired => NoticeKind::Paired,
            Self::PairFailed => NoticeKind::PairFailed,
            Self::PeerLost => NoticeKind::PeerLost,
            Self::PeerBack => NoticeKind::PeerBack,
            Self::Unpaired => NoticeKind::Unpaired,
            Self::LayoutReplaced => NoticeKind::LayoutReplaced,
            Self::Attention => NoticeKind::Attention,
        }
    }
}

/// `urgency` of the `notice` event, and the `urgency` hint of `Notify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Normal,
    Critical,
}

impl Urgency {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Critical => "critical",
        }
    }

    const fn notice(self) -> ferry_core::api::Urgency {
        match self {
            Self::Normal => ferry_core::api::Urgency::Normal,
            Self::Critical => ferry_core::api::Urgency::Critical,
        }
    }

    /// The notification spec's byte: 0 low, 1 normal, 2 critical.
    pub const fn hint(self) -> u8 {
        match self {
            Self::Normal => 1,
            Self::Critical => 2,
        }
    }
}

/// The one action a toast offers: a button that asks a frontend to open a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToastAction {
    /// The key `ActionInvoked` reports.
    pub key: &'static str,
    /// The button's text.
    pub label: &'static str,
    /// The view a frontend opens: `ui {"open": …}`.
    pub opens: UiView,
}

/// The pairing toast's action: open the Pair tab.
pub const OPEN_PAIR: ToastAction = ToastAction {
    key: "open",
    label: "Open",
    opens: UiView::Pair,
};

#[derive(Clone, PartialEq, Eq)]
pub struct Toast {
    pub kind: Kind,
    pub title: String,
    pub body: String,
    /// The peer it is about, by name.
    pub peer: Option<String>,
    pub urgency: Urgency,
    /// How long the desktop should keep it, when ferryd knows. `None`: the daemon's default.
    pub expire_ms: Option<u32>,
    pub action: Option<ToastAction>,
}

impl fmt::Debug for Toast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Toast");
        d.field("kind", &self.kind)
            .field("peer", &self.peer)
            .field("urgency", &self.urgency);
        if self.kind != Kind::PairCode {
            d.field("title", &self.title).field("body", &self.body);
        }
        d.finish_non_exhaustive()
    }
}

/// Control characters become spaces, runs of spaces become one, and a text over `max`
/// characters ends in an ellipsis.
fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut space = true; // drops leading spaces too
    let mut count = 0usize;
    for c in text.chars() {
        let c = if c.is_control() || c.is_whitespace() {
            ' '
        } else {
            c
        };
        if c == ' ' && space {
            continue;
        }
        space = c == ' ';
        if count >= max {
            out.truncate(out.trim_end().len());
            out.push('…');
            return out;
        }
        out.push(c);
        count = count.saturating_add(1);
    }
    out.truncate(out.trim_end().len());
    out
}

/// `text` with a capital first letter.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl Toast {
    /// This toast as the `notice` event every frontend gets: the same words, whether or
    /// not a notification daemon shows them. The title of a code toast holds the pairing code.
    pub fn notice(&self) -> Notice {
        Notice {
            kind: self.kind.notice(),
            title: self.title.clone(),
            body: self.body.clone(),
            peer: self.peer.clone(),
            urgency: self.urgency.notice(),
        }
    }

    fn new(kind: Kind, title: &str, body: &str, peer: Option<&str>, urgency: Urgency) -> Self {
        Self {
            kind,
            title: clean(title, TITLE_MAX),
            body: clean(body, BODY_MAX),
            peer: peer.map(|p| clean(p, TITLE_MAX)),
            urgency,
            expire_ms: None,
            action: None,
        }
    }

    /// An incoming pairing request shows its code: critical, with the action that
    /// opens the Pair tab. `code` is `418 207`, and `window_ms` is what is left of the pairing
    /// window.
    pub fn pair_code(peer: &str, code: &str, window_ms: u64) -> Self {
        let mut t = Self::new(
            Kind::PairCode,
            &format!("Pairing code {code}"),
            &format!("{peer} is asking to pair. Confirm only if it shows the same code."),
            Some(peer),
            Urgency::Critical,
        );
        let ms = window_ms.clamp(MIN_EXPIRE_MS, WINDOW_MS);
        t.expire_ms = Some(u32::try_from(ms).unwrap_or(u32::MAX));
        t.action = Some(OPEN_PAIR);
        t
    }

    /// J2 step 4. `note` is why the pin has another name than the peer sent, when it has.
    pub fn paired(peer: &str, note: Option<&str>) -> Self {
        Self::new(
            Kind::Paired,
            &format!("Paired with {peer}"),
            &note.map(capitalized).unwrap_or_default(),
            Some(peer),
            Urgency::Normal,
        )
    }

    /// J2 step 5. `reason` is the socket API's (`rejected`, `timeout`, `bad_reveal`, …) and
    /// `message` is what `pairing` events tell the user (`cancelled: codes differ`).
    pub fn pair_failed(reason: &str, message: &str) -> Self {
        // A cancel is not a fault: the title says cancelled and the body says why.
        let cancelled = matches!(reason, "rejected" | "cancelled" | "timeout" | "other");
        let detail = message
            .strip_prefix("cancelled")
            .map_or(message, |rest| rest.trim_start_matches([':', ' ']));
        Self::new(
            Kind::PairFailed,
            if cancelled {
                "Pairing cancelled"
            } else {
                "Pairing failed"
            },
            &capitalized(detail),
            None,
            Urgency::Normal,
        )
    }

    /// J6 step 3.
    pub fn peer_lost(peer: &str) -> Self {
        Self::new(
            Kind::PeerLost,
            &format!("Lost {peer}"),
            "",
            Some(peer),
            Urgency::Normal,
        )
    }

    /// J6 step 4.
    pub fn peer_back(peer: &str) -> Self {
        Self::new(
            Kind::PeerBack,
            &format!("{peer} is back"),
            "",
            Some(peer),
            Urgency::Normal,
        )
    }

    /// The other side unpaired this machine.
    pub fn unpaired(peer: &str) -> Self {
        Self::new(
            Kind::Unpaired,
            &format!("{peer} unpaired this machine"),
            "Pair again to reconnect",
            Some(peer),
            Urgency::Normal,
        )
    }

    /// FR-LAYOUT-10.
    pub fn layout_replaced(winner: &str) -> Self {
        Self::new(
            Kind::LayoutReplaced,
            &format!("Layout replaced by {winner}"),
            "Your last change was replaced by a newer one",
            Some(winner),
            Urgency::Normal,
        )
    }

    /// A problem the user must act on: the attention entry's `message` and `hint`, as they are.
    pub fn attention(message: &str, hint: Option<&str>, peer: Option<&str>) -> Self {
        Self::new(
            Kind::Attention,
            message,
            hint.unwrap_or_default(),
            peer,
            Urgency::Critical,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_are_the_frds() {
        // J6 steps 3 and 4, J2 step 4, FR-LAYOUT-10
        assert_eq!(Toast::peer_lost("lap").title, "Lost lap");
        assert_eq!(Toast::peer_back("lap").title, "lap is back");
        assert_eq!(Toast::paired("lap", None).title, "Paired with lap");
        assert_eq!(
            Toast::layout_replaced("lap").title,
            "Layout replaced by lap"
        );
    }

    #[test]
    fn a_pairing_code_is_critical_with_the_open_action_and_the_window_as_lifetime() {
        let t = Toast::pair_code("lap", "418 207", 119_400);
        assert_eq!(t.kind, Kind::PairCode);
        assert_eq!(t.urgency, Urgency::Critical);
        assert_eq!(t.title, "Pairing code 418 207");
        assert!(t.body.starts_with("lap is asking to pair"), "{}", t.body);
        assert_eq!(t.action, Some(OPEN_PAIR));
        assert_eq!(t.expire_ms, Some(119_400));
        // never `0` (never expires), never longer than the window
        assert_eq!(
            Toast::pair_code("lap", "1", 0).expire_ms,
            Some(MIN_EXPIRE_MS as u32)
        );
        assert_eq!(
            Toast::pair_code("lap", "1", u64::MAX).expire_ms,
            Some(WINDOW_MS as u32)
        );
    }

    #[test]
    fn only_the_attention_and_the_code_toasts_are_critical() {
        let all = [
            Toast::pair_code("lap", "418 207", 1),
            Toast::paired("lap", None),
            Toast::pair_failed("rejected", "cancelled: codes differ"),
            Toast::peer_lost("lap"),
            Toast::peer_back("lap"),
            Toast::unpaired("lap"),
            Toast::layout_replaced("lap"),
            Toast::attention("lap's fingerprint changed", None, Some("lap")),
        ];
        let critical: Vec<_> = all
            .iter()
            .filter(|t| t.urgency == Urgency::Critical)
            .map(|t| t.kind.as_str())
            .collect();
        assert_eq!(critical, ["pair_code", "attention"]);
        // only the code toast has a button, and it opens the Pair tab
        let actions: Vec<_> = all.iter().filter_map(|t| t.action).collect();
        assert_eq!(actions, [OPEN_PAIR]);
    }

    #[test]
    fn a_failed_pairing_says_cancelled_for_a_cancel_and_failed_for_a_fault() {
        let t = Toast::pair_failed("rejected", "cancelled: codes differ");
        assert_eq!(
            (t.title.as_str(), t.body.as_str()),
            ("Pairing cancelled", "Codes differ")
        );
        let t = Toast::pair_failed("cancelled", "cancelled");
        assert_eq!(
            (t.title.as_str(), t.body.as_str()),
            ("Pairing cancelled", "")
        );
        let t = Toast::pair_failed("timeout", "timed out");
        assert_eq!(
            (t.title.as_str(), t.body.as_str()),
            ("Pairing cancelled", "Timed out")
        );
        let t = Toast::pair_failed("version", "version mismatch: 1.0 vs 2.0");
        assert_eq!(
            (t.title.as_str(), t.body.as_str()),
            ("Pairing failed", "Version mismatch: 1.0 vs 2.0")
        );
        let t = Toast::pair_failed("lost", "connection lost");
        assert_eq!(
            (t.title.as_str(), t.body.as_str()),
            ("Pairing failed", "Connection lost")
        );
    }

    #[test]
    fn a_toast_is_the_notice_with_the_same_words() {
        use ferry_core::api::{NoticeKind, Urgency as Api};
        let n = Toast::peer_back("lap").notice();
        assert_eq!(
            (
                n.kind,
                n.title.as_str(),
                n.body.as_str(),
                n.peer.as_deref(),
                n.urgency
            ),
            (
                NoticeKind::PeerBack,
                "lap is back",
                "",
                Some("lap"),
                Api::Normal
            )
        );
        let n = Toast::pair_code("lap", "418 207", 60_000).notice();
        assert_eq!((n.kind, n.urgency), (NoticeKind::PairCode, Api::Critical));
        assert_eq!(n.title, "Pairing code 418 207");
        // the kind names of the two types are the same words
        for t in [
            Toast::pair_code("lap", "418 207", 1),
            Toast::paired("lap", None),
            Toast::pair_failed("rejected", "cancelled: codes differ"),
            Toast::peer_lost("lap"),
            Toast::peer_back("lap"),
            Toast::unpaired("lap"),
            Toast::layout_replaced("lap"),
            Toast::attention("m", None, None),
        ] {
            let json = serde_json::to_value(t.notice().kind).unwrap();
            assert_eq!(json, t.kind.as_str());
            let json = serde_json::to_value(t.notice().urgency).unwrap();
            assert_eq!(json, t.urgency.as_str());
        }
        // and its Debug keeps the code out of a log
        let shown = format!("{:?}", Toast::pair_code("lap", "418 207", 1).notice());
        assert!(!shown.contains("418") && !shown.contains("207"), "{shown}");
    }

    #[test]
    fn every_kind_has_its_own_name() {
        let kinds = [
            Kind::PairCode,
            Kind::Paired,
            Kind::PairFailed,
            Kind::PeerLost,
            Kind::PeerBack,
            Kind::Unpaired,
            Kind::LayoutReplaced,
            Kind::Attention,
        ];
        let names: std::collections::BTreeSet<_> = kinds.iter().map(|k| k.as_str()).collect();
        assert_eq!(names.len(), kinds.len());
        assert!(
            names
                .iter()
                .all(|n| { n.chars().all(|c| c.is_ascii_lowercase() || c == '_') })
        );
    }

    #[test]
    fn text_from_elsewhere_is_one_clean_line_of_bounded_length() {
        let t = Toast::attention(
            "lap\u{1b}[31m\nsays\t\tso   twice",
            Some(&"x".repeat(1000)),
            None,
        );
        assert_eq!(t.title, "lap [31m says so twice");
        assert_eq!(t.body.chars().count(), BODY_MAX + 1);
        assert!(t.body.ends_with('…'));
        assert!(!t.title.chars().any(char::is_control));
        assert_eq!(clean("  \n ", 10), "");
        assert_eq!(clean("ünï", 3), "ünï");
    }

    #[test]
    fn the_debug_of_a_code_toast_hides_the_code() {
        let t = Toast::pair_code("lap", "418 207", 60_000);
        let shown = format!("{t:?}");
        assert!(!shown.contains("418") && !shown.contains("207"), "{shown}");
        assert!(
            shown.contains("PairCode") && shown.contains("lap"),
            "{shown}"
        );
        // the other kinds show their words: they are not secret
        let shown = format!("{:?}", Toast::peer_lost("lap"));
        assert!(shown.contains("Lost lap"), "{shown}");
    }
}
