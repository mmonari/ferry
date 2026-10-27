//! rustls configuration: TLS 1.3 only, mandatory client auth, both sides pinned to the
//! [`TrustSet`] .
//!
//! Each connection gets its own verifier instance, so a refusal can be reported with the
//! fingerprint the peer presented ([`Rejection`]); rustls itself only carries an alert.
//!
//! **Pairing** (design `m1-rest-design.md` §2.2). On `ferry-pair/1` the verifier accepts any
//! single certificate whose key is not this peer's own, still checks the TLS 1.3 handshake
//! signature, and records the fingerprint. The acceptor learns which ALPN a connection asks for
//! from the ClientHello, which rustls hands to the certificate resolver before it verifies the
//! client's certificate: [`AlpnResolver`] records it in the connection's [`ConnAlpn`], and the
//! verifier reads it there. `ferry/1` keeps the pinned rule even while a window is open.

use crate::pairing::{Claim, PairingGate};
use crate::{ALPN, ALPN_PAIRING, Fingerprint, Identity, TrustSet};
use rustls::client::Resumption;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, ring};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::NoServerSessionStorage;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error, PeerIncompatible,
    SignatureScheme,
};
use std::net::IpAddr;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

/// The server name a dialer puts in its ClientHello. Nothing checks it: the pin does.
pub(crate) const SERVER_NAME: &str = "ferry";

/// Why a verifier refused the peer's certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// The presented key is not pinned.
    Unknown(Fingerprint),
    /// The dialer expected one peer and another key answered, trusted or not.
    Changed {
        expected: Fingerprint,
        got: Fingerprint,
    },
    /// Not a single parsable certificate.
    Malformed,
    /// Pairing: the peer presented this peer's own key.
    OwnKey,
}

/// Which rule a verifier applies.
#[derive(Debug)]
pub(crate) enum Rule {
    /// `ferry/1`: the presented key must be pinned (and the expected one, when dialling).
    Pinned,
    /// `ferry-pair/1`, dialling: any key but our own.
    Pairing { own: Fingerprint },
    /// An incoming connection: the ALPN in its ClientHello decides ([`ConnAlpn`]).
    ByAlpn {
        own: Fingerprint,
        conn: Arc<ConnAlpn>,
    },
}

/// What a ClientHello asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    Session,
    Pairing,
    /// `ferry-pair/1` on a connection that does not take one: no window, busy, or over the
    /// address's limit. The handshake fails, and the acceptor reports it as
    /// [`ConnectError::PairingRefused`](crate::ConnectError::PairingRefused).
    PairingRefused,
}

/// One incoming connection's ALPN facts, shared by its resolver and its verifier.
#[derive(Debug)]
pub(crate) struct ConnAlpn {
    /// This connection's server config offers `ferry-pair/1`.
    pub(crate) offer_pairing: bool,
    pub(crate) asked: OnceLock<Asked>,
    /// The pairing attempt, claimed when the ClientHello asked for it. The accept task takes
    /// it once the handshake is over (it must not live as long as rustls keeps the resolver).
    pub(crate) claim: Mutex<Option<Claim>>,
    gate: Arc<PairingGate>,
    ip: IpAddr,
}

impl ConnAlpn {
    pub(crate) fn new(gate: Arc<PairingGate>, ip: IpAddr, now: Instant) -> Arc<Self> {
        Arc::new(Self {
            offer_pairing: gate.offer(ip, now),
            asked: OnceLock::new(),
            claim: Mutex::new(None),
            gate,
            ip,
        })
    }

    pub(crate) fn take_claim(&self) -> Option<Claim> {
        self.claim
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn is_pairing(&self) -> bool {
        self.asked.get() == Some(&Asked::Pairing)
    }
}

/// The server's certificate resolver for one incoming connection: records the offered ALPN
/// before rustls verifies the client certificate.
///
/// - The client must offer exactly one ALPN, or the handshake fails (no certificate).
/// - `ferry/1` is a session: the pinned rule.
/// - `ferry-pair/1` on a connection whose config offers it claims the attempt (one at a time,
///   and the address is charged); if that fails now, the handshake fails. On a connection whose
///   config does not offer it, the certificate is returned and ALPN negotiation then fails with
///   alert 120 (`no_application_protocol`): the dialer sees "not accepting".
#[derive(Debug)]
pub(crate) struct AlpnResolver {
    inner: Arc<SingleCertAndKey>,
    conn: Arc<ConnAlpn>,
}

impl AlpnResolver {
    pub(crate) fn new(identity: &Identity, conn: Arc<ConnAlpn>) -> Arc<Self> {
        Arc::new(Self {
            inner: identity.resolver(),
            conn,
        })
    }
}

impl ResolvesServerCert for AlpnResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let offered: Vec<&[u8]> = hello.alpn().map(Iterator::collect).unwrap_or_default();
        let [one] = offered.as_slice() else {
            return None;
        };
        if *one == ALPN {
            let _ = self.conn.asked.set(Asked::Session);
        } else if *one == ALPN_PAIRING {
            if self.conn.offer_pairing {
                let Some(claim) = self.conn.gate.claim(self.conn.ip, Instant::now()) else {
                    let _ = self.conn.asked.set(Asked::PairingRefused);
                    return None;
                };
                *self
                    .conn
                    .claim
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(claim);
                let _ = self.conn.asked.set(Asked::Pairing);
            } else {
                // not offered: no window, busy, or over the limit; ALPN fails with alert 120
                self.conn.gate.refuse();
                let _ = self.conn.asked.set(Asked::PairingRefused);
            }
        }
        // another ALPN (another major) fails in ALPN negotiation, as before
        self.inner.resolve(hello)
    }
}

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

