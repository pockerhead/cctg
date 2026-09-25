//! TLS for the agent link and the hook endpoint (TASK-035).
//!
//! Hub: `CCTG_TLS_CERT` and `CCTG_TLS_KEY` (PEM files) turn both listeners
//! into TLS 1.3 with that certificate; the hub logs its sha256 at start.
//!
//! Device: `CCTG_HUB_CERT_SHA256` (that sha256, hex, colons allowed) turns
//! both links into TLS and pins the hub certificate: no CA, no host name, no
//! dates, only these exact certificate bytes. The handshake signature is
//! still checked with the certificate's key, so only the holder of the
//! private key gets through; verification is never switched off. Without a
//! pin a device talks plain TCP, and only to a loopback address: the secret
//! never crosses a network in clear text ([`crate::device`]).
//!
//! Errors never quote a PEM file: a parse error of the key file could carry
//! a line of the key.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, aws_lc_rs};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Device: sha256 of the hub certificate; set means TLS.
pub const PIN_VAR: &str = "CCTG_HUB_CERT_SHA256";
/// Hub: PEM certificate chain of both listeners.
pub const CERT_VAR: &str = "CCTG_TLS_CERT";
/// Hub: PEM private key of [`CERT_VAR`].
pub const KEY_VAR: &str = "CCTG_TLS_KEY";

/// sha256 of a DER certificate: what a device pins. Not a secret.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CertPin([u8; 32]);

impl CertPin {
    /// 64 hex digits, any case, `:` or spaces between them allowed; an
    /// `openssl x509 -fingerprint -sha256` line (`sha256 Fingerprint=AB:..`)
    /// is taken as it is printed.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.rsplit_once('=').map_or(text, |(_, value)| value);
        let digits: Vec<u8> = text
            .bytes()
            .filter(|byte| !matches!(byte, b':' | b' '))
            .collect();
        if digits.len() != 64 {
            return None;
        }
        let mut pin = [0u8; 32];
        for (index, pair) in digits.chunks(2).enumerate() {
            let pair = std::str::from_utf8(pair).ok()?;
            pin[index] = u8::from_str_radix(pair, 16).ok()?;
        }
        // `from_str_radix` takes a sign: "+f" must not pass as a digit pair.
        digits
            .iter()
            .all(u8::is_ascii_hexdigit)
            .then_some(Self(pin))
    }

    pub fn of(der: &[u8]) -> Self {
        let digest = ::aws_lc_rs::digest::digest(&::aws_lc_rs::digest::SHA256, der);
        let mut pin = [0u8; 32];
        pin.copy_from_slice(digest.as_ref());
        Self(pin)
    }

    fn matches(&self, der: &[u8]) -> bool {
        bool::from(Self::of(der).0.ct_eq(&self.0))
    }
}

/// `AB:CD:...`, as openssl prints it.
impl fmt::Display for CertPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for CertPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertPin({self})")
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(aws_lc_rs::default_provider())
}

/// Accepts exactly the pinned certificate; the handshake signature is
/// checked as usual.
#[derive(Debug)]
struct Pinned {
    pin: CertPin,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self.pin.matches(end_entity.as_ref()) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Why an address cannot be used. Never carries the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AddrError {
    #[error("the hub address is not host:port")]
    NotHostPort,
    #[error("TLS could not be set up")]
    Tls,
}

/// The host of `host:port` or `[v6]:port`.
pub fn host_of(addr: &str) -> Option<&str> {
    let (host, port) = match addr.strip_prefix('[') {
        Some(rest) => {
            let (host, port) = rest.split_once("]:")?;
            (host, port)
        }
        None => addr.rsplit_once(':')?,
    };
    let valid_port = !port.is_empty() && port.parse::<u16>().is_ok();
    (!host.is_empty() && valid_port).then_some(host)
}

/// `localhost` or a loopback IP (`127.x`, `::1`).
pub fn is_loopback_addr(addr: &str) -> bool {
    host_of(addr).is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    })
}

