//! Per-host cache of the HTTP version negotiated via ALPN.

use std::time::Duration;

use dashmap::DashMap;
use tokio::time::Instant;

use crate::target::Target;

/// Upper bound on cached hosts. The key space is the set of distinct
/// upstream hosts, typically far below this.
const CAPACITY: usize = 1024;

/// What a host negotiated last time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedProtocol {
    /// The host answered ALPN with `http/1.1` (or no ALPN).
    Http1Only,
    /// The host answered ALPN with `h2`.
    Http2,
}

/// Remembers the negotiated protocol per `scheme://host:port` for `ttl`.
/// A zero `ttl` disables the cache.
pub struct ProtocolCache {
    entries: DashMap<String, (CachedProtocol, Instant)>,
    ttl: Duration,
}

impl ProtocolCache {
    /// A cache keeping entries for `ttl`; a zero `ttl` disables it.
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: DashMap::new(),
            ttl,
        }
    }

    fn enabled(&self) -> bool {
        !self.ttl.is_zero()
    }

    /// The protocol recorded for `target`, unless it expired.
    pub fn get(&self, target: &Target) -> Option<CachedProtocol> {
        if !self.enabled() {
            return None;
        }
        let key = target.protocol_key();
        let hit = self.entries.get(&key).map(|e| *e.value());
        match hit {
            Some((proto, at)) if at.elapsed() < self.ttl => Some(proto),
            Some(_) => {
                self.entries.remove(&key);
                None
            }
            None => None,
        }
    }

    /// Remember `proto` for `target`.
    pub fn record(&self, target: &Target, proto: CachedProtocol) {
        if !self.enabled() {
            return;
        }
        if self.entries.len() >= CAPACITY {
            self.entries.retain(|_, (_, at)| at.elapsed() < self.ttl);
            if self.entries.len() >= CAPACITY {
                // Still full of live entries: drop everything rather than
                // grow; the cache is only a hint.
                self.entries.clear();
            }
        }
        self.entries
            .insert(target.protocol_key(), (proto, Instant::now()));
    }

    /// Forget what was recorded for `target`.
    pub fn evict(&self, target: &Target) {
        if self.enabled() {
            self.entries.remove(&target.protocol_key());
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use crate::target::Transport;

    fn target(host: &str, port: u16) -> Target {
        Target::new(
            SocketAddr::from(([127, 0, 0, 1], port)),
            host,
            Transport::Tls,
        )
    }

    #[tokio::test]
    async fn records_and_returns() {
        let cache = ProtocolCache::new(Duration::from_secs(60));
        let t = target("a.example", 443);
        assert_eq!(cache.get(&t), None);
        cache.record(&t, CachedProtocol::Http2);
        assert_eq!(cache.get(&t), Some(CachedProtocol::Http2));
        cache.record(&t, CachedProtocol::Http1Only);
        assert_eq!(cache.get(&t), Some(CachedProtocol::Http1Only));
    }

    #[tokio::test]
    async fn keyed_by_host_and_port() {
        let cache = ProtocolCache::new(Duration::from_secs(60));
        cache.record(&target("a.example", 443), CachedProtocol::Http2);
        assert_eq!(cache.get(&target("b.example", 443)), None);
        assert_eq!(cache.get(&target("a.example", 8443)), None);
    }

    #[tokio::test]
    async fn evict_forces_renegotiation() {
        let cache = ProtocolCache::new(Duration::from_secs(60));
        let t = target("a.example", 443);
        cache.record(&t, CachedProtocol::Http2);
        cache.evict(&t);
        assert_eq!(cache.get(&t), None);
    }

    #[tokio::test]
    async fn zero_ttl_disables() {
        let cache = ProtocolCache::new(Duration::ZERO);
        let t = target("a.example", 443);
        cache.record(&t, CachedProtocol::Http2);
        assert_eq!(cache.get(&t), None);
    }

    #[tokio::test(start_paused = true)]
    async fn entries_expire() {
        let cache = ProtocolCache::new(Duration::from_secs(10));
        let t = target("a.example", 443);
        cache.record(&t, CachedProtocol::Http2);
        tokio::time::advance(Duration::from_secs(11)).await;
        assert_eq!(cache.get(&t), None);
    }

    #[tokio::test]
    async fn bounded_capacity() {
        let cache = ProtocolCache::new(Duration::from_secs(60));
        for port in 0..u16::try_from(CAPACITY * 2).unwrap() {
            cache.record(&target("h", port), CachedProtocol::Http2);
        }
        assert!(cache.entries.len() <= CAPACITY);
    }
}
