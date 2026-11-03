//! The proxy client: connection acquisition, dispatch and upgrades.

use std::future::{Future, poll_fn};
use std::io;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, Uri, Version, header};
use http_body::Body as _;
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio_util::sync::DropGuard;

use crate::body::{BodySettings, Hold, OnEnd, ProxyBody, ReplayableBody, ResponseBody, full_body};
use crate::config::ProxyClientConfig;
use crate::connect::{KillableIo, connect};
use crate::error::{ErrorKind, ProxyError};
use crate::http2::{self, H2Body, H2Sender, Refusal, Started};
use crate::idle::IdleTimer;
use crate::observe::{ConnectionInfo, NoopObserver, Observer};
use crate::pool::{ConnHandle, H1Conn, H2Lease, Pool, PoolKey, reap_loop};
use crate::protocol_cache::{CachedProtocol, ProtocolCache};
use crate::sanitize::{self, RequestKind};
use crate::target::{ProtocolPolicy, Target, Transport};
use crate::timer::Timers;
use crate::tls::{AlpnOffer, TlsConnectors};

/// `Keep-Alive` (RFC 9112 Appendix C.2.2), looked up on every response.
const KEEP_ALIVE: http::HeaderName = http::HeaderName::from_static("keep-alive");

/// Result of [`ProxyClient::upgrade`].
// One value per upgrade, returned and consumed at once, never stored: its
// size does not matter, and boxing `Rejected` would only cost callers an
// allocation and a deref.
#[allow(clippy::large_enum_variant)]
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
/// reaper and a task per HTTP/2 connection (HTTP/1.1 connections are driven
/// by the exchange that uses them).
#[derive(Clone)]
pub struct ProxyClient {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: ProxyClientConfig,
    timers: Timers,
    tls: TlsConnectors,
    pool: Arc<Pool>,
    protocols: ProtocolCache,
    observer: Arc<dyn Observer>,
    h1: http1::Builder,
}

enum Sender {
    H1(H1Conn),
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
    /// The upstream asked for HTTP/1.1 (`HTTP_1_1_REQUIRED`): the retry
    /// must not negotiate h2 again, whatever the protocol cache holds.
    http1: bool,
}

enum DispatchError {
    /// The request provably never left: it comes back for a retry.
    Unsent(Box<Request<ProxyBody>>, ProxyError),
    /// The request left, but provably was not processed (an RFC-safe
    /// HTTP/2 refusal) or met a stale keep-alive connection: it may be sent
    /// again with its replayable body. Only returned for replayable
    /// requests.
    Resend {
        head: Box<http::request::Parts>,
        err: ProxyError,
        http1: bool,
    },
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
            .max_buf_size(cfg.h1.max_buf_size)
            // Real-world response heads, normalized as RFC 9112 §5.1/§5.2
            // ask of a proxy: whitespace before the colon is removed, an
            // obs-fold becomes a space. hyper alone decides the framing, so
            // this cannot desynchronise anything.
            .allow_spaces_after_header_name_in_responses(true)
            .allow_obsolete_multiline_headers_in_responses(true)
            .preserve_header_case(cfg.preserve_header_case);

