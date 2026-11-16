# Use live credential state with cached parent policies (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "replicas": [
    "A",
    "B"
  ],
  "primary": "shared",
  "routing_cache_on_B": "warm",
  "drop_change_notifications": true
}
```

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Credential: research

{"messages":[]}
```

## Expected response

200 before suspend; 503 after suspend. Validate the resource response or operation result described below.

## What to check

- Suspend on A and wait for success. A new live lookup on B returns 503 CREDENTIAL_SUSPENDED, including preview; zero secret reads or debits.
- A missing name is not cached: create it on A and B finds it immediately. Disable/delete/recreate also take effect through the live row.
- Change ancestor sharing to enforce on A while B's old routing entry is still valid. B may use the previous policy within ADR-0020's stale bounds, but never ignore the current credential state.
- Expire/invalidate and reload B's parent entry: enforce now returns 403 CREDENTIAL_POLICY_DENIED. Deny unapproved new endpoints before any secret read.
- SDK preview/proxy follows the same freshness rules. A previously admitted stream may finish.
