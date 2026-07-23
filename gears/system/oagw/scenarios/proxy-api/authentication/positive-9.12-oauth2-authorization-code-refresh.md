# Outbound auth: OAuth2 authorization code — bearer injection and transparent refresh

Once a subject is enrolled, proxy calls inject the stored token and refresh it
transparently before it expires. No user interaction is involved.

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

## Scenario A: valid token injected

The subject's stored token is well within its validity window.

Expected:
- The plugin injects `Authorization: Bearer <access_token>`.
- No token endpoint call is made.

## Scenario B: near-expiry token refreshed

The stored access token is within the 60-second refresh margin of expiry and a
refresh token is present.

Expected:
- The plugin exchanges the refresh token at the token endpoint.
- The rotated record (new access token, and rotated refresh token when the
  server issues one) is written back to `cred_store` under the same
  `Private` reference.
- The request proceeds with the refreshed bearer token.
- The plugin does not retry the original upstream request beyond obtaining a
  valid token.
