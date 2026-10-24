---
description: "Replace Cloudflare Pingora with an in-house hyper-based upstream proxy engine (`toolkit-http-proxy`) shared by OAGW, the event-broker dispatcher and future gateways."
status: proposed
date: 2026-10-25
---

# Build an in-house upstream proxy engine on hyper instead of using Pingora

**ID**: `cpt-toolkit-adr-http-proxy-engine`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Engine Shape](#engine-shape)
  - [Security Properties](#security-properties)
  - [FIPS 140-3](#fips-140-3)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep Pingora](#keep-pingora)
  - [Fork Pingora](#fork-pingora)
  - [hyper-util's legacy Client](#hyper-utils-legacy-client)
  - [In-house engine on hyper::client::conn](#in-house-engine-on-hyperclientconn)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

OAGW forwarded every proxied request through Cloudflare Pingora 0.8, and the event-broker dispatcher used Pingora's HTTP/1 connector. OAGW never ran Pingora as a server. It spawned Pingora's `HttpProxy` per request on an in-memory `tokio::io::duplex` pipe: the request was serialized to HTTP/1.1 text, Pingora parsed it back, proxied it, and wrote the response as HTTP/1.1 text again, which a hand-written `httparse` + chunked decoder parsed once more. Request context travelled in `x-oagw-internal-*` headers. The parts of Pingora actually used were small: the upstream connector (pool, ALPN, TLS), `LoadBalancer<RoundRobin>` with `TcpHealthCheck`, the `ProxyHttp` hooks for header filtering and error mapping, and `pingora-memory-cache`.

That dependency cost more than it gave:

- **FIPS 140-3.** `pingora-rustls` builds its own TLS configuration outside `toolkit-http`, pulls `ring` into the graph, and is not governed by the workspace `fips` feature (cloudflare/pingora#887 is still open). It is one of the blockers of [FIPS ADR-0005](../../security/fips/adrs/0005-fips-dependency-policy.md) Phase B.
- **Runtime pin.** Every released `pingora-core` builds a zero-capacity `BufStream`, which tokio 1.54 rejects (tokio-rs/tokio#8494). The whole workspace was pinned to `tokio <1.54` until cloudflare/pingora#1060 ships.
- **Maintenance latency.** Fixes we need (the DNS `unwrap` panic in `HttpPeer::new`, pingora#570; the pin above) wait on upstream releases.
- **The bridge.** Two HTTP/1.1 serializations and parses per request, a spawned task and a 64 KiB pipe; trailers were lost (no gRPC), and the hand-written parser was extra request-smuggling surface.

The question: keep Pingora, fork it, or own a small engine for the part we use?

## Decision Drivers

* FIPS 140-3: one place builds every outbound TLS configuration (`toolkit-http`), fail-closed without the FIPS provider; no second crypto provider in the graph because of the proxy.
* Security: no request-smuggling or desync surface of our own, and bounded resources against a malicious upstream (HTTP/2 HPACK and CONTINUATION abuse, reset floods, flow-control stalls).
* No runtime pin: track tokio releases freely.
* Performance: no per-request serialization, spawn or pipe; HTTP/2 multiplexing; latency at least as good as before.
* Flexibility: pluggable resolution, address policy (SSRF), health checks, balancing and observation, for OAGW and for other gateways (`toolkit-gateway`, event-broker).
* Maintainability: depend on the most widely deployed Rust HTTP stack (hyper, h2, rustls) instead of a framework.

## Considered Options

* Keep Pingora
* Fork Pingora
* hyper-util's legacy `Client`
* In-house engine on `hyper::client::conn` (`toolkit-http-proxy`)

## Decision Outcome

Chosen option: "In-house engine on `hyper::client::conn` (HTTP/1.1) and `h2` (HTTP/2)", because it is the only option that puts TLS under `toolkit-http`'s FIPS hardening, removes Pingora (and with it the tokio pin and its `ring` path) from the graph, and lets the engine connect to an already resolved, SSRF-filtered address while presenting the original host name, which hyper-util's client cannot do without workarounds.

### Engine Shape

`libs/toolkit-http-proxy` (`cf-gears-toolkit-http-proxy`):

- **`ProxyClient`**: `send(target, request)` and `upgrade(target, request)`. A `Target` carries the resolved socket address, the server name (SNI, certificate check, default authority), the transport and a protocol policy (`Auto`, `Http1Only`, `Http2Only`). The engine never resolves names.
- **Protocols**: HTTP/1.1 on `hyper::client::conn::http1`; HTTP/2 on the `h2` crate directly. The engine uploads HTTP/2 request bodies itself and queues only as much DATA as the peer's flow-control window allows, so nothing piles up in a send buffer behind a closed window; trailers are forwarded both ways.
- **Pool**: keyed by address, server name and transport, so connections with different SNI are never shared. HTTP/1.1: LIFO idle stack; a connection returns only after its response body reached its clean end. HTTP/2: one multiplexed connection per key with a generation number (a failure on an old connection never evicts its replacement) and an active-stream count covering both the upload and the response (the reaper never detaches a busy connection).
- **ALPN**: TLS targets offer `h2` and `http/1.1`; the negotiated protocol is cached per host (TTL, bounded, evicted on HTTP/2 connection errors) so later connections offer only what the host speaks.
- **Retry**: at most once, only on a reused connection, only when hyper hands the request back unsent. A request that may have reached the upstream is never replayed.
- **Timeouts**: connect (TCP and TLS separately), response head and per-body-frame idle (`read_timeout`), and `write_timeout`: a socket write or flush pending that long (the upstream stopped reading) severs the connection; an HTTP/2 stream whose window stays closed that long is reset (`CANCEL`) without touching its siblings. Every socket sits behind a kill switch, so the engine can sever connections owned by background tasks.
- **`balance`**: `BackendPool` with pluggable `Resolver` (system DNS with retries), `AddressFilter` (SSRF), `HealthCheck` (TCP by default; first probe immediately, then periodic), `Balancer` (round-robin), background DNS refresh, passive `mark_unhealthy`.
- **`Observer`** hook for metrics and logs without an OpenTelemetry dependency in the engine.

OAGW keeps its own request pipeline and maps engine error kinds onto its error model one-to-one with the former `fail_to_proxy` table ([OAGW ADR-0019](../../../gears/system/oagw/docs/ADR/0019-upstream-proxy-engine.md)).

### Security Properties

| Threat | Engine behaviour | Regression test (`libs/toolkit-http-proxy/tests/security.rs` unless noted) |
|---|---|---|
| Premature upgrade passthrough (CVE-2026-2833 / RUSTSEC-2026-0033) | A tunnel is handed out only after the upstream answered `101` with `Upgrade: websocket`; only `GET` + a single `Upgrade: websocket` token without a body is accepted; `h2c` and `HTTP2-Settings` never leave; a rejected handshake's connection is never pooled. | `upgrade_without_101_tunnels_nothing`, `bogus_101_is_rejected`, `tests/client.rs::invalid_upgrade_requests_never_leave` |
| TE/CL ambiguity, `identity, chunked`, HTTP/1.0 + TE, close-delimited request bodies (CVE-2026-2835) | Inbound `Content-Length`, `Transfer-Encoding`, `Connection` and everything it nominates, `Keep-Alive`, `Proxy-*`, `Trailer`, `Upgrade` are dropped; hyper derives framing from the body; requests are always HTTP/1.1 or HTTP/2; unknown-length bodies are chunked explicitly (also for `GET`). | `ambiguous_inbound_framing_is_never_forwarded`, `streaming_bodies_use_chunked_framing_only`, `unknown_length_get_bodies_are_framed_not_dropped`, proptest in `sanitize_tests.rs` |
| 0.CL / `Expect` desync (Kettle, Black Hat 2025) | `Expect` is dropped; bodies are sent whole with explicit framing. | `ambiguous_inbound_framing_is_never_forwarded` |
| Chunk-extension parsing differentials ("funky chunks", CVE-2026-2332) | No hand-written decoder; hyper rejects bare LF, LF-only size lines and oversized extensions; the engine never emits extensions; a failed connection is never reused. | `malformed_chunked_bodies_fail_and_are_not_pooled` |
| Response desync / pool poisoning | Pooled only after a clean end of body; extra bytes after a response are never served; a response carrying both `Content-Length` and `Transfer-Encoding` loses its length and its connection. | `injected_pipelined_response_is_never_served`, `ambiguous_response_framing_drops_length_and_connection`, `tests/client.rs::half_read_body_closes_the_connection` |
| Malformed or oversized response heads | hyper strict parsing; at most 128 header fields; read buffer capped at 256 KiB. | `malformed_response_heads_are_rejected` |
| CRLF injection via percent-decoded paths (CVE-2026-75922 class) | The request target is passed through undecoded; `http` types reject CR/LF/NUL. | `percent_encoded_crlf_reaches_the_wire_encoded` |
| Cross-SNI connection reuse | Pool key includes the server name. | `tests/client.rs::pool_is_keyed_by_server_name` |
| HTTP/2 Bomb (CVE-2026-49975: HPACK indexed references + zero-window stall) | `max_header_list_size` 64 KiB counted on decoded size with 32 octets per field; header table 4 KiB; uploads queue only what the window allows and a stream whose window stays closed for `write_timeout` is reset; a socket that stops accepting writes is severed. | `hpack_indexed_reference_bomb_is_contained`, `zero_window_slowloris_hits_the_read_timeout`, `early_h2_response_with_zero_window_upload_is_torn_down`, `early_h1_response_with_unread_upload_severs_the_connection` |
| CONTINUATION flood, Rapid Reset, MadeYouReset, unbounded empty DATA (RUSTSEC-2024-0332, RUSTSEC-2026-0258) | `h2 >= 0.4.16` pinned in the workspace; reset-stream limits configured; read timeouts bound everything else. | `continuation_flood_is_contained`, `empty_data_frame_flood_is_contained` |
| SSRF / DNS rebinding | The engine connects only to the address it is given; `BackendPool` filters every resolved address. | `balance_tests.rs::filter_*`, OAGW `endpoint_selector_tests.rs::selector_ssrf_*` |

A fuzz target (`tools/fuzz/fuzz_targets/fuzz_http_proxy_sanitize.rs`) checks that no framing or hop-by-hop header survives request sanitization for arbitrary input.

Out of scope: the inbound side is the platform's hyper server (API gateway); its HTTP/2 limits against CVE-2026-49975 are a separate follow-up.

### FIPS 140-3

Every `rustls::ClientConfig` comes from `toolkit_http::{native,webpki,custom}_roots_client_config`, which fails closed without an installed provider under `fips`, sets `require_ems` and checks `ClientConfig::fips()`. The engine clones that configuration per ALPN offer and checks `fips()` again. The test-only "accept any certificate" verifier refuses to build under `fips`. The engine adds no crypto dependency of its own.

### Consequences

* `toolkit-http` exports its TLS builders (`native_roots_client_config`, `webpki_roots_client_config`, new `custom_roots_client_config`) and `TlsConfig` types; they are now public API.
* Pingora, `pingora-memory-cache`, `httparse` and Pingora's transitive `ring`, `prometheus 0.13` and `protobuf 2` leave the graph; the `RUSTSEC-2024-0437` ignore in `deny.toml` goes.
* The workspace tokio pin and the matching Dependabot ignore are removed.
* The workspace depends on `h2 = "0.4.16"` directly: the engine drives HTTP/2 with it, and the version floor carries the CONTINUATION, MadeYouReset and empty-DATA fixes.
* Gateways own a security-critical component; changes to `libs/toolkit-http-proxy` need the security regression suite to stay green and a security review.
* Follow-ups: carry trailers (gRPC) through OAGW's SDK body type (the engine already forwards them); migrate `toolkit-gateway::Forwarder` to stream request bodies (#4488); review the inbound server's HTTP/2 limits.

### Confirmation

* `cargo nextest run -p cf-gears-toolkit-http-proxy`: unit, functional (`tests/client.rs`) and security (`tests/security.rs`) suites; with `--features fips` also `tests/fips.rs` (the `test-fips` CI lane runs it on Linux).
* OAGW's unit, integration and E2E suites pass unchanged on the new engine.
* `cargo tree -i pingora-core` finds nothing; `deny-fips.toml` passes.
* `cargo bench -p cf-gears-oagw --bench proxy_roundtrip` compares against the recorded Pingora baseline.

## Pros and Cons of the Options

### Keep Pingora

* Good, because it is battle-tested at Cloudflare scale.
* Bad, because its TLS path bypasses `toolkit-http`'s FIPS hardening and pulls `ring`.
* Bad, because it pins the workspace's tokio and we wait for upstream releases for fixes.
* Bad, because using it as a library forces the in-memory bridge, with its overhead and parsing surface.

### Fork Pingora

* Good, because we could fix FIPS and the tokio issue ourselves.
* Bad, because we would maintain a large framework (server, cache, modules) to use its connector and balancer.
* Bad, because the bridge and its overhead stay.

### hyper-util's legacy Client

* Good, because it is maintained by hyper's authors and widely used (reqwest).
* Neutral, because pooling, ALPN and retries of canceled requests come for free.
* Bad, because its HTTP/2 path hands the request body to an internal pipe task that buffers DATA beyond the peer's window; uploads stalled by a zero window cannot be observed or bounded.
* Bad, because its connector sees only the URI: connecting to a pre-resolved, SSRF-filtered address while presenting the original name needs a side channel.
* Bad, because pool keys, retry semantics, protocol hints and per-connection teardown are not under our control.

### In-house engine on hyper::client::conn

* Good, because TLS, pooling, retries, limits and teardown are explicit and testable.
* Good, because it removes the bridge: no serialization, no pipe, no per-request spawn.
* Good, because hyper (HTTP/1.1), h2 and rustls carry the protocol work, while uploads, flow control and teardown stay under the engine's control.
* Bad, because we own the pool and its edge cases (mitigated by the regression suites).

## More Information

* Pingora advisories: CVE-2026-2833, CVE-2026-2835, CVE-2026-2836 (fixed in 0.8.0); CVE-2025-4366.
* HTTP/2 Bomb: CVE-2026-49975 (HAProxy, NGINX, Apache, Envoy, IIS and Pingora reports).
* RustSec: RUSTSEC-2026-0258 (h2 empty DATA), RUSTSEC-2024-0332 (CONTINUATION flood).
* J. Kettle, "HTTP/1.1 must die: the desync endgame", Black Hat USA 2025.
* Benchmark (Windows 11, local upstreams, `proxy_roundtrip`, before/after) is recorded in the change's pull request.

## Traceability

- **FIPS PRD**: [docs/security/fips/PRD.md](../../security/fips/PRD.md)
- **FIPS dependency policy**: [ADR-0005](../../security/fips/adrs/0005-fips-dependency-policy.md)
- **HTTP client**: [ADR-0001](./0001-toolkit-hyper-tower-http-client.md)
- **OAGW**: [ADR-0019](../../../gears/system/oagw/docs/ADR/0019-upstream-proxy-engine.md)

This decision directly addresses:

* `cpt-toolkit-adr-fips-dependency-policy` — removes Pingora's `ring` path and its unmanaged TLS configuration.
* `cpt-cf-oagw-nfr-low-latency` — removes the per-request serialization bridge.
