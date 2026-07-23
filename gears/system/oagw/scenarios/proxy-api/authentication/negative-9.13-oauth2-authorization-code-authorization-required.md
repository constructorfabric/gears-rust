# Outbound auth: OAuth2 authorization code — authorization required

When the calling subject has no stored token for the upstream, the proxy call
fails with a typed, actionable error rather than a generic authentication
failure.

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

## Scenario: no stored authorization

A subject who has never enrolled (or whose token was revoked) issues a proxy
request to the upstream.

Expected:
- The response is `CanonicalError::Unauthenticated` (HTTP `401`) with a stable
  `type`.
- The `reason` is `AUTHORIZATION_REQUIRED` (distinct from `AUTH_PLUGIN_FAILED`
  and `AUTH_PLUGIN_INTERNAL`).
- `resource_name` is the upstream GTS id, and the context carries an
  `authorization_uri` the caller can send the user to.
- `oagw_sdk::reason::auth::FailureReason::from(&err)` projects to
  `AuthorizationRequired { authorization_url }`.
- No bearer header is injected and no upstream request is made.
