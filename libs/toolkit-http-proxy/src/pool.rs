//! Connection pool.
//!
//! Keyed by everything that makes a connection interchangeable: the socket
//! address, the TLS server name and the transport. Two upstreams that share
//! an IP but present different SNI never share a TLS connection.
//!
//! - HTTP/1.1: a LIFO stack of idle connections per key. A connection is put
//!   back only when the previous response body reached its clean end; a
//!   half-read or failed exchange closes the connection instead.
//! - HTTP/2: a small set of multiplexed connections per key. A request uses
//!   the least loaded one; another connection opens once every connection
//!   carries `h2_streams_per_connection` exchanges (up to
//!   `h2_max_connections_per_host`), which spreads TLS and framing work over
//!   cores. Each connection carries a generation number, so a failure
//!   observed on an old connection never evicts its replacement, and an
//!   active-exchange count, so the reaper never detaches a busy connection.
//!
//! An HTTP/1.1 connection is driven by whoever owns it (see [`H1Conn`]); an
//! HTTP/2 connection runs a task of its own. Every connection can be killed
//! through its [`ConnHandle`]; the engine uses that to tear down connections
//! whose request upload stalled.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use dashmap::DashMap;
// Keys are configured upstreams, not attacker input; still seeded per pool.
use foldhash::fast::RandomState;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use sync_wrapper::SyncWrapper;
use tokio::time::Instant;

use crate::body::ProxyBody;
use crate::config::PoolConfig;
use crate::connect::KillableIo;
use crate::error::{ErrorKind, ProxyError};
use crate::http2::H2Sender;
use crate::kill::Kill;
use crate::target::{Target, Transport};

/// What makes two connections interchangeable: the socket address, the
/// TLS server name and the transport.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    addr: SocketAddr,
    server_name: Arc<str>,
    transport: Transport,
}

impl PoolKey {
    /// The key of `target`'s connections.
    pub fn of(target: &Target) -> Self {
        Self {
            addr: target.addr(),
            server_name: Arc::from(target.server_name()),
            transport: target.transport(),
        }
    }
}

/// Identity of one upstream connection and the switch that kills its task.
#[derive(Debug, Clone)]
pub struct ConnHandle {
    /// Unique per connection, so a failure seen on an old connection never
    /// evicts its replacement.
    pub generation: u64,
    /// Closes the connection when flipped (see [`Kill`]).
    pub kill: Kill,
    /// A write or flush on the socket is pending: the upstream is not
    /// reading what was sent (kept up to date by [`KillableIo`]).
    pub write_blocked: Arc<AtomicBool>,
}

/// hyper's state machine for one HTTP/1.1 connection.
pub type H1Driver = http1::Connection<TokioIo<KillableIo>, ProxyBody>;

/// Sole owner of an HTTP/1.1 connection, from its handshake until it is
/// idle in the pool, leased to one exchange, or waiting for its response
/// body to end. Dropping it closes the connection: whoever loses it (expiry,
/// eviction, a timed-out or cancelled exchange, an abandoned or ambiguously
/// framed response) can never leave the connection running on its own, e.g.
/// still polling a request body the upstream answered early.
///
/// The owner also drives the connection ([`H1Conn::drive`]) while it waits on
/// it: the exchange while it waits for the response head, the response body
/// while it is read, the pool when it checks a connection out or reaps.
/// Socket reads then happen in the task that consumes the data, with no
/// hand-off between tasks per body chunk, and nothing reads ahead of the
/// consumer beyond hyper's read buffer.
pub struct H1Conn {
    /// hyper's request sender for this connection.
    pub sender: http1::SendRequest<ProxyBody>,
    /// Identity and kill switch of the connection.
    pub handle: ConnHandle,
    /// `None` once the connection ended, or when a task of its own drives
    /// it (upgrades). Only ever used through `&mut`.
    driver: Option<SyncWrapper<Pin<Box<H1Driver>>>>,
    armed: bool,
}

impl H1Conn {
    /// A connection driven by its owner.
    pub fn new(
        sender: http1::SendRequest<ProxyBody>,
        driver: H1Driver,
        handle: ConnHandle,
    ) -> Self {
        Self {
            sender,
            handle,
            driver: Some(SyncWrapper::new(Box::pin(driver))),
            armed: true,
        }
    }

