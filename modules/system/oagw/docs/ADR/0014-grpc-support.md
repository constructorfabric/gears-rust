---
status: proposed
date: 2026-02-03
decision-makers: OAGW Team
---

# gRPC Support — HTTP/2 Multiplexing with Content-Type Detection


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [gRPC Route Matching](#grpc-route-matching)
  - [gRPC Error Mapping](#grpc-error-mapping)
  - [All Streaming Patterns Supported *(Phase 3 — not yet implemented)*](#all-streaming-patterns-supported-phase-3--not-yet-implemented)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Separate port](#separate-port)
  - [Connection hijacking](#connection-hijacking)
  - [HTTP/2 with gRPC multiplexing](#http2-with-grpc-multiplexing)
- [Comparison Matrix](#comparison-matrix)
- [More Information](#more-information)
- [Links](#links)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-grpc-support`

## Context and Problem Statement

OAGW currently supports HTTP/1.1 requests. Modern APIs increasingly use gRPC for efficient service-to-service communication. OAGW needs to proxy gRPC requests while maintaining the same routing, authentication, and policy enforcement as HTTP. Key challenges: gRPC uses HTTP/2 exclusively, requires specific headers (`content-type: application/grpc`), uses bidirectional streaming, and protocol detection is needed when HTTP and gRPC share the same port.

## Decision Drivers

* Minimize infrastructure complexity (avoid multiple ports)
* Transparent proxying without breaking gRPC semantics
* Support all gRPC patterns (unary, client streaming, server streaming, bidirectional)
* Reuse existing auth/routing/rate-limiting infrastructure
* Performance (minimal overhead)
* Protocol detection reliability

## Considered Options

* Separate port for gRPC-only traffic
* Connection hijacking (port 443, ALPN-based protocol detection)
* HTTP/2 with gRPC multiplexing (content-type header detection)

## Decision Outcome

Chosen option: "HTTP/2 with gRPC multiplexing", because gRPC is HTTP/2 with specific headers, making content-type detection simple, reliable, and standard (matches Envoy, Istio, Linkerd).

> **Note:** gRPC proxy support is not implemented yet and is targeted for Phase 3; current OAGW proxy paths do not route gRPC.

Single port handles both HTTP/1.1 and gRPC (HTTP/2) *(Phase 3 — not yet implemented)*. Detection via `content-type: application/grpc*` header check *(Phase 3 — not yet implemented)*:

```rust
let is_grpc = req.headers()
    .get(CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .map(|v| v.starts_with("application/grpc"))
    .unwrap_or(false);
```

### gRPC Route Matching

gRPC routes use service/method instead of HTTP path:

```json
{
  "match": {
    "grpc": {
      "service": "example.v1.UserService",
      "method": "GetUser"
    }
  }
}
```

Internally maps to HTTP/2 path: `/example.v1.UserService/GetUser`.

### gRPC Error Mapping

| gRPC Status | Code | OAGW Error |
|---|---|---|
| OK | 0 | Success |
| UNAUTHENTICATED | 16 | AuthenticationFailed |
| PERMISSION_DENIED | 7 | Forbidden |
| RESOURCE_EXHAUSTED | 8 | RateLimitExceeded |
| UNAVAILABLE | 14 | LinkUnavailable |
| DEADLINE_EXCEEDED | 4 | RequestTimeout |

### All Streaming Patterns Supported *(Phase 3 — not yet implemented)*

OAGW acts as transparent proxy for unary, server streaming, client streaming, and bidirectional streaming. Does not buffer streams — forwards gRPC frames directly without parsing Protobuf.

> **Note:** gRPC streaming proxy support is not implemented yet and is targeted for Phase 3.

```text
Unary:          Client ──request──> Server ──response──> Client
Server stream:  Client ──request──> Server ──stream───> Client
Client stream:  Client ──stream──> Server ──response──> Client
Bidirectional:  Client <=stream==> Server
```

### Consequences

* Good, because single endpoint simplifies deployment, firewall rules, TLS management
* Good, because native HTTP/2 (no protocol translation hacks)
* Good, because standard approach (battle-tested in Envoy, Istio, Linkerd)
* Good, because works seamlessly with Kubernetes ingress and cloud load balancers
* Bad, because all OAGW nodes must support HTTP/2 (modern Rust stacks do)
* Bad, because slightly more complex than HTTP/1.1-only (but hyper handles this)

### Confirmation

Prototype must validate: (1) ALPN negotiation with target Rust TLS stack, (2) reliable gRPC detection from content-type, (3) bidirectional streaming without buffering, (4) <5% overhead vs direct gRPC, (5) gRPC status code preservation, (6) HTTP/1.1 coexistence on same port.

Acceptance criteria *(Phase 3 — not yet implemented)*:

> **Note:** gRPC proxy support is not implemented yet and is targeted for Phase 3; the criteria below describe the future-state validation targets.

* gRPC health check (`grpc.health.v1.Health/Check`) works end-to-end
* HTTP/1.1 REST request to same port succeeds
* gRPC streaming (server/client/bidi) works without timeouts
* Rate limiting applies to gRPC requests
* Auth plugin can inspect gRPC metadata

## Pros and Cons of the Options

### Separate port

Dedicated port (e.g., 50051) for gRPC traffic.

```text
Client → :443 (HTTP/REST)
Client → :50051 (gRPC only)
         ↓
      OAGW → Upstream
```

Configuration:

```json
{
  "server": {
    "http_port": 443,
    "grpc_port": 50051
  }
}
```

* Good, because simple (no protocol detection), clear separation
* Good, because easy to configure separate TLS settings
* Bad, because extra port management (firewall, load balancer)
* Bad, because clients must know which port to use
* Bad, because doesn't work well with API gateways that expect single endpoint
* Bad, because incompatible with many cloud environments (single ingress port)

### Connection hijacking

Single-port, ALPN-based detection with first-request peeking.

```text
Client → :443
         ↓
   TLS Handshake (ALPN: h2)
         ↓
   Protocol Detection
         ├─ h2 + content-type: application/grpc → gRPC handler
         └─ h2/http/1.1 → HTTP handler
         ↓
      OAGW → Upstream
```

```rust
async fn handle_connection(stream: TcpStream, tls_acceptor: TlsAcceptor) {
    let tls_stream = tls_acceptor.accept(stream).await?;
    
    match tls_stream.negotiated_alpn_protocol() {
        Some(b"h2") => {
            let first_bytes = peek_first_request_header(&tls_stream).await?;
            
            if is_grpc_content_type(&first_bytes) {
                handle_grpc_request(tls_stream).await
            } else {
                handle_http2_request(tls_stream).await
            }
        }
        Some(b"http/1.1") | None => {
            handle_http1_request(tls_stream).await
        }
        _ => return Err("Unsupported protocol")
    }
}
```

* Good, because single port (443) for all traffic
* Good, because works with standard cloud load balancers
* Good, because transparent to clients
* Good, because industry standard (Envoy, Linkerd, Istio)
* Good, because better for Kubernetes ingress
* Bad, because complex protocol detection logic
* Bad, because small overhead for first request analysis
* Bad, because requires HTTP/2 support in OAGW core
* Bad, because edge cases with protocol misdetection

### HTTP/2 with gRPC multiplexing

Single-port, native HTTP/2, content-type header detection.

Unified configuration:

```json
{
  "server": {
    "port": 443,
    "protocols": ["http/1.1", "h2", "h2c"],
    "grpc_enabled": true
  },
  "upstream": {
    "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.grpc.v1",
    "server": {
      "endpoints": [
        {"scheme": "grpc", "host": "grpc-service.example.com", "port": 50051}
      ]
    }
  }
}
```

* Good, because simple detection (content-type header check)
* Good, because single port, single TLS config
* Good, because ALPN negotiation handled by TLS library
* Good, because works with all gRPC patterns (streaming included)
* Good, because standard approach matching industry practice
* Bad, because requires HTTP/2 support in OAGW core (but hyper supports this)
* Bad, because all OAGW nodes must support HTTP/2

## Comparison Matrix

| Criteria | Separate Port | Connection Hijacking | HTTP/2 Multiplexing |
|---|:---:|:---:|:---:|
| Single ingress point | No | Yes | Yes |
| Implementation complexity | Low | High | Medium |
| Protocol detection | Not needed | ALPN + peeking | Content-type hdr |
| Works with cloud LB | Partial | Yes | Yes |
| HTTP/2 requirement | Optional | Mandatory | Mandatory |
| Performance overhead | Minimal | Small (detection) | Minimal |
| Kubernetes-friendly | No | Yes | Yes |
| Streaming support | Full | Full | Full |

## More Information

Performance considerations: HTTP/2 connection pooling (multiplexing), direct gRPC frame forwarding without Protobuf parsing, HTTP/2 flow control window respect, gRPC keep-alive pings.

gRPC-specific headers preserved during proxying: `content-type` (validate), `grpc-encoding` (passthrough), `grpc-timeout` (enforce), `grpc-status` (passthrough), `grpc-message` (passthrough).

## Links

* [gRPC over HTTP/2](https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md)
* [ALPN Protocol Negotiation](https://datatracker.ietf.org/doc/html/rfc7301)
* [Envoy gRPC Proxying](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/other_protocols/grpc)
* [hyper HTTP/2 Support](https://docs.rs/hyper/latest/hyper/server/conn/http2/index.html)
* [gRPC Status Codes](https://grpc.io/docs/guides/status-codes/)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-streaming` — gRPC streaming patterns (server, client, bidirectional)
* `cpt-cf-oagw-fr-request-proxy` — gRPC proxy execution via content-type detection
* `cpt-cf-oagw-nfr-low-latency` — <5% overhead vs direct gRPC proxy