/// Whether plain TCP (the secret in the clear) may go to `peer`: a
/// loopback address only.
fn plain_peer_allowed(peer: SocketAddr) -> bool {
    peer.ip().to_canonical().is_loopback()
}

/// The client side of TLS 1.3 accepting only the certificate `pin`.
pub(crate) fn pinned_config(pin: CertPin) -> Result<Arc<rustls::ClientConfig>, AddrError> {
    let provider = provider();
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| AddrError::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin, provider }))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A way to the hub: plain TCP, or TLS pinned to the hub certificate.
#[derive(Clone)]
pub struct HubAddr {
    addr: String,
    tls: Option<(TlsConnector, ServerName<'static>)>,
}

impl fmt::Debug for HubAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubAddr")
            .field("addr", &self.addr)
            .field("tls", &self.tls.is_some())
            .finish()
    }
}

impl HubAddr {
    /// Plain TCP to `addr`, unchecked: [`crate::device::DeviceConfig::hub`]
    /// is where a device decides that plain is allowed.
    pub fn plain(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            tls: None,
        }
    }

    /// TLS 1.3 to `addr`, accepting only the certificate `pin`.
    pub fn pinned(addr: &str, pin: CertPin) -> Result<Self, AddrError> {
        let host = host_of(addr).ok_or(AddrError::NotHostPort)?;
        let name = ServerName::try_from(host.to_owned()).map_err(|_| AddrError::NotHostPort)?;
        Ok(Self {
            addr: addr.to_owned(),
            tls: Some((TlsConnector::from(pinned_config(pin)?), name)),
        })
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    /// TCP connect and, with TLS, the handshake. The caller bounds the time.
    /// Plain TCP goes on only when the peer really is this machine: a name
    /// such as `localhost` is resolved by the system and could point
    /// elsewhere.
    pub async fn connect(&self) -> io::Result<Stream> {
        let tcp = TcpStream::connect(self.addr.as_str()).await?;
        let _ = tcp.set_nodelay(true);
        match &self.tls {
            None if !plain_peer_allowed(tcp.peer_addr()?) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "plain TCP only to this machine",
            )),
            None => Ok(Stream::Plain(tcp)),
            Some((connector, name)) => {
                let tls = connector.connect(name.clone(), tcp).await?;
                Ok(Stream::Tls(Box::new(tls.into())))
            }
        }
    }
}

/// One connection of either end, plain or TLS.
#[derive(Debug)]
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::TlsStream<TcpStream>>),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(&mut **stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(&mut **stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(&mut **stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(&mut **stream).poll_shutdown(cx),
        }
    }
}

/// The task reading one half of a `tokio::io::split` [`Stream`]. A split
/// connection stays open while either half lives, so the task is aborted
/// when this is dropped: also when the task owning the connection is
/// cancelled (a stopping hub), not only at its orderly end.
pub struct ReadTask(JoinHandle<()>);

impl ReadTask {
    pub fn spawn(read: impl Future<Output = ()> + Send + 'static) -> Self {
        Self(tokio::spawn(read))
    }

    /// Aborts the task and waits until it has let go of its half.
    pub async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}

impl Drop for ReadTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Why the hub's certificate files are unusable. Fixed text only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TlsFileError {
    #[error("{CERT_VAR} cannot be read as a PEM certificate (contents are not shown)")]
    Cert,
    #[error("{KEY_VAR} cannot be read as a PEM private key (contents are not shown)")]
    Key,
    #[error("{CERT_VAR} and {KEY_VAR} do not make a usable TLS certificate")]
    Pair,
}

/// The hub end of TLS: both listeners use it.
#[derive(Clone)]
pub struct Acceptor(TlsAcceptor);

impl fmt::Debug for Acceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Acceptor")
    }
}

