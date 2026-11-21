# Circuit breaker: circuit opens after consecutive failures (503)

An upstream with a `circuit_breaker` stops receiving requests after `failure_threshold` failures in a row. Until `timeout_seconds` pass, OAGW answers every request itself with `503` and `Retry-After`, without calling the upstream.

State is per replica: each OAGW replica counts the failures it sees and opens its own circuit. Nothing is shared between replicas. On a replica, the breaker is shared by every tenant that resolves to the upstream, so only failures the upstream caused count.

## Setup

```http
POST /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": { "endpoints": [{ "scheme": "https", "host": "api.example.com", "port": 443 }] },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "circuit_breaker": {
    "failure_threshold": 2,
    "timeout_seconds": 30,
    "success_threshold": 1
  }
}
```

Omitted fields default to `sharing: private`, `enabled: true`, `failure_threshold: 5`, `timeout_seconds: 30`, `success_threshold: 3`. Bounds: `failure_threshold` 1–1000, `timeout_seconds` 1–86400, `success_threshold` 1–100 (see 2.15-A); an unknown field is refused (`422`).

Add a route `GET /v1` on the upstream.

## Steps

1. The upstream answers two requests in a row with `503`, `502` or `504`, or two requests fail on the transport (connection refused, TLS failure, DNS failure, connect timeout, or the request timeout while the upstream had the whole of it).
2. Send another request.

## Expected behavior

- The two failing requests get the upstream's answer (or the gateway's `502`/`503`/`504` for a transport failure), as without a breaker.
- The third request is not sent to the upstream:

```http
HTTP/1.1 503 Service Unavailable
Content-Type: application/problem+json
Retry-After: 30
X-OAGW-Error-Source: gateway

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
  "title": "Service Unavailable",
  "status": 503,
  ...
}
```

- `Retry-After` is the time left until the circuit lets a probe through, in whole seconds rounded up (at least 1), so it counts down on later requests.
- Any other status (`2xx`, `3xx`, `4xx`, `500`, `501`) is a success and resets the count: `503`, `200`, `503` does not open a circuit with `failure_threshold: 2`.
- Errors on the gateway's side count as nothing: auth plugin, guard, rate limit, CredStore, body too large, and the SSRF policy refusing every address the host resolves to. Requests the client cancels before the response head count as nothing too: when to give up is the caller's choice.
- While the client is still sending a streamed body, a timeout or a broken exchange counts as nothing (a client trickling its upload must not open the circuit for every tenant); a refused connection still counts. A request pinned with `X-OAGW-Target-Host` to one endpoint of several counts as nothing, and is still rejected while the circuit is open.
- A request timeout counts only when the upstream had the whole timeout: if the gateway's own steps before the exchange (the `request_ref` CredStore read, endpoint selection) used more than a tenth of it, the `504` counts as nothing. A slow CredStore cannot open the breakers of healthy upstreams.
- A WebSocket upgrade counts by its handshake: a `101` or another status is an answer, a refused or timed-out connection a failure. What happens on an open tunnel is not counted.
- With `"enabled": false` the configuration is kept and every request goes through.
- The breaker emits `resilience.circuit_breaker.calls` (by outcome, `rejected` included), `resilience.circuit_breaker.state` and `resilience.circuit_breaker.transitions`, with `resilience.name` = `{alias}/{upstream_id}`.
