//! `host[:port]` as `ferry trust --addr` takes it, and the machine's own addresses for `ferry id`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// A parsed `host[:port]`. The host may be an IP address or a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddrError {
    #[error("empty address")]
    Empty,
    #[error("bad port {0:?}")]
    Port(String),
    #[error("bad host {0:?}")]
    Host(String),
}

/// Accepts `1.2.3.4`, `1.2.3.4:5000`, `host`, `host:5000`, `::1`, `[::1]` and `[::1]:5000`.
/// A bare IPv6 address (several colons, no brackets) is a host without a port.
pub fn parse_host_port(s: &str, default_port: u16) -> Result<HostPort, AddrError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(AddrError::Empty);
    }
    let port = |p: &str| p.parse::<u16>().map_err(|_| AddrError::Port(p.to_string()));
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| AddrError::Host(s.to_string()))?;
        match tail {
            "" => (host, default_port),
            t => (
                host,
                port(
                    t.strip_prefix(':')
                        .ok_or_else(|| AddrError::Host(s.to_string()))?,
                )?,
            ),
        }
    } else if s.parse::<IpAddr>().is_ok() || s.matches(':').count() > 1 {
        (s, default_port)
    } else if let Some((host, p)) = s.split_once(':') {
        (host, port(p)?)
    } else {
        (s, default_port)
    };
    let valid_name = |h: &str| {
        !h.is_empty()
            && h.len() <= 253
            && h.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
    };
    if host.parse::<IpAddr>().is_err() && !valid_name(host) {
        return Err(AddrError::Host(host.to_string()));
    }
    Ok(HostPort {
        host: host.to_string(),
        port,
    })
}

impl std::fmt::Display for HostPort {
    /// `host:port`, with an IPv6 host in brackets: the form a `SocketAddr` prints.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// A stored address as `status` and `peers` show it: always `host:port`, so the address typed
/// into `ferry pair` or `ferry trust` without a port reads like one learned from a connection.
/// The stored text itself is kept as given.
pub fn shown(addr: &str) -> String {
    parse_host_port(addr, ferry_net::DEFAULT_PORT)
        .map_or_else(|_| addr.to_string(), |hp| hp.to_string())
}

impl HostPort {
    /// The socket address when the host is an IP literal.
    pub fn literal(&self) -> Option<SocketAddr> {
        self.host
            .parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, self.port))
    }

    /// Resolves the host (a DNS lookup for a name). The first IPv4 address wins over IPv6,
    /// because first light runs over IPv4 on a LAN.
    pub async fn resolve(&self) -> std::io::Result<SocketAddr> {
        self.resolve_with(&system_lookup()).await
    }

    /// [`resolve`](Self::resolve) through `lookup`, which only a name needs.
    pub async fn resolve_with(&self, lookup: &Lookup) -> std::io::Result<SocketAddr> {
        if let Some(a) = self.literal() {
            return Ok(a);
        }
        let addrs = lookup(self.host.clone(), self.port).await?;
        addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.first())
            .copied()
            .ok_or_else(|| std::io::Error::other(format!("{} has no address", self.host)))
    }
}

/// What a name lookup returns.
pub type LookupFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;

/// How the net thread turns a host name into addresses: `(host, port)`. The system's resolver in
/// the daemon (getaddrinfo, which may wait for DNS or mDNS for seconds); a test injects a slow
/// or scripted one through `Options::lookup`.
pub type Lookup = std::sync::Arc<dyn Fn(String, u16) -> LookupFuture + Send + Sync>;

/// The system's resolver (`tokio::net::lookup_host`, on tokio's blocking pool).
pub fn system_lookup() -> Lookup {
    std::sync::Arc::new(|host, port| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), port))
                .await?
                .collect())
        })
    })
}

