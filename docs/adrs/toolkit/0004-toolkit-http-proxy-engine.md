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
- **Pool**: keyed by address, server name and transport, so connections with different SNI are never shared. HTTP/1.1: LIFO idle stack, at most `max_idle_per_host` (128) per key and `max_idle_total` (1024) across all keys (beyond it the oldest idle connection of any host is closed), retired one second before the idle timeout an upstream announces in `Keep-Alive: timeout=N`; a connection returns only after its response body reached its clean end. Each HTTP/1.1 connection has exactly one owner from its handshake on (idle in the pool, leased to an exchange, or held until its response body ends), and losing it closes the connection: expiry, eviction, pooling disabled, the client dropped, a timed-out, failed or cancelled exchange, checkout or upgrade handshake, an abandoned or ambiguously framed response; only a validated `101` hands an upgrade's socket to its tunnel. The owner also drives the connection: the exchange until the response head arrives, the response body while it is read, the pool when it checks a connection out or reaps. Socket reads happen in the task that consumes the data, with no hand-off between tasks per body chunk, so the read buffer can stay small (64 KiB) without stalling large responses. Nothing drives a connection after its owner lets go, and a connection whose request upload is still running when its response ends (the upstream answered early) is closed rather than pooled. HTTP/2: a small set of multiplexed connections per key (the least loaded one is leased; a new one opens once every connection carries `h2_streams_per_connection` streams, up to `h2_max_connections_per_host` on every path, including HTTP/1 fallback attempts that negotiate h2), each with a generation number (a failure on an old connection never evicts its replacement) and an active-stream count covering both the upload and the response (the reaper never detaches a busy connection). Requests waiting for a connection attempt that fails share its error instead of each repeating the attempt, so a burst against an unresponsive host fails after one connect timeout.
- **ALPN**: TLS targets offer `h2` and `http/1.1`; the negotiated protocol is cached per host (TTL, bounded, evicted on HTTP/2 connection errors) so later connections offer only what the host speaks.
- **Retry**: at most once per request. A request the upstream may have processed is sent again only if its method is idempotent (RFC 9110 §9.2.2), with one exception, `HTTP_1_1_REQUIRED`:
  - the request provably never left (a reused connection was already closed): sent again as is;
  - the peer refused the HTTP/2 stream (`REFUSED_STREAM`, RFC 9113 §8.7): sent again for any method;
  - the peer asked for HTTP/1.1 (`HTTP_1_1_REQUIRED`) and the target allows it: sent again over HTTP/1.1 for any method, and the host is used over HTTP/1.1 from then on. RFC 9113 does not promise that such a stream was unprocessed, but servers send this code before processing, when the request needs something HTTP/2 cannot do (IIS with Windows authentication, a TLS renegotiation for a client certificate). Without the resend, the first `POST` to such a host would fail each time its cached protocol expires. Pingora, curl and browsers resend it as well;
  - a graceful `GOAWAY` (`NO_ERROR`), or a reused HTTP/1.1 connection the upstream closed before answering (the keep-alive race): sent again only for idempotent methods. h2 reports a graceful `GOAWAY` the same way to a stream the peer may already have processed when the connection then drops, so it is not proof that the stream was unprocessed.

  Pingora retries more: up to 16 times by default, any method after a graceful `GOAWAY`, and any method on a reused connection unless the caller narrows it.

  Resending needs the body again: an empty body always works, a buffered one when the caller attaches it as a `ReplayableBody` request extension (OAGW does for every buffered request). Streamed bodies are never resent.
