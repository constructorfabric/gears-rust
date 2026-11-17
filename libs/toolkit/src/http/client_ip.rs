//! The caller's address as the edge resolved it.

use std::net::IpAddr;

/// Request extension carrying the client address the API gateway resolved
/// from the connection (and, behind trusted proxies, from their forwarding
/// headers).
///
/// Only the gateway inserts it; handlers read it instead of parsing
/// `X-Forwarded-For` / `X-Real-IP`, which the client controls. A request that
/// did not come through the gateway (an in-process client call) has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientIp(pub IpAddr);