        Ok(Self {
            inner: Arc::new(Inner {
                protocols: ProtocolCache::new(cfg.protocol_cache_ttl),
                timers: Timers::new(),
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
        // Fields the head's `Connection` nominated: HTTP/2 request trailers
        // lose them too (HTTP/1.1 sends no request trailers: `Trailer` is
        // never forwarded on requests).
        let hop = sanitize::connection_nominated(&parts.headers);
        if !hop.is_empty() {
            parts.extensions.insert(sanitize::HopFields(hop.into()));
        }
        sanitize::sanitize_request_headers(&mut parts.headers, RequestKind::Regular);
        // A body that can be sent again: an empty one, or the caller's copy.
        let replay = if body.is_end_stream() {
            Some(Bytes::new())
        } else {
            parts
                .extensions
                .get::<ReplayableBody>()
                .map(|copy| copy.0.clone())
        };
        let mut req = Request::from_parts(parts, body);
        let key = PoolKey::of(target);

        // At most one retry, and never of a request the upstream may have
        // processed, except an idempotent one (RFC 9110 §9.2.2):
        // - the request provably never left (a reused connection was
        //   already closed): resend it as is;
        // - the peer refused the HTTP/2 stream or asked for HTTP/1.1, or a
        //   reused connection turned out stale: resend it with its
        //   replayable body.
        let mut retry: Option<Retry> = None;
        loop {
            let acquired = self.acquire(target, &key, retry).await?;
            let reused = acquired.reused;
            let generation = acquired.conn.generation;
            let failed_generation = Some(Retry {
                failed_generation: generation,
                http1: false,
            });
            match self
                .dispatch(target, &key, acquired, req, replay.is_some())
                .await
            {
                Ok(resp) => return Ok(resp),
                Err(DispatchError::Unsent(back, err)) if reused && retry.is_none() => {
                    tracing::debug!(target = %target, error = %err, "pooled connection unusable, retrying on another connection");
                    req = *back;
                    retry = failed_generation;
                }
                Err(DispatchError::Resend { head, err, http1 }) if retry.is_none() => {
                    let Some(body) = replay.clone() else {
                        return Err(err);
                    };
                    tracing::debug!(target = %target, error = %err, "upstream did not process the request, sending it again");
                    req = Request::from_parts(*head, full_body(body));
                    retry = Some(Retry {
                        failed_generation: generation,
                        http1,
                    });
                }
                Err(
                    DispatchError::Unsent(_, err)
                    | DispatchError::Resend { err, .. }
                    | DispatchError::Failed(err),
                ) => return Err(err),
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
        let io = TokioIo::new(KillableIo::new(
            connected.stream,
            &handle,
            None,
            &self.inner.timers,
        ));
        let (tx, conn) = self
            .inner
            .h1
            .handshake::<_, ProxyBody>(io)
            .await
            .map_err(ProxyError::from_hyper)?;
        // The connection task ends when hyper hands the socket to `Upgraded`,
        // so it never cancels the handle itself. `h1` owns the connection
        // until then: a failed, timed-out or cancelled handshake (or a
        // rejected answer, once its body ends) closes it; only a validated
        // switch lets it go to the tunnel.
        // `KillableIo` wakes nothing on a kill: this task watches the switch
        // itself, so a lost handshake closes the socket right away.
        let kill = handle.kill.clone();
        tokio::spawn(async move {
            tokio::select! {
                res = conn.with_upgrades() => {
                    if let Err(e) = res {
                        tracing::debug!(error = %e, "upstream upgrade connection closed with error");
                    }
                }
                () = kill.cancelled() => {}
            }
        });
        let mut h1 = H1Conn::detached(tx, handle);
        self.inner.observer.on_connection(&ConnectionInfo {
            target,
            reused: false,
            version: Version::HTTP_11,
            connect_time: Some(connect_time),
        });

        let mut resp = self
            .read_timeout(h1.sender.send_request(req))
            .await?
            .map_err(ProxyError::from_hyper)?;
        if resp.status() != StatusCode::SWITCHING_PROTOCOLS {
            return Ok(UpgradeOutcome::Rejected(self.finish_h1(resp, h1, None)));
        }
        // `101` exists only in HTTP/1.1 (RFC 9110 §15.2.2): an HTTP/1.0 one
        // is no switch, and its connection is dropped with `h1`.
        if resp.version() != Version::HTTP_11 {
            return Err(ProxyError::new(
                ErrorKind::Protocol,
                "upstream answered 101 over HTTP/1.0",
            ));
        }
        sanitize::validate_websocket_switch(resp.headers())?;
        let io = hyper::upgrade::on(&mut resp)
            .await
            .map_err(ProxyError::from_hyper)?;
        h1.into_tunnel();
        let (mut head, _) = resp.into_parts();
        sanitize::sanitize_response_headers(&mut head.headers, head.status);
        Ok(UpgradeOutcome::Switched {
            response: Response::from_parts(head, ()),
            io,
        })
    }

    /// `conn` owns the connection until the body ends: a clean end returns
    /// it to the pool under `pool_key` (if any), a dropped or failed body
    /// drops it and with it the connection.
    fn finish_h1(
        &self,
        resp: Response<Incoming>,
        conn: H1Conn,
        pool_key: Option<PoolKey>,
    ) -> Response<ResponseBody> {
        let (mut head, body) = resp.into_parts();
        // RFC 9112 §6.3: a response carrying both framings must not leave
        // its connection reusable, whatever hyper decided. It still owns the
        // connection until the body ends, so the body is not cut short.
        let ambiguous = head.headers.contains_key(header::TRANSFER_ENCODING)
            && head.headers.contains_key(header::CONTENT_LENGTH);
        let on_end: Option<OnEnd> = match pool_key {
            Some(key) if !ambiguous => {
                let pool = Arc::clone(&self.inner.pool);
                let keep_alive = keep_alive_timeout(&head.headers);
                Some(Box::new(move |conn| pool.checkin_h1(key, conn, keep_alive)))
            }
            _ => None,
        };
        let hop = sanitize::sanitize_response_head(&mut head.headers, head.status);
        Response::from_parts(
            head,
            ResponseBody::http1(body, conn, self.body_settings(), on_end, hop),
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
                self.body_settings(),
                hold,
            ),
        )
    }

    /// The per-frame idle timeout and merge limit for a response body.
    fn body_settings(&self) -> BodySettings {
        BodySettings {
            idle: self.inner.cfg.read_timeout,
            timers: self.inner.timers.clone(),
            merge_limit: self.inner.cfg.response_merge_limit,
        }
    }

    async fn read_timeout<F: Future>(&self, fut: F) -> Result<F::Output, ProxyError> {
        match self.inner.cfg.read_timeout {
            Some(limit) => self.inner.timers.timeout(limit, fut).await.map_err(|_| {
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
        let avoid = retry.map(|r| r.failed_generation);

        if retry.is_some_and(|r| r.http1) {
            return self.connect_new(target, key, AlpnOffer::H1, None).await;
        }

        if retry.is_none() {
            if plan != Plan::H1
                && let Some(lease) = pool.live_h2(key)
            {
                return Ok(leased(lease, true, None));
            }
            if matches!(plan, Plan::H1 | Plan::Negotiate)
                && let Some(conn) = self.checkout_h1(key).await
            {
                return Ok(reused_h1(conn));
            }
        }

        if plan == Plan::H1 {
            return self.connect_new(target, key, AlpnOffer::H1, None).await;
        }

        // HTTP/2-capable: serialise connection setup per key so a burst of
        // first requests shares one multiplexed connection. A retry may use a
        // replacement someone else installed after its failure.
        let slot = pool.h2_slot(key);
        let seen = slot.connect_attempts();
        let connecting = slot.connecting.lock().await;
        // An attempt we waited for failed: share its outcome instead of
        // repeating it, so a burst against an unresponsive host fails after
        // one connect timeout, not one per request.
        if let Some(err) = slot.failed_after(seen) {
            return Err(err);
        }
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
                    && let Some(conn) = self.checkout_h1(key).await
                {
                    return Ok(reused_h1(conn));
                }
                let offer = if cached_h1 {
                    AlpnOffer::H1
                } else {
                    AlpnOffer::H2AndH1
                };
                return self.connect_new(target, key, offer, avoid).await;
            }
        }
        let offer = if plan == Plan::Negotiate {
            AlpnOffer::H2AndH1
        } else {
            AlpnOffer::H2
        };
        let acquired = self.connect_new(target, key, offer, avoid).await;
        slot.note_connect(&acquired);
        drop(connecting);
        acquired
    }

    /// Pop an idle HTTP/1.1 connection. Nobody drove it while it was idle:
    /// driving it now lets it finish the previous exchange (it was released
    /// at the end of that response body) or notice that the upstream closed
    /// it. A connection that is not ready in time is treated as dead.
    async fn checkout_h1(&self, key: &PoolKey) -> Option<H1Conn> {
        const READY_GRACE: Duration = Duration::from_millis(100);
        // A connection that is not ready in time (or whose wait is
        // cancelled) is dropped, which closes it.
        while let Some(mut conn) = self.inner.pool.checkout_h1(key) {
            let mut ready = pin!(poll_fn(|cx| {
                conn.drive(cx);
                conn.sender.poll_ready(cx)
            }));
            // Ready right away is the common case: no timer for it.
            let now = ready.as_mut().poll(&mut Context::from_waker(Waker::noop()));
            let ready = match now {
                Poll::Ready(res) => res.is_ok(),
                Poll::Pending => {
                    matches!(tokio::time::timeout(READY_GRACE, ready).await, Ok(Ok(())))
                }
            };
            if ready {
                return Some(conn);
            }
        }
        None
    }

    async fn connect_new(
        &self,
        target: &Target,
        key: &PoolKey,
        offer: AlpnOffer,
        avoid: Option<u64>,
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
        let io = KillableIo::new(
            connected.stream,
            &handle,
            inner.cfg.write_timeout,
            &inner.timers,
        );
        if use_h2 {
            let setup =
                http2::handshake(io, &inner.cfg.h2, &handle.kill, inner.cfg.connect_timeout).await;
            let sender = match setup {
                Ok(sender) => sender,
                // The upstream turned HTTP/2 down before anything was sent:
                // remember that and connect again over HTTP/1.1.
                Err(e)
                    if http2::requires_http1(&e)
                        && tls
                        && target.policy() == ProtocolPolicy::Auto =>
                {
                    inner.protocols.record(target, CachedProtocol::Http1Only);
                    inner.pool.h2_slot(key).set_h1_hint(true);
                    return Box::pin(self.connect_new(target, key, AlpnOffer::H1, None)).await;
                }
                Err(e) => return Err(e),
            };
            let slot = inner.pool.h2_slot(key);
            let (lease, reused) = inner.pool.store_h2(&slot, sender, handle, avoid);
            Ok(leased(lease, reused, connect_time))
        } else {
            let (tx, conn) = inner
                .h1
                .handshake::<TokioIo<KillableIo>, ProxyBody>(TokioIo::new(io))
                .await
                .map_err(ProxyError::from_hyper)?;
            Ok(Acquired {
                sender: Sender::H1(H1Conn::new(tx, conn, handle.clone())),
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
        replayable: bool,
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
            Sender::H1(mut h1) => {
                // A stale keep-alive connection may only cost an idempotent,
                // replayable request a resend; keep its head for that.
                let resend = (reused && replayable && is_idempotent(req.method()))
                    .then(|| Box::new(head_of(&req)));
                // `h1` owns the connection: on any failure, timeout or
                // cancellation below it is dropped, closing the connection.
                // Until the response head arrives, this task drives it.
                let mut sent = pin!(h1.sender.try_send_request(req));
                let res = self
                    .read_timeout(poll_fn(|cx| {
                        h1.drive(cx);
                        sent.as_mut().poll(cx)
                    }))
                    .await;
                match res {
                    Ok(Ok(resp)) => Ok(self.finish_h1(resp, h1, Some(key.clone()))),
                    Ok(Err(mut e)) => Err(match (e.take_message(), resend) {
                        (Some(back), _) => DispatchError::Unsent(
                            Box::new(back),
                            ProxyError::from_hyper(e.into_error()),
                        ),
                        (None, Some(head)) if is_stale(e.error()) => DispatchError::Resend {
                            head,
                            err: ProxyError::from_hyper(e.into_error()),
                            http1: false,
                        },
                        (None, _) => DispatchError::Failed(ProxyError::from_hyper(e.into_error())),
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
                        inner
                            .cfg
                            .write_timeout
                            .map(|limit| IdleTimer::new(limit, &inner.timers)),
                        hold.clone(),
                        exchange.clone(),
                    ))
                    .await
                    .map_err(DispatchError::Failed)?;
                let (response, sent_head) = match started {
                    Started::Sent(response, head) => (response, head),
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
                        let refusal = http2::refusal(&e);
                        // Only a dead connection is forgotten (and only this
                        // generation of it); a stream-level reset leaves the
                        // connection serving its other streams. A graceful
                        // GOAWAY retires the connection but says nothing
                        // against the host speaking h2.
                        if http2::is_connection_error(&e) {
                            inner.pool.drop_h2(key, conn.generation);
                            if refusal != Some(Refusal::GoingAway) {
                                inner.protocols.evict(target);
                            }
                        }
                        let fallback = refusal == Some(Refusal::Http1Required)
                            && target.policy() == ProtocolPolicy::Auto
                            && target.transport() == Transport::Tls;
                        if fallback {
                            // Later requests to this host go over HTTP/1.1,
                            // and this connection is not leased again.
                            inner.protocols.record(target, CachedProtocol::Http1Only);
                            inner.pool.h2_slot(key).set_h1_hint(true);
                            inner.pool.drop_h2(key, conn.generation);
                        }
                        let resend = replayable
                            && match refusal {
                                Some(Refusal::Refused) => true,
                                Some(Refusal::Http1Required) => fallback,
                                Some(Refusal::GoingAway) => is_idempotent(&sent_head.method),
                                None => false,
                            };
                        let err =
                            http2::from_h2(&e, "upstream HTTP/2 exchange failed").with_source(e);
                        Err(if resend {
                            DispatchError::Resend {
                                head: sent_head,
                                err,
                                http1: fallback,
                            }
                        } else {
                            DispatchError::Failed(err)
                        })
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

fn reused_h1(conn: H1Conn) -> Acquired {
    Acquired {
        conn: conn.handle.clone(),
        sender: Sender::H1(conn),
        hold: None,
        reused: true,
        connect_time: None,
    }
}

/// A copy of the request head (everything but the body).
fn head_of<B>(req: &Request<B>) -> http::request::Parts {
    let (mut head, ()) = Request::new(()).into_parts();
    head.method = req.method().clone();
    head.uri = req.uri().clone();
    head.version = req.version();
    head.headers = req.headers().clone();
    head.extensions = req.extensions().clone();
    head
}

/// RFC 9110 §9.2.2: repeating the request has the same effect as sending it
/// once.
fn is_idempotent(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE | Method::PUT | Method::DELETE
    )
}

/// Methods whose requests carry no meaningful body, so an empty one needs
/// no `Content-Length`.
fn is_bodiless_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET
            | Method::HEAD
            | Method::DELETE
            | Method::OPTIONS
            | Method::CONNECT
            | Method::TRACE
    )
}

/// The upstream closed a reused HTTP/1.1 connection before answering: the
/// classic keep-alive race. A connection the engine killed itself
/// (`ConnectionAborted`, write stall) does not count.
fn is_stale(err: &hyper::Error) -> bool {
    if err.is_incomplete_message() || err.is_canceled() {
        return true;
    }
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<io::Error>() {
            return matches!(
                io.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::UnexpectedEof
            );
        }
        source = cause.source();
    }
    false
}

/// The idle timeout an HTTP/1.1 upstream announces in `Keep-Alive:
/// timeout=N` (RFC 2068 §19.7.1.1, still common).
fn keep_alive_timeout(headers: &http::HeaderMap) -> Option<Duration> {
    headers
        .get_all(&KEEP_ALIVE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|param| {
            let (name, value) = param.split_once('=')?;
            if !name.trim().eq_ignore_ascii_case("timeout") {
                return None;
            }
            value.trim().trim_matches('"').parse::<u64>().ok()
        })
        .map(Duration::from_secs)
}

/// HTTP/2 lets a client split `cookie` into several fields; HTTP/1.1
/// expects one (RFC 9113 §8.2.3, RFC 6265 §5.4), and many servers only read
/// the first. Join them with `"; "` before an HTTP/1.1 hop.
fn join_cookies(headers: &mut http::HeaderMap) -> Result<(), http::Error> {
    let mut values = headers.get_all(header::COOKIE).iter();
    let (Some(first), Some(_)) = (values.next(), values.next()) else {
        return Ok(());
    };
    let mut joined = first.as_bytes().to_vec();
    for value in headers.get_all(header::COOKIE).iter().skip(1) {
        joined.extend_from_slice(b"; ");
        joined.extend_from_slice(value.as_bytes());
    }
    let joined = HeaderValue::from_bytes(&joined)?;
    headers.insert(header::COOKIE, joined);
    Ok(())
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
        // hyper's HTTP/1.1 encoder writes a known length itself; h2 does
        // not. State it, with the HTTP/1.1 rule for empty bodies below:
        // upstreams that demand a length on HTTP/1.1 often do on h2 too.
        match body.size_hint().exact() {
            Some(len) if len != 0 || !is_bodiless_method(&parts.method) => {
                parts
                    .headers
                    .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            }
            _ => {
                parts.headers.remove(header::CONTENT_LENGTH);
            }
        }
    } else {
        parts.uri = Uri::try_from(path_and_query).map_err(|e| bad(e.into()))?;
        let host = HeaderValue::try_from(authority).map_err(|e| bad(e.into()))?;
        parts.headers.insert(header::HOST, host);
        join_cookies(&mut parts.headers).map_err(bad)?;
        // hyper assumes an unknown-length GET/HEAD body is empty unless the
        // framing is explicit; say so for every unknown-length body.
        if !body.is_end_stream() && body.size_hint().exact().is_none() {
            parts.headers.insert(
                header::TRANSFER_ENCODING,
                HeaderValue::from_static("chunked"),
            );
        } else {
            parts.headers.remove(header::TRANSFER_ENCODING);
            // RFC 9110 §8.6: send a length when the method gives the body a
            // meaning, even if it is empty (hyper writes none for an empty
            // body, and strict upstreams answer 411 without it).
            if body.is_end_stream() && !is_bodiless_method(&parts.method) {
                parts
                    .headers
                    .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
            }
        }
    }
    parts.version = version;
    Ok(Request::from_parts(parts, body))
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