/// Non-loopback addresses of interfaces that are up: IPv4 first, then global-looking IPv6.
/// Best effort: an error yields an empty list.
pub fn local_addrs() -> Vec<String> {
    use nix::net::if_::InterfaceFlags;
    let Ok(ifaces) = nix::ifaddrs::getifaddrs() else {
        return Vec::new();
    };
    let (mut v4, mut v6) = (Vec::<Ipv4Addr>::new(), Vec::new());
    for ia in ifaces {
        if !ia.flags.contains(InterfaceFlags::IFF_UP)
            || ia.flags.contains(InterfaceFlags::IFF_LOOPBACK)
        {
            continue;
        }
        let Some(addr) = ia.address else { continue };
        if let Some(a) = addr.as_sockaddr_in() {
            let ip = a.ip();
            if !ip.is_loopback() && !ip.is_link_local() && !v4.contains(&ip) {
                v4.push(ip);
            }
        } else if let Some(a) = addr.as_sockaddr_in6() {
            let ip = a.ip();
            let link_local = ip.segments()[0] & 0xffc0 == 0xfe80;
            if !ip.is_loopback() && !link_local && !v6.contains(&ip) {
                v6.push(ip);
            }
        }
    }
    v4.into_iter()
        .map(|i| i.to_string())
        .chain(v6.into_iter().map(|i| i.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hp(s: &str) -> Result<(String, u16), AddrError> {
        parse_host_port(s, 4243).map(|h| (h.host, h.port))
    }

    #[test]
    fn parses_the_forms() {
        assert_eq!(hp("192.0.2.11"), Ok(("192.0.2.11".into(), 4243)));
        assert_eq!(hp("192.0.2.11:5000"), Ok(("192.0.2.11".into(), 5000)));
        assert_eq!(hp("laptop.local"), Ok(("laptop.local".into(), 4243)));
        assert_eq!(hp("laptop:1"), Ok(("laptop".into(), 1)));
        assert_eq!(hp("::1"), Ok(("::1".into(), 4243)));
        assert_eq!(hp("[::1]"), Ok(("::1".into(), 4243)));
        assert_eq!(hp("[fe80::1]:99"), Ok(("fe80::1".into(), 99)));
        assert_eq!(hp(" 10.0.0.1 "), Ok(("10.0.0.1".into(), 4243)));
    }

    #[test]
    fn a_shown_address_always_has_its_port() {
        // The exit check's row 2: the address typed into `ferry pair` against the one learned.
        assert_eq!(shown("192.0.2.11"), "192.0.2.11:4243");
        assert_eq!(shown("192.0.2.111:4243"), "192.0.2.111:4243");
        assert_eq!(shown("laptop.local"), "laptop.local:4243");
        assert_eq!(shown("laptop.local:5000"), "laptop.local:5000");
        assert_eq!(shown("fe80::1"), "[fe80::1]:4243");
        assert_eq!(shown("[fe80::1]:5000"), "[fe80::1]:5000");
        // the form a learned one is stored in
        let learned: SocketAddr = "[fe80::1]:4243".parse().unwrap();
        assert_eq!(shown(&learned.to_string()), learned.to_string());
        // a stored text that does not parse is shown as it is
        assert_eq!(shown("not an addr"), "not an addr");
    }

    #[test]
    fn rejects_the_rest() {
        assert_eq!(hp(""), Err(AddrError::Empty));
        assert!(matches!(hp("10.0.0.1:99999"), Err(AddrError::Port(_))));
        assert!(matches!(hp("10.0.0.1:"), Err(AddrError::Port(_))));
        assert!(matches!(hp(":4243"), Err(AddrError::Host(_))));
        assert!(matches!(hp("[::1"), Err(AddrError::Host(_))));
        assert!(matches!(hp("[::1]x"), Err(AddrError::Host(_))));
        assert!(matches!(hp("bad host"), Err(AddrError::Host(_))));
        assert!(matches!(hp("a/b"), Err(AddrError::Host(_))));
    }

    #[test]
    fn literal_and_resolve() {
        let h = parse_host_port("127.0.0.1:7", 1).unwrap();
        assert_eq!(h.literal(), Some("127.0.0.1:7".parse().unwrap()));
        assert_eq!(parse_host_port("example", 1).unwrap().literal(), None);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(
            rt.block_on(h.resolve()).unwrap(),
            "127.0.0.1:7".parse().unwrap()
        );
    }

    #[test]
    fn local_addrs_are_not_loopback() {
        for a in local_addrs() {
            let ip: IpAddr = a.parse().unwrap();
            assert!(!ip.is_loopback(), "{a}");
        }
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// `pair.start`, `trust` and `trusted.toml` hand any text to the parser.
        #[test]
        fn parse_host_port_never_panics(s in "\\PC{0,300}", port in any::<u16>()) {
            let _ = parse_host_port(&s, port);
            let _ = shown(&s);
        }

        /// What `Display` prints, the parser reads back: names, IPv4 and IPv6 hosts.
        #[test]
        fn a_host_port_parses_back_from_its_text(
            host in prop_oneof![
                "[a-z0-9_-]{1,20}(\\.[a-z0-9_-]{1,20}){0,3}",
                any::<std::net::Ipv4Addr>().prop_map(|a| a.to_string()),
                any::<std::net::Ipv6Addr>().prop_map(|a| a.to_string()),
            ],
            port in any::<u16>(),
        ) {
            let hp = HostPort { host, port };
            prop_assert_eq!(parse_host_port(&hp.to_string(), 1), Ok(hp));
        }
    }
}
