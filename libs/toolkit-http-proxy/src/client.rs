//! The proxy client: connection acquisition, dispatch and upgrades.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use http::{HeaderValue, Request, Response, StatusCode, Uri, Version, header};
use http_body::Body as _;
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio_util::sync::DropGuard;

use crate::body::{Hold, OnEnd, ProxyBody, ResponseBody};
use crate::config::ProxyClientConfig;
use crate::connect::{KillableIo, connect};
use crate::error::{ErrorKind, ProxyError};
use crate::http2::{self, H2Body, H2Sender, Started};
use crate::observe::{ConnectionInfo, NoopObserver, Observer};
use crate::pool::{ConnHandle, H2Lease, Pool, PoolKey, reap_loop};
use crate::protocol_cache::{CachedProtocol, ProtocolCache};
use crate::sanitize::{self, RequestKind};
use crate::target::{ProtocolPolicy, Target, Transport};
use crate::tls::{AlpnOffer, TlsConnectors};

/// Result of [`ProxyClient::upgrade`].
#[derive(Debug)]
pub enum UpgradeOutcome {
    /// The upstream answered `101 Switching Protocols`; `io` is the raw
    /// bidirectional tunnel. Response headers are already sanitized.
    Switched {
        /// The `101` response head.
        response: Response<()>,
        /// The upgraded connection.
        io: Upgraded,
    },
    /// The upstream answered with a regular response. Nothing is tunnelled
    /// and the connection is never reused.
    Rejected(Response<ResponseBody>),
}

/// Pooled HTTP/1.1 + HTTP/2 client for proxying to upstreams.
///
/// Cheap to clone. Create it inside a Tokio runtime: it spawns a pool
/// reaper and tasks per connection.
#[derive(Clone)]
pub struct ProxyClient {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: ProxyClientConfig,
    tls: TlsConnectors,
    pool: Arc<Pool>,
    protocols: ProtocolCache,
    observer: Arc<dyn Observer>,
    h1: http1::Builder,
}

enum Sender {
    H1(http1::SendRequest<ProxyBody>),
    H2(H2Sender),
}

/// A connection ready to carry one exchange.
struct Acquired {
    sender: Sender,
    conn: ConnHandle,
    /// Keeps the HTTP/2 active-stream count up while the exchange runs.
    hold: Option<Hold>,
    reused: bool,
    connect_time: Option<Duration>,
}

/// Which protocols a connection for this request may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// HTTP/1.1 only.
    H1,
    /// HTTP/2 only (ALPN `h2`, or prior knowledge in cleartext).
    H2,
    /// The cache says the host speaks `h2`: offer only `h2`.
    PreferH2,
    /// Unknown host: offer `h2` and `http/1.1`.
    Negotiate,
}

/// What a retry must avoid: the connection the first attempt failed on.
#[derive(Debug, Clone, Copy)]
struct Retry {
    failed_generation: u64,
}

enum DispatchError {
    /// The request provably never left: it comes back for a retry.
    Unsent(Box<Request<ProxyBody>>, ProxyError),
    Failed(ProxyError),
}

impl ProxyClient {
    /// Build a client.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Configuration`] if the TLS configuration cannot be built
    /// (no trust anchors, FIPS provider missing under `fips`, ...).
    pub fn new(cfg: ProxyClientConfig) -> Result<Self, ProxyError> {
        Self::with_observer(cfg, Arc::new(NoopObserver))
    }

    /// Build a client that reports connection events to `observer`.
    ///
    /// # Errors
    ///
    /// See [`ProxyClient::new`].
    pub fn with_observer(
        cfg: ProxyClientConfig,
        observer: Arc<dyn Observer>,
    ) -> Result<Self, ProxyError> {
        let tls = TlsConnectors::build(&cfg.tls)?;
        let pool = Arc::new(Pool::new(cfg.pool.clone()));
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(reap_loop(Arc::downgrade(&pool), cfg.pool.reap_interval));
        }

        let mut h1 = http1::Builder::new();
        h1.max_headers(cfg.h1.max_headers)
            .max_buf_size(cfg.h1.max_buf_size);

