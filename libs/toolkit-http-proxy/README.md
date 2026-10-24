# toolkit-http-proxy

Upstream proxy engine for ToolKit gateways (OAGW, the event-broker
dispatcher). It replaces Cloudflare Pingora; see
[ADR toolkit/0004](../../docs/adrs/toolkit/0004-toolkit-http-proxy-engine.md).

- `ProxyClient::send` / `ProxyClient::upgrade`: pooled HTTP/1.1 (hyper) and
  HTTP/2 (`h2`) with ALPN and a per-host protocol hint cache, FIPS-hardened
  TLS from `toolkit-http`, retry only for requests that never left a stale
  connection, WebSocket tunnels only after `101`.
- `sanitize`: outbound framing and hop-by-hop hygiene (request smuggling
  defences) and response header sanitization.
- `balance`: `BackendPool` with pluggable `Resolver`, `AddressFilter` (SSRF),
  `HealthCheck` and `Balancer`.
- Timeouts: `connect_timeout`, `read_timeout` (response head and each body
  frame), `write_timeout` (socket write stalls sever the connection; an
  HTTP/2 stream whose flow-control window stays closed is reset).

The engine never resolves host names: a `Target` carries the resolved
address plus the server name used for SNI and the authority.

```rust
use toolkit_http_proxy::{ProxyClient, ProxyClientConfig, Target, Transport, empty_body};

let client = ProxyClient::new(ProxyClientConfig::default())?;
let target = Target::new("203.0.113.10:443".parse()?, "api.example.com", Transport::Tls);
let resp = client
    .send(&target, http::Request::get("/v1/models").body(empty_body())?)
    .await?;
```

Security regression tests live in `tests/security.rs`; FIPS checks in
`tests/fips.rs` (`--features fips`).