impl Acceptor {
    /// Reads the PEM files; returns the acceptor and the pin of the
    /// certificate (the first one of the chain).
    pub fn from_files(cert: &Path, key: &Path) -> Result<(Self, CertPin), TlsFileError> {
        let chain = CertificateDer::pem_file_iter(cert)
            .map_err(|_| TlsFileError::Cert)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TlsFileError::Cert)?;
        let key = PrivateKeyDer::from_pem_file(key).map_err(|_| TlsFileError::Key)?;
        Self::new(chain, key)
    }

    pub fn new(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<(Self, CertPin), TlsFileError> {
        let pin = CertPin::of(chain.first().ok_or(TlsFileError::Cert)?.as_ref());
        let config = rustls::ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| TlsFileError::Pair)?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|_| TlsFileError::Pair)?;
        Ok((Self(TlsAcceptor::from(Arc::new(config))), pin))
    }

    /// The server handshake on an accepted connection. The caller bounds
    /// the time.
    pub async fn accept(&self, tcp: TcpStream) -> io::Result<Stream> {
        let tls = self.0.accept(tcp).await?;
        Ok(Stream::Tls(Box::new(tls.into())))
    }
}

/// How a listener takes connections: plain, or TLS through `Acceptor`.
#[derive(Debug, Clone, Default)]
pub struct Incoming(Option<Acceptor>);

impl Incoming {
    pub fn plain() -> Self {
        Self(None)
    }

    pub fn tls(acceptor: Acceptor) -> Self {
        Self(Some(acceptor))
    }

    pub fn is_tls(&self) -> bool {
        self.0.is_some()
    }

    pub async fn accept(&self, tcp: TcpStream) -> io::Result<Stream> {
        let _ = tcp.set_nodelay(true);
        match &self.0 {
            None => Ok(Stream::Plain(tcp)),
            Some(acceptor) => acceptor.accept(tcp).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const FP: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn plain_tcp_goes_only_to_a_loopback_peer() {
        for peer in [
            "127.0.0.1:1",
            "127.8.9.10:1",
            "[::1]:1",
            "[::ffff:127.0.0.1]:1",
        ] {
            assert!(plain_peer_allowed(peer.parse().unwrap()), "{peer}");
        }
        for peer in [
            "192.0.2.1:1",
            "[2001:db8::1]:1",
            "[::ffff:192.0.2.1]:1",
            "0.0.0.0:1",
        ] {
            assert!(!plain_peer_allowed(peer.parse().unwrap()), "{peer}");
        }
    }

    #[test]
    fn pins_parse_in_every_usual_spelling() {
        let pin = CertPin::parse(FP).unwrap();
        assert_eq!(pin, CertPin::of(b"abc"));
        let colons = pin.to_string();
        assert_eq!(colons.len(), 64 + 31);
        assert_eq!(CertPin::parse(&colons), Some(pin));
        assert_eq!(CertPin::parse(&FP.to_uppercase()), Some(pin));
        assert_eq!(
            CertPin::parse(&format!("sha256 Fingerprint={colons}")),
            Some(pin)
        );
        for bad in [
            "",
            "abc",
            &FP[..62],
            &format!("{FP}00"),
            &FP.replace('b', "g"),
        ] {
            assert_eq!(CertPin::parse(bad), None, "{bad}");
        }
        let signed = format!("+f{}", &FP[2..]);
        assert_eq!(CertPin::parse(&signed), None);
    }

    #[test]
    fn hosts_and_loopback() {
        assert_eq!(host_of("hub.example.org:47291"), Some("hub.example.org"));
        assert_eq!(host_of("[::1]:5"), Some("::1"));
        assert_eq!(host_of("10.0.0.1:5"), Some("10.0.0.1"));
        for bad in ["hub", "hub:", ":5", "hub:99999", "[::1]5", "[::1]"] {
            assert_eq!(host_of(bad), None, "{bad}");
        }
        for loopback in ["127.0.0.1:1", "127.9.9.9:1", "[::1]:1", "LocalHost:1"] {
            assert!(is_loopback_addr(loopback), "{loopback}");
        }
        for remote in [
            "10.0.0.1:1",
            "hub.tail:1",
            "100.64.0.7:1",
            "[fd00::1]:1",
            "x",
        ] {
            assert!(!is_loopback_addr(remote), "{remote}");
        }
    }

    fn certified(name: &str) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![name.to_owned()]).unwrap();
        let key = PrivateKeyDer::from_pem_slice(signing_key.serialize_pem().as_bytes()).unwrap();
        (vec![cert.der().clone()], key)
    }