        Ok(Self {
            inner: Arc::new(Inner {
                protocols: ProtocolCache::new(cfg.protocol_cache_ttl),
                cfg,
                tls,
                pool,
                observer,
                h1,
            }),
        })
    }

    /// The configuration this client was built with.
    #[must_use]
    pub fn config(&self) -> &ProxyClientConfig {
        &self.inner.cfg
    }

    /// The protocol the per-host cache currently holds for `target`.
    #[must_use]
    pub fn cached_protocol(&self, target: &Target) -> Option<CachedProtocol> {
        self.inner.protocols.get(target)
    }

    /// Send `req` to `target` and return the response head; the body
    /// streams.
    ///
    /// The request URI may be origin-form (`/path?q`) or absolute; the
    /// authority is taken from the `Host` header, then the URI, then the
    /// target. Framing and hop-by-hop headers are rewritten (see
    /// [`crate::sanitize`]). Upgrade requests must use
    /// [`ProxyClient::upgrade`].
    ///
    /// # Errors
    ///
    /// A [`ProxyError`] classifying the connect, TLS, write or read failure.
    pub async fn send(
        &self,
        target: &Target,
        req: Request<ProxyBody>,
    ) -> Result<Response<ResponseBody>, ProxyError> {
        let (mut parts, body) = req.into_parts();
        sanitize::sanitize_request_headers(&mut parts.headers, RequestKind::Regular);
        let mut req = Request::from_parts(parts, body);
        let key = PoolKey::of(target);

        let mut retry: Option<Retry> = None;
        loop {
            let acquired = self.acquire(target, &key, retry).await?;
            let reused = acquired.reused;
            let generation = acquired.conn.generation;
            match self.dispatch(target, &key, acquired, req).await {
                Ok(resp) => return Ok(resp),
                // At most one retry, only for a reused connection, only when
                // the request provably never left: a stale keep-alive
                // connection must not cost the caller a request, and a
                // request that may have reached the upstream is never
                // replayed.
                Err(DispatchError::Unsent(back, err)) if reused && retry.is_none() => {
                    tracing::debug!(target = %target, error = %err, "pooled connection unusable, retrying on another connection");
                    req = *back;
                    retry = Some(Retry {
                        failed_generation: generation,
                    });
                }
                Err(DispatchError::Unsent(_, err) | DispatchError::Failed(err)) => return Err(err),
            }
        }
    }

    /// Perform a WebSocket opening handshake with `target` on a dedicated
    /// HTTP/1.1 connection.
    ///
    /// The request is validated (`GET`, `Upgrade: websocket`, no body) and
    /// the tunnel is handed out only after the upstream answered `101` with
    /// a websocket switch; any other answer is returned as
    /// [`UpgradeOutcome::Rejected`] and nothing is tunnelled.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidRequest`] for an invalid handshake request,
    /// [`ErrorKind::Protocol`] for an invalid `101`, or a transport error.
    pub async fn upgrade(
        &self,
        target: &Target,
        req: Request<ProxyBody>,
    ) -> Result<UpgradeOutcome, ProxyError> {
        let (mut parts, body) = req.into_parts();
        let empty = body.is_end_stream() || body.size_hint().exact() == Some(0);
        sanitize::validate_websocket_upgrade(&parts.method, &parts.headers, empty)?;
        sanitize::sanitize_request_headers(&mut parts.headers, RequestKind::WebSocketUpgrade);
        let req = shape(Request::from_parts(parts, body), target, Version::HTTP_11)?;

        let connected = connect(
            target,
            AlpnOffer::H1,
            &self.inner.tls,
            self.inner.cfg.connect_timeout,
        )
        .await?;
        let connect_time = connected.elapsed;
        let handle = self.inner.pool.new_handle();
        // No write-stall budget: an upgraded tunnel's pace belongs to its
        // peers (the WebSocket relay has its own idle handling).
        let io = TokioIo::new(KillableIo::new(connected.stream, &handle.kill, None));
        let (mut tx, conn) = self
            .inner
            .h1
            .handshake::<_, ProxyBody>(io)
            .await
            .map_err(ProxyError::from_hyper)?;
        // The connection task ends when hyper hands the socket to `Upgraded`;
        // the handle must stay alive (not cancelled) for the tunnel's sake.
        tokio::spawn(async move {
            if let Err(e) = conn.with_upgrades().await {
                tracing::debug!(error = %e, "upstream upgrade connection closed with error");
            }
            drop(handle);
        });
        self.inner.observer.on_connection(&ConnectionInfo {
            target,
            reused: false,
            version: Version::HTTP_11,
            connect_time: Some(connect_time),
        });

        let mut resp = self
            .read_timeout(tx.send_request(req))
            .await?
            .map_err(ProxyError::from_hyper)?;
        if resp.status() != StatusCode::SWITCHING_PROTOCOLS {
            return Ok(UpgradeOutcome::Rejected(self.finish_h1(resp, None)));
        }
        sanitize::validate_websocket_switch(resp.headers())?;
        let io = hyper::upgrade::on(&mut resp)
            .await
            .map_err(ProxyError::from_hyper)?;
        let (mut head, _) = resp.into_parts();
        sanitize::sanitize_response_headers(&mut head.headers, head.status);
        Ok(UpgradeOutcome::Switched {
            response: Response::from_parts(head, ()),
            io,
        })
    }

    fn finish_h1(&self, resp: Response<Incoming>, on_end: Option<OnEnd>) -> Response<ResponseBody> {
        let (mut head, body) = resp.into_parts();
        // RFC 9112 §6.3: a response carrying both framings must not leave
        // its connection reusable, whatever hyper decided.
        let ambiguous = head.headers.contains_key(header::TRANSFER_ENCODING)
            && head.headers.contains_key(header::CONTENT_LENGTH);
        let on_end = if ambiguous { None } else { on_end };
        sanitize::sanitize_response_headers(&mut head.headers, head.status);
        Response::from_parts(
            head,
            ResponseBody::http1(body, self.inner.cfg.read_timeout, on_end),
        )
    }

    fn finish_h2(
        &self,
        resp: Response<h2::RecvStream>,
        hold: Option<Hold>,
        exchange: Arc<http2::Exchange>,
        owner: DropGuard,
    ) -> Response<ResponseBody> {
        let (mut head, body) = resp.into_parts();
        sanitize::sanitize_response_headers(&mut head.headers, head.status);
        Response::from_parts(
            head,
            ResponseBody::http2(
                H2Body::new(body, exchange, owner),
                self.inner.cfg.read_timeout,
                hold,
            ),
        )
    }

    async fn read_timeout<F: Future>(&self, fut: F) -> Result<F::Output, ProxyError> {
        match self.inner.cfg.read_timeout {
            Some(limit) => tokio::time::timeout(limit, fut).await.map_err(|_| {
                ProxyError::new(
                    ErrorKind::ReadTimeout,
                    "no upstream response within the read timeout",
                )
            }),
            None => Ok(fut.await),
        }
    }

    fn plan(&self, target: &Target) -> Plan {
        match (target.transport(), target.policy()) {
            (_, ProtocolPolicy::Http2Only) => Plan::H2,
            (Transport::Plain, _) | (Transport::Tls, ProtocolPolicy::Http1Only) => Plan::H1,
            (Transport::Tls, _) => match self.inner.protocols.get(target) {
                Some(CachedProtocol::Http1Only) => Plan::H1,
                Some(CachedProtocol::Http2) => Plan::PreferH2,
                None => Plan::Negotiate,
            },
        }
    }

    async fn acquire(
        &self,
        target: &Target,
        key: &PoolKey,
        retry: Option<Retry>,
    ) -> Result<Acquired, ProxyError> {
        let pool = &self.inner.pool;
        let plan = self.plan(target);

        if retry.is_none() {
            if plan != Plan::H1
                && let Some(lease) = pool.live_h2(key)
            {
                return Ok(leased(lease, true, None));
            }
            if matches!(plan, Plan::H1 | Plan::Negotiate)
                && let Some((tx, conn)) = self.checkout_h1(key).await
            {
                return Ok(reused_h1(tx, conn));
            }
        }

        if plan == Plan::H1 {
            return self.connect_new(target, key, AlpnOffer::H1).await;
        }

        // HTTP/2-capable: serialise connection setup per key so a burst of
        // first requests shares one multiplexed connection. A retry may use a
        // replacement someone else installed after its failure.
        let slot = pool.h2_slot(key);
        let connecting = slot.connecting.lock().await;
        let avoid = retry.map(|r| r.failed_generation);
        if let Some(lease) = pool.lease(&slot, avoid) {
            return Ok(leased(lease, true, None));
        }
        if plan == Plan::Negotiate {
            let cached_h1 = self.inner.protocols.get(target) == Some(CachedProtocol::Http1Only);
            if cached_h1 || slot.h1_hint() {
                // This host speaks HTTP/1.1: don't keep HTTP/2 setup
                // serialised behind HTTP/1.1 connects.
                drop(connecting);
                if retry.is_none()
                    && let Some((tx, conn)) = self.checkout_h1(key).await
                {
                    return Ok(reused_h1(tx, conn));
                }
                let offer = if cached_h1 {
                    AlpnOffer::H1
                } else {
                    AlpnOffer::H2AndH1
                };
                return self.connect_new(target, key, offer).await;
            }
        }
        let offer = if plan == Plan::Negotiate {
            AlpnOffer::H2AndH1
        } else {
            AlpnOffer::H2
        };
        let acquired = self.connect_new(target, key, offer).await;
        drop(connecting);
        acquired
    }

    /// Pop an idle HTTP/1.1 connection. Idle entries were released at the
    /// end of their previous response body, so readiness follows within a
    /// scheduler tick; anything slower is treated as dead.
    async fn checkout_h1(
        &self,
        key: &PoolKey,
    ) -> Option<(http1::SendRequest<ProxyBody>, ConnHandle)> {
        const READY_GRACE: Duration = Duration::from_millis(100);
        while let Some((mut tx, conn)) = self.inner.pool.checkout_h1(key) {
            if tx.is_ready() {
                return Some((tx, conn));
            }
            if let Ok(Ok(())) = tokio::time::timeout(READY_GRACE, tx.ready()).await {
                return Some((tx, conn));
            }
            conn.kill.cancel();
        }
        None
    }

    async fn connect_new(
        &self,
        target: &Target,
        key: &PoolKey,
        offer: AlpnOffer,
    ) -> Result<Acquired, ProxyError> {
        let inner = &self.inner;
        let connected = connect(target, offer, &inner.tls, inner.cfg.connect_timeout)
            .await
            .inspect_err(|e| {
                // A cached hint may be stale (the host dropped h2): forget it
                // so the next attempt negotiates from scratch.
                if offer != AlpnOffer::H2AndH1
                    && matches!(
                        e.kind(),
                        ErrorKind::TlsHandshake | ErrorKind::ConnectionClosed
                    )
                {
                    inner.protocols.evict(target);
                }
            })?;
        let connect_time = Some(connected.elapsed);
        let tls = target.transport() == Transport::Tls;
        let use_h2 = match target.policy() {
            ProtocolPolicy::Http2Only => {
                if tls && !connected.alpn_h2 {
                    return Err(ProxyError::new(
                        ErrorKind::Protocol,
                        "upstream did not negotiate h2 for an HTTP/2-only target",
                    ));
                }
                true
            }
            _ => connected.alpn_h2,
        };

        if tls && target.policy() == ProtocolPolicy::Auto {
            let learned = if connected.alpn_h2 {
                CachedProtocol::Http2
            } else {
                CachedProtocol::Http1Only
            };
            inner.protocols.record(target, learned);
            inner.pool.h2_slot(key).set_h1_hint(!connected.alpn_h2);
        }

        let handle = inner.pool.new_handle();
        let io = KillableIo::new(connected.stream, &handle.kill, inner.cfg.write_timeout);
        if use_h2 {
            let sender = http2::handshake(io, &inner.cfg.h2, &handle.kill).await?;
            let slot = inner.pool.h2_slot(key);
            let (lease, reused) = inner.pool.store_h2(&slot, sender, handle);
            Ok(leased(lease, reused, connect_time))
        } else {
            let (tx, conn) = inner
                .h1
                .handshake::<TokioIo<KillableIo>, ProxyBody>(TokioIo::new(io))
                .await
                .map_err(ProxyError::from_hyper)?;
            tokio::spawn(drive("http1", conn, handle.clone()));
            Ok(Acquired {
                sender: Sender::H1(tx),
                conn: handle,
                hold: None,
                reused: false,
                connect_time,
            })
        }
    }

    async fn dispatch(
        &self,
        target: &Target,
        key: &PoolKey,
        acquired: Acquired,
        req: Request<ProxyBody>,
    ) -> Result<Response<ResponseBody>, DispatchError> {
        let inner = &self.inner;
        let Acquired {
            sender,
            conn,
            hold,
            reused,
            connect_time,
        } = acquired;
        let version = match sender {
            Sender::H1(_) => Version::HTTP_11,
            Sender::H2(_) => Version::HTTP_2,
        };
        inner.observer.on_connection(&ConnectionInfo {
            target,
            reused,
            version,
            connect_time,
        });
        let req = shape(req, target, version).map_err(DispatchError::Failed)?;

        match sender {
            Sender::H1(mut tx) => {
                let res = self.read_timeout(tx.try_send_request(req)).await;
                match res {
                    Ok(Ok(resp)) => {
                        // The connection goes back to the pool when the body
                        // reaches its clean end; a dropped or failed body
                        // closes it instead.
                        let pool = Arc::clone(&inner.pool);
                        let key = key.clone();
                        let on_end: OnEnd = Box::new(move || pool.checkin_h1(key, tx, conn));
                        Ok(self.finish_h1(resp, Some(on_end)))
                    }
                    Ok(Err(mut e)) => Err(match e.take_message() {
                        Some(back) => DispatchError::Unsent(
                            Box::new(back),
                            ProxyError::from_hyper(e.into_error()),
                        ),
                        None => DispatchError::Failed(ProxyError::from_hyper(e.into_error())),
                    }),
                    Err(err) => Err(DispatchError::Failed(err)),
                }
            }
            Sender::H2(tx) => {
                // `owner` cancels the exchange (resetting the stream and
                // stopping the upload) if this future is dropped or fails
                // before the response body takes it over.
                let (exchange, owner) = http2::Exchange::new();
                let started = self
                    .read_timeout(http2::start(
                        tx,
                        req,
                        inner.cfg.write_timeout,
                        hold.clone(),
                        exchange.clone(),
                    ))
                    .await
                    .map_err(DispatchError::Failed)?;
                let response = match started {
                    Started::Sent(response) => response,
                    Started::Unsent(back, err) => {
                        // The connection refused a new stream: forget it so
                        // the retry (or the next request) opens another.
                        inner.pool.drop_h2(key, conn.generation);
                        return Err(DispatchError::Unsent(back, err));
                    }
                };
                let head = self
                    .read_timeout(http2::response(response, &exchange))
                    .await
                    .and_then(|head| head);
                match head {
                    Ok(Ok(resp)) => Ok(self.finish_h2(resp, hold, exchange, owner)),
                    Ok(Err(e)) => {
                        // Only a dead connection is forgotten (and only this
                        // generation of it); a stream-level reset leaves the
                        // connection serving its other streams.
                        if http2::is_connection_error(&e) {
                            inner.pool.drop_h2(key, conn.generation);
                            inner.protocols.evict(target);
                        }
                        let err =
                            http2::from_h2(&e, "upstream HTTP/2 exchange failed").with_source(e);
                        Err(DispatchError::Failed(err))
                    }
                    Err(timeout) => Err(DispatchError::Failed(timeout)),
                }
            }
        }
    }
}

