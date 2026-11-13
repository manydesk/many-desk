// MDP authentication: each side makes a throwaway self-signed certificate and sends its
// SHA-256 fingerprint through the (encrypted) rendezvous handshake. QUIC/TLS 1.3 then
// accepts exactly that certificate in both directions — mutual pinning, no CA. The
// six-digit security code shown to users is derived from the same two fingerprints.
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};

pub const ALPN: &[u8] = b"mdp1";

pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: Vec<u8>,
    pub fp: String,
}

impl Identity {
    pub fn new() -> Result<Self, String> {
        let ck = rcgen::generate_simple_self_signed(vec!["manydesk".into()]).map_err(|e| e.to_string())?;
        let cert = CertificateDer::from(ck.cert.der().to_vec());
        let fp = fingerprint(&cert);
        Ok(Identity { cert, key: ck.key_pair.serialize_der(), fp })
    }

    fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }
}

pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    hex::encode(Sha256::digest(cert.as_ref()))
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct Pinned {
    fp: String,
    algs: WebPkiSupportedAlgorithms,
}

impl Pinned {
    fn new(fp: &str) -> Arc<Self> {
        Arc::new(Pinned { fp: fp.to_lowercase(), algs: provider().signature_verification_algorithms })
    }
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        if fingerprint(cert) == self.fp {
            Ok(())
        } else {
            Err(rustls::Error::General("peer certificate does not match the handshake".into()))
        }
    }
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, cert: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        self.check(cert).map(|_| ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.algs)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

impl ClientCertVerifier for Pinned {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(&self, cert: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        self.check(cert).map(|_| ClientCertVerified::assertion())
    }
    fn client_auth_mandatory(&self) -> bool {
        true
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.algs)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.max_idle_timeout(Some(Duration::from_secs(15).try_into().unwrap()));
    t.keep_alive_interval(Some(Duration::from_secs(3)));
    // BBR: paces by measured bandwidth instead of filling buffers (lower latency for video).
    // MANYDESK_CC=cubic|newreno: another congestion controller (comparison on weak links)
    match std::env::var("MANYDESK_CC").as_deref() {
        Ok("cubic") => t.congestion_controller_factory(Arc::new(quinn::congestion::CubicConfig::default())),
        Ok("newreno") => t.congestion_controller_factory(Arc::new(quinn::congestion::NewRenoConfig::default())),
        _ => t.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default())),
    };
    t.max_concurrent_uni_streams(1024u32.into());
    t.stream_receive_window((16u32 << 20).into());
    t.receive_window((64u32 << 20).into());
    t.send_window(64 << 20);
    t.datagram_receive_buffer_size(Some(1 << 20));
    Arc::new(t)
}

pub fn client(me: &Identity, peer_fp: &str) -> Result<quinn::ClientConfig, String> {
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Pinned::new(peer_fp))
        .with_client_auth_cert(vec![me.cert.clone()], me.key())
        .map_err(|e| e.to_string())?;
    cfg.alpn_protocols = vec![ALPN.to_vec()];
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(cfg).map_err(|e| e.to_string())?;
    let mut c = quinn::ClientConfig::new(Arc::new(quic));
    c.transport_config(transport());
    Ok(c)
}

pub fn server(me: &Identity, peer_fp: &str) -> Result<quinn::ServerConfig, String> {
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(Pinned::new(peer_fp))
        .with_single_cert(vec![me.cert.clone()], me.key())
        .map_err(|e| e.to_string())?;
    cfg.alpn_protocols = vec![ALPN.to_vec()];
    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(cfg).map_err(|e| e.to_string())?;
    let mut s = quinn::ServerConfig::with_crypto(Arc::new(quic));
    s.transport_config(transport());
    Ok(s)
}