- **Response bodies**: small chunks or DATA frames that have already arrived are merged into one frame of up to `response_merge_limit` bytes (32 KiB by default; zero disables merging) and at most 128 upstream frames; a larger upstream frame is handed out as is. The engine never waits for more data to fill a frame, and the bytes, trailers and any error keep their order. A burst of small chunks, such as an event stream catching up, then costs the caller far fewer writes than one per chunk. Pingora's and Go's proxies do the same (Pingora merges the body tasks already queued; Go reads into a 32 KiB buffer). Chunk and frame boundaries carry no meaning in HTTP (RFC 9112 §7.1, RFC 9113 §8.1).
- **Real-world responses**: the engine accepts whatever it can take without letting two parties disagree about where a message ends. hyper alone decides the framing, and the engine re-frames for the caller.
  - Whitespace before a header's colon is removed and an obs-fold is replaced by a space, as RFC 9112 §5.1/§5.2 ask of a proxy.
  - A malformed `Content-Length`, conflicting lengths, a malformed chunk or garbage header lines are still rejected.
  - On HTTP/1.1, header names can keep their case end to end for case-sensitive legacy peers, when the caller's server is hyper with `preserve_header_case`.
    - Requests always go out with the case map such a server recorded, at no cost to the engine.
    - Response case is recorded only with the engine's `preserve_header_case` option. The option is off by default because it copies every response header name per response, which costs about 8% more client CPU on small responses.
  - Interim (1xx) responses are skipped and the final one is returned.
  - A `101` counts only on HTTP/1.1.
  - A `204` or `1xx` loses `Content-Length`; `HEAD` and `304` keep it.
  - Trailers keep every field except framing, connection- and proxy-hop-scoped fields, `Host`, and whatever the message head's `Connection` nominated (RFC 9110 §6.5.1, §7.6.1, §11.7). This covers response trailers on both protocols and request trailers on HTTP/2; HTTP/1.1 requests carry no trailers, because `Trailer` is not forwarded on requests. The response keeps its `Trailer` declaration, minus those names, so an HTTP/1.1 recipient knows which trailers follow.
- **Framing**: request framing is derived from the body, never from the caller's headers. A body of known size carries its `Content-Length` on both protocols (hyper writes it on HTTP/1.1, the engine sets it on HTTP/2); a body of unknown size is chunked on HTTP/1.1 and carries no length on HTTP/2. An empty body is sent with `Content-Length: 0` for methods that give a body a meaning (`POST`, `PUT`, `PATCH`, ...; RFC 9110 §8.6) and without framing headers for `GET`, `HEAD`, `DELETE`, `OPTIONS`, `CONNECT` and `TRACE`.
- **Timeouts**: connect (TCP and TLS separately), response head and per-body-frame idle (`read_timeout`), and `write_timeout`: a socket write or flush pending that long (the upstream stopped reading) severs the connection; an HTTP/2 upload whose window stays closed that long fails its exchange as `WriteTimeout` (the stream is reset without touching its siblings). An HTTP/2 exchange has exactly one owner: the `send` future until the response head arrives, the response body afterwards. Only the owner ends it (dropped, failed, timed out, or the body reached its clean end), and only then is the stream reset and the upload stopped. The upload task never resets on its own (h2 lets a local reset erase a response that was already complete): a stalled window or a failed request body only signals the owner, which still delivers everything the upstream already sent, so a response completed early (RFC 9113 §8.1) stays readable however long the caller holds it, while an unfinished one fails as soon as nothing more is buffered; after an abort the upstream gets no more receive credit, so that drain is bounded by the stream window. A waiting upload task also ends when the peer resets the stream or the connection goes away. A response body kept without being polled keeps its stream, as an unread HTTP/1.1 body keeps its connection. Every socket sits behind a kill switch, so the engine can sever connections owned by background tasks.
- **Timers**: tokio's default timer driver keeps every timer in one wheel behind one lock (the per-worker wheel is `tokio_unstable` only), so per-request and per-frame timers would make all connections contend on it. The engine therefore:
  - records progress (a body frame, granted window, a completed write) as a timestamp only; an idle deadline is armed the first time something waits and is re-checked when it fires, so steady traffic touches it at most once per limit;
  - runs its timeouts on a shared coarse timer service (one per client): deadlines are rounded up to a 10 ms tick, waiters for the same tick share one bucket, buckets are sharded per thread, and one driver task on tokio's clock fires due buckets. tokio's timer lock is taken once per tick instead of once per timeout, timeouts never fire early and at most one tick late, and `tokio::time::pause` keeps working in tests.