fn leased(lease: H2Lease, reused: bool, connect_time: Option<Duration>) -> Acquired {
    Acquired {
        sender: Sender::H2(lease.sender),
        conn: lease.conn,
        hold: Some(Arc::new(lease.active)),
        reused,
        connect_time,
    }
}

fn reused_h1(sender: http1::SendRequest<ProxyBody>, conn: ConnHandle) -> Acquired {
    Acquired {
        sender: Sender::H1(sender),
        conn,
        hold: None,
        reused: true,
        connect_time: None,
    }
}

/// Rewrite the request line, authority and framing for the wire version.
///
/// Idempotent, so a request handed back unsent can be re-shaped for a
/// connection of the other version.
fn shape(
    req: Request<ProxyBody>,
    target: &Target,
    version: Version,
) -> Result<Request<ProxyBody>, ProxyError> {
    let (mut parts, body) = req.into_parts();
    let authority = parts
        .headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned)
        .or_else(|| parts.uri.authority().map(|a| a.as_str().to_owned()))
        .unwrap_or_else(|| target.authority());
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or_else(|| "/".to_owned(), |pq| pq.as_str().to_owned());
    let bad = |e: http::Error| {
        ProxyError::new(ErrorKind::InvalidRequest, "invalid request target").with_source(e)
    };

    if version == Version::HTTP_2 {
        parts.uri = Uri::builder()
            .scheme(target.scheme())
            .authority(authority)
            .path_and_query(path_and_query)
            .build()
            .map_err(bad)?;
        parts.headers.remove(header::HOST);
        parts.headers.remove(header::TRANSFER_ENCODING);
    } else {
        parts.uri = Uri::try_from(path_and_query).map_err(|e| bad(e.into()))?;
        let host = HeaderValue::try_from(authority).map_err(|e| bad(e.into()))?;
        parts.headers.insert(header::HOST, host);
        // hyper assumes an unknown-length GET/HEAD body is empty unless the
        // framing is explicit; say so for every unknown-length body.
        if !body.is_end_stream() && body.size_hint().exact().is_none() {
            parts.headers.insert(
                header::TRANSFER_ENCODING,
                HeaderValue::from_static("chunked"),
            );
        } else {
            parts.headers.remove(header::TRANSFER_ENCODING);
        }
    }
    parts.version = version;
    Ok(Request::from_parts(parts, body))
}

/// Run an HTTP/1.1 connection task until it ends or is killed, logging how
/// it ended; either way the connection is marked dead.
async fn drive<F, E>(kind: &'static str, conn: F, handle: ConnHandle)
where
    F: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    tokio::select! {
        res = conn => {
            if let Err(e) = res {
                tracing::debug!(kind, error = %e, "upstream connection closed with error");
            }
        }
        () = handle.kill.cancelled() => {
            tracing::debug!(kind, "upstream connection closed by the engine");
        }
    }
    handle.kill.cancel();
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
