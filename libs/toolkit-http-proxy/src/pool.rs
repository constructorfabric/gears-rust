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
//! Every connection task can be killed through its [`ConnHandle`]; the
//! engine uses that to tear down connections whose request upload stalled.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use hyper::client::conn::http1;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::body::ProxyBody;
use crate::config::PoolConfig;
use crate::http2::H2Sender;
use crate::target::{Target, Transport};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    addr: SocketAddr,
    server_name: Arc<str>,
    transport: Transport,
}

impl PoolKey {
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
    pub generation: u64,
    pub kill: CancellationToken,
}

/// Sole owner of an HTTP/1.1 connection, from its handshake until it is
/// idle in the pool, leased to one exchange, or waiting for its response
/// body to end. Dropping it closes the connection: whoever loses it (expiry,
/// eviction, a timed-out or cancelled exchange, an abandoned or ambiguously
/// framed response) can never leave hyper's connection task running on its
/// own, e.g. still polling a request body the upstream answered early.
pub struct H1Conn {
    pub sender: http1::SendRequest<ProxyBody>,
    pub handle: ConnHandle,
}

impl H1Conn {
    fn is_open(&self) -> bool {
        !self.sender.is_closed() && !self.handle.kill.is_cancelled()
    }
}

impl Drop for H1Conn {
    fn drop(&mut self) {
        self.handle.kill.cancel();
    }
}

struct IdleH1 {
    conn: H1Conn,
    since: Instant,
}

impl IdleH1 {
    fn usable(&self, idle_timeout: Duration) -> bool {
        self.conn.is_open() && self.since.elapsed() < idle_timeout
    }
}

struct H2Entry {
    sender: H2Sender,
    conn: ConnHandle,
    active: Arc<AtomicUsize>,
}

/// The HTTP/2 connections for one key plus a lock that serialises
/// establishing them, so a burst of first requests opens one connection.
pub struct H2Slot {
    entries: parking_lot::Mutex<Vec<H2Entry>>,
    pub connecting: tokio::sync::Mutex<()>,
    last_used: AtomicU64,
    active: AtomicUsize,
    /// The last negotiation on this key ended on HTTP/1.1: don't hold
    /// `connecting` across further HTTP/1.1 connects (works with the
    /// protocol cache disabled too).
    h1_hint: AtomicBool,
}

impl H2Slot {
    pub fn h1_hint(&self) -> bool {
        self.h1_hint.load(Ordering::Relaxed)
    }

    pub fn set_h1_hint(&self, h1: bool) {
        self.h1_hint.store(h1, Ordering::Relaxed);
    }
}

/// An HTTP/2 sender checked out for one exchange.
pub struct H2Lease {
    pub sender: H2Sender,
    pub conn: ConnHandle,
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

pub struct Pool {
    h1: DashMap<PoolKey, Vec<IdleH1>>,
    h2: DashMap<PoolKey, Arc<H2Slot>>,
    cfg: PoolConfig,
    epoch: Instant,
    generations: AtomicU64,
}

impl Pool {
    pub fn new(cfg: PoolConfig) -> Self {
        Self {
            h1: DashMap::new(),
            h2: DashMap::new(),
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
            kill: CancellationToken::new(),
        }
    }

    /// Most recently idled HTTP/1.1 connection that is still open. It may
    /// need a moment to become ready (see `ProxyClient::checkout_h1`).
    pub fn checkout_h1(&self, key: &PoolKey) -> Option<H1Conn> {
        let mut stack = self.h1.get_mut(key)?;
        while let Some(idle) = stack.pop() {
            if idle.usable(self.cfg.idle_timeout) {
                return Some(idle.conn);
            }
        }
        None
    }

    /// Park a connection whose previous response body was read to the end.
    pub fn checkin_h1(&self, key: PoolKey, conn: H1Conn) {
        let idle = IdleH1 {
            conn,
            since: Instant::now(),
        };
        if !idle.usable(self.cfg.idle_timeout) || self.cfg.max_idle_per_host == 0 {
            return;
        }
        let mut stack = self.h1.entry(key).or_default();
        if stack.len() >= self.cfg.max_idle_per_host {
            // Drop the oldest; the newest is the warmest.
            stack.remove(0);
        }
        stack.push(idle);
    }

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
        let best = entries
            .iter()
            .filter(|e| not_gen != Some(e.conn.generation))
            .min_by_key(|e| e.active.load(Ordering::Relaxed))?;
        let saturated = best.active.load(Ordering::Relaxed) >= self.cfg.h2_streams_per_connection;
        if saturated && entries.len() < self.cfg.h2_max_connections_per_host {
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
    ) -> (H2Lease, bool) {
        slot.last_used.store(self.now_secs(), Ordering::Relaxed);
        let mut entries = slot.entries.lock();
        entries.retain(|e| !e.conn.kill.is_cancelled());
        if entries.len() >= self.cfg.h2_max_connections_per_host.max(1)
            && let Some(best) = entries
                .iter()
                .min_by_key(|e| e.active.load(Ordering::Relaxed))
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
        self.h1.retain(|_, stack| {
            stack.retain(|c| c.usable(idle));
            !stack.is_empty()
        });
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
            leases.push(pool.store_h2(&slot, h2_sender().await, handle));
        }
        assert_eq!(slot.entries.lock().len(), 2);
        let reused: Vec<bool> = leases.iter().map(|(_, reused)| *reused).collect();
        assert_eq!(reused, [false, false, true, true]);
        let killed: Vec<bool> = handles.iter().map(|h| h.kill.is_cancelled()).collect();
        assert_eq!(killed, [false, false, true, true]);
        assert_eq!(slot.active.load(Ordering::Relaxed), 4);
    }
}
