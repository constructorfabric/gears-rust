# Outbound auth: OAuth2 authorization code — interactive enrollment

Per-user delegated access. The user consents once through the browser; OAGW
stores the resulting token and injects it on subsequent proxy calls.

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
      "resource": "https://mcp.example.com/",
      "scope": "profile email"
    }
  }
}
```

## Flow

1. `POST /oagw/v1/upstreams/{id}/oauth/authorize` with
   `{ "scope": "profile email", "return_to": "https://app.example/connected" }`.
2. OAGW discovers the authorization server from `resource`, registers (or reuses)
   a PKCE client, generates a `code_verifier` and CSRF `state`, stashes the
   pending record, and returns `{ authorization_url, state }`.
3. The user consents in the browser; the authorization server redirects to the
   configured `redirect_uri` with `code` and `state`.
4. The redirect reaches OAGW's standard callback `GET /oagw/v1/oauth/callback?code&state`
   — directly (exposed OAGW) or reverse-proxied by the BFF (internal OAGW).

## What to check

- The `authorization_url` carries `response_type=code`, `code_challenge`,
  `code_challenge_method=S256`, the configured `redirect_uri`, and the returned
  `state`.
- The callback matches the pending record by `state` (single-use) and exchanges
  the `code` with the `code_verifier` and the same `redirect_uri` from the
  pending record (so exposed and relayed paths behave identically).
- The resulting token is stored in `cred_store` with `SharingMode::Private`,
  scoped to the calling subject; the stored value is opaque (never logged).
- The pending record is removed after a successful exchange, and the callback
  redirects the browser to `return_to`.
- `GET /oagw/v1/upstreams/{id}/oauth` then reports `connected: true` with the
  access-token expiry.
- A subsequent proxy request injects `Authorization: Bearer <access_token>`.
