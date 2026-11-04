//! Where a request goes.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

/// Transport security for a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    /// Cleartext TCP.
    Plain,
    /// TLS over TCP.
    Tls,
}

/// Which HTTP versions the engine may use for a target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProtocolPolicy {
    /// TLS: offer `h2` and `http/1.1` via ALPN, guided by the per-host
    /// protocol cache. Cleartext: HTTP/1.1.
    #[default]
    Auto,
    /// HTTP/1.1 only (e.g. WebSocket upgrades).
    Http1Only,
    /// HTTP/2 only: ALPN `h2` over TLS, prior knowledge over cleartext.
    Http2Only,
}

/// An upstream endpoint with an already resolved address.
///
/// The engine connects to [`Target::addr`] as is and never resolves
/// [`Target::server_name`]; callers that need SSRF protection filter the
/// address before building the target.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Target {
    addr: SocketAddr,
    server_name: Arc<str>,
    port: u16,
    transport: Transport,
    policy: ProtocolPolicy,
}

impl Target {
    /// Target `addr`, presenting `server_name` for SNI, certificate
    /// verification and the default `Host`/`:authority`. The authority port
    /// defaults to `addr`'s port.
    pub fn new(addr: SocketAddr, server_name: impl Into<Arc<str>>, transport: Transport) -> Self {
        Self {
            addr,
            server_name: server_name.into(),
            port: addr.port(),
            transport,
            policy: ProtocolPolicy::Auto,
        }
    }

    /// Restrict the HTTP versions used for this target.
    #[must_use]
    pub fn with_policy(mut self, policy: ProtocolPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Override the port used in the default authority (when the connect
    /// address differs from the logical endpoint, e.g. behind a NAT).
    #[must_use]
    pub fn with_authority_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Socket address the engine connects to.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Host name (or IP literal) used for SNI and the default authority.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Transport security.
    #[must_use]
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// Protocol policy.
    #[must_use]
    pub fn policy(&self) -> ProtocolPolicy {
        self.policy
    }

    /// URI scheme matching the transport.
    #[must_use]
    pub fn scheme(&self) -> &'static str {
        match self.transport {
            Transport::Plain => "http",
            Transport::Tls => "https",
        }
    }

    /// Default authority: the server name, plus the port unless it is the
    /// scheme default. IPv6 literals are bracketed.
    #[must_use]
    pub fn authority(&self) -> String {
        let host = if self.server_name.contains(':') && !self.server_name.starts_with('[') {
            format!("[{}]", self.server_name)
        } else {
            self.server_name.to_string()
        };
        let default_port = match self.transport {
            Transport::Plain => 80,
            Transport::Tls => 443,
        };
        if self.port == default_port {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    /// Key of the per-host protocol cache: `scheme://server_name:port`.
    pub(crate) fn protocol_key(&self) -> String {
        format!("{}://{}:{}", self.scheme(), self.server_name, self.port)
    }
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("addr", &self.addr)
            .field("server_name", &self.server_name)
            .field("port", &self.port)
            .field("transport", &self.transport)
            .field("policy", &self.policy)
            .finish()
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{} ({})",
            self.scheme(),
            self.authority(),
            self.addr
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn authority_omits_default_port() {
        assert_eq!(
            Target::new(addr(443), "api.example.com", Transport::Tls).authority(),
            "api.example.com"
        );
        assert_eq!(
            Target::new(addr(80), "api.example.com", Transport::Plain).authority(),
            "api.example.com"
        );
        assert_eq!(
            Target::new(addr(8443), "api.example.com", Transport::Tls).authority(),
            "api.example.com:8443"
        );
    }

    #[test]
    fn authority_brackets_ipv6() {
        let t = Target::new(
            SocketAddr::from(([0u16, 0, 0, 0, 0, 0, 0, 1], 8080)),
            "::1",
            Transport::Plain,
        );
        assert_eq!(t.authority(), "[::1]:8080");
    }

    #[test]
    fn authority_port_override() {
        let t = Target::new(addr(10443), "svc", Transport::Tls).with_authority_port(443);
        assert_eq!(t.authority(), "svc");
        assert_eq!(t.protocol_key(), "https://svc:443");
    }
}
