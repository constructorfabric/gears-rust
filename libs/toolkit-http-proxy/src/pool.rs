//! Connection pool.
//!
//! Keyed by everything that makes a connection interchangeable: the socket
//! address, the TLS server name and the transport. Two upstreams that share
//! an IP but present different SNI never share a TLS connection.
//!
//! - HTTP/1.1: a LIFO stack of idle senders per key. A sender is put back
//!   only when the previous response body reached its clean end; a
//!   half-read or failed exchange closes the connection instead.
//! - HTTP/2: one multiplexed sender per key, replaced when closed.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use hyper::client::conn::{http1, http2};
use tokio::time::Instant;

use crate::body::ProxyBody;
use crate::config::PoolConfig;
use crate::target::{Target, Transport};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PoolKey {
    addr: SocketAddr,
    server_name: Arc<str>,
    transport: Transport,
}

impl PoolKey {
    pub(crate) fn of(target: &Target) -> Self {
        Self {
            addr: target.addr(),
            server_name: Arc::from(target.server_name()),
            transport: target.transport(),
        }
    }
}

struct IdleH1 {
    sender: http1::SendRequest<ProxyBody>,
    since: Instant,
}

/// The HTTP/2 connection for one key plus a lock that serialises
/// establishing it, so a burst of first requests opens one connection.
pub(crate) struct H2Slot {
    sender: parking_lot::Mutex<Option<http2::SendRequest<ProxyBody>>>,
    pub(crate) connecting: tokio::sync::Mutex<()>,
    last_used: AtomicU64,
}

pub(crate) struct Pool {
    h1: DashMap<PoolKey, Vec<IdleH1>>,
    h2: DashMap<PoolKey, Arc<H2Slot>>,
    cfg: PoolConfig,
    epoch: Instant,
}

impl Pool {
    pub(crate) fn new(cfg: PoolConfig) -> Self {
        Self {
            h1: DashMap::new(),
            h2: DashMap::new(),
            cfg,
            epoch: Instant::now(),
        }
    }

    fn now_secs(&self) -> u64 {
        self.epoch.elapsed().as_secs()
    }

    /// Most recently idled HTTP/1.1 sender that is still open. It may need
    /// a moment to become ready (see `ProxyClient::checkout_h1`).
    pub(crate) fn checkout_h1(&self, key: &PoolKey) -> Option<http1::SendRequest<ProxyBody>> {
        let mut stack = self.h1.get_mut(key)?;
        while let Some(idle) = stack.pop() {
            if !idle.sender.is_closed() && idle.since.elapsed() < self.cfg.idle_timeout {
                return Some(idle.sender);
            }
        }
        None
    }

    /// Park a sender whose previous response body was read to the end.
    pub(crate) fn checkin_h1(&self, key: PoolKey, sender: http1::SendRequest<ProxyBody>) {
        if sender.is_closed() || self.cfg.max_idle_per_host == 0 {
            return;
        }
        let mut stack = self.h1.entry(key).or_default();
        if stack.len() >= self.cfg.max_idle_per_host {
            // Drop the oldest; the newest is the warmest.
            stack.remove(0);
        }
        stack.push(IdleH1 {
            sender,
            since: Instant::now(),
        });
    }

    pub(crate) fn h2_slot(&self, key: &PoolKey) -> Arc<H2Slot> {
        if let Some(slot) = self.h2.get(key) {
            return Arc::clone(&slot);
        }
        Arc::clone(&self.h2.entry(key.clone()).or_insert_with(|| {
            Arc::new(H2Slot {
                sender: parking_lot::Mutex::new(None),
                connecting: tokio::sync::Mutex::new(()),
                last_used: AtomicU64::new(0),
            })
        }))
    }

    /// Live HTTP/2 sender for `key`, if any.
    pub(crate) fn live_h2(&self, key: &PoolKey) -> Option<http2::SendRequest<ProxyBody>> {
        let slot = self.h2.get(key)?;
        self.slot_sender(&slot)
    }

    pub(crate) fn slot_sender(&self, slot: &H2Slot) -> Option<http2::SendRequest<ProxyBody>> {
        let mut guard = slot.sender.lock();
        match guard.as_ref() {
            Some(sender) if !sender.is_closed() => {
                slot.last_used.store(self.now_secs(), Ordering::Relaxed);
                Some(sender.clone())
            }
            Some(_) => {
                *guard = None;
                None
            }
            None => None,
        }
    }

    pub(crate) fn store_h2(&self, slot: &H2Slot, sender: http2::SendRequest<ProxyBody>) {
        slot.last_used.store(self.now_secs(), Ordering::Relaxed);
        *slot.sender.lock() = Some(sender);
    }

    /// Forget the HTTP/2 connection for `key` (after a protocol error), so
    /// the next request opens a fresh one.
    pub(crate) fn drop_h2(&self, key: &PoolKey) {
        if let Some(slot) = self.h2.get(key) {
            *slot.sender.lock() = None;
        }
    }

    /// Close idle and dead connections.
    pub(crate) fn reap(&self) {
        let idle = self.cfg.idle_timeout;
        self.h1.retain(|_, stack| {
            stack.retain(|c| !c.sender.is_closed() && c.since.elapsed() < idle);
            !stack.is_empty()
        });
        let now = self.now_secs();
        self.h2.retain(|_, slot| {
            let mut guard = slot.sender.lock();
            let unused = now.saturating_sub(slot.last_used.load(Ordering::Relaxed)) >= idle.as_secs();
            if guard.as_ref().is_some_and(|s| s.is_closed() || unused) {
                *guard = None;
            }
            // Keep slots that are mid-connect.
            guard.is_some() || slot.connecting.try_lock().is_err()
        });
    }
}

/// Reaper loop; ends when the pool is dropped.
pub(crate) async fn reap_loop(pool: std::sync::Weak<Pool>, every: Duration) {
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
