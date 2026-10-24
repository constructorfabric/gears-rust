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
use tokio::time::{Instant, Sleep};
use tokio_rustls::client::TlsStream;

use crate::error::{ErrorKind, ProxyError, WriteStall};
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

/// A connected stream the engine can sever at any time, with a write-stall
/// budget.
///
/// Connection tasks run on their own (hyper's HTTP/1.1 dispatcher, the
/// HTTP/2 driver), so dropping a handle is not enough to close a socket.
/// Once the token is cancelled every operation fails, and any operation that
/// is parked registers for the cancellation wake-up, so the owning task
/// observes it promptly. A write or flush that stays pending for longer than
/// the write timeout (the upstream stopped reading) fails with a
/// [`WriteStall`] error and severs the connection.
pub struct KillableIo {
    inner: Stream,
    kill: CancellationToken,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    write_timeout: Option<Duration>,
    write_stall: Option<Pin<Box<Sleep>>>,
}

impl KillableIo {
    pub fn new(inner: Stream, kill: &CancellationToken, write_timeout: Option<Duration>) -> Self {
        Self {
            inner,
            kill: kill.clone(),
            cancelled: Box::pin(kill.clone().cancelled_owned()),
            write_timeout,
            write_stall: None,
        }
    }

    /// Registers for the cancellation wake-up; true once cancelled.
    fn killed(&mut self, cx: &mut Context<'_>) -> bool {
        self.cancelled.as_mut().poll(cx).is_ready()
    }

    /// Bookkeeping for a write-side operation that returned `poll`.
    fn on_write<T>(
        &mut self,
        cx: &mut Context<'_>,
        poll: Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        match poll {
            Poll::Ready(res) => {
                self.write_stall = None;
                Poll::Ready(res)
            }
            Poll::Pending => {
                if self.killed(cx) {
                    return Poll::Ready(Err(aborted()));
                }
                if let Some(limit) = self.write_timeout {
                    let timer = self
                        .write_stall
                        .get_or_insert_with(|| Box::pin(tokio::time::sleep(limit)));
                    if timer.as_mut().poll(cx).is_ready() {
                        self.write_stall = None;
                        self.kill.cancel();
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            WriteStall,
                        )));
                    }
                }
                Poll::Pending
            }
        }
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
        if this.kill.is_cancelled() {
            return Poll::Ready(Err(aborted()));
        }
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
        let poll = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.on_write(cx, poll)
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
        let poll = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        this.on_write(cx, poll)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.kill.is_cancelled() {
            return Poll::Ready(Err(aborted()));
        }
        let poll = Pin::new(&mut this.inner).poll_flush(cx);
        this.on_write(cx, poll)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_shutdown(cx) {
            Poll::Pending if this.killed(cx) => Poll::Ready(Err(aborted())),
            other => other,
        }
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