- **`balance`**: `BackendPool` with pluggable `Resolver` (system DNS with retries; when a refresh fails for a host, its last good addresses are kept), `AddressFilter` (SSRF), `HealthCheck` (TCP by default; first probe immediately, then periodic; a member leaves rotation after `unhealthy_threshold` failed probes in a row and rejoins after `healthy_threshold` successful ones, both 1 by default, and the state survives a DNS refresh), `Balancer` (round-robin over healthy members; a member that goes down mid-pick never turns into "no healthy member"), background DNS refresh, passive `mark_unhealthy`.
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
| Malformed or oversized response heads | hyper strict parsing; at most 128 header fields; read buffer capped at 64 KiB. Whitespace before a colon and obs-fold are normalized, and a length written that way still frames the body exactly. | `malformed_response_heads_are_rejected`, `tests/upstream_edges.rs::lenient_response_heads_are_normalized`, `::invalid_content_length_is_rejected_and_not_pooled` |
| Desync after an irregular response (overread, unsolicited bytes on an idle connection, close-delimited or `Connection: close` responses, interim responses) | A connection is reused only when its response was framed and ended cleanly with nothing after it. | `tests/upstream_edges.rs::bytes_beyond_the_content_length_retire_the_connection`, `::unsolicited_bytes_on_an_idle_connection_retire_it`, `::close_delimited_body_is_read_to_the_close_and_not_pooled`, `::connection_close_is_honoured_and_not_relayed`, `::interim_responses_are_skipped_and_the_connection_kept` |
| Framing, hop-scoped or proxy-credential fields smuggled in trailers | Response trailers, and HTTP/2 request trailers, lose framing fields, connection- and proxy-hop-scoped fields, `Host`, and whatever the head's `Connection` nominated. | `tests/upstream_edges.rs::http1_trailers_reach_the_caller_sanitized`, `::trailers_lose_what_the_head_connection_nominated`, `h2_trailers_lose_framing_fields_only`, `tests/client.rs::h2_request_trailers_lose_hop_fields`, `sanitize_tests.rs::trailers_keep_application_fields_only`, `::trailers_lose_proxy_hop_fields_and_what_the_head_nominated` |
| CRLF injection via percent-decoded paths (CVE-2026-75922 class) | The request target is passed through undecoded; `http` types reject CR/LF/NUL. | `percent_encoded_crlf_reaches_the_wire_encoded` |
| Cross-SNI connection reuse | Pool key includes the server name. | `tests/client.rs::pool_is_keyed_by_server_name` |
| HTTP/2 Bomb (CVE-2026-49975: HPACK indexed references + zero-window stall) | `max_header_list_size` 64 KiB counted on decoded size with 32 octets per field; header table 4 KiB; uploads queue only what the window allows and a stream whose window stays closed for `write_timeout` is reset; a socket that stops accepting writes is severed. | `hpack_indexed_reference_bomb_is_contained`, `zero_window_slowloris_hits_the_read_timeout`, `early_h2_response_with_zero_window_upload_is_torn_down`, `h2_upload_stall_is_reported_as_a_write_timeout`, `complete_h2_response_survives_an_upload_stall`, `h2_upload_stall_after_the_head_still_fails_the_exchange`, `h2_upload_stall_is_not_hidden_by_a_response_flood`, `early_h1_response_with_unread_upload_severs_the_connection`, `h1_connection_with_an_unfinished_upload_closes_when_the_pool_lets_go`, `h1_ambiguous_response_to_an_unfinished_upload_closes_the_connection`, `cancelled_h1_checkout_closes_the_connection_it_took`, `timed_out_upgrade_handshake_closes_its_connection` (Linux) |
| Replaying a request the upstream may have processed (duplicate side effects) | Resent only when provably unprocessed (unsent, `REFUSED_STREAM`, `HTTP_1_1_REQUIRED`) or, for idempotent methods, after a graceful `GOAWAY` or a keep-alive race; at most once; streamed bodies never. | `stale_keep_alive_connection_is_resent_for_idempotent_requests`, `stale_keep_alive_connection_never_resends_unsafe_requests`, `refused_h2_stream_is_resent_for_any_method`, `graceful_goaway_resends_only_idempotent_requests`, `tests/client.rs::http_1_1_required_falls_back_to_http1_for_the_host` |
| Ambiguous empty request bodies (411 from strict upstreams) | `Content-Length: 0` for methods that give a body a meaning, no framing headers otherwise. | `empty_bodies_carry_content_length_zero_for_methods_with_body_semantics` |
| Resource exhaustion through idle connections or an unresponsive upstream | Global and per-host idle caps; announced `Keep-Alive` timeouts honoured; requests waiting for a failed connect share its error. | `pool.rs::idle_h1_connections_are_capped_across_hosts`, `pool.rs::keep_alive_timeout_shortens_the_idle_lifetime`, `concurrent_connects_to_an_unresponsive_host_fail_together` |
| Abandoned HTTP/2 exchanges (upload tasks outliving a timed-out or dropped request) | The caller owns each exchange; cancellation resets the stream and ends its upload task, releasing its stream count. | `h2_head_timeout_resets_the_stream_and_stops_the_upload`, `dropping_an_h2_send_resets_the_stream_and_stops_the_upload`, `completed_h2_response_stops_the_upload_while_still_held` |
| Malformed HTTP/2 response heads (conflicting `content-length`, missing `:status`) and an upstream-enlarged HPACK encoder table | `h2 >= 0.4.20` rejects such heads and caps the encoder table regardless of the peer's `SETTINGS_HEADER_TABLE_SIZE`. | `malformed_h2_response_heads_are_rejected`, `h2_body_cut_by_a_reset_never_reports_its_end` |
| CONTINUATION flood, Rapid Reset, MadeYouReset, unbounded empty DATA (RUSTSEC-2024-0332, RUSTSEC-2026-0258) | `h2 >= 0.4.20` pinned in the workspace; reset-stream limits configured; read timeouts bound everything else. | `continuation_flood_is_contained`, `empty_data_frame_flood_is_contained` |
| SSRF / DNS rebinding | The engine connects only to the address it is given; `BackendPool` filters every resolved address. | `balance_tests.rs::filter_*`, OAGW `endpoint_selector_tests.rs::selector_ssrf_*` |

