---
description: "OAGW forwards upstream traffic through the in-house toolkit-http-proxy engine instead of Pingora's in-memory duplex bridge."
status: proposed
date: 2026-10-25
---

# Upstream Proxy Engine — toolkit-http-proxy Instead of the Pingora Bridge

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Forwarding](#forwarding)
  - [Endpoint Selection](#endpoint-selection)
  - [Error Mapping](#error-mapping)
  - [WebSocket](#websocket)
  - [OAuth2 Token Cache](#oauth2-token-cache)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [toolkit-http-proxy engine](#toolkit-http-proxy-engine)
  - [Keep the Pingora bridge](#keep-the-pingora-bridge)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-upstream-proxy-engine`

## Context and Problem Statement

The data plane forwarded each request into Pingora over an in-memory `tokio::io::duplex` pipe: the request was serialized to HTTP/1.1 text with routing context in `x-oagw-internal-*` headers, Pingora's `ProxyHttp` hooks picked the peer, filtered headers and mapped errors, and OAGW parsed the HTTP/1.1 response back from the pipe. Pingora blocks the FIPS dependency policy and pins tokio, and the bridge costs two serializations and a spawned task per request while dropping trailers. The platform decided to replace Pingora with a shared engine ([toolkit ADR-0004](../../../../../docs/adrs/toolkit/0004-toolkit-http-proxy-engine.md)). This ADR records how OAGW uses it.

## Decision Drivers

* Keep OAGW's externally visible behaviour: status codes, error types, `X-OAGW-Error-Source`, retry semantics (scenario 12.6), SSE and WebSocket behaviour, SSRF policy.
* Keep the gear's pipeline (auth, guards, transforms, rate limits) in OAGW; the engine only moves bytes.
* Remove every Pingora type and the bridge from the gear.

## Considered Options

* Forward through the `toolkit-http-proxy` engine
* Keep the Pingora bridge

## Decision Outcome

Chosen option: "Forward through the `toolkit-http-proxy` engine".

### Forwarding

`infra/proxy/engine.rs` builds one `ProxyClient` per gear (connect timeout 10 s, read timeout `proxy_timeout_secs`, idle connections 90 s, 128 idle connections per host, ALPN cache TTL `protocol_cache_ttl_secs`). `DataPlaneServiceImpl::forward` hands the request to it directly: buffered bodies go out with `Content-Length`, streaming bodies chunk by chunk with `max_body_size` enforced on the fly (413 on breach, `DownstreamError` when the client's stream fails; the upstream request is aborted without a terminating chunk). `request_timeout` bounds target resolution and the time to the response head together. As with the Pingora bridge, a streamed body that fails or breaches the limit after the upstream already answered cannot change that answer: the upload is aborted and the response is relayed (or fails mid-stream if the upstream stops). The response body streams back to the response pipeline; SSE responses keep the idle/shutdown lifecycle wrapper. Upstream responses lose every `x-oagw-*` header before the pipeline sees them, so an upstream can never claim `X-OAGW-Error-Source: gateway`.

`https`, `wss` and `wt` endpoints use TLS; `wss` is HTTP/1.1 only; `http` and `grpc` are cleartext HTTP/1.1.

The internal context headers (`x-oagw-internal-*`) are gone; inbound `x-oagw-*` headers are still stripped.

### Endpoint Selection

`PoolEndpointSelector` replaces `PingoraEndpointSelector` with the same contract ([ADR-0018](./0018-optional-persistence.md), "Endpoint Pool Freshness Across Replicas"): one `BackendPool` per multi-endpoint upstream, rebuilt when the endpoint list changes, dropped after 10 minutes unused (swept every minute), DNS refreshed every 30 s, TCP health checks every 10 s (the first one immediately), round-robin over healthy members, every resolved address checked by the SSRF policy. Single-endpoint and `X-OAGW-Target-Host` selection resolve DNS on the request path with retries (100/500/2500 ms) and connect to the first address the SSRF policy allows.

### Error Mapping

| Engine error kind | `DomainError` |
|---|---|
| `ConnectTimeout` | `ConnectionTimeout` |
| `ReadTimeout` | `RequestTimeout` |
| `Http2` | `ProtocolError` (the engine also evicts the host's ALPN hint) |
| `Read`, `Write` | `StreamAborted` |
| `ConnectNoRoute`, `Connect` | `LinkUnavailable` |
| `ConnectRefused`, `ConnectionClosed`, `TlsHandshake`, `TlsHandshakeTimeout`, `InvalidCertificate`, `Protocol` | `DownstreamError` |
| `InvalidRequest` | `Validation` |
| `Configuration` | `Internal` |

This is the former `fail_to_proxy` table. Engine failures now return through the normal `DomainError` path (so transform `on_error` plugins run for them, which they did not with Pingora-written responses); the REST layer writes the RFC 9457 body with `X-OAGW-Error-Source: gateway`.

A request is retried at most once, only when it never left a stale pooled connection; nothing that may have reached the upstream is replayed.

### WebSocket

`ProxyClient::upgrade` opens a dedicated HTTP/1.1 connection, validates the handshake (`GET`, `Upgrade: websocket`, no body) and hands out hyper's upgraded stream only after a `101` with `Upgrade: websocket`; any other answer becomes `ProtocolError` ("upstream rejected WebSocket upgrade"). The relay (`websocket.rs`) is unchanged and now runs over `TokioIo<Upgraded>` instead of the duplex pipe.

### OAuth2 Token Cache

`pingora-memory-cache` is replaced by `infra/ttl_cache.rs`: a bounded, sharded LRU (`lru` crate) with per-entry TTL. Semantics kept from [ADR-0016](./0016-oauth2-client-credentials-auth-plugin.md): capacity-bounded, per-entry TTL, a zero TTL is not cached, entries are dropped (and zeroized) on eviction. TinyUFO admission is not reproduced; with capacity well above the number of distinct client credentials, LRU behaves the same.

### Consequences

* OAGW depends on `toolkit-http-proxy` and `lru`; the `fips` feature forwards to `toolkit-http-proxy/fips`, `test-utils` to `toolkit-http-proxy/test-utils`.
* `session_bridge.rs` and `pingora_proxy.rs` are removed; `streaming.rs`, `engine.rs` and `endpoint_selector.rs` replace them.
* Feature 0004's bridge algorithm and Pingora DoD are replaced (`cpt-cf-oagw-algo-upstream-forwarding`, `cpt-cf-oagw-dod-upstream-proxy-engine`).

### Confirmation

* `cargo nextest run -p cf-gears-oagw`: the existing unit and integration suites (`proxy_integration`, `e2e_http2_test`, `e2e_smoke_test`, WebSocket) pass without changes to their assertions.
* `make e2e-local SUITE=oagw` and `make e2e-oagw-sqlite`.
* `cargo bench -p cf-gears-oagw --bench proxy_roundtrip` against the Pingora baseline.

## Pros and Cons of the Options

### toolkit-http-proxy engine

* Good, because FIPS-hardened TLS, no tokio pin, no per-request bridge.
* Good, because errors are typed end to end instead of round-tripping through a problem body on the pipe.
* Bad, because the gear depends on an in-house engine (owned and tested in `libs/`).

### Keep the Pingora bridge

* Good, because no migration.
* Bad, because every driver of toolkit ADR-0004 remains unaddressed.

## More Information

Benchmarks and the security regression matrix: [toolkit ADR-0004](../../../../../docs/adrs/toolkit/0004-toolkit-http-proxy-engine.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-nfr-low-latency` — removes the per-request serialization bridge and adds HTTP/2 multiplexing.
* `cpt-cf-oagw-dod-upstream-proxy-engine` — defines the engine contract the proxy feature implements.
* `cpt-cf-oagw-adr-optional-persistence` — keeps its endpoint-pool freshness rule on the new selector.
* `cpt-cf-oagw-adr-oauth2-client-credentials-auth-plugin` — replaces the token cache implementation, keeping its semantics.
