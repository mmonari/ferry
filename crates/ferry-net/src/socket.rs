//! ferry's own UDP socket for quinn: plain `sendto`, `recvmsg` for the TOS byte only, no GSO,
//! GRO or ECN, DSCP EF set once on the socket.
//!
//! Stock quinn-udp 0.5.16 puts an `IP_TOS` cmsg carrying the ECN bits on every `sendmsg`,
//! which overrides a socket-level DSCP, so EF never leaves the host. This socket never
//! passes a per-packet TOS, so the socket option holds.

use nix::sys::socket::{ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use socket2::{Domain, Protocol, Socket, Type};
use std::io::{self, IoSliceMut};
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};
use tokio::io::Interest;

/// The TOS / traffic-class byte for DSCP EF (46 << 2).
pub const DSCP_EF: u8 = 0xB8;

/// Counters of ferry's socket, for `ferry doctor` and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SocketStats {
    /// UDP packets received.
    pub rx_packets: u64,
    /// Received packets whose IPv4 TOS or IPv6 traffic class carried DSCP EF. Below
    /// `rx_packets` while the peer marks EF means something on the path re-marks it.
    pub rx_ef: u64,
    /// The TOS / traffic-class byte of the last received packet that reported one.
    pub rx_last_tos: Option<u8>,
    /// Packets dropped because `sendto` failed with something other than `EAGAIN`, such as
    /// `ENETUNREACH` while the Wi-Fi is down. QUIC recovers them like any loss.
    pub tx_errors: u64,
}

#[derive(Debug, Default)]
struct Counters {
    rx_packets: AtomicU64,
    rx_ef: AtomicU64,
    /// 0 = none yet, else 0x100 | tos
    rx_last_tos: AtomicU64,
    tx_errors: AtomicU64,
}

/// Binds the UDP socket. An unspecified IPv6 address (`[::]`) gets a dual-stack socket, and
/// falls back to `0.0.0.0` on a host without IPv6. DSCP EF is set for both families when
/// `dscp_ef` is true; reception of the TOS byte is always on (for [`SocketStats`]).
pub(crate) fn bind(addr: SocketAddr, dscp_ef: bool) -> io::Result<std::net::UdpSocket> {
    match bind_exact(addr, dscp_ef) {
        Err(e)
            if addr.ip().is_unspecified()
                && addr.is_ipv6()
                && e.raw_os_error() == Some(nix::libc::EAFNOSUPPORT) =>
        {
            bind_exact(SocketAddr::from(([0, 0, 0, 0], addr.port())), dscp_ef)
        }
        r => r,
    }
}

fn bind_exact(addr: SocketAddr, dscp_ef: bool) -> io::Result<std::net::UdpSocket> {
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let s = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        // IPv4 peers arrive as v4-mapped addresses; the IPv4 options below still apply to them.
        s.set_only_v6(false)?;
        s.set_recv_tclass_v6(true)?;
        if dscp_ef {
            s.set_tclass_v6(u32::from(DSCP_EF))?;
        }
    }
    // IP_TOS and IP_RECVTOS also govern IPv4 traffic on a dual-stack IPv6 socket (Linux).
    s.set_recv_tos_v4(true)?;
    if dscp_ef {
        s.set_tos_v4(u32::from(DSCP_EF))?;
    }
    s.set_nonblocking(true)?;
    s.bind(&addr.into())?;
    Ok(s.into())
}

/// The quinn socket. Created inside a tokio runtime, used only from it.
#[derive(Debug)]
pub(crate) struct FerrySocket {
    io: Arc<tokio::net::UdpSocket>,
    cmsg: Mutex<Vec<u8>>,
    counters: Arc<Counters>,
}

/// Reads [`SocketStats`] after the socket has moved into quinn.
#[derive(Debug, Clone)]
pub(crate) struct StatsHandle(Arc<Counters>);

impl StatsHandle {
    pub(crate) fn get(&self) -> SocketStats {
        let c = &self.0;
        let last = c.rx_last_tos.load(Ordering::Relaxed);
        SocketStats {
            rx_packets: c.rx_packets.load(Ordering::Relaxed),
            rx_ef: c.rx_ef.load(Ordering::Relaxed),
            rx_last_tos: (last != 0).then_some(last as u8),
            tx_errors: c.tx_errors.load(Ordering::Relaxed),
        }
    }
}

impl FerrySocket {
    pub(crate) fn new(sock: std::net::UdpSocket) -> io::Result<(Self, StatsHandle)> {
        let counters = Arc::new(Counters::default());
        Ok((
            Self {
                io: Arc::new(tokio::net::UdpSocket::from_std(sock)?),
                // room for an IPv4 TOS (1 byte) and an IPv6 TCLASS (int) message
                cmsg: Mutex::new(nix::cmsg_space!(u8, i32)),
                counters: counters.clone(),
            },
            StatsHandle(counters),
        ))
    }