    /// A connection whose driver runs elsewhere (a spawned task).
    pub fn detached(sender: http1::SendRequest<ProxyBody>, handle: ConnHandle) -> Self {
        Self {
            sender,
            handle,
            driver: None,
            armed: true,
        }
    }

    /// Let the connection make progress: write the request, read the
    /// response, notice that the upstream closed it. Registers `cx` for
    /// wake-ups, so call it from the task that waits on the connection.
    pub fn drive(&mut self, cx: &mut Context<'_>) {
        let Some(driver) = self.driver.as_mut() else {
            return;
        };
        if let Poll::Ready(res) = driver.get_mut().as_mut().poll(cx) {
            if let Err(e) = res {
                tracing::debug!(error = %e, "upstream connection closed with error");
            }
            // Ended (closed, failed or killed): never handed out again.
            self.driver = None;
            self.handle.kill.cancel();
        }
    }

    /// Whether the connection can carry another exchange right away, i.e.
    /// may be pooled, where nobody drives it. hyper reports it ready once the
    /// exchange that just ended has handed over its whole request body;
    /// unless it already did, it is driven once more (nothing waits on it any
    /// more) to finish that exchange. Not pooled:
    /// - a connection still taking its request body, because the upstream
    ///   answered before reading all of it (RFC 9112 §9.6: the rest of that
    ///   body would be read as the next request);
    /// - one whose last write or flush is still pending: "ready" means the
    ///   request is queued (in hyper or rustls), not on the wire, and once
    ///   idle nothing would drive it to its end or enforce the write-stall
    ///   timeout before the next exchange stalls behind it.
    fn settle(&mut self) -> bool {
        if self.driver.is_none() {
            return self.is_open();
        }
        // Usually ready already: driving again would only cost a socket read.
        if !self.sender.is_ready() {
            self.drive(&mut Context::from_waker(Waker::noop()));
        }
        self.is_open()
            && self.sender.is_ready()
            && !self.handle.write_blocked.load(Ordering::Relaxed)
    }

    /// Let go without closing: the socket now belongs to an upgraded
    /// tunnel.
    pub fn into_tunnel(mut self) {
        self.armed = false;
    }

    fn is_open(&self) -> bool {
        !self.sender.is_closed() && !self.handle.kill.is_cancelled()
    }
}

impl Drop for H1Conn {
    fn drop(&mut self) {
        if self.armed {
            self.handle.kill.cancel();
        }
    }
}

struct IdleH1 {
    conn: H1Conn,
    since: Instant,
    /// How long it may stay idle: the pool's idle timeout, or less if the
    /// upstream announced a shorter `Keep-Alive: timeout`.
    ttl: Duration,
}

impl IdleH1 {
    fn usable(&self) -> bool {
        self.conn.is_open() && self.since.elapsed() < self.ttl
    }
}

/// A failed connection attempt, shared with requests that were waiting for
/// it so they fail together instead of each repeating the attempt.
struct ConnectFailure {
    /// Which attempt (see `H2Slot::connect_attempts`).
    attempt: u64,
    kind: ErrorKind,
    message: String,
}

/// Outcome of one global-eviction step.
enum Evicted {
    One,
    /// The chosen entry went away (checked out or evicted) meanwhile.
    Raced,
    Nothing,
}

struct H2Entry {
    sender: H2Sender,
    conn: ConnHandle,
    active: Arc<AtomicUsize>,
}

/// The HTTP/2 connection to use: the least loaded one with room for another
/// stream (our per-connection target, capped by the upstream's
/// `SETTINGS_MAX_CONCURRENT_STREAMS`). When none has room: the least loaded
/// one that takes streams at all (h2 queues the stream), then any. The flag
/// says whether the chosen one had room.
fn pick_h2<'a>(
    entries: impl Iterator<Item = &'a H2Entry>,
    target: usize,
) -> Option<(&'a H2Entry, bool)> {
    entries
        .map(|e| {
            let active = e.active.load(Ordering::Relaxed);
            let limit = target.min(e.sender.current_max_send_streams());
            (e, active < limit, limit > 0, active)
        })
        .min_by_key(|&(_, room, accepts, active)| (!room, !accepts, active))
        .map(|(e, room, ..)| (e, room))
}

