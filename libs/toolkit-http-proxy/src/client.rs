//! The proxy client: connection acquisition, dispatch and upgrades.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use http::{HeaderValue, Request, Response, StatusCode, Uri, Version, header};
use http_body::Body as _;
use http_body_util::BodyExt as _;
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper::upgrade::Upgraded;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio_util::sync::CancellationToken;

use crate::body::{Hold, OnEnd, ProxyBody, ResponseBody, TrackedBody, UploadProgress};
use crate::config::ProxyClientConfig;
use crate::connect::{KillableIo, connect};
use crate::error::{ErrorKind, ProxyError};
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
/// reaper and one task per connection.
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
    h2: http2::Builder<TokioExecutor>,
}

enum Sender {
    H1(http1::SendRequest<ProxyBody>),
    H2(http2::SendRequest<ProxyBody>),
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
    /// hyper handed the request back: nothing reached the upstream.
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

        let mut h2 = http2::Builder::new(TokioExecutor::new());
        h2.timer(TokioTimer::new())
            .max_header_list_size(cfg.h2.max_header_list_size)
            .header_table_size(cfg.h2.header_table_size)
            .max_frame_size(cfg.h2.max_frame_size)
            .initial_stream_window_size(cfg.h2.initial_stream_window_size)
            .initial_connection_window_size(cfg.h2.initial_connection_window_size)
            .adaptive_window(false)
            .max_concurrent_reset_streams(cfg.h2.max_concurrent_reset_streams)
            .max_pending_accept_reset_streams(cfg.h2.max_pending_accept_reset_streams)
            .max_local_error_reset_streams(cfg.h2.max_local_error_reset_streams)
            .max_send_buf_size(cfg.h2.max_send_buf_size)
            .keep_alive_interval(cfg.h2.keep_alive_interval)
            .keep_alive_timeout(cfg.h2.keep_alive_timeout)
            .keep_alive_while_idle(true);

