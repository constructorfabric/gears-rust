# Hierarchical auth sharing modes

## Scenario A: inherit + child override

Parent upstream:

```json
{
  "alias": "api.openai.com",
  "auth": {
    "sharing": "inherit",
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1",
    "config": { "secret_ref": "cred://partner/openai" }
  }
}
```

Child binding provides its own auth:

```json
{
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1",
    "config": { "secret_ref": "cred://customer/openai" }
  }
}
```

Expected:
- Effective auth uses child secret if override is allowed.

## Scenario B: enforce

Parent sets `auth.sharing=enforce`.

Expected:
- Child cannot override auth.

## Scenario C: private

Parent sets `auth.sharing=private` (example: apikey with `secret_ref`) and shares something else, so its upstream is visible to the child (example: `rate_limit.sharing=inherit`).

Expected:
- The parent's own requests carry its credential.
- A child request never carries it: the upstream receives no `Authorization` from OAGW (the client's own token is not forwarded either).
- This holds when the child has no upstream of its own and the alias resolves to the parent's upstream, and when the child binds the alias without auth.
- The child must provide its own auth.