A fuzz target (`tools/fuzz/fuzz_targets/fuzz_http_proxy_sanitize.rs`) checks that no framing or hop-by-hop header survives request sanitization for arbitrary input.

Out of scope: the inbound side is the platform's hyper server (API gateway); its HTTP/2 limits against CVE-2026-49975 are a separate follow-up.

### FIPS 140-3

Every `rustls::ClientConfig` comes from `toolkit_http::{native,webpki,custom}_roots_client_config`, which fails closed without an installed provider under `fips`, sets `require_ems` and checks `ClientConfig::fips()`. The engine clones that configuration per ALPN offer and checks `fips()` again. The test-only "accept any certificate" verifier refuses to build under `fips`. The engine adds no crypto dependency of its own.

### Consequences

* `toolkit-http` exports its TLS builders (`native_roots_client_config`, `webpki_roots_client_config`, new `custom_roots_client_config`) and `TlsConfig` types; they are now public API.
* Pingora, `pingora-memory-cache` and Pingora's transitive `ring`, `prometheus 0.13` and `protobuf 2` leave the graph; the `RUSTSEC-2024-0437` ignore in `deny.toml` goes. OAGW no longer depends on `httparse` itself (its hand-written bridge parser is gone); hyper still uses it.
* The workspace tokio pin and the matching Dependabot ignore are removed, and the workspace now requires tokio 1.54 or later.
* The workspace depends on `h2 = "0.4.20"` directly: the engine drives HTTP/2 with it, and the version floor carries the CONTINUATION, MadeYouReset, empty-DATA, HPACK encoder-table and malformed-response-head fixes.
* Gateways own a security-critical component; changes to `libs/toolkit-http-proxy` need the security regression suite to stay green and a security review.
* Follow-ups: carry trailers (gRPC) through OAGW's SDK body type (the engine already forwards them); migrate `toolkit-gateway::Forwarder` to stream request bodies (#4488); review the inbound server's HTTP/2 limits.

