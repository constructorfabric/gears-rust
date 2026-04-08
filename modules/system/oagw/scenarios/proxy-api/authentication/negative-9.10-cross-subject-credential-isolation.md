# Cross-subject credential isolation (token cache)

Applies when token caching is introduced. Two subjects within the same tenant
must never receive a cached token that was fetched using the other subject's
credentials.

## Upstream configuration

```json
{
  "alias": "vendor.example.com",
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "vendor.example.com", "port": 443 }
    ]
  },
  "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
  "auth": {
    "type": "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred.v1",
    "config": {
      "token_url": "https://<oauth-host>/oauth/token",
      "client_id_ref": "cred://vendor/oauth2/client_id",
      "client_secret_ref": "cred://vendor/oauth2/client_secret"
    }
  }
}
```

## Scenario A: private-mode secrets resolve differently per subject

CredStore holds private-mode secrets for the same reference:

- Subject A owns `cred://vendor/oauth2/client_id` → `client-id-A`
- Subject B owns `cred://vendor/oauth2/client_id` → `client-id-B`

Subject A proxies a request. The plugin resolves Subject A's credentials and
fetches a token from the IdP.

Subject B then proxies a request to the same upstream.

Expected:
- Subject B's request MUST NOT use Subject A's cached token.
- The plugin resolves Subject B's credentials independently.
- The IdP receives Subject B's `client-id-B`, not `client-id-A`.

## Scenario B: tenant-mode secrets with distinct subjects

CredStore holds a tenant-mode secret accessible to all subjects in the tenant.
Both Subject A and Subject B resolve the same `client_id` and `client_secret`.

Subject A proxies a request. Subject B then proxies a request.

Expected:
- Both subjects resolve the same credentials (tenant-mode).
- The cache key includes `subject_id`, so each subject gets an independent
  cache entry. This is a deliberate over-partition for safety — the cache
  cannot know the sharing mode at lookup time.

## Scenario C: cross-tenant isolation

Subject A belongs to Tenant X. Subject B belongs to Tenant Y. Both tenants
have access to the same upstream configuration.

Subject A proxies a request. Subject B then proxies a request.

Expected:
- Subject B's request MUST NOT use Subject A's cached token.
- The cache key includes `subject_tenant_id`, ensuring full tenant isolation
  even if the upstream config is identical.
