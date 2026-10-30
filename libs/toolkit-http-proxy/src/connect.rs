//! TCP + TLS connection establishment.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_rustls::client::TlsStream;

use crate::error::{ErrorKind, ProxyError, WriteStall};
use crate::idle::IdleTimer;
use crate::kill::Kill;
use crate::pool::ConnHandle;
use crate::target::{Target, Transport};
use crate::timer::Timers;
use crate::tls::{AlpnOffer, TlsConnectors};

/// A connected byte stream, cleartext or TLS.
pub enum Stream {
    /// Cleartext TCP.
    Plain(TcpStream),
    /// TLS over TCP.
    Tls(Box<TlsStream<TcpStream>>),
}

/// Result of a successful connect.
pub struct Connected {
    /// The connected stream.
    pub stream: Stream,
    /// Whether ALPN selected `h2`.
    pub alpn_h2: bool,
    /// TCP connect plus TLS handshake time.
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
/// Once the connection is killed every operation fails. Nothing here wakes
/// a parked operation (that would cost a registration per pending read or
/// write): whoever may kill a connection while it is parked also stops
/// driving it — the HTTP/2 driver and the upgrade task select on the kill
/// switch, and an HTTP/1.1 connection is closed by dropping its owner. A
/// write or flush that stays pending for longer than the write timeout (the
/// upstream stopped reading) fails with a [`WriteStall`] error and severs the
/// connection.
pub struct KillableIo {
    inner: Stream,
    kill: Kill,
    /// Shared with the connection's owner: a write-side operation is pending.
    write_blocked: Arc<AtomicBool>,
    /// Kept across writes; a pending write only records when it started.
    write_stall: Option<IdleTimer>,
    /// A write-side operation is currently pending.
    stalling: bool,
}

impl KillableIo {
    /// Wrap `inner`. `handle` provides the kill switch and the shared
    /// write-blocked flag; `write_timeout` bounds a stalled write or flush.
    pub fn new(
        inner: Stream,
        handle: &ConnHandle,
        write_timeout: Option<Duration>,
        timers: &Timers,
    ) -> Self {
        Self {
            inner,
            kill: handle.kill.clone(),
            write_blocked: Arc::clone(&handle.write_blocked),
            write_stall: write_timeout.map(|limit| IdleTimer::new(limit, timers)),
            stalling: false,
        }
    }

    /// Bookkeeping for a write-side operation that returned `poll`.
    fn on_write<T>(
        &mut self,
        cx: &mut Context<'_>,
        poll: Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        match poll {
            Poll::Ready(res) => {
                if self.stalling {
                    self.stalling = false;
                    self.write_blocked.store(false, Ordering::Relaxed);
                }
                Poll::Ready(res)
            }
            Poll::Pending => {
                if !self.stalling {
                    self.stalling = true;
                    self.write_blocked.store(true, Ordering::Relaxed);
                    if let Some(timer) = self.write_stall.as_mut() {
                        timer.touch();
                    }
                }
                if let Some(timer) = self.write_stall.as_mut()
                    && timer.poll_expired(cx).is_ready()
                {
                    self.kill.cancel();
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::TimedOut, WriteStall)));
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
        Pin::new(&mut this.inner).poll_read(cx, buf)
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

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;
    use crate::config::PoolConfig;
    use crate::pool::Pool;

    #[tokio::test]
    async fn write_blocked_tracks_a_peer_that_stops_reading() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let handle = Pool::new(PoolConfig::default()).new_handle();
        let mut io = KillableIo::new(Stream::Plain(client), &handle, None, &Timers::new());

        // The peer reads nothing: once the socket buffers fill, writes park.
        let chunk = vec![0u8; 1 << 20];
        let blocked = tokio::time::timeout(Duration::from_millis(300), async {
            loop {
                io.write_all(&chunk).await.unwrap();
            }
        })
        .await;
        assert!(blocked.is_err(), "writes never parked");
        assert!(handle.write_blocked.load(Ordering::Relaxed));

        // Once the peer drains, the next write-side operation completes.
        let drain = tokio::spawn(async move {
            let mut buf = vec![0u8; 1 << 20];
            while peer.read(&mut buf).await.is_ok_and(|n| n > 0) {}
        });
        io.write_all(b"x").await.unwrap();
        io.flush().await.unwrap();
        assert!(!handle.write_blocked.load(Ordering::Relaxed));
        drop(io);
        drain.await.unwrap();
    }
}
