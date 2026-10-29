//! The clipboard's wire pieces besides its control messages: the header of a data
//! stream, transfer ids, abort reasons, and which mime types are shared.
//!
//! A transfer is lazy. On a copy only [`Control::ClipOffer`](super::Control::ClipOffer) moves;
//! when an application on the other peer pastes, that peer sends
//! [`Control::ClipRequest`](super::Control::ClipRequest), and the provider answers on a
//! unidirectional stream of its own: a [`DataHeader`], then the bytes, then the finish. A
//! provider that cannot serve answers with [`Control::ClipAbort`](super::Control::ClipAbort)
//! instead.

/// The start of a data stream: `u32 LE id | u64 LE length`, then `length` bytes and the finish.
/// The length is known before the first byte goes: the provider reads the whole item first, so
/// an item over the cap is refused before anything moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataHeader {
    pub id: u32,
    pub len: u64,
}

impl DataHeader {
    pub const LEN: usize = 12;

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[..4].copy_from_slice(&self.id.to_le_bytes());
        b[4..].copy_from_slice(&self.len.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; Self::LEN]) -> Self {
        let mut id = [0u8; 4];
        let mut len = [0u8; 8];
        id.copy_from_slice(&b[..4]);
        len.copy_from_slice(&b[4..]);
        Self {
            id: u32::from_le_bytes(id),
            len: u64::from_le_bytes(len),
        }
    }
}

/// Request ids: odd from the dialer, even from the acceptor. An id names one request in both
/// directions, so `ClipAbort{id}` says by its parity whether it is about a request this peer
/// made or one it serves.
#[derive(Debug, Clone, Copy)]
pub struct TransferIds {
    next: u32,
}

impl TransferIds {
    pub fn new(dialer: bool) -> Self {
        Self {
            next: if dialer { 1 } else { 2 },
        }
    }

    pub fn next_id(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(2);
        if self.next == 0 {
            self.next = 2;
        }
        id
    }

    /// Whether `id` is one of this side's own requests.
    pub fn is_ours(dialer: bool, id: u32) -> bool {
        (id % 2 == 1) == dialer
    }
}

/// Why a transfer ended without data, as a `u8` on the wire. Unknown codes decode as `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AbortReason {
    Other = 0,
    /// The offer named by the request is no longer the provider's clipboard.
    Stale = 1,
    /// The item is larger than the provider's `clipboard.max_bytes`.
    TooLarge = 2,
    /// The provider could not read it, or shares nothing (sharing off).
    Unavailable = 3,
    /// The requester no longer wants it.
    Cancelled = 4,
}

impl AbortReason {
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Stale,
            2 => Self::TooLarge,
            3 => Self::Unavailable,
            4 => Self::Cancelled,
            _ => Self::Other,
        }
    }

    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Other => "other",
            Self::Stale => "stale",
            Self::TooLarge => "too_large",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Set on a selection by a password manager (KeePassXC, KDE): never shared.
pub const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

/// The mime types of a local selection that are offered to peers, in the order the application
/// listed them, or `None` when nothing of it is shared: a password manager's item,
/// or one with no text or image type. Files (`text/uri-list` and the file managers' own types)
/// are not shared in v1: their paths would name files the other machine lacks.
pub fn shareable(mimes: &[String]) -> Option<Vec<String>> {
    if mimes.iter().any(|m| m == PASSWORD_HINT) {
        return None;
    }
    let out: Vec<String> = mimes
        .iter()
        .filter(|m| is_shared_mime(m))
        .take(super::caps::CLIP_MIMES)
        .cloned()
        .collect();
    (!out.is_empty()).then_some(out)
}

fn is_shared_mime(m: &str) -> bool {
    if m.is_empty()
        || m.len() > super::caps::CLIP_MIME_LEN
        || !m.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
    {
        return false;
    }
    let base = m.split(';').next().unwrap_or(m).trim();
    if base == "text/uri-list" || base == "text/x-moz-url" {
        return false;
    }
    matches!(base, "UTF8_STRING" | "STRING" | "TEXT" | "COMPOUND_TEXT")
        || base.starts_with("text/")
        || base.starts_with("image/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_header_round_trips() {
        let h = DataHeader {
            id: 7,
            len: 20 << 20,
        };
        assert_eq!(DataHeader::decode(&h.encode()), h);
    }

    #[test]
    fn ids_are_odd_from_the_dialer_and_even_from_the_acceptor() {
        let (mut d, mut a) = (TransferIds::new(true), TransferIds::new(false));
        assert_eq!([d.next_id(), d.next_id()], [1, 3]);
        assert_eq!([a.next_id(), a.next_id()], [2, 4]);
        assert!(TransferIds::is_ours(true, 3));
        assert!(!TransferIds::is_ours(true, 4));
        assert!(TransferIds::is_ours(false, 4));
    }

    #[test]
    fn ids_never_become_zero_or_change_parity_when_they_wrap() {
        let mut d = TransferIds { next: u32::MAX };
        assert_eq!(d.next_id(), u32::MAX);
        assert_eq!(d.next_id(), 1);
        let mut a = TransferIds { next: u32::MAX - 1 };
        assert_eq!(a.next_id(), u32::MAX - 1);
        assert_eq!(a.next_id(), 2);
    }

    #[test]
    fn text_and_images_are_shared_files_and_app_types_are_not() {
        let got = shareable(&v(&[
            "text/plain;charset=utf-8",
            "UTF8_STRING",
            "text/html",
            "image/png",
            "text/uri-list",
            "x-special/gnome-copied-files",
            "chromium/x-web-custom-data",
            "application/x-ferry-source",
        ]));
        assert_eq!(
            got,
            Some(v(&[
                "text/plain;charset=utf-8",
                "UTF8_STRING",
                "text/html",
                "image/png"
            ]))
        );
    }

    #[test]
    fn a_password_managers_item_and_a_file_copy_are_not_offered() {
        assert_eq!(
            shareable(&v(&["text/plain", PASSWORD_HINT])),
            None,
            "FR-CLIP-07"
        );
        assert_eq!(shareable(&v(&["text/uri-list"])), None);
        assert_eq!(shareable(&[]), None);
    }

    #[test]
    fn an_unknown_abort_code_is_other() {
        assert_eq!(AbortReason::from_code(200), AbortReason::Other);
        for r in [
            AbortReason::Stale,
            AbortReason::TooLarge,
            AbortReason::Unavailable,
            AbortReason::Cancelled,
        ] {
            assert_eq!(AbortReason::from_code(r.code()), r);
        }
    }
}
