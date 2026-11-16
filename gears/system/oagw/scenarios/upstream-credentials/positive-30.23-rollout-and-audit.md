# Keep wrong-account forwarding out of rollout (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "replicas": [
    "old",
    "capable"
  ],
  "named_admission": "blocked until all capable"
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

Rejected at admission until rollout completes. Validate the resource response or operation result described below.

## What to check

- Use an old replica as a negative control: it would strip the selector and send key-D. The release procedure must route no named request to it, including SDK paths.
- After every target/writer is capable, enable named calls and verify key-A/key-B. Before rollback, block, drain and freeze credential writes, including PATCH; fence registry writers.
- Inspect create/replace/delete/suspend/resume audit entries including repeats and registry source. Assert actor/source, tenant, credential UUID, action, UTC time and result.
- Canary secret values, auth headers, reference values and pins must not appear in errors/audit. Metrics contain no names, IDs or references.
