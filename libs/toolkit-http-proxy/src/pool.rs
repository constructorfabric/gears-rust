//! Connection pool.
//!
//! Keyed by everything that makes a connection interchangeable: the socket
//! address, the TLS server name and the transport. Two upstreams that share
//! an IP but present different SNI never share a TLS connection.
//!
//! - HTTP/1.1: a LIFO stack of idle connections per key. A connection is put
//!   back only when the previous response body reached its clean end; a
//!   half-read or failed exchange closes the connection instead.
//! - HTTP/2: one multiplexed connection per key. Each connection carries a
//!   generation number, so a failure observed on an old connection never
//!   evicts its replacement, and an active-stream count, so the reaper never
//!   detaches a connection that is still serving responses.
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

struct IdleH1 {
    sender: http1::SendRequest<ProxyBody>,
    conn: ConnHandle,
    since: Instant,
}

struct H2Entry {
    sender: H2Sender,
    conn: ConnHandle,
}

/// The HTTP/2 connection for one key plus a lock that serialises
/// establishing it, so a burst of first requests opens one connection.
pub struct H2Slot {
    entry: parking_lot::Mutex<Option<H2Entry>>,
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

/// Counts one in-flight exchange on an HTTP/2 slot until dropped.
pub struct ActiveStream(Arc<H2Slot>);

impl Drop for ActiveStream {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
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
    pub fn checkout_h1(
        &self,
        key: &PoolKey,
    ) -> Option<(http1::SendRequest<ProxyBody>, ConnHandle)> {
        let mut stack = self.h1.get_mut(key)?;
        while let Some(idle) = stack.pop() {
            if !idle.sender.is_closed()
                && !idle.conn.kill.is_cancelled()
                && idle.since.elapsed() < self.cfg.idle_timeout
            {
                return Some((idle.sender, idle.conn));
            }
        }
        None
    }

    /// Park a connection whose previous response body was read to the end.
    pub fn checkin_h1(
        &self,
        key: PoolKey,
        sender: http1::SendRequest<ProxyBody>,
        conn: ConnHandle,
    ) {
        if sender.is_closed() || conn.kill.is_cancelled() || self.cfg.max_idle_per_host == 0 {
            return;
        }
        let mut stack = self.h1.entry(key).or_default();
        if stack.len() >= self.cfg.max_idle_per_host {
            // Drop the oldest; the newest is the warmest.
            stack.remove(0);
        }
        stack.push(IdleH1 {
            sender,
            conn,
            since: Instant::now(),
        });
    }

    pub fn h2_slot(&self, key: &PoolKey) -> Arc<H2Slot> {
        if let Some(slot) = self.h2.get(key) {
            return Arc::clone(&slot);
        }
        Arc::clone(&self.h2.entry(key.clone()).or_insert_with(|| {
            Arc::new(H2Slot {
                entry: parking_lot::Mutex::new(None),
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

    /// Lease the slot's live connection unless it is generation `not_gen`
    /// (the connection a retry just failed on).
    pub fn lease(&self, slot: &Arc<H2Slot>, not_gen: Option<u64>) -> Option<H2Lease> {
        let mut guard = slot.entry.lock();
        let entry = guard.as_ref()?;
        if entry.conn.kill.is_cancelled() {
            *guard = None;
            return None;
        }
        if not_gen == Some(entry.conn.generation) {
            return None;
        }
        slot.last_used.store(self.now_secs(), Ordering::Relaxed);
        slot.active.fetch_add(1, Ordering::Relaxed);
        Some(H2Lease {
            sender: entry.sender.clone(),
            conn: entry.conn.clone(),
            active: ActiveStream(Arc::clone(slot)),
        })
    }

    /// Install a new HTTP/2 connection in `slot` and lease it.
    pub fn store_h2(&self, slot: &Arc<H2Slot>, sender: H2Sender, conn: ConnHandle) -> H2Lease {
        slot.last_used.store(self.now_secs(), Ordering::Relaxed);
        slot.active.fetch_add(1, Ordering::Relaxed);
        *slot.entry.lock() = Some(H2Entry {
            sender: sender.clone(),
            conn: conn.clone(),
        });
        H2Lease {
            sender,
            conn,
            active: ActiveStream(Arc::clone(slot)),
        }
    }

    /// Forget the HTTP/2 connection for `key` if it is still generation
    /// `generation`; a newer replacement is left alone.
    pub fn drop_h2(&self, key: &PoolKey, generation: u64) {
        if let Some(slot) = self.h2.get(key) {
            let mut guard = slot.entry.lock();
            if guard
                .as_ref()
                .is_some_and(|e| e.conn.generation == generation)
            {
                *guard = None;
            }
        }
    }

    /// Close idle and dead connections.
    pub fn reap(&self) {
        let idle = self.cfg.idle_timeout;
        self.h1.retain(|_, stack| {
            stack.retain(|c| {
                !c.sender.is_closed() && !c.conn.kill.is_cancelled() && c.since.elapsed() < idle
            });
            !stack.is_empty()
        });
        let now = self.now_secs();
        self.h2.retain(|_, slot| {
            let mut guard = slot.entry.lock();
            let busy = slot.active.load(Ordering::Relaxed) > 0;
            let unused =
                now.saturating_sub(slot.last_used.load(Ordering::Relaxed)) >= idle.as_secs();
            if guard
                .as_ref()
                .is_some_and(|e| e.conn.kill.is_cancelled() || (unused && !busy))
            {
                *guard = None;
            }
            // Keep slots someone still holds (mid-connect or mid-exchange):
            // detaching them would orphan the connection they install.
            guard.is_some() || busy || Arc::strong_count(slot) > 1
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
