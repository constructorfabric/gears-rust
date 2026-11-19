# Rate limiting: spoofed forwarding headers keep the IP bucket

A `scope: ip` limit keys on the client address the API gateway resolved and hands to OAGW as a request extension. OAGW does not read `X-Forwarded-For` or `X-Real-IP`, so a client cannot open a fresh bucket by sending a new value.

## Setup

The API gateway runs with `trusted_proxy_hops: 0` (the default): the client address is the connection's peer address.

```json
{
  "rate_limit": {
    "sustained": { "rate": 1, "window": "minute" },
    "burst": { "capacity": 1 },
    "scope": "ip",
    "strategy": "reject"
  }
}
```

## Steps

1. `GET /api/oagw/v1/proxy/{alias}/v1/models` with `X-Forwarded-For: 198.51.100.1`.
2. The same request with `X-Forwarded-For: 198.51.100.2`, then `X-Forwarded-For: 203.0.113.9, 198.51.100.3`, then `X-Real-IP: 198.51.100.4`.

## Expected behavior

- Step 1 → `200`.
- Every request of step 2 → `429` `application/problem+json` (`resource_exhausted`) with `X-OAGW-Error-Source: gateway`, `Retry-After` about 60 s, `X-RateLimit-Limit: 1` and `X-RateLimit-Remaining: 0`: all requests share the peer address's bucket.
- With `trusted_proxy_hops: n` ≥ 1, the bucket is the `X-Forwarded-For` entry `n` positions from the right (written by the outermost trusted proxy); entries a client prepends do not change it. This is covered by the API gateway's Rust tests (`middleware::client_ip`), not E2E.