### Confirmation

* `cargo nextest run -p cf-gears-toolkit-http-proxy`: unit, functional (`tests/client.rs`) and security (`tests/security.rs`) suites; with `--features fips` also `tests/fips.rs` (the `test-fips` CI lane runs it on Linux).
* OAGW's unit, integration and E2E suites pass unchanged on the new engine.
* `cargo tree --workspace --all-features --prefix none` lists no `pingora-*` package (`pingora-core`, `pingora-rustls`, `pingora-memory-cache`, ...), and `cargo tree -p cf-gears-oagw --depth 1 --edges normal` no `httparse`; `deny-fips.toml` passes.
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
* Benchmark: `cargo bench -p cf-gears-oagw --bench proxy_roundtrip` (full OAGW pipeline, local upstreams, Windows 11, 2000 sequential requests for latency, 64 concurrent clients for 3 s for throughput). Engine numbers are the median of three runs of the final code:

  | Scenario | p50 Pingora → engine | p99 Pingora → engine | c64 req/s Pingora → engine |
  |---|---|---|---|
  | HTTP/1.1, 1 KiB response | 287 → 69 µs | 469 → 177 µs | 18.5k → 69.9k |
  | HTTP/1.1, 256 KiB response | 518 → 163 µs | 1.2 → 5.3 ms | 10.2k → 30.2k |
  | HTTP/1.1, 64 KiB upload | 343 → 93 µs | 1.1 ms → 209 µs | 12.9k → 59.2k |
  | HTTP/2 + TLS, 1 KiB response | 193 → 92 µs | 374 → 229 µs | 56.4k → 167.4k |
  | HTTP/2 + TLS, 256 KiB response | 346 → 381 µs | 708 → 806 µs | 15.8k → 17.6k |
  | HTTP/2 + TLS, 64 KiB upload | 218 → 171 µs | 545 → 371 µs | 19.7k → 57.8k |

  Throughput varies by about ±10% between runs on this machine. The p99 of the 256 KiB HTTP/1.1 case is unstable: 2.3 to 5.3 ms across the three runs, and 2.8 to 9.3 ms for an earlier engine build measured in the same session, so it is not caused by the final changes. In the same session, HTTP/1.1 throughput for the 1 KiB and upload cases was about 10% lower than that earlier build; the cause is not known yet.

## Traceability

- **FIPS PRD**: [docs/security/fips/PRD.md](../../security/fips/PRD.md)
- **FIPS dependency policy**: [ADR-0005](../../security/fips/adrs/0005-fips-dependency-policy.md)
- **HTTP client**: [ADR-0001](./0001-toolkit-hyper-tower-http-client.md)
- **OAGW**: [ADR-0019](../../../gears/system/oagw/docs/ADR/0019-upstream-proxy-engine.md)

This decision directly addresses:

* `cpt-toolkit-adr-fips-dependency-policy` — removes Pingora's `ring` path and its unmanaged TLS configuration.
* `cpt-cf-oagw-nfr-low-latency` — removes the per-request serialization bridge.