/// The HTTP/2 connections for one key plus a lock that serialises
/// establishing them, so a burst of first requests opens one connection.
pub struct H2Slot {
    entries: parking_lot::Mutex<Vec<H2Entry>>,
    /// Held while a connection for this key is being established.
    pub connecting: tokio::sync::Mutex<()>,
    last_used: AtomicU64,
    active: AtomicUsize,
    /// The last negotiation on this key ended on HTTP/1.1: don't hold
    /// `connecting` across further HTTP/1.1 connects (works with the
    /// protocol cache disabled too).
    h1_hint: AtomicBool,
    /// Completed connection attempts (under `connecting`).
    attempts: AtomicU64,
    last_connect_failure: parking_lot::Mutex<Option<ConnectFailure>>,
}

impl H2Slot {
    /// Whether the last negotiation on this key ended on HTTP/1.1.
    pub fn h1_hint(&self) -> bool {
        self.h1_hint.load(Ordering::Relaxed)
    }

    /// Record how the last negotiation on this key ended.
    pub fn set_h1_hint(&self, h1: bool) {
        self.h1_hint.store(h1, Ordering::Relaxed);
    }

    /// Attempts completed so far; read before waiting for `connecting`.
    pub fn connect_attempts(&self) -> u64 {
        self.attempts.load(Ordering::Acquire)
    }

    /// Remember how a connection attempt on this key ended (call it while
    /// holding `connecting`).
    pub fn note_connect<T>(&self, result: &Result<T, ProxyError>) {
        let attempt = self.attempts.fetch_add(1, Ordering::AcqRel) + 1;
        *self.last_connect_failure.lock() = result.as_ref().err().map(|e| ConnectFailure {
            attempt,
            kind: e.kind(),
            message: e.message().to_owned(),
        });
    }

    /// The failure of the latest attempt, if it completed after `seen`
    /// attempts: a request that waited for it gets its outcome instead of
    /// trying again; one that arrives later tries itself.
    pub fn failed_after(&self, seen: u64) -> Option<ProxyError> {
        let failure = self.last_connect_failure.lock();
        let failure = failure.as_ref().filter(|f| f.attempt > seen)?;
        Some(ProxyError::new(failure.kind, failure.message.clone()))
    }
}

/// An HTTP/2 sender checked out for one exchange.
pub struct H2Lease {
    /// Opens the exchange's stream on the leased connection.
    pub sender: H2Sender,
    /// Identity and kill switch of the leased connection.
    pub conn: ConnHandle,
    /// Counts the exchange on the connection until dropped.
    pub active: ActiveStream,
}

/// Counts one in-flight exchange on an HTTP/2 connection (and its slot)
/// until dropped.
pub struct ActiveStream {
    slot: Arc<H2Slot>,
    conn: Arc<AtomicUsize>,
}