/// Accepts exactly one self-signed certificate whose SPKI fingerprint is in the trust store
/// (and, when dialling, is the expected peer), and verifies the TLS 1.3 handshake signature
/// with the provider's algorithms. ferry keys are Ed25519, so that is the only scheme offered.
///
/// Certificate validity dates and names are not checked: the pin is the identity, and the
/// handshake signature proves the peer holds the pinned key.
#[derive(Debug)]
pub(crate) struct PinVerifier {
    trust: TrustSet,
    expected: Option<Fingerprint>,
    rule: Rule,
    algs: WebPkiSupportedAlgorithms,
    rejection: Mutex<Option<Rejection>>,
}

impl PinVerifier {
    pub(crate) fn new(trust: TrustSet, expected: Option<Fingerprint>) -> Arc<Self> {
        Self::with_rule(trust, expected, Rule::Pinned)
    }

    pub(crate) fn with_rule(
        trust: TrustSet,
        expected: Option<Fingerprint>,
        rule: Rule,
    ) -> Arc<Self> {
        Arc::new(Self {
            trust,
            expected,
            rule,
            algs: ring::default_provider().signature_verification_algorithms,
            rejection: Mutex::new(None),
        })
    }

    /// The pairing rule applies to this handshake; `Some(own)` gives this peer's fingerprint.
    fn pairing_own(&self) -> Option<Fingerprint> {
        match &self.rule {
            Rule::Pinned => None,
            Rule::Pairing { own } => Some(*own),
            Rule::ByAlpn { own, conn } => conn.is_pairing().then_some(*own),
        }
    }

    pub(crate) fn rejection(&self) -> Option<Rejection> {
        *self
            .rejection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn check(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
    ) -> Result<(), Error> {
        let outcome = if !intermediates.is_empty() {
            Err(Rejection::Malformed)
        } else {
            match Fingerprint::of_cert(end_entity) {
                Err(_) => Err(Rejection::Malformed),
                // pairing: any key but our own; the handshake signature is still checked
                Ok(got) => match (self.pairing_own(), self.expected) {
                    (Some(own), _) if got == own => Err(Rejection::OwnKey),
                    (Some(_), _) => Ok(()),
                    (None, expected) => match expected {
                        Some(expected) if expected != got => {
                            Err(Rejection::Changed { expected, got })
                        }
                        _ if !self.trust.contains(&got) => Err(Rejection::Unknown(got)),
                        _ => Ok(()),
                    },
                },
            }
        };
        outcome.map_err(|r| {
            *self
                .rejection
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(r);
            // ApplicationVerificationFailure goes out as the `access_denied` alert, which the
            // other side classifies as "this peer does not trust us" (error.rs).
            Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
        })
    }

    fn verify13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        if dss.scheme != SignatureScheme::ED25519 {
            return Err(Error::PeerMisbehaved(
                rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
            ));
        }
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.check(end_entity, intermediates)
            .map(|()| ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::PeerIncompatible(PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

impl ClientCertVerifier for PinVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        self.check(end_entity, intermediates)
            .map(|()| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::PeerIncompatible(PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    // client_auth_mandatory() and offer_client_auth() keep their default, `true`.
}

/// Server side of one connection. Session tickets and resumption are off: a resumed session
/// would skip the verifier, so a pin removed after the first handshake would not apply.
/// `resolver` is the identity's own, or an [`AlpnResolver`] for an incoming connection.
pub(crate) fn server(
    resolver: Arc<dyn ResolvesServerCert>,
    verifier: Arc<PinVerifier>,
    alpns: &[&[u8]],
) -> Result<rustls::ServerConfig, Error> {
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(resolver);
    cfg.alpn_protocols = alpns.iter().map(|a| a.to_vec()).collect();
    cfg.session_storage = Arc::new(NoServerSessionStorage {});
    cfg.send_tls13_tickets = 0;
    cfg.max_early_data_size = 0;
    Ok(cfg)
}

/// Client side of one dial; see [`server`] for why resumption is off.
pub(crate) fn client(
    identity: &Identity,
    verifier: Arc<PinVerifier>,
    alpn: &[u8],
) -> Result<rustls::ClientConfig, Error> {
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous() // a custom verifier: it pins, nothing is skipped
        .with_custom_certificate_verifier(verifier)
        .with_client_cert_resolver(identity.resolver());
    cfg.alpn_protocols = vec![alpn.to_vec()];
    cfg.resumption = Resumption::disabled();
    cfg.enable_early_data = false;
    Ok(cfg)
}
