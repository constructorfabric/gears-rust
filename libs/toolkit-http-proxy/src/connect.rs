//! TCP + TLS connection establishment.

use std::future::Future as _;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_rustls::client::TlsStream;

use crate::error::{ErrorKind, ProxyError};
use crate::target::{Target, Transport};
use crate::tls::{AlpnOffer, TlsConnectors};

/// A connected byte stream, cleartext or TLS.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

/// Result of a successful connect.
pub struct Connected {
    pub stream: Stream,
    /// Whether ALPN selected `h2`.
    pub alpn_h2: bool,
    pub elapsed: Duration,
}

/// Connect to `target.addr()` and, for TLS targets, run the handshake with
/// the given ALPN offer. Never resolves names.
pub async fn connect(
    target: &Target,
    offer: AlpnOffer,
    tls: &TlsConnectors,
    timeout: Duration,
) -> Result<Connected, ProxyError> {
    let started = Instant::now();
    let tcp = tokio::time::timeout(timeout, TcpStream::connect(target.addr()))
        .await
        .map_err(|_| ProxyError::new(ErrorKind::ConnectTimeout, "upstream connect timed out"))?
        .map_err(ProxyError::from_connect_io)?;
    // Latency over Nagle: requests and small frames go out immediately.
    if let Err(e) = tcp.set_nodelay(true) {
        tracing::debug!(error = %e, "failed to set TCP_NODELAY");
    }

    match target.transport() {
        Transport::Plain => Ok(Connected {
            stream: Stream::Plain(tcp),
            alpn_h2: false,
            elapsed: started.elapsed(),
        }),
        Transport::Tls => {
            let server_name =
                ServerName::try_from(target.server_name().to_owned()).map_err(|e| {
                    ProxyError::new(ErrorKind::InvalidRequest, "invalid TLS server name")
                        .with_source(e)
                })?;
            let tls_stream =
                tokio::time::timeout(timeout, tls.get(offer).connect(server_name, tcp))
                    .await
                    .map_err(|_| {
                        ProxyError::new(
                            ErrorKind::TlsHandshakeTimeout,
                            "upstream TLS handshake timed out",
                        )
                    })?
                    .map_err(ProxyError::from_tls_io)?;
            let alpn_h2 = tls_stream.get_ref().1.alpn_protocol() == Some(b"h2");
            Ok(Connected {
                stream: Stream::Tls(Box::new(tls_stream)),
                alpn_h2,
                elapsed: started.elapsed(),
            })
        }
    }
}

/// A connected stream the engine can sever at any time.
///
/// hyper drives HTTP/2 connections from tasks the engine does not own, so
/// dropping a connection future is not enough to close the socket. Once the
/// token is cancelled, every read and write fails (and a task parked on the
/// socket is woken to observe it), which tears the connection and all its
/// streams down.
pub struct KillableIo {
    inner: Stream,
    kill: CancellationToken,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl KillableIo {
    pub fn new(inner: Stream, kill: &CancellationToken) -> Self {
        Self {
            inner,
            kill: kill.clone(),
            cancelled: Box::pin(kill.clone().cancelled_owned()),
        }
    }

    /// Registers for wake-up on cancellation; used when the socket is idle.
    fn killed(&mut self, cx: &mut Context<'_>) -> bool {
        self.cancelled.as_mut().poll(cx).is_ready()
    }
}

fn aborted() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "connection closed by the engine",
    )
}

impl AsyncRead for KillableIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Pending if this.killed(cx) => Poll::Ready(Err(aborted())),
            other => other,
        }
    }
}

impl AsyncWrite for KillableIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.kill.is_cancelled() {
            return Poll::Ready(Err(aborted()));
        }
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.kill.is_cancelled() {
            return Poll::Ready(Err(aborted()));
        }
        Pin::new(&mut this.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
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
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Plain(s) => s.is_write_vectored(),
            Self::Tls(s) => s.is_write_vectored(),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}
