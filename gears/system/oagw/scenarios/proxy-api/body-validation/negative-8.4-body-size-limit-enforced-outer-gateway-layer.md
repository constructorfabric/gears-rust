# Body size limit enforced at the outer api-gateway layer (64MB)

## Inbound request

Declare a `Content-Length` above the outer api-gateway's own body-limit layer (e2e default: 64MB, see `config/e2e-local.yaml`), sent to the platform's front door rather than directly to OAGW.

```http
POST /api/oagw/v1/proxy/<alias>/v1/test HTTP/1.1
Host: api-gateway.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Content-Length: 200000000

small body
```

## Expected response

- `413 Payload Too Large`
- Plain-text body (not `application/problem+json`) — this rejection happens at the api-gateway's `RequestBodyLimitLayer`, in front of OAGW, before the request ever reaches the OAGW proxy handler.
- No `X-OAGW-Error-Source` header — OAGW never saw the request.

## What to check

- There are **two independent body-size ceilings** in the deployed topology, not one:
  1. The outer api-gateway's `RequestBodyLimitLayer` (64MB in e2e config) — rejects first, with a plain-text 413.
  2. OAGW's own cap (100MB, see [negative-8.1](negative-8.1-maximum-body-size-limit-enforced.md)) — rejects with a `problem+json` `400` (`OutOfRange`/"Out of Range"), not a `413`. Since 100MB is *higher* than the outer 64MB ceiling, this cap is unreachable in this topology (see below) — it only fires in a deployment that bypasses or raises the outer limit.
- Because the outer ceiling is lower, a request large enough to hit OAGW's own 100MB cap never reaches OAGW in this topology — the outer layer always fires first. [negative-8.1](negative-8.1-maximum-body-size-limit-enforced.md) documents OAGW's own logical limit and error shape, which is the contract OAGW's code owns; this scenario documents the outer layer that actually fronts it in production/e2e deployments.
- **No enforced invariant ties the two ceilings together.** OAGW's 100MB cap is a fixed constant in its own code; the outer gateway's body-limit layer is configured independently, per deployment/environment. Nothing prevents an environment from raising or removing the outer limit above 100MB, which would activate OAGW's own 400/`OutOfRange` path for the first time outside direct-to-OAGW testing. This is not currently checked or asserted anywhere — it's a configuration-drift risk between two independently-owned layers, not a bug in either one individually.