    /// Serves one TLS connection that echoes one line; returns the port.
    async fn echo_server(acceptor: Acceptor) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = Incoming::tls(acceptor).accept(tcp).await else {
                        return;
                    };
                    let mut buf = [0u8; 5];
                    if stream.read_exact(&mut buf).await.is_ok() {
                        let _ = stream.write_all(&buf).await;
                        let _ = stream.shutdown().await;
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn only_the_pinned_certificate_gets_through() {
        let (chain, key) = certified("localhost");
        let der = chain[0].clone();
        let (acceptor, pin) = Acceptor::new(chain, key).unwrap();
        assert_eq!(pin, CertPin::of(der.as_ref()));
        let addr = echo_server(acceptor).await;

        let hub = HubAddr::pinned(&addr.to_string(), pin).unwrap();
        assert!(hub.is_tls());
        let mut stream = hub.connect().await.unwrap();
        stream.write_all(b"hello").await.unwrap();
        let mut back = Vec::new();
        stream.read_to_end(&mut back).await.unwrap();
        assert_eq!(back, b"hello");

        // Another certificate for the same name: refused by the client.
        let (other, _) = certified("localhost");
        let wrong = HubAddr::pinned(&addr.to_string(), CertPin::of(other[0].as_ref())).unwrap();
        assert!(wrong.connect().await.is_err());
    }

    #[tokio::test]
    async fn the_pinned_certificate_without_its_key_does_not_pass() {
        // A server that shows the pinned certificate but signs with another
        // key: the handshake signature check must fail.
        let (chain, _) = certified("localhost");
        let (_, other_key) = certified("localhost");
        let pin = CertPin::of(chain[0].as_ref());
        // The hub's own loader refuses such a pair outright.
        assert_eq!(
            Acceptor::new(chain.clone(), other_key.clone_key()).unwrap_err(),
            TlsFileError::Pair
        );
        // A forged server skips that check: only the signature can stop it.
        let provider = provider();
        let signer = provider.key_provider.load_private_key(other_key).unwrap();
        let forged = Arc::new(rustls::sign::CertifiedKey::new(chain, signer));
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Forged(forged)));
        let addr = echo_server(Acceptor(TlsAcceptor::from(Arc::new(config)))).await;
        let hub = HubAddr::pinned(&addr.to_string(), pin).unwrap();
        assert!(hub.connect().await.is_err());
    }

    /// Serves one certificate with whatever key it was given.
    #[derive(Debug)]
    struct Forged(Arc<rustls::sign::CertifiedKey>);

    impl rustls::server::ResolvesServerCert for Forged {
        fn resolve(
            &self,
            _: rustls::server::ClientHello<'_>,
        ) -> Option<Arc<rustls::sign::CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    #[test]
    fn broken_files_are_named_but_not_quoted() {
        let dir = crate::hub::testdir::TempDir::new("tls-files");
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        std::fs::write(
            &cert,
            "-----BEGIN CERTIFICATE-----\nnot base64 secret-line\n",
        )
        .unwrap();
        std::fs::write(&key, "-----BEGIN PRIVATE KEY-----\nsecret-line\n").unwrap();
        let error = Acceptor::from_files(&cert, &key).unwrap_err();
        assert!(!error.to_string().contains("secret-line"));
        let (chain, _) = certified("localhost");
        std::fs::write(&cert, pem_cert(&chain[0])).unwrap();
        let error = Acceptor::from_files(&cert, &key).unwrap_err();
        assert_eq!(error, TlsFileError::Key);
        assert!(!format!("{error} {error:?}").contains("secret-line"));
        assert_eq!(
            Acceptor::from_files(&dir.path().join("none"), &key).unwrap_err(),
            TlsFileError::Cert
        );
    }

    fn pem_cert(der: &CertificateDer<'_>) -> String {
        use base64::Engine;
        let body = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect();
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            lines.join("\n")
        )
    }
}
