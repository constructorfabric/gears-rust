//! Backend selection: DNS discovery, address filtering, health checks and
//! load balancing.
//!
//! A [`BackendPool`] turns a list of logical endpoints ([`Seed`]s) into a
//! live set of resolved, filtered socket addresses, keeps it fresh in the
//! background (DNS refresh and active health checks) and picks one member
//! per request through a [`Balancer`]. Every piece is a trait so gateways
//! can plug in their own resolver, SSRF policy, probe or strategy.

mod pool;
mod resolve;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

pub use pool::{BackendPool, BackendPoolOptions, Member, Seed, SelectError};
pub use resolve::{DnsResolver, Resolver};

/// Decides whether a resolved address may be connected to (SSRF policy).
pub trait AddressFilter: Send + Sync {
    /// `Err(reason)` blocks `addr`, which `host` resolved to. The host lets a
    /// policy exempt a name it trusts.
    ///
    /// # Errors
    ///
    /// Returns the reason when the address must not be used.
    fn check<'a>(&'a self, host: &str, addr: &SocketAddr) -> Result<(), &'a str>;
}

/// Filter that allows every address.
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAll;

impl AddressFilter for AllowAll {
    fn check<'a>(&'a self, _host: &str, _addr: &SocketAddr) -> Result<(), &'a str> {
        Ok(())
    }
}

/// Active health probe for one backend address.
#[async_trait]
pub trait HealthCheck: Send + Sync {
    /// Whether `addr` is healthy.
    async fn check(&self, addr: SocketAddr) -> bool;
}

/// Healthy when a TCP connection can be opened within `timeout`.
#[derive(Debug, Clone, Copy)]
pub struct TcpHealthCheck {
    /// Connect budget for a probe.
    pub timeout: Duration,
}

impl Default for TcpHealthCheck {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(1),
        }
    }
}

#[async_trait]
impl HealthCheck for TcpHealthCheck {
    async fn check(&self, addr: SocketAddr) -> bool {
        matches!(
            tokio::time::timeout(self.timeout, tokio::net::TcpStream::connect(addr)).await,
            Ok(Ok(_))
        )
    }
}

/// Load-balancing strategy over the current member list.
pub trait Balancer: Send + Sync {
    /// Index of the member to use, considering only healthy ones; `None`
    /// when no member is healthy.
    fn pick(&self, members: &[Member]) -> Option<usize>;
}

/// Round-robin over the healthy members: each healthy member gets an equal
/// share, however the unhealthy ones are placed.
#[derive(Debug, Default)]
pub struct RoundRobin {
    next: AtomicUsize,
}

impl Balancer for RoundRobin {
    fn pick(&self, members: &[Member]) -> Option<usize> {
        let healthy = members.iter().filter(|m| m.is_healthy()).count();
        self.pick_among(members, healthy)
    }
}

impl RoundRobin {
    /// Second pass of [`RoundRobin::pick`], given the healthy count of the
    /// first one.
    fn pick_among(&self, members: &[Member], healthy: usize) -> Option<usize> {
        if healthy == 0 {
            return None;
        }
        let nth = self.next.fetch_add(1, Ordering::Relaxed) % healthy;
        let mut healthy = members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_healthy())
            .map(|(i, _)| i);
        // Health may flip between the two passes; a member that went down
        // in between must not turn into "no healthy member".
        healthy.clone().nth(nth).or_else(|| healthy.next())
    }
}

#[cfg(test)]
#[path = "balance_tests.rs"]
mod balance_tests;
