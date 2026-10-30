//! Name resolution with retry.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;

/// Resolves a host name to socket addresses.
#[async_trait]
pub trait Resolver: Send + Sync {
    /// Resolve `host:port`. IP literals must resolve to themselves.
    ///
    /// # Errors
    ///
    /// The resolver's error after its own retries.
    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

/// System resolver (`getaddrinfo` on a blocking thread) with retries for
/// transient failures, common in container DNS under load.
#[derive(Debug, Clone)]
pub struct DnsResolver {
    /// Delays before each retry; the number of entries is the retry count.
    pub backoff: Vec<Duration>,
}

impl Default for DnsResolver {
    /// Three retries after 100 ms, 500 ms and 2.5 s.
    fn default() -> Self {
        Self {
            backoff: vec![
                Duration::from_millis(100),
                Duration::from_millis(500),
                Duration::from_millis(2500),
            ],
        }
    }
}

#[async_trait]
impl Resolver for DnsResolver {
    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let literal = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = literal.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let mut delays = self.backoff.iter();
        let mut attempt = 1u32;
        loop {
            match tokio::net::lookup_host((host, port)).await {
                Ok(addrs) => return Ok(addrs.collect()),
                Err(e) => match delays.next() {
                    Some(delay) => {
                        tracing::debug!(host, port, attempt, error = %e, "DNS lookup failed, retrying");
                        tokio::time::sleep(*delay).await;
                        attempt += 1;
                    }
                    None => return Err(e),
                },
            }
        }
    }
}
