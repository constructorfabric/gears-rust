//! Engine error type and classification of transport failures.

use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;
use std::io;

use crate::body::BoxError;

/// What went wrong, independent of the underlying library.
///
/// Callers map this onto their own error model; the variants follow the
/// phases of a proxied request (connect, TLS, send, read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// TCP connect did not finish within the connect timeout.
    ConnectTimeout,
    /// The upstream actively refused the TCP connection.
    ConnectRefused,
    /// No route to the upstream host or network.
    ConnectNoRoute,
    /// Any other TCP connect failure.
    Connect,
    /// TLS handshake did not finish within the connect timeout.
    TlsHandshakeTimeout,
    /// TLS handshake failed for a reason other than the certificate.
    TlsHandshake,
    /// The upstream certificate was rejected.
    InvalidCertificate,
    /// No response head, or no body frame, within the read timeout.
    ReadTimeout,
    /// The upstream stopped accepting request data (socket or HTTP/2
    /// flow control) for longer than the write timeout.
    WriteTimeout,
    /// The connection closed before a complete response was received.
    ConnectionClosed,
    /// Reading the response failed mid-stream.
    Read,
    /// Writing the request (head or body) failed.
    Write,
    /// HTTP/2 protocol error (stream or connection reset, GOAWAY, ...).
    Http2,
    /// The upstream sent a malformed or over-limit response.
    Protocol,
    /// The request cannot be sent as given (e.g. an invalid upgrade).
    InvalidRequest,
    /// The engine could not be constructed (e.g. TLS configuration).
    Configuration,
}

impl ErrorKind {
    /// Short, stable, lowercase name for logs and metrics labels.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConnectTimeout => "connect_timeout",
            Self::ConnectRefused => "connect_refused",
            Self::ConnectNoRoute => "connect_no_route",
            Self::Connect => "connect",
            Self::TlsHandshakeTimeout => "tls_handshake_timeout",
            Self::TlsHandshake => "tls_handshake",
            Self::InvalidCertificate => "invalid_certificate",
            Self::ReadTimeout => "read_timeout",
            Self::WriteTimeout => "write_timeout",
            Self::ConnectionClosed => "connection_closed",
            Self::Read => "read",
            Self::Write => "write",
            Self::Http2 => "http2",
            Self::Protocol => "protocol",
            Self::InvalidRequest => "invalid_request",
            Self::Configuration => "configuration",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned by the proxy engine.
#[derive(Debug, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct ProxyError {
    kind: ErrorKind,
    message: Cow<'static, str>,
    #[source]
    source: Option<BoxError>,
}

