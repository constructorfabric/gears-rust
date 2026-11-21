# Reject an invalid credential mode on write

## Setup

Apply [shared setup](README.md#shared-setup), but create the upstream with each `auth.config` below.

## Inbound request

```http
POST /oagw/v1/upstreams HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"server": {...}, "protocol": "...", "auth": {"type": "...apikey.v1", "config": {"header": "Authorization", "credential_mode": "header_ref"}}}
```

## Expected response

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json
```

- `type` contains `invalid_argument`, with one field violation on `auth.config.credential_mode` (SREF-CFG-002).

## What to check

- `request_secret_types` that is empty, has an empty entry or an entry that is not a GTS type id, or appears without `credential_mode: request_ref`, is refused with a field violation on `auth.config.request_secret_types` (SREF-CFG-003).
- A comma-separated list of GTS type ids in `request_ref` mode is accepted.
- `config` mode without `secret_ref` is refused with a field violation on `auth.config.secret_ref` (SREF-CFG-004); `request_ref` mode without it is accepted.
- In both modes, a missing or invalid `header`, or one the gateway sets or strips itself (`Host`, `Content-Length`, hop-by-hop, forwarding headers such as `X-Forwarded-For`, `X-OAGW-*`), is refused with a field violation on `auth.config.header` (SREF-CFG-007).
- In both modes, a `prefix` that is not a valid header value or is longer than 64 bytes is refused with a field violation on `auth.config.prefix` (SREF-CFG-008).
- In `request_ref` mode, a secret value that is not UTF-8 or not a valid header value returns 401 `AUTH_PLUGIN_INTERNAL` with no forward. The ADR's error table does not list this case; it keeps the API-key plugin's mapping.
- A `request_ref` config stored before a write check refused it (no usable `header`) is a plugin misconfiguration at proxy time: 500 `internal`, no forward, as for the API-key plugin.
