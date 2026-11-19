# Per-request secret references

Acceptance scenarios for [ADR-0022](../../docs/ADR/0022-request-secret-refs.md): a caller names a CredStore secret in `X-OAGW-Secret-Ref`, and OAGW reads it as the caller and injects it under the upstream's guards. The [scenario index](../INDEX.md#per-request-secret-references) lists every case. The `SREF-*` IDs in each file are the ADR's confirmation IDs; the tests that prove them carry the same IDs.

## Shared setup

Tenant tree: Root → Partner → Tenant, and Other, Tenant's sibling under Partner. Use a mock provider at `api.example.com` that records each request, and a CredStore that records every read with the context it ran under.

Create the upstream with `POST /oagw/v1/upstreams` (201) as Tenant, unless a case says otherwise:

```json
{
  "server": {"endpoints": [{"scheme": "https", "host": "api.example.com", "port": 443}]},
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1",
    "sharing": "private",
    "config": {
      "header": "Authorization",
      "prefix": "Bearer ",
      "credential_mode": "request_ref"
    }
  }
}
```

When a case says "then:" and shows JSON, merge it into this body. Its `auth.config` is merged into the shared `auth.config`: the keys it lists replace the shared values, and a key the case says to remove is removed. Other fields are added to the body.

Create one route with `POST /oagw/v1/routes` (201) for `POST /v1/chat/completions` with `path_suffix_mode` `disabled`.

Seed CredStore (mock data, not a new CredStore API):

| Reference | Owner | Visible to | Type | Value |
|---|---|---|---|---|
| `tenant-openai` | Tenant | Tenant | api-key | `key-T` |
| `partner-openai` | Partner | Partner, Tenant, Other | api-key | `key-P` |
| `platform-openai` | Root | everyone | api-key | `key-R` |
| `tenant-db` | Tenant | Tenant | connection-string | `key-DB` |

## Request and response notation

`<tenant-token>` authenticates as Tenant. The default body is `{"model":"gpt-4o","messages":[]}`; the mock returns `{"ok":true}`. Gateway errors are RFC 9457 problems with `X-OAGW-Error-Source: gateway`; the examples show only the asserted fields. No error body, log line or metric label may contain a reference, a secret value or an auth header.
