# ferry-net

ferry's transport: QUIC (`quinn` 0.11.12) on one UDP port, on ferry's own DSCP-marked socket,
with TLS 1.3 mutual authentication pinned to peer fingerprints. It carries opaque bytes; the
message format is `ferry-core`'s. Decisions: ADR 0006 (trust), ADR 0016 (transport).

## Contract for ferryd and ferry-core

| Item | Value |
|---|---|
| ALPN | `ferry/1` for sessions; `ferry-pair/1` for pairing (below). A client offers exactly one |
| Port | 4243/udp, one dual-stack socket (`[::]`), IPv4-only fallback |
| Fingerprint | SHA-256 of the certificate's SubjectPublicKeyInfo DER (`Fingerprint(pub [u8; 32])`), equal to `ferry_core::PeerId::of_spki`; `Display` = 64 lowercase hex, `short()` = first 16 |
| Identity files | `<dir>/key.der` (Ed25519 PKCS#8), `<dir>/cert.der`; dir 0700, files 0600; a group- or world-accessible dir or key is refused |
| Trust | `TrustSet`, shared with the verifiers; `Endpoint::set_trust` swaps it live. Unknown key: `ConnectError::UnknownPeer(fp)`; a dial answered by another key: `FingerprintChanged{expected, got}` |
| Frames | `try_send_frame`: sync, non-blocking, 256-frame queue per connection, `FrameError::{Full, Closed, TooLarge, Empty}`. Control stream opened by the dialer; `u32 LE length \| body`; body 1 B … 1 MiB (`MAX_FRAME`); empty frames reserved |
| Over-cap frame | the receiver closes with code 3 |
| Datagrams | `send_datagram`: sync, never blocks; 4 KiB send buffer, oldest dropped when full. A peer with datagrams off or a limit below 28 bytes is refused (`DatagramsUnsupported`) |
| Close codes | 0 shutdown, 1 version, 2 protocol, 3 frame too large, 4 unpaired, 5 duplicate |
| Closing | `close(code, reason)` at once; `close_graceful(code, reason, timeout)` delivers queued frames first |
| Inbound | `Events::next()`: `Frame`, `Datagram`, then `Closed(CloseReason)` once (`.code()`, `.reason()`, `.by_peer()`) after every earlier frame |
| Keep-alive / idle | 5 s / 10 s |
| Manager | dials and redials trusted peers, reports one `Connected` per peer even on a simultaneous dial; `forget(fp)` closes with `SHUTDOWN`, `forget_with(fp, code)` with the given code (`UNPAIRED` on unpair) |
| Handshake limit | before TLS: 8 per source IP, refilling at 2/s; 32 in flight; refused with CONNECTION_REFUSED; `Endpoint::handshakes_refused()` |
| Pairing | `set_pairing(Some(PairingWindow{until}))` opens the window. An incoming connection is offered `ferry-pair/1` only while it is open, no attempt runs, and its address is under 3 attempts per 60 s (`PAIRING_BURST`, one back every 20 s); else a pairing dialer gets `ConnectError::NotAccepting`. On `ferry-pair/1` either side accepts any key but its own (`ConnectError::OwnKey`) and records it; `ferry/1` stays pinned. `accept_pairing()` / `dial_pairing(addr)` give a `PairingConnection{conn, events, remote}` that never reaches `accept()` or the `Manager`; the attempt counts as running until it is dropped. `pairing_refused()` counts refusals |
| Stateless reset | the reset key and the connection-ID check key are derived from the identity (HKDF-SHA256), so a peer restarted with the same key resets its previous run's connections: the dialer sees `CloseReason::Reset` at its first packet after the restart |
| Threading | create the `Endpoint` and await `accept`, `dial`, `Events::next`, `Manager::next`, `close_graceful` on one tokio runtime (`net`); `Connection` is `Clone + Send + Sync`, and its sync methods work from any thread |

## Tests

`cargo test -p ferry-net`: loopback only, no compositor. `tests/dscp.rs` relays packets through
a socket that reads each packet's TOS with `IP_RECVTOS` / `IPV6_RECVTCLASS`.

## Real-wire probe

```bash
cargo build --release -p ferry-net --example net-probe
net-probe id <dir>                                  # prints this machine's fingerprint
net-probe serve <dir> <client-fp> 4243 60           # on the target
net-probe client <dir> <server-fp> <ip>:4243 10000 1000
```

The client prints RTT/2 percentiles of 24-byte datagrams echoed over a pinned connection; both
sides print how many received packets carried DSCP EF.
