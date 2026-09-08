//! TLS for the HTTP/1.1 and HTTP/2 transports.
//!
//! One place builds every rustls configuration this crate uses over TCP, so
//! the trust decision a request makes is a property of the crate rather than of
//! whichever call site assembled a config. The HTTP/3 half keeps its own
//! QUIC-specific configuration: QUIC requires TLS 1.3 and carries its own ALPN,
//! while this side has to speak to servers that still negotiate TLS 1.2.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::runtime::NetworkError;

/// The ALPN identifier for HTTP/1.1.
pub(crate) const ALPN_HTTP1: &[u8] = b"http/1.1";
/// The ALPN identifier for HTTP/2 over TLS.
pub(crate) const ALPN_HTTP2: &[u8] = b"h2";

/// Builds a client configuration trusting the public roots plus `extra_roots`.
///
/// The public set is the compiled-in Mozilla bundle rather than the platform
/// store: a Kira program's trust decision is then the same on every machine
/// that runs it, which is what makes a failure reproducible somewhere other
/// than the machine that saw it. `extra_roots` is how a caller trusts something
/// the bundle does not — a loopback server's generated certificate, or a
/// private CA — without giving up the public set.
pub(crate) fn client_config(
    extra_roots: &[Vec<u8>],
    alpn_protocols: &[&[u8]],
) -> Result<rustls::ClientConfig, NetworkError> {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    for certificate in extra_roots {
        roots
            .add(CertificateDer::from(certificate.clone()))
            .map_err(|_| NetworkError::InvalidConfig)?;
    }
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
    .map_err(|_| NetworkError::InvalidConfig)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = alpn_protocols.iter().map(|name| name.to_vec()).collect();
    Ok(config)
}

/// Builds a server configuration from a DER certificate and key.
pub(crate) fn server_config(
    certificate_der: Vec<u8>,
    private_key_der: Vec<u8>,
    alpn_protocols: &[&[u8]],
) -> Result<rustls::ServerConfig, NetworkError> {
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
    .map_err(|_| NetworkError::InvalidConfig)?
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(certificate_der)],
        PrivateKeyDer::try_from(private_key_der).map_err(|_| NetworkError::InvalidConfig)?,
    )
    .map_err(|_| NetworkError::InvalidConfig)?;
    config.alpn_protocols = alpn_protocols.iter().map(|name| name.to_vec()).collect();
    Ok(config)
}

/// Generates a short-lived localhost certificate and its key, both DER.
///
/// Both the name and the address are subject alternative names, because a
/// loopback client reaches the server by whichever the caller wrote: `localhost`
/// resolves to `::1` before `127.0.0.1` on some machines, and a certificate that
/// named only the other one would fail on exactly those.
pub(crate) fn self_signed_localhost() -> Result<(Vec<u8>, Vec<u8>), NetworkError> {
    let generated =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .map_err(|_| NetworkError::Protocol)?;
    Ok((
        generated.cert.der().to_vec(),
        generated.key_pair.serialize_der(),
    ))
}

/// Connects to `host:port` and completes a TLS handshake against `host`.
pub(crate) async fn connect(
    host: &str,
    port: u16,
    config: Arc<rustls::ClientConfig>,
) -> Result<TlsStream<TcpStream>, NetworkError> {
    let name = ServerName::try_from(host.to_owned()).map_err(|_| NetworkError::InvalidUri)?;
    let stream = TcpStream::connect((host, port))
        .await
        .map_err(|_| NetworkError::Connect)?;
    TlsConnector::from(config)
        .connect(name, stream)
        .await
        .map_err(|_| NetworkError::Protocol)
}

/// A client connection that has or has not been wrapped in TLS.
///
/// The two are one type because the HTTP/1.1 and HTTP/2 handshakes above them
/// are identical either way: only the bytes on the wire differ, and a caller
/// that had to name which transport it got would end up with the protocol code
/// written twice.
pub(crate) enum ClientStream {
    /// A cleartext connection, for an `http` URI.
    Plain(TcpStream),
    /// A TLS connection, for an `https` URI.
    Tls(Box<TlsStream<TcpStream>>),
}

impl ClientStream {
    /// The ALPN protocol the peer selected, empty when there was no handshake.
    pub(crate) fn negotiated_protocol(&self) -> &[u8] {
        match self {
            Self::Plain(_) => &[],
            Self::Tls(stream) => stream.get_ref().1.alpn_protocol().unwrap_or_default(),
        }
    }
}

impl AsyncRead for ClientStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(context, buffer),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(context, buffer),
        }
    }
}

impl AsyncWrite for ClientStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(context, buffer),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(context, buffer),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(context),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(context),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(context),
        }
    }
}