        Ok(Self {
            inner: Arc::new(Inner {
                protocols: ProtocolCache::new(cfg.protocol_cache_ttl),
                cfg,
                tls,
                pool,
                observer,
                h1,
                h2,
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
        let (body, progress) = TrackedBody::new(body);
        let mut req = Request::from_parts(parts, body.boxed_unsync());
        let key = PoolKey::of(target);

        let mut retry: Option<Retry> = None;
        loop {
            let acquired = self.acquire(target, &key, retry).await?;
            let reused = acquired.reused;
            let generation = acquired.conn.generation;
            match self.dispatch(target, &key, acquired, req, &progress).await {
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
        let io = TokioIo::new(KillableIo::new(connected.stream, &handle.kill));
        let (mut tx, conn) = self
            .inner
            .h1
            .handshake::<_, ProxyBody>(io)
            .await
            .map_err(ProxyError::from_hyper)?;
        tokio::spawn(drive(
            "http1-upgrade",
            conn.with_upgrades(),
            handle.kill.clone(),
        ));
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
            return Ok(UpgradeOutcome::Rejected(self.finish(resp, None, None)));
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

    fn finish(
        &self,
        resp: Response<Incoming>,
        on_end: Option<OnEnd>,
        hold: Option<Hold>,
    ) -> Response<ResponseBody> {
        let (mut head, body) = resp.into_parts();
        // RFC 9112 §6.3: a response carrying both framings must not leave
        // its connection reusable, whatever hyper decided.
        let ambiguous = head.headers.contains_key(header::TRANSFER_ENCODING)
            && head.headers.contains_key(header::CONTENT_LENGTH);
        let on_end = if ambiguous { None } else { on_end };
        sanitize::sanitize_response_headers(&mut head.headers, head.status);
        Response::from_parts(
            head,
            ResponseBody::new(body, self.inner.cfg.read_timeout, on_end, hold),
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
        if plan == Plan::Negotiate
            && self.inner.protocols.get(target) == Some(CachedProtocol::Http1Only)
        {
            // A concurrent connect just learned this host is HTTP/1.1 only;
            // don't keep HTTP/2 setup serialised behind HTTP/1.1 connects.
            drop(connecting);
            if retry.is_none()
                && let Some((tx, conn)) = self.checkout_h1(key).await
            {
                return Ok(reused_h1(tx, conn));
            }
            return self.connect_new(target, key, AlpnOffer::H1).await;
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
        }

        let handle = inner.pool.new_handle();
        let io = TokioIo::new(KillableIo::new(connected.stream, &handle.kill));
        if use_h2 {
            let (tx, conn) = inner
                .h2
                .handshake::<_, ProxyBody>(io)
                .await
                .map_err(ProxyError::from_hyper)?;
            tokio::spawn(drive("http2", conn, handle.kill.clone()));
            let slot = inner.pool.h2_slot(key);
            let lease = inner.pool.store_h2(&slot, tx, handle);
            Ok(leased(lease, false, connect_time))
        } else {
            let (tx, conn) = inner
                .h1
                .handshake::<TokioIo<KillableIo>, ProxyBody>(io)
                .await
                .map_err(ProxyError::from_hyper)?;
            tokio::spawn(drive("http1", conn, handle.kill.clone()));
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
        progress: &Arc<UploadProgress>,
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

        let outcome = match sender {
            Sender::H1(mut tx) => {
                let res = self.read_timeout(tx.try_send_request(req)).await;
                match res {
                    Ok(Ok(resp)) => {
                        // The connection goes back to the pool when the body
                        // reaches its clean end; a dropped or failed body
                        // closes it instead.
                        let pool = Arc::clone(&inner.pool);
                        let (key, pooled) = (key.clone(), conn.clone());
                        let on_end: OnEnd = Box::new(move || pool.checkin_h1(key, tx, pooled));
                        Ok(self.finish(resp, Some(on_end), None))
                    }
                    Ok(Err(mut e)) => Err(match e.take_message() {
                        Some(back) => DispatchError::Unsent(
                            Box::new(back),
                            ProxyError::from_hyper(e.into_error()),
                        ),
                        None => DispatchError::Failed(ProxyError::from_hyper(e.into_error())),
                    }),
                    Err(timeout) => Err(DispatchError::Failed(timeout)),
                }
            }
            Sender::H2(mut tx) => {
                let res = self.read_timeout(tx.try_send_request(req)).await;
                match res {
                    Ok(Ok(resp)) => Ok(self.finish(resp, None, hold)),
                    Ok(Err(mut e)) => {
                        let back = e.take_message();
                        let err = ProxyError::from_hyper(e.into_error());
                        // Only a dead connection is forgotten (and only this
                        // generation of it); a stream-level reset leaves the
                        // connection serving its other streams.
                        if back.is_some() || tx.is_closed() {
                            inner.pool.drop_h2(key, conn.generation);
                            if err.kind() == ErrorKind::Http2 {
                                // The host may have broken HTTP/2: renegotiate.
                                inner.protocols.evict(target);
                            }
                        }
                        Err(match back {
                            Some(back) => DispatchError::Unsent(Box::new(back), err),
                            None => DispatchError::Failed(err),
                        })
                    }
                    Err(timeout) => Err(DispatchError::Failed(timeout)),
                }
            }
        };

        // The response head may arrive (or the head timeout fire) while the
        // request body is still uploading. If the upload then stops making
        // progress — e.g. the upstream holds the flow-control window at
        // zero — tear the connection down instead of parking the upload
        // forever.
        if !progress.is_done()
            && !matches!(outcome, Err(DispatchError::Unsent(..)))
            && let Some(budget) = inner.cfg.write_timeout
        {
            tokio::spawn(watch_upload(Arc::clone(progress), conn.kill, budget));
        }
        outcome
    }
}

fn leased(lease: H2Lease, reused: bool, connect_time: Option<Duration>) -> Acquired {
    Acquired {
        sender: Sender::H2(lease.sender),
        conn: lease.conn,
        hold: Some(Box::new(lease.active)),
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

/// Kill the connection if the upload makes no progress for `budget`.
async fn watch_upload(progress: Arc<UploadProgress>, kill: CancellationToken, budget: Duration) {
    let mut seen = progress.frames();
    loop {
        tokio::select! {
            () = kill.cancelled() => return,
            () = tokio::time::sleep(budget) => {}
        }
        if progress.is_done() {
            return;
        }
        let now = progress.frames();
        if now == seen {
            tracing::warn!(
                ?budget,
                "request upload stalled; closing the upstream connection"
            );
            kill.cancel();
            return;
        }
        seen = now;
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

/// Run a connection task until it ends or is killed, logging how it ended.
async fn drive<F, E>(kind: &'static str, conn: F, kill: CancellationToken)
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
        () = kill.cancelled() => {
            tracing::debug!(kind, "upstream connection closed by the engine");
        }
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
