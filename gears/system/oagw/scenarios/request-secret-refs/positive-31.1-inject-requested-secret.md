# Inject the requested secret

## Setup

Apply [shared setup](README.md#shared-setup).

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Secret-Ref: cred://tenant-openai

{"model":"gpt-4o","messages":[]}
```

## Expected response

200 OK. The mock returns `{"ok":true}`.

## Expected outbound request

```http
POST /v1/chat/completions HTTP/1.1
Host: api.example.com
Content-Type: application/json
Authorization: Bearer key-T

{"model":"gpt-4o","messages":[]}
```

## What to check

- The provider receives `Authorization: Bearer key-T` exactly once and no `X-OAGW-Secret-Ref` (SREF-SDK-001, SREF-REQ-006), under every passthrough mode, including an allowlist that names the header.
- `tenant-openai` and `cred://tenant-openai` give the same result (SREF-REQ-002); the header name is case-insensitive (SREF-REQ-004).
- Exactly one CredStore read, for `tenant-openai`, under the caller's context, not Partner's or a system context (SREF-RES-001).
- A client `Authorization` header is replaced, never sent beside the injected one (SREF-HDR-004). A custom `header`/`prefix` (`X-Api-Key`, `Key `) is used as configured (SREF-RES-002).
- Rotate the secret in CredStore: the next request sends the new value with no OAGW write, and each request reads CredStore once (SREF-RES-003).
- The same through the in-process SDK (`proxy_request` with `oagw_sdk::SECRET_REF_HEADER`), and for an SSE request and a WebSocket handshake (SREF-SDK-004).
- The log line `request_ref secret resolved` names the CredStore record ID, owner tenant, upstream, route and inbound `X-Request-Id` (SREF-OBS-002); request metrics carry `credential_mode=request_ref` (SREF-OBS-003).
- A CORS preflight for the same path reads no secret and takes no capacity (SREF-ERR-006).
