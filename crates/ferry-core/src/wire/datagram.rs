//! The datagram lane: fixed 28-byte little-endian layout.
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 1 | type: 1 Motion, 2 Ping, 3 Pong |
//! | 1 | 1 | flags: 0 (receivers ignore unknown bits) |
//! | 2 | 2 | session, low 16 bits (0 for Ping/Pong) |
//! | 4 | 4 | seq u32: Motion seq (per connection), or the Ping id |
//! | 8 | 4 | x f32: target-local logical (0 for Ping/Pong) |
//! | 12 | 4 | y f32 |
//! | 16 | 4 | dx f32: relative delta after the source's acceleration |
//! | 20 | 4 | dy f32 |
//! | 24 | 4 | t_us: sender monotonic µs, low 32 bits (Pong echoes the Ping's) |

/// Every datagram type is this long. A datagram of another length is dropped.
pub const DATAGRAM_LEN: usize = 28;

pub const TYPE_MOTION: u8 = 1;
pub const TYPE_PING: u8 = 2;
pub const TYPE_PONG: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub session_lo: u16,
    pub seq: u32,
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
    pub t_us: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Datagram {
    Motion(Motion),
    /// Answered at once with a `Pong` carrying the same `id` and `t_us`.
    Ping {
        id: u32,
        t_us: u32,
    },
    Pong {
        id: u32,
        t_us: u32,
    },
}

impl Datagram {
    pub fn encode(&self, out: &mut [u8; DATAGRAM_LEN]) {
        let (ty, session, seq, x, y, dx, dy, t_us) = match *self {
            Datagram::Motion(m) => (
                TYPE_MOTION,
                m.session_lo,
                m.seq,
                m.x,
                m.y,
                m.dx,
                m.dy,
                m.t_us,
            ),
            Datagram::Ping { id, t_us } => (TYPE_PING, 0, id, 0.0, 0.0, 0.0, 0.0, t_us),
            Datagram::Pong { id, t_us } => (TYPE_PONG, 0, id, 0.0, 0.0, 0.0, 0.0, t_us),
        };
        out[0] = ty;
        out[1] = 0;
        out[2..4].copy_from_slice(&session.to_le_bytes());
        out[4..8].copy_from_slice(&seq.to_le_bytes());
        out[8..12].copy_from_slice(&x.to_le_bytes());
        out[12..16].copy_from_slice(&y.to_le_bytes());
        out[16..20].copy_from_slice(&dx.to_le_bytes());
        out[20..24].copy_from_slice(&dy.to_le_bytes());
        out[24..28].copy_from_slice(&t_us.to_le_bytes());
    }

    /// Bounds check + six loads; never panics on any input.
    pub fn decode(b: &[u8]) -> Result<Datagram, super::DecodeError> {
        let b: &[u8; DATAGRAM_LEN] = b
            .try_into()
            .map_err(|_| super::DecodeError::DatagramLength(b.len()))?;
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let f32_at = |o: usize| f32::from_bits(u32_at(o));
        // b[1] is flags: unknown bits are ignored.
        let seq = u32_at(4);
        let t_us = u32_at(24);
        match b[0] {
            TYPE_MOTION => Ok(Datagram::Motion(Motion {
                session_lo: u16::from_le_bytes([b[2], b[3]]),
                seq,
                x: f32_at(8),
                y: f32_at(12),
                dx: f32_at(16),
                dy: f32_at(20),
                t_us,
            })),
            TYPE_PING => Ok(Datagram::Ping { id: seq, t_us }),
            TYPE_PONG => Ok(Datagram::Pong { id: seq, t_us }),
            t => Err(super::DecodeError::DatagramType(t)),
        }
    }
}

/// RFC 1982 serial-number order on `u32`: is `a` newer than `b`?
/// (`(a - b) as i32 > 0`, wrapping).
pub fn seq_newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}
