# OAuth2 identity provider unavailable

The OAuth2 client credentials auth plugin fetches its token from the identity provider the tenant configured. When that token endpoint is down, the request fails with a reason that names the identity provider, not the credential store.

## Upstream configuration

```json
{
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1",
    "config": {
      "token_endpoint": "https://<oauth-host>/oauth/token",
      "client_id_ref": "cred://vendor-client-id",
      "client_secret_ref": "cred://vendor-client-secret"
    }
  }
}
```

The secrets exist and no token is cached.

## Inbound request

```http
POST /oagw/v1/proxy/<alias>/echo HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"test":true}
```

The token endpoint answers `503`.

## Expected response

```http
HTTP/1.1 503 Service Unavailable
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway
Retry-After: 5
```

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
  "title": "Service Unavailable",
  "status": 503,
  "detail": "Service temporarily unavailable",
  "instance": "/oagw/v1/proxy/<alias>/echo",
  "context": { "retry_after_seconds": 5, "reason": "IDENTITY_PROVIDER_UNAVAILABLE" },
  "trace_id": "<trace id>"
}
```

## What to check

- The upstream receives nothing.
- The same response when the token endpoint is unreachable, times out, or answers `408`, `429` or another `5xx`. The identity provider's own `Retry-After` is not passed on.
- The SDK gets `Unavailable { retry_after_secs: Some(5), reason: Some(UnavailableReason::IdentityProviderUnavailable) }`.
- A credential store outage while the plugin reads `client_id_ref` or `client_secret_ref` is `503` `CREDENTIAL_STORE_UNAVAILABLE` instead ([9.7](negative-9.7-secret-access-control-cred-store.md)).
- An identity provider that rejects the client (`400`, `401`, `403` to the token request) is `401` `AUTH_PLUGIN_FAILED`; one that answers with an invalid response (discovery `400`, `401` and `403` included) is `502` `REQUEST_NOT_SENT`.
- The gateway logs one WARN, `auth plugin: OAuth2 identity provider unavailable`, with the caller's tenant, upstream id and alias, and plugin type. Log content is covered by Rust tests; E2E checks the response.
- The failure does not count toward the upstream's circuit breaker.