    /// Receives one packet into `buf`. `Ok(None)` means it was dropped (truncated, or from an
    /// address family quinn cannot use).
    fn recv_one(&self, buf: &mut IoSliceMut<'_>) -> io::Result<Option<RecvMeta>> {
        let mut cmsg = self.cmsg.lock().unwrap_or_else(PoisonError::into_inner);
        let fd = self.io.as_raw_fd();
        let msg = self.io.try_io(Interest::READABLE, || {
            recvmsg::<SockaddrStorage>(
                fd,
                std::slice::from_mut(buf),
                Some(&mut cmsg),
                MsgFlags::empty(),
            )
            .map_err(io::Error::from)
        })?;
        let c = &self.counters;
        c.rx_packets.fetch_add(1, Ordering::Relaxed);
        let mut tos = None;
        if let Ok(cmsgs) = msg.cmsgs() {
            for m in cmsgs {
                match m {
                    ControlMessageOwned::Ipv4Tos(t) => tos = Some(t),
                    ControlMessageOwned::Ipv6TClass(t) => tos = u8::try_from(t).ok(),
                    _ => {}
                }
            }
        }
        if let Some(t) = tos {
            c.rx_last_tos.store(0x100 | u64::from(t), Ordering::Relaxed);
            if t & 0xFC == DSCP_EF {
                c.rx_ef.fetch_add(1, Ordering::Relaxed);
            }
        }
        if msg.flags.contains(MsgFlags::MSG_TRUNC) {
            return Ok(None);
        }
        let addr = msg.address.as_ref().and_then(to_std);
        Ok(addr.map(|addr| RecvMeta {
            addr,
            len: msg.bytes,
            stride: msg.bytes,
            ecn: None,
            dst_ip: None,
        }))
    }
}

fn to_std(a: &SockaddrStorage) -> Option<SocketAddr> {
    if let Some(v4) = a.as_sockaddr_in() {
        Some(SocketAddr::V4(SocketAddrV4::from(*v4)))
    } else {
        a.as_sockaddr_in6()
            .map(|v6| SocketAddr::V6(SocketAddrV6::from(*v6)))
    }
}

/// Errors after which the socket is still usable: the packet is lost, QUIC recovers.
fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::AddrNotAvailable
            | io::ErrorKind::PermissionDenied
    ) || e.raw_os_error() == Some(nix::libc::EMSGSIZE)
}

#[derive(Debug)]
struct Poller(Arc<tokio::net::UdpSocket>);

impl UdpPoller for Poller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        self.0.poll_send_ready(cx)
    }
}

impl AsyncUdpSocket for FerrySocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Poller(self.io.clone()))
    }

    /// One `sendto` per packet. ECN, source address and segmentation are ignored: this socket
    /// declares one segment per transmit and no ECN support.
    ///
    /// Only `WouldBlock` reaches quinn. Any other error would end quinn's connection driver
    /// (quinn 0.11.12 `drive_transmit`), so it is counted and the packet dropped, as stock
    /// quinn-udp does.
    fn try_send(&self, t: &Transmit) -> io::Result<()> {
        loop {
            match self.io.try_send_to(t.contents, t.destination) {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(e),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let n = self.counters.tx_errors.fetch_add(1, Ordering::Relaxed);
                    if n.is_power_of_two() {
                        tracing::debug!(error = %e, dest = %t.destination, dropped = n + 1, "udp send failed");
                    }
                    return Ok(());
                }
            }
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let max = bufs.len().min(meta.len());
        loop {
            ready!(self.io.poll_recv_ready(cx))?;
            let mut n = 0;
            while n < max {
                match self.recv_one(&mut bufs[n]) {
                    Ok(Some(m)) => {
                        meta[n] = m;
                        n += 1;
                    }
                    Ok(None) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if transient(&e) => {}
                    Err(e) if n > 0 => {
                        tracing::debug!(error = %e, "udp recv failed");
                        break;
                    }
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }
            if n > 0 {
                return Poll::Ready(Ok(n));
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }

    // max_transmit_segments, max_receive_segments: the default of 1 (no GSO, no GRO).
    // may_fragment: the default `true`, so quinn keeps path-MTU discovery off and stays at its
    // 1200-byte initial MTU, plenty for input.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_do_not_reach_quinn() {
        for kind in [
            io::ErrorKind::NetworkUnreachable,
            io::ErrorKind::HostUnreachable,
            io::ErrorKind::ConnectionRefused,
        ] {
            assert!(transient(&io::Error::from(kind)));
        }
        assert!(!transient(&io::Error::from(io::ErrorKind::WouldBlock)));
    }
}
