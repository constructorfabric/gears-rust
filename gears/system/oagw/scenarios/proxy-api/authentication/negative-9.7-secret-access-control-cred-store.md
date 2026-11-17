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
      "secret_ref": "cred://partner-only-secret"
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

Use a `secret_ref` (or OAuth2 `client_id_ref` / `client_secret_ref`) that does not exist.

Expected on create or update of the upstream:
- `400 Bad Request` with `type` = `...failed_precondition...`, `context.resource_type` = `gts.cf.core.oagw.upstream.v1~`, and one violation: `type` = `STATE`, `subject` = the key (`auth.config.client_id_ref`). The response does not contain the reference.

A reference the credential store refuses for another reason (for example access denied) is a `400` `...invalid_argument...` field violation on the key, reason `VALIDATION`; a store that is down or does not answer within 5 s is a `500`.

The write-time check is best effort. If the secret is deleted after the write, expected at proxy time:
- `500 Internal Server Error` with the canonical `type` = `...internal...` and a generic `detail`. (The status for a secret missing at proxy time is an open #4985 decision; xfail PLG-10 tracks a 401.)
- The response does not contain the reference. The server log names the auth config key (`auth.config.secret_ref`, `auth.config.client_id_ref` or `auth.config.client_secret_ref`) and never the reference, nor the credential store's own error text, which may quote it. The same holds when the credential store fails at proxy time. Log content is covered by Rust tests; E2E checks the response.

## Scenario C: malformed reference

Use a reference that is not a valid secret reference, for example `"secret_ref": "cred://not a ref"`. The `cred://` prefix is optional; the rest may only contain `[a-zA-Z0-9_-]`.

Expected on create or update of the upstream, before the credential store is asked:
- `400 Bad Request` with `type` = `...invalid_argument...` and one field violation: `field` = `auth.config.secret_ref` (the key), `reason` = `VALIDATION`, `description` = `auth.config.secret_ref: invalid credential reference: contains invalid characters; only [a-zA-Z0-9_-] are allowed`. The response does not contain the reference.

`secret_ref`, `client_id_ref` and `client_secret_ref` are checked whenever they are present, whatever the auth plugin: a noop upstream with a malformed `secret_ref` is refused too.
