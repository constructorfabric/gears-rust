# Named upstream credentials (planned)

These are phase-1 acceptance specifications for [ADR-0021](../../docs/ADR/0021-upstream-credentials.md), not executable tests. The [scenario index](../INDEX.md#named-upstream-credentials-planned) lists every case.

## Shared setup

Use a tenant-local HTTPS mock provider at `api.example.com`, a CredStore spy, a controllable routing cache, a fixed limiter clock and recorded policy calls. Resolve/intercept the fixture host through the test transport; never send requests to a public provider. Run repository cases on the same supported backends as the existing four-level alias tests. Multi-replica cases share a primary database but have independent caches and limiters.

Create the following upstream with `POST /oagw/v1/upstreams` (201); use the returned GTS identifier as `<upstream-id>`. Requests use gear-relative paths; deployments with the `/api` mount prepend it.

```json
{
  "server": {"endpoints": [{"scheme":"https","host":"api.example.com","port":443}]},
  "protocol":"gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "auth": {
    "type":"gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1",
    "config":{"header":"X-Api-Key","prefix":"","secret_ref":"cred://provider-default"}
  }
}
```

Create one route with `POST /oagw/v1/routes` (201):

```json
{
  "upstream_id":"<upstream-id>",
  "match":{"http":{"methods":["POST"],"path":"/v1/chat/completions","path_suffix_mode":"disabled"}}
}
```

Seed the CredStore fixture before upstream/credential creation. These are mock data, not a new CredStore API:

```json
{
  "tenant_secrets":[
    {"reference":"provider-default","record_id":"11111111-1111-4111-8111-111111111111","value":"key-D"},
    {"reference":"provider-research","record_id":"22222222-2222-4222-8222-222222222222","value":"key-A"},
    {"reference":"provider-production","record_id":"33333333-3333-4333-8333-333333333333","value":"key-B"}
  ],
  "credential_bodies":[
    {"name":"research","secret_ref":"cred://provider-research","rate_limit":{"requests_per_minute":1}},
    {"name":"production","secret_ref":"cred://provider-production","rate_limit":{"requests_per_minute":60}}
  ]
}
```

POST each credential body to `/oagw/v1/upstreams/<upstream-id>/credentials` (201). Save each returned credential GTS ID and internal UUID in the harness. Start with an API-managed local parent, no ancestor, no upstream/route rate limit and no extra header rules. Unless a case overrides them, grant proxy invoke and tenant-wide credential create/read/override/delete/use, allow the caller to read the three tenant secrets, and enable CRUD. Reset spy counters after provisioning.

## Request and response notation

`<tenant-token>` identifies the caller. Each file's setup JSON lists changes to this shared fixture. It is harness configuration rather than a management payload unless the file names a management operation.

The default proxy body is `{"messages":[]}`; the mock returns `{"ok":true}`. Successful named requests must carry the chosen key exactly once and no `X-OAGW-Credential`, including after passthrough or transforms. Problem examples show asserted fields only; additionally validate the canonical category/type, instance, trace ID and relevant quota/permission fields. New named error codes use `error_domain=oagw.credential.v1`. Never assert dynamic trace IDs or diagnostic text as literals.

Preview uses the new SDK request with alias `api.example.com`, method `POST`, path `/v1/chat/completions` and the tested name. It performs the same pre-secret eligibility checks but makes no secret read, debit or provider call.

GET/POST/PUT/PATCH responses validate against `definitions.response` in the [credential schema](../../docs/schemas/credential.v1.schema.json); GET collection validates against `definitions.list`. Use the schema definitions directly (do not combine their entry point with the root create schema).
