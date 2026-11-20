# Non-public IPv6 address refused at write and at connect

With the SSRF policy on (the default), an address in a non-public IPv6 range is never used as an upstream destination. The built-in list covers every block of the IANA IPv6 Special-Purpose Address Registry that is not globally reachable, plus site-local `fec0::/10`, IPv4-compatible `::/96`, multicast `ff00::/8` and every other address outside `2000::/3`, the global unicast space (see [DESIGN](../../../docs/DESIGN.md#security-considerations)).

## Scenario A: IP endpoint refused at write

### Request (example: SRv6 SID)

```http
POST /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": { "endpoints": [{ "scheme": "https", "host": "[5f00::1]", "port": 443 }] },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "alias": "srv6-sid"
}
```

### Expected response

A format error: `detail` and `context.format` carry the same text. `trace_id` is present when the request is traced. Nothing is stored.

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "endpoint[0] IP address '[5f00::1]' is blocked by SSRF protection: SRv6 segment identifier (5f00::/16)",
  "instance": "/oagw/v1/upstreams",
  "trace_id": "<trace id>",
  "context": {
    "format": "endpoint[0] IP address '[5f00::1]' is blocked by SSRF protection: SRv6 segment identifier (5f00::/16)",
    "resource_type": "gts.cf.core.oagw.proxy.v1~"
  }
}
```

### The same for

| Host | Reason in `detail` |
|---|---|
| `[fec0::1]` | `site-local address (fec0::/10)` |
| `[64:ff9b:1::a00:1]` | `local-use NAT64 address (64:ff9b:1::/48)` |
| `[2001:2::1]` | `benchmarking address (2001:2::/48)` |
| `[2001:10::1]` | `deprecated ORCHID address (2001:10::/28)` |
| `[2001:5::1]` | `IETF protocol assignment (2001::/23)` |
| `[3fff::1]` | `documentation address (3fff::/20)` |
| `[100:0:0:1::1]` | `dummy prefix (100:0:0:1::/64)` |
| `[4000::1]` | `address outside global unicast space (2000::/3)` |
| `[::808:808]` | `deprecated IPv4-compatible address (::/96)` |

`PUT /api/oagw/v1/upstreams/{id}` and types-registry provisioning refuse it the same way. An IPv6 endpoint outside the denied ranges is still refused, with `endpoint[0] uses IPv6 address '…'; IPv6 endpoints are not yet supported`.

## Scenario B: resolved address refused at connect

An upstream with a hostname endpoint is stored; the hostname later resolves to an address in one of these ranges. The proxy skips that address and connects to none of them: with every address refused, the request fails with `503` (single endpoint) or `502` (pool), and the server log names the reason.

The E2E environment cannot make a hostname resolve into these ranges, so this scenario is covered by Rust tests only (`resolved_addresses_in_non_public_ipv6_ranges_are_refused` in `infra/proxy/engine_tests.rs`, the range tests in `domain/ssrf.rs`).
