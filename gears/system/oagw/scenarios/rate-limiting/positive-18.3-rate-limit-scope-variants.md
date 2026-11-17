# Rate limit scope variants

## Goal

Lock behavior for each supported `scope`:
- `global`
- `tenant`
- `user`
- `ip`
- `route`

## Setup

Create one upstream with multiple routes and configure rate limit scope per route in separate test runs.

Example config skeleton:

```json
{ "rate_limit": { "sustained": { "rate": 2, "window": "second" }, "scope": "<scope>" } }
```

## Expected behavior

- `global`: limit shared across all tenants.
- `tenant`: limit shared within tenant.
- `user`: limit shared within authenticated principal.
- `ip`: limit shared for the client address the API gateway resolved: the connection's peer address, or, with the gateway's `trusted_proxy_hops` ≥ 1, the `X-Forwarded-For` entry the outermost trusted proxy wrote. Forwarding headers the client sends never choose the bucket (see [negative-18.8](negative-18.8-spoofed-forwarding-headers-keep-the-ip-bucket.md)). A request that did not come through the gateway (an in-process SDK call) uses one shared `unknown` bucket.
- `route`: limit isolated per route, on an upstream limit as on a route limit: with an upstream `scope: route` limit of 1/min and two routes, the first request to each route passes and the second to either is `429`.

For each scope, verify the counter keying by sending requests from different principals/tenants/IPs/routes.
