# Keep approvals on replace (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "new_parent_endpoint": {
    "scheme": "https",
    "host": "other.example.com",
    "port": 443
  },
  "reload_parent_cache": true
}
```

## Inbound request

```http
PUT /oagw/v1/upstreams/<upstream-id>/credentials/research HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"secret_ref":"cred://provider-research","rate_limit":{"requests_per_minute":1}}
```

## Expected response

```http
HTTP/1.1 403 Forbidden
Content-Type: application/problem+json

{
  "status": 403,
  "error_domain": "oagw.credential.v1",
  "error_code": "CREDENTIAL_POLICY_DENIED"
}
```

## What to check

- The invalid PUT does not expand approvals or alter the existing row.
- Restore the parent pool and repeat PUT (200), then add the other endpoint again: GET still reports the original approvals and proxy/preview reject the expanded pool.
- Also check unchanged approvals after rotation, enabling/disabling and registry replay. A target-host selection cannot bypass the check.
