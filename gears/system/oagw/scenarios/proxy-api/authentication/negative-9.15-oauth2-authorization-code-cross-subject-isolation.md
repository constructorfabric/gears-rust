# Outbound auth: OAuth2 authorization code — cross-subject token isolation

Per-user tokens are stored with `Private` sharing. A subject must never receive
a token enrolled by another subject, even within the same tenant and even for an
identical upstream configuration.

## Upstream configuration

```json
{
  "alias": "mcp.example.com",
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "mcp.example.com", "port": 443 }
    ]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_auth_code.v1",
    "config": {
      "resource": "https://mcp.example.com/"
    }
  }
}
```

## Scenario A: distinct subjects in the same tenant

Subject A enrolls and obtains a token. Subject B (same tenant) has not enrolled.

Expected:
- Subject B's proxy request MUST NOT use Subject A's token.
- Subject B receives `Unauthenticated` with `reason = AUTHORIZATION_REQUIRED`.
- The token record is keyed by `(subject_id, upstream_id)`; `Private` sharing in
  `cred_store` scopes it to `owner_id = subject_id`.

## Scenario B: cross-tenant isolation

Subject A (Tenant X) is enrolled. Subject B (Tenant Y) accesses the same upstream
configuration.

Expected:
- Subject B's request MUST NOT use Subject A's token.
- CredStore resolution isolates by tenant as well as owner, so an identical
  upstream config in another tenant resolves no token.

## Scenario C: revocation is subject-scoped

Subject A revokes their authorization while Subject B remains enrolled.

Expected:
- Only Subject A's token is deleted.
- Subject B's proxy calls continue to succeed with their own token.