impl Drop for ActiveStream {
    fn drop(&mut self) {
        self.conn.fetch_sub(1, Ordering::Relaxed);
        self.slot.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Idle HTTP/1.1 connections and live HTTP/2 connections, keyed by
/// [`PoolKey`].
pub struct Pool {
    h1: DashMap<PoolKey, Vec<IdleH1>, RandomState>,
    /// Idle HTTP/1.1 connections across all keys.
    idle_h1: AtomicUsize,
    /// One global eviction at a time (taken only when over the cap).
    evicting: parking_lot::Mutex<()>,
    h2: DashMap<PoolKey, Arc<H2Slot>, RandomState>,
    cfg: PoolConfig,
    epoch: Instant,
    generations: AtomicU64,
}

impl Pool {
    /// An empty pool with `cfg`'s limits.
    pub fn new(cfg: PoolConfig) -> Self {
        Self {
            h1: DashMap::with_hasher(RandomState::default()),
            idle_h1: AtomicUsize::new(0),
            evicting: parking_lot::Mutex::new(()),
            h2: DashMap::with_hasher(RandomState::default()),
            cfg,
            epoch: Instant::now(),
            generations: AtomicU64::new(0),
        }
    }

    fn now_secs(&self) -> u64 {
        self.epoch.elapsed().as_secs()
    }

    /// A fresh handle for a new connection.
    pub fn new_handle(&self) -> ConnHandle {
        ConnHandle {
            generation: self.generations.fetch_add(1, Ordering::Relaxed) + 1,
            kill: Kill::new(),
            write_blocked: Arc::default(),
        }
    }

    /// Most recently idled HTTP/1.1 connection that is still open. It may
    /// need a moment to become ready (see `ProxyClient::checkout_h1`).
    pub fn checkout_h1(&self, key: &PoolKey) -> Option<H1Conn> {
        let mut stack = self.h1.get_mut(key)?;
        while let Some(idle) = stack.pop() {
            self.idle_h1.fetch_sub(1, Ordering::Relaxed);
            if idle.usable() {
                return Some(idle.conn);
            }
        }
        None
    }

    /// Park a connection whose previous response body was read to the end.
    /// `keep_alive` is the upstream's announced idle timeout, if any; the
    /// connection is retired a second before it, so a request is never
    /// written onto a connection the upstream is about to close.
    pub fn checkin_h1(&self, key: PoolKey, mut conn: H1Conn, keep_alive: Option<Duration>) {
        if !conn.settle() {
            return;
        }
        let ttl = keep_alive.map_or(self.cfg.idle_timeout, |t| {
            self.cfg
                .idle_timeout
                .min(t.saturating_sub(Duration::from_secs(1)))
        });
        let idle = IdleH1 {
            conn,
            since: Instant::now(),
            ttl,
        };
        if !idle.usable() || self.cfg.max_idle_per_host == 0 || self.cfg.max_idle_total == 0 {
            return;
        }
        {
            let mut stack = self.h1.entry(key).or_default();
            if stack.len() >= self.cfg.max_idle_per_host {
                // Drop the oldest; the newest is the warmest.
                stack.remove(0);
            } else {
                self.idle_h1.fetch_add(1, Ordering::Relaxed);
            }
            stack.push(idle);
        }
        // The entry guard above is released first, so scanning the map
        // cannot deadlock on it.
        self.enforce_idle_total();
    }

    /// Close the oldest idle connections (of any host) until the total cap
    /// holds.
    ///
    /// Evictions are serialised, so they never compete for the same victim:
    /// a round that loses its victim lost it to a checkout or a per-host
    /// eviction, which already removed an entry, and the next round looks
    /// again. It ends once the cap holds or nothing is idle.
    fn enforce_idle_total(&self) {
        if self.idle_h1.load(Ordering::Relaxed) <= self.cfg.max_idle_total {
            return;
        }
        let _evicting = self.evicting.lock();
        while self.idle_h1.load(Ordering::Relaxed) > self.cfg.max_idle_total {
            match self.evict_oldest_h1() {
                Evicted::One | Evicted::Raced => {}
                Evicted::Nothing => return,
            }
        }
    }

    /// Close the oldest idle HTTP/1.1 connection across all keys.
    fn evict_oldest_h1(&self) -> Evicted {
        let oldest = self
            .h1
            .iter()
            .filter_map(|entry| {
                entry
                    .value()
                    .first()
                    .map(|c| (c.since, entry.key().clone()))
            })
            .min_by_key(|(since, _)| *since);
        let Some((since, key)) = oldest else {
            return Evicted::Nothing;
        };
        let Some(mut stack) = self.h1.get_mut(&key) else {
            return Evicted::Raced;
        };
        if stack.first().is_none_or(|c| c.since != since) {
            return Evicted::Raced;
        }
        stack.remove(0);
        self.idle_h1.fetch_sub(1, Ordering::Relaxed);
        Evicted::One
    }

    /// Idle HTTP/1.1 connections across all keys.
    #[cfg(test)]
    fn idle_h1_count(&self) -> usize {
        self.idle_h1.load(Ordering::Relaxed)
    }

    /// The HTTP/2 slot of `key`, created on first use.
    pub fn h2_slot(&self, key: &PoolKey) -> Arc<H2Slot> {
        if let Some(slot) = self.h2.get(key) {
            return Arc::clone(&slot);
        }
        Arc::clone(&self.h2.entry(key.clone()).or_insert_with(|| {
            Arc::new(H2Slot {
                entries: parking_lot::Mutex::new(Vec::new()),
                connecting: tokio::sync::Mutex::new(()),
                last_used: AtomicU64::new(0),
                active: AtomicUsize::new(0),
                h1_hint: AtomicBool::new(false),
                attempts: AtomicU64::new(0),
                last_connect_failure: parking_lot::Mutex::new(None),
            })
        }))
    }

    /// Lease the live HTTP/2 connection for `key`, if any.
    pub fn live_h2(&self, key: &PoolKey) -> Option<H2Lease> {
        let slot = Arc::clone(&*self.h2.get(key)?);
        self.lease(&slot, None)
    }

    /// Lease a live connection of the slot, skipping generation `not_gen`
    /// (the connection a retry just failed on). Returns `None` when there is
    /// none, or when every connection is at `h2_streams_per_connection` and
    /// another one may still be opened.
    pub fn lease(&self, slot: &Arc<H2Slot>, not_gen: Option<u64>) -> Option<H2Lease> {
        let mut entries = slot.entries.lock();
        entries.retain(|e| !e.conn.kill.is_cancelled());
        let (best, room) = pick_h2(
            entries
                .iter()
                .filter(|e| not_gen != Some(e.conn.generation)),
            self.cfg.h2_streams_per_connection,
        )?;
        // Every connection is full: open another one while the cap allows.
        if !room && entries.len() < self.cfg.h2_max_connections_per_host {
            return None;
        }
        let lease = H2Lease {
            sender: best.sender.clone(),
            conn: best.conn.clone(),
            active: Self::activate(slot, &best.active),
        };
        slot.last_used.store(self.now_secs(), Ordering::Relaxed);
        Some(lease)
    }

    fn activate(slot: &Arc<H2Slot>, conn: &Arc<AtomicUsize>) -> ActiveStream {
        conn.fetch_add(1, Ordering::Relaxed);
        slot.active.fetch_add(1, Ordering::Relaxed);
        ActiveStream {
            slot: Arc::clone(slot),
            conn: Arc::clone(conn),
        }
    }

    /// Add a new HTTP/2 connection to `slot` and lease it.
    ///
    /// Connections negotiated outside the slot's setup lock (HTTP/1 fallback
    /// attempts that got h2) can race past `h2_max_connections_per_host`: at
    /// the cap the new connection is closed and the least-loaded existing one
    /// is leased instead. The flag is `true` in that case.
    pub fn store_h2(
        &self,
        slot: &Arc<H2Slot>,
        sender: H2Sender,
        conn: ConnHandle,
        retire: Option<u64>,
    ) -> (H2Lease, bool) {
        slot.last_used.store(self.now_secs(), Ordering::Relaxed);
        let mut entries = slot.entries.lock();
        // A connection a retry had to avoid (it refused the stream) gives its
        // place to this one; it keeps serving the streams it already has.
        entries.retain(|e| !e.conn.kill.is_cancelled() && Some(e.conn.generation) != retire);
        if entries.len() >= self.cfg.h2_max_connections_per_host.max(1)
            && let Some((best, _)) = pick_h2(entries.iter(), self.cfg.h2_streams_per_connection)
        {
            conn.kill.cancel();
            let lease = H2Lease {
                sender: best.sender.clone(),
                conn: best.conn.clone(),
                active: Self::activate(slot, &best.active),
            };
            return (lease, true);
        }
        let active = Arc::new(AtomicUsize::new(0));
        let lease = H2Lease {
            sender: sender.clone(),
            conn: conn.clone(),
            active: Self::activate(slot, &active),
        };
        entries.push(H2Entry {
            sender,
            conn,
            active,
        });
        (lease, false)
    }

    /// Forget the HTTP/2 connection of generation `generation` for `key`;
    /// its siblings and any newer replacement are left alone.
    pub fn drop_h2(&self, key: &PoolKey, generation: u64) {
        if let Some(slot) = self.h2.get(key) {
            slot.entries
                .lock()
                .retain(|e| e.conn.generation != generation);
        }
    }

    /// Close idle and dead connections.
    pub fn reap(&self) {
        let idle = self.cfg.idle_timeout;
        let mut closed = 0;
        // Nobody drives an idle connection: give each a poll, so one the
        // upstream closed is noticed (and its socket released) here rather
        // than at its next checkout. The no-op waker is replaced by the real
        // one as soon as an exchange polls the connection again.
        let mut cx = Context::from_waker(Waker::noop());
        self.h1.retain(|_, stack| {
            let before = stack.len();
            stack.retain_mut(|idle| {
                idle.conn.drive(&mut cx);
                idle.usable()
            });
            closed += before - stack.len();
            !stack.is_empty()
        });
        self.idle_h1.fetch_sub(closed, Ordering::Relaxed);
        self.enforce_idle_total();
        let now = self.now_secs();
        self.h2.retain(|_, slot| {
            let mut entries = slot.entries.lock();
            let unused =
                now.saturating_sub(slot.last_used.load(Ordering::Relaxed)) >= idle.as_secs();
            entries.retain(|e| {
                !e.conn.kill.is_cancelled() && (!unused || e.active.load(Ordering::Relaxed) > 0)
            });
            let busy = slot.active.load(Ordering::Relaxed) > 0;
            // Keep slots someone still holds (mid-connect or mid-exchange):
            // detaching them would orphan the connection they install.
            !entries.is_empty() || busy || Arc::strong_count(slot) > 1
        });
    }
}

/// Reaper loop; ends when the pool is dropped.
pub async fn reap_loop(pool: std::sync::Weak<Pool>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(pool) = pool.upgrade() else {
            return;
        };
        pool.reap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn h2_sender() -> H2Sender {
        let (client, server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            if let Ok(mut conn) = h2::server::handshake(server).await {
                while conn.accept().await.is_some() {}
            }
        });
        let (tx, conn) = h2::client::handshake(client).await.unwrap();
        tokio::spawn(conn);
        tx
    }

    #[tokio::test]
    async fn store_h2_never_exceeds_the_connection_cap() {
        // Connections negotiated outside the setup lock (HTTP/1 fallback that
        // got h2) all end up in `store_h2`; the cap must still hold.
        let pool = Pool::new(PoolConfig {
            h2_max_connections_per_host: 2,
            ..PoolConfig::default()
        });
        let target = Target::new(([127, 0, 0, 1], 1).into(), "localhost", Transport::Plain);
        let slot = pool.h2_slot(&PoolKey::of(&target));
        let mut handles = Vec::new();
        let mut leases = Vec::new();
        for _ in 0..4 {
            let handle = pool.new_handle();
            handles.push(handle.clone());
            leases.push(pool.store_h2(&slot, h2_sender().await, handle, None));
        }
        assert_eq!(slot.entries.lock().len(), 2);
        let reused: Vec<bool> = leases.iter().map(|(_, reused)| *reused).collect();
        assert_eq!(reused, [false, false, true, true]);
        let killed: Vec<bool> = handles.iter().map(|h| h.kill.is_cancelled()).collect();
        assert_eq!(killed, [false, false, true, true]);
        assert_eq!(slot.active.load(Ordering::Relaxed), 4);
    }

    /// An HTTP/1.1 connection over an in-memory pipe whose other end stays
    /// open (the connection task keeps the sender usable).
    async fn h1_conn(pool: &Pool) -> (H1Conn, tokio::io::DuplexStream) {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (tx, conn) = hyper::client::conn::http1::handshake::<_, ProxyBody>(
            hyper_util::rt::TokioIo::new(client),
        )
        .await
        .unwrap();
        tokio::spawn(conn);
        (H1Conn::detached(tx, pool.new_handle()), server)
    }

    fn key(port: u16) -> PoolKey {
        PoolKey::of(&Target::new(
            ([127, 0, 0, 1], port).into(),
            "localhost",
            Transport::Plain,
        ))
    }

    #[tokio::test(start_paused = true)]
    async fn idle_h1_connections_are_capped_across_hosts() {
        let pool = Pool::new(PoolConfig {
            max_idle_total: 2,
            ..PoolConfig::default()
        });
        let mut pipes = Vec::new();
        let mut handles = Vec::new();
        for port in 1..=3 {
            let (conn, pipe) = h1_conn(&pool).await;
            pipes.push(pipe);
            handles.push(conn.handle.clone());
            pool.checkin_h1(key(port), conn, None);
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        // The oldest idle connection (host 1) was closed to stay at the cap.
        assert_eq!(pool.idle_h1_count(), 2);
        let killed: Vec<bool> = handles.iter().map(|h| h.kill.is_cancelled()).collect();
        assert_eq!(killed, [true, false, false]);
        assert!(pool.checkout_h1(&key(1)).is_none());

        let conn = pool.checkout_h1(&key(2)).expect("host 2 still pooled");
        assert_eq!(pool.idle_h1_count(), 1);
        drop(conn);

        // Expired entries leave the count when the reaper drops them.
        tokio::time::advance(PoolConfig::default().idle_timeout).await;
        pool.reap();
        assert_eq!(pool.idle_h1_count(), 0);
        assert!(handles[2].kill.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn idle_h1_connections_are_capped_per_host() {
        let pool = Pool::new(PoolConfig {
            max_idle_per_host: 2,
            ..PoolConfig::default()
        });
        let mut pipes = Vec::new();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let (conn, pipe) = h1_conn(&pool).await;
            pipes.push(pipe);
            handles.push(conn.handle.clone());
            pool.checkin_h1(key(1), conn, None);
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        // The host's oldest idle connection was closed; other hosts are
        // unaffected by this cap.
        let killed: Vec<bool> = handles.iter().map(|h| h.kill.is_cancelled()).collect();
        assert_eq!(killed, [true, false, false]);
        assert_eq!(pool.idle_h1_count(), 2);
        let (other, _pipe) = h1_conn(&pool).await;
        pool.checkin_h1(key(2), other, None);
        assert_eq!(pool.idle_h1_count(), 3);

        // Most recently used first (warmest connection).
        let newest = pool.checkout_h1(&key(1)).expect("pooled");
        assert_eq!(newest.handle.generation, handles[2].generation);
        let older = pool.checkout_h1(&key(1)).expect("pooled");
        assert_eq!(older.handle.generation, handles[1].generation);
        assert!(pool.checkout_h1(&key(1)).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn keep_alive_timeout_shortens_the_idle_lifetime() {
        let pool = Pool::new(PoolConfig::default());
        // Retired a second before the upstream's announced timeout.
        let (conn, _pipe) = h1_conn(&pool).await;
        pool.checkin_h1(key(1), conn, Some(Duration::from_secs(5)));
        tokio::time::advance(Duration::from_millis(3900)).await;
        assert!(pool.checkout_h1(&key(1)).is_some());

        let (conn, _pipe) = h1_conn(&pool).await;
        pool.checkin_h1(key(1), conn, Some(Duration::from_secs(5)));
        tokio::time::advance(Duration::from_millis(4100)).await;
        assert!(pool.checkout_h1(&key(1)).is_none());

        // A timeout of a second or less leaves no safe idle time at all.
        let (conn, _pipe) = h1_conn(&pool).await;
        let handle = conn.handle.clone();
        pool.checkin_h1(key(1), conn, Some(Duration::from_secs(1)));
        assert_eq!(pool.idle_h1_count(), 0);
        assert!(handle.kill.is_cancelled());
    }

    /// Idle HTTP/1.1 entries actually in the map.
    fn idle_h1_entries(pool: &Pool) -> usize {
        pool.h1.iter().map(|e| e.value().len()).sum()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn idle_cap_holds_under_concurrent_checkins_and_checkouts() {
        let pool = Arc::new(Pool::new(PoolConfig {
            max_idle_total: 3,
            ..PoolConfig::default()
        }));
        let tasks: Vec<_> = (0..8u16)
            .map(|t| {
                let pool = Arc::clone(&pool);
                tokio::spawn(async move {
                    let mut pipes = Vec::new();
                    for i in 0..25u16 {
                        let (conn, pipe) = h1_conn(&pool).await;
                        pipes.push(pipe);
                        pool.checkin_h1(key(1 + (t + i) % 4), conn, None);
                        if i % 3 == 0 {
                            drop(pool.checkout_h1(&key(1 + i % 4)));
                        }
                    }
                    pipes
                })
            })
            .collect();
        let mut pipes = Vec::new();
        for task in tasks {
            pipes.extend(task.await.unwrap());
        }
        assert_eq!(pool.idle_h1_count(), idle_h1_entries(&pool));
        assert!(pool.idle_h1_count() <= 3, "{}", pool.idle_h1_count());
    }

    #[tokio::test]
    async fn only_requests_that_waited_share_a_connect_failure() {
        let pool = Pool::new(PoolConfig::default());
        let slot = pool.h2_slot(&key(1));
        let waited = slot.connect_attempts();
        slot.note_connect::<()>(&Err(ProxyError::new(
            ErrorKind::ConnectTimeout,
            "timed out",
        )));
        let shared = slot
            .failed_after(waited)
            .expect("a waiter gets the failure");
        assert_eq!(shared.kind(), ErrorKind::ConnectTimeout);
        // A request arriving after the failure tries itself, even at the
        // same instant.
        assert!(slot.failed_after(slot.connect_attempts()).is_none());
        // A later success clears it for everyone.
        slot.note_connect::<()>(&Ok(()));
        assert!(slot.failed_after(waited).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn the_reaper_restores_the_idle_cap_after_a_race() {
        // Concurrent check-ins can briefly leave more idle connections than
        // the cap (two evictions racing for the same victim); the next reap
        // brings the pool back under it.
        let pool = Pool::new(PoolConfig {
            max_idle_total: 2,
            ..PoolConfig::default()
        });
        let mut pipes = Vec::new();
        for port in 1..=3 {
            let (conn, pipe) = h1_conn(&pool).await;
            pipes.push(pipe);
            pool.h1.entry(key(port)).or_default().push(IdleH1 {
                conn,
                since: Instant::now(),
                ttl: pool.cfg.idle_timeout,
            });
            pool.idle_h1.fetch_add(1, Ordering::Relaxed);
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        assert_eq!(pool.idle_h1_count(), 3);
        pool.reap();
        assert_eq!(pool.idle_h1_count(), 2);
        assert_eq!(idle_h1_entries(&pool), 2);
        assert!(pool.checkout_h1(&key(1)).is_none(), "the oldest went first");
    }

    /// An HTTP/2 sender whose upstream advertises `max_streams`, once the
    /// client has seen that SETTINGS frame.
    async fn h2_sender_limited(max_streams: u32) -> H2Sender {
        let (client, server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            if let Ok(mut conn) = h2::server::Builder::new()
                .max_concurrent_streams(max_streams)
                .handshake::<_, bytes::Bytes>(server)
                .await
            {
                while conn.accept().await.is_some() {}
            }
        });
        let (tx, conn) = h2::client::handshake(client).await.unwrap();
        tokio::spawn(conn);
        for _ in 0..100 {
            if tx.current_max_send_streams() == max_streams as usize {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(tx.current_max_send_streams(), max_streams as usize);
        tx
    }

    #[tokio::test]
    async fn lease_prefers_a_connection_with_room_over_one_that_takes_no_streams() {
        let pool = Pool::new(PoolConfig {
            h2_max_connections_per_host: 2,
            ..PoolConfig::default()
        });
        let slot = pool.h2_slot(&key(1));
        // A: idle but the upstream allows no streams on it.
        let a = pool.new_handle();
        drop(pool.store_h2(&slot, h2_sender_limited(0).await, a.clone(), None));
        // B: room for more, one exchange running.
        let b = pool.new_handle();
        let busy_b = pool.store_h2(&slot, h2_sender_limited(8).await, b.clone(), None);
        assert!(!busy_b.1);

        let lease = pool.lease(&slot, None).expect("a connection with room");
        assert_eq!(lease.conn.generation, b.generation);
    }
}
