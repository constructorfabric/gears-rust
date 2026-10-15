# Secret access control via cred_store

## Scenario A: cred_store denies access

Upstream auth references a secret not accessible to the calling tenant:

```json
{
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1",
    "config": {
      "header": "Authorization",
      "prefix": "Bearer ",
      "secret_ref": "cred://partner-only/secret"
    }
  }
}
```

Invoke proxy as a tenant without secret access.

Expected:
- `401 Unauthorized`
- `Content-Type: application/problem+json`
- `X-OAGW-Error-Source: gateway`

## Scenario B: secret missing

Use `secret_ref` that does not exist.

Expected:
- `500 Internal Server Error` with `type` = `...secret.not_found...`.
- Applies the same way to any auth plugin backed by a `cred_store` reference, not just `apikey` — e.g. OAuth2 client-credentials' `client_id_ref`/`client_secret_ref` pointing at nonexistent secrets fails identically.
