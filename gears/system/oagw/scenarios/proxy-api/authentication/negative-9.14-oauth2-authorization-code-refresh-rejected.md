# Outbound auth: OAuth2 authorization code — refresh rejection forces re-authorization

When the authorization server rejects the refresh token (revoked, expired, or
consent withdrawn), OAGW discards the stale record and requires the user to
re-consent.

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

## Scenario A: refresh token rejected

The stored token is expired and the token endpoint responds with
`invalid_grant` to the refresh attempt.

Expected:
- The plugin deletes the subject's stored token from `cred_store`.
- The proxy call returns `Unauthenticated` with `reason = AUTHORIZATION_REQUIRED`
  and an `authorization_uri`.
- The next enrollment (`authorize` → browser consent → callback) restores access.

## Scenario B: expired token with no refresh token

The stored token is expired and no refresh token was issued.

Expected:
- The plugin does not call the token endpoint.
- The proxy call returns `Unauthenticated` with `reason = AUTHORIZATION_REQUIRED`.

## Scenario C: explicit revocation

`DELETE /oagw/v1/upstreams/{id}/oauth` is called for the subject.

Expected:
- The subject's stored token is removed (idempotent — succeeds whether or not a
  token existed).
- Subsequent proxy calls return `AUTHORIZATION_REQUIRED` until re-enrollment.
