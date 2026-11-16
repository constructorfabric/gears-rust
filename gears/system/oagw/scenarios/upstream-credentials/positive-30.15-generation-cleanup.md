# Ignore delayed cleanup of retired counters (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "replicas": [
    "A",
    "B"
  ],
  "primary": "shared",
  "clock": "fixed",
  "cleanup_delivery": "paused"
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

First request 200; later request 429. Validate the resource response or operation result described below.

## What to check

- Remove research's limit on A, then restore 1/minute with a fresh generation. B admits exactly one request.
- Deliver the retired generation event twice and retry on B. It remains exhausted, with one provider call in total.
- Repeat with a rate change and reversed events. Drop notifications entirely: current enforcement is unchanged.
- Pause a same-rate PUT before removal and commit it after restoration and B's first debit: it keeps the current generation, not the original one.
- Rotation, unchanged replay, suspend/resume retain generation. Deleting/recreating the credential gets a new UUID. Idle sweep eventually reclaims unreachable generations.