impl ProxyError {
    /// An error of `kind` (for custom balancers, filters and tests).
    pub fn new(kind: ErrorKind, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// Attach the underlying cause (reported by `Error::source`).
    pub(crate) fn with_source(mut self, source: impl Into<BoxError>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The failure class.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Human-readable detail (never contains request or response data).
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Classify a TCP connect failure.
    pub(crate) fn from_connect_io(err: io::Error) -> Self {
        let kind = match err.kind() {
            io::ErrorKind::ConnectionRefused => ErrorKind::ConnectRefused,
            io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => {
                ErrorKind::ConnectNoRoute
            }
            io::ErrorKind::TimedOut => ErrorKind::ConnectTimeout,
            _ => ErrorKind::Connect,
        };
        Self::new(kind, "upstream connect failed").with_source(err)
    }

    /// Classify a TLS handshake failure surfaced by `tokio-rustls`.
    pub(crate) fn from_tls_io(err: io::Error) -> Self {
        let kind = match err
            .get_ref()
            .and_then(|e| e.downcast_ref::<rustls::Error>())
        {
            Some(
                rustls::Error::InvalidCertificate(_) | rustls::Error::InvalidCertRevocationList(_),
            ) => ErrorKind::InvalidCertificate,
            Some(_) => ErrorKind::TlsHandshake,
            None => match err.kind() {
                io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::UnexpectedEof => ErrorKind::ConnectionClosed,
                _ => ErrorKind::TlsHandshake,
            },
        };
        Self::new(kind, "upstream TLS handshake failed").with_source(err)
    }

    /// Classify a `hyper` error raised while exchanging a request.
    pub(crate) fn from_hyper(err: hyper::Error) -> Self {
        let kind = classify_hyper(&err);
        Self::new(kind, "upstream exchange failed").with_source(err)
    }

    /// Classify an error raised while reading a response body frame.
    pub(crate) fn from_body(err: hyper::Error) -> Self {
        let kind = match classify_hyper(&err) {
            // Mid-body, an unexpected close is an aborted stream.
            ErrorKind::ConnectionClosed => ErrorKind::Read,
            other => other,
        };
        Self::new(kind, "upstream response body failed").with_source(err)
    }
}

fn classify_hyper(err: &hyper::Error) -> ErrorKind {
    if err.is_timeout() {
        return ErrorKind::ReadTimeout;
    }
    if let Some(h2) = find_source::<h2::Error>(err) {
        if let Some(io) = h2.get_io() {
            return classify_io(io);
        }
        return ErrorKind::Http2;
    }
    if err.is_parse() || err.is_parse_status() {
        return ErrorKind::Protocol;
    }
    if err.is_body_write_aborted() {
        return ErrorKind::Write;
    }
    if err.is_user() {
        return ErrorKind::InvalidRequest;
    }
    if let Some(io) = find_source::<io::Error>(err) {
        return classify_io(io);
    }
    if err.is_incomplete_message() || err.is_canceled() || err.is_closed() {
        return ErrorKind::ConnectionClosed;
    }
    ErrorKind::Read
}

/// Marker carried by the `io::Error` a socket write stall produces.
#[derive(Debug, thiserror::Error)]
#[error("upstream stopped accepting data within the write timeout")]
pub struct WriteStall;

/// Classify an I/O error from an established connection.
pub fn classify_io(err: &io::Error) -> ErrorKind {
    if err
        .get_ref()
        .is_some_and(<dyn StdError + Send + Sync>::is::<WriteStall>)
    {
        return ErrorKind::WriteTimeout;
    }
    match err.kind() {
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::UnexpectedEof => ErrorKind::ConnectionClosed,
        io::ErrorKind::TimedOut => ErrorKind::ReadTimeout,
        io::ErrorKind::WriteZero => ErrorKind::Write,
        _ => ErrorKind::Read,
    }
}

/// Walk the `source()` chain looking for an error of type `T`.
fn find_source<'a, T: StdError + 'static>(err: &'a (dyn StdError + 'static)) -> Option<&'a T> {
    let mut cur: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(found) = e.downcast_ref::<T>() {
            return Some(found);
        }
        cur = e.source();
    }
    None
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn connect_io_kinds_are_classified() {
        let cases = [
            (io::ErrorKind::ConnectionRefused, ErrorKind::ConnectRefused),
            (io::ErrorKind::HostUnreachable, ErrorKind::ConnectNoRoute),
            (io::ErrorKind::NetworkUnreachable, ErrorKind::ConnectNoRoute),
            (io::ErrorKind::TimedOut, ErrorKind::ConnectTimeout),
            (io::ErrorKind::PermissionDenied, ErrorKind::Connect),
        ];
        for (io_kind, expected) in cases {
            let err = ProxyError::from_connect_io(io::Error::from(io_kind));
            assert_eq!(err.kind(), expected, "{io_kind:?}");
        }
    }

    #[test]
    fn tls_certificate_errors_are_classified() {
        let cert = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::Expired),
        );
        assert_eq!(
            ProxyError::from_tls_io(cert).kind(),
            ErrorKind::InvalidCertificate
        );
        let other = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::HandshakeNotComplete,
        );
        assert_eq!(
            ProxyError::from_tls_io(other).kind(),
            ErrorKind::TlsHandshake
        );
        let reset = io::Error::from(io::ErrorKind::ConnectionReset);
        assert_eq!(
            ProxyError::from_tls_io(reset).kind(),
            ErrorKind::ConnectionClosed
        );
    }

    #[test]
    fn display_carries_kind_and_message() {
        let err = ProxyError::new(ErrorKind::Protocol, "bad status line");
        assert_eq!(err.to_string(), "protocol: bad status line");
    }
}
