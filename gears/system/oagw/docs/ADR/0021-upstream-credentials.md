---
description: "A caller can select an upstream API key by name and limit requests through that key."
status: proposed
date: 2026-11-12
---

# Named Upstream Credentials

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Resource and management API](#resource-and-management-api)
  - [Ownership and sharing](#ownership-and-sharing)
  - [Request sequence and SDK](#request-sequence-and-sdk)
  - [Errors and observability](#errors-and-observability)
  - [Request limits](#request-limits)
  - [Storage and provisioning](#storage-and-provisioning)
  - [Revocation and deployment](#revocation-and-deployment)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. Alternate hostname alias](#a-alternate-hostname-alias)
  - [B. Route authentication](#b-route-authentication)
  - [C. Allowlisted secret reference](#c-allowlisted-secret-reference)
  - [D. Named upstream credential](#d-named-upstream-credential)
  - [E. Consumer supplies the key](#e-consumer-supplies-the-key)
  - [F. Named map in upstream auth config](#f-named-map-in-upstream-auth-config)
- [Open Questions](#open-questions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-upstream-credentials`

## Context and Problem Statement

A tenant uses several accounts at the same provider hostname. Each account calls the same method and path through one upstream and route. The consumer chooses the account and handles retries, failover, usage accounting, prices, spend and budgets. It needs to tell OAGW which credential to use without handling the key value itself. If OAGW cannot use that credential, it must return an error rather than send the request under another account.

Today OAGW derives the alias of a single-host upstream from its hostname and includes a nonstandard port. Pools can share a domain suffix; IP addresses and pools without a usable suffix need an explicit alias. Within a tenant, two upstreams cannot claim the same alias. Management returns a conflict and registry provisioning reports `duplicate_alias`. See the [alias rules](../../oagw/src/domain/services/management/alias.rs) and [upstream model](../../oagw/src/infra/storage/entity/upstream.rs).

The proxy checks permission to invoke, then [resolves the alias and route](../../oagw/src/domain/services/management/resolution.rs) across visible, enabled upstreams. The [tenant merge](../../oagw/src/domain/services/management/merge.rs) and [bind rules](../../oagw/src/domain/services/management/bind.rs) protect the credential owner's destinations and preserve enforced ancestor policies. Sharing an alias alone grants no right to send a secret to an endpoint.

The upstream chooses the [auth plugin](../features/0003-cpt-cf-oagw-feature-plugin-system.md). The [API-key plugin](../../oagw/src/infra/plugin/apikey_auth.rs) reads its configured secret under the invoking caller's security context. A consumer can also obtain a key itself and send an authenticated request through a suitably configured upstream. This proposal adds an OAGW-owned credential path. It does not require existing consumers to replace their key handling; adoption is a separate integration decision.

## Decision Drivers

- Let any authorized consumer choose a credential on the same alias and route without receiving its secret value.
- Preserve tenant boundaries, destination approvals and ancestor policy enforcement.
- Give each credential an independent request limit, lifecycle and audit identity.
- Retain ADR-0020's availability for routing configuration while checking credential state on every named request.
- Keep account selection and monetary accounting with the consumer.

## Considered Options

1. Allow another hostname alias for the provider.
2. Attach authentication to routes.
3. Accept a secret reference from an upstream allowlist.
4. Name a credential under the upstream.
5. Let the consumer retrieve and inject the key.
6. Store a named map in the upstream's auth config.

## Decision Outcome

We propose **D, named upstream credentials**, starting with tenant-local API keys. The upstream still chooses the auth plugin, header and prefix. Routes keep their guards and transforms. [ADR-0010](0010-resource-identification.md) calls the upstream row a Layer 2 tenant binding; the new credential is a separate child with its own `credential_id`. Phase 1 specifies this design. Implementation requires a separate approval.

### Resource and management API

Manage credentials at `/oagw/v1/upstreams/{id}/credentials`. The parent path takes the same GTS identifier as upstream CRUD. POST creates a credential (201), GET lists credentials (200) using the existing limit and offset, and `/{name}` supports GET (200), PUT (200), DELETE (204) and PATCH (200). PATCH accepts exactly `{"suspended":true|false}`.

The [schema](../schemas/credential.v1.schema.json) defines create, replace, registry, suspension, response and list bodies. PUT replaces configuration and uses the create defaults for omitted optional fields. A client cannot change tenant, parent, name or UUID. A name collision returns 409; neither the API nor the registry takes over a credential from the other source.

REST responses use GTS identifiers for credential and parent IDs, UUIDs for tenant IDs, and UTC RFC 3339 timestamps. SDK models use UUIDs for resource IDs. GET, POST, PUT and PATCH return the same resource shape; list returns an array of that shape. Responses include `enabled`, `suspended`, `managed_by` (`api|registry`), approved endpoints, secret reference, optional limit, tags and timestamps. The record pin, row version, limit generation and registry source UUID remain internal. OAGW never returns secret values.

Credential CRUD respects the management write mode in both REST and the new SDK capability. This is an explicit rule for the new resource, not a claim about existing upstream SDK writes. Operational suspend and resume remain available in both transports when `management_api_enabled=false`, including for registry credentials. Read operations remain available. Every operation still requires its policy grant.

Names start with a lowercase ASCII letter and may contain lowercase letters, digits, underscores and hyphens, up to 63 characters. OAGW rejects other spellings and reserves `default`; clients cannot create or explicitly select it. Omitting the selector retains existing upstream auth behavior, including upstreams without auth. No default row or backfill is required.

The local parent must configure the built-in API-key plugin. It supplies the header and prefix; the credential supplies only the secret reference. Create and replacement validate the authoritative management hierarchy and compatible auth; replacement also requires the current server to fit the existing endpoint approvals. Use and preview check compatibility against the routing configuration they resolve. The plugin cannot inject a key into reserved internal or routing headers.

On create, OAGW normalizes the secret reference, reads CredStore as the writer and stores `GetSecretResponse.id`. CredStore may resolve the same reference to a tenant record for one subject and a private record for another. On a named proxy request, the plugin reads once under the invoking caller's context, compares that response's ID with the pin, and uses the value from that same response only on a match. A mismatch returns 403 before forwarding and discloses neither ID. See the [CredStore response](../../../../credstore/credstore-sdk/src/models.rs).

An unchanged normalized reference must retain its record pin through PUT, resume and registry replay. To use another reference, the owner explicitly changes the configuration; OAGW captures its pin as the writer and updates both atomically. To accept a recreated record under the same reference, delete and recreate the credential. Rotating the value within the pinned record needs no OAGW change. Preview does not read CredStore, so it cannot certify secret access or a pin match.

Named OAuth credentials and exporting named credentials to descendants are outside this proposal. Existing OAuth requests keep their current token cache. A future extension must preserve the upstream's issuer and auth-method authority and the [OAuth cache's isolation checks](../../oagw/src/infra/plugin/oauth2_client_cred_auth.rs).

### Ownership and sharing

A tenant attaches credentials only to an upstream it owns. A descendant needing its own key first creates a local upstream with the same alias through the existing bind path. Ancestors may share upstreams, but their named credentials stay private. Lookup uses the caller's tenant and the selected local upstream UUID, even when an ancestor owns the matched route.

The PDP resource type is `gts.cf.core.oagw.credential.v1~`; its supported property is `owner_tenant_id`. Item requests use the credential UUID as resource ID, while create and list use no item ID. The verified local parent supplies the owner, never the request body. Actions follow the management vocabulary in [PR #5963](https://github.com/constructorfabric/gears-rust/pull/5963):

| Operation | Credential action |
|---|---|
| POST | create |
| GET item or list | read |
| PUT, suspend or resume | override |
| DELETE | delete |
| Named proxy or preview | use, plus existing proxy invoke |

Names and parent IDs locate the resource; they are not undeclared PDP properties. Before item lookup, check the operation at credential type/owner scope with no item ID; this makes an operation denial independent of name existence. After a scoped lookup finds a row, check its UUID before exposing state. Apply the enforcer's tenant scope before pagination. Refuse constraints narrower than the repository can enforce with `AUTHZ_SCOPE_NOT_SUPPORTED`; never turn an unsupported constrained grant into tenant-wide access. This first delivery requires tenant-wide action grants and does not promise per-name grants. Registry reconciliation retains its existing trusted provisioning path, with owning-tenant CredStore checks rather than a runtime management grant.

| Visible ancestor auth above the local parent | Create/replace local credential | Use/preview local name | Access ancestor credential |
|---|---|---|---|
| No visible ancestor | Local create/override | Local use | Invisible |
| Absent auth on a visible ancestor | Local create/override + override_auth on that ancestor | Local use | Invisible |
| private | Local create/override | Local use | Invisible |
| inherit | Local create/override + override_auth on that ancestor | Local use | Invisible |
| enforce anywhere in the effective chain | Block | Block, including existing credentials | Invisible |

Bind and `override_auth` are management checks. An invoking caller needs `use`, not management permission to change an ancestor's auth. The closest visible ancestor determines the override check; any more distant `enforce` still takes precedence. Proxy and preview reapply the hierarchy checks to their resolved configuration; parent-policy freshness follows ADR-0020, as described below.

A credential serves traffic only when `enabled && !suspended`. New credentials start with `suspended=false`. An owner with `override` may suspend an API or registry credential even when CRUD is off, CredStore is down, the parent plugin is incompatible or an ancestor enforces auth. Suspension checks identity, tenant, parent and permission, but skips secret, destination and ancestor override checks. Every successful suspend increases the row version, including a repeat on an already suspended credential.

Only an explicit resume clears suspension. It requires enabled configuration, compatible auth, approved endpoints, current management hierarchy and override grants, and a matching pinned secret under the acting manager. On failure, suspension remains. Resume compares the UUID and row version it validated. A concurrent change, including a repeated suspend, returns 409; the caller must start a new resume. PUT and registry replay preserve the latest suspension.

Create captures normalized host, port and scheme tuples from the authoritative parent. Every named use checks the entire resolved server with `server_within` against that approval before reading the secret. The target-host header can select only a member of that checked pool. Removing endpoints is safe; adding unapproved endpoints blocks use once the new server reaches the cache. PUT, secret rotation, suspend/resume and registry replay never expand approvals; approving a larger pool requires recreation.

### Request sequence and SDK

Both requests use the same alias, method, path and body. Only the credential name differs:

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Content-Type: application/json
X-OAGW-Credential: research

{"model":"example","messages":[]}
```

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Content-Type: application/json
X-OAGW-Credential: production

{"model":"example","messages":[]}
```

```mermaid
sequenceDiagram
    participant C as Consumer
    participant P as OAGW proxy
    participant K as Routing cache
    participant R as Credential repository
    participant A as Policy enforcer
    participant S as CredStore
    participant U as Provider
    C->>P: Alias, route and credential name
    P->>A: Check invoke
    P->>P: Validate and consume selector
    P->>K: Resolve upstream, route and hierarchy
    P->>A: Check type/owner use
    P->>R: Read current tenant-local credential
    P->>A: Check item use
    P->>P: State, hierarchy, plugin and destination checks
    P->>P: CORS and input checks; preflight returns here
    P->>P: Debit upstream, credential, route limits
    P->>S: Read secret under caller context
    S-->>P: Record ID and value
    P->>P: Check pin; inject that response's value
    P->>P: Remaining guards/transforms and final header/endpoint checks
    P->>U: Forward with selected key
    U-->>C: Response through OAGW
```

Header names are case-insensitive. Accept exactly one lowercase credential name; reject empty, malformed, repeated or comma-separated values, even when duplicate values match. URI and query carry no selector. Consume the header before the pipeline and strip it again before forwarding.

After all header changes, verify that outbound auth still contains exactly the value the plugin supplied. Removal, replacement or duplication stops forwarding without logging values. Existing route, disabled-upstream, CORS, SSRF, pinned-address, timeout, TLS and guard rules still apply. SSE and WebSocket handshakes count once. Preflight neither reads a secret nor consumes capacity.

Keep `ServiceGatewayClientV1` and its `resolve_proxy_target` result intact. Consumers set the header on its existing `proxy_request` HTTP request. Register a separate `ServiceGatewayCredentialsClientV1` for CRUD, `suspend_credential(ctx, upstream_id, name)`, `resume_credential(ctx, upstream_id, name)` and `preview_credential(ctx, request)`. The non-exhaustive `PreviewCredentialRequest` contains `alias`, `method`, `path` and `name`. Capability lookup fails when a deployment lacks it.

Preview applies invoke/use, visibility, state, hierarchy, plugin and endpoint checks using the same cached routing and live credential policy as proxy. It returns only upstream, route and credential UUIDs and the resolved name. It never fetches a secret, debits capacity or calls the provider.

### Errors and observability

Use canonical RFC 9457 Problems. New credential failures use the existing contract-error envelope: `error_domain=oagw.credential.v1` and a stable `error_code` from the table below. The new SDK capability exposes matching typed credential errors; existing proxy HTTP responses carry the same envelope. This avoids adding a reason field to the canonical 503 context, which currently has only retry guidance. Existing permission, validation and quota errors retain their own context discriminators. Check invoke and selector syntax first. Check operation permission and tenant/item visibility before exposing state. For an authorized item, check disabled state, then suspension, then hierarchy/plugin/destination eligibility. These checks precede rate admission and secret access. Operation-level denial is 403 independently of name existence; absent, foreign or wrong-parent credentials give indistinguishable 404s.

| Condition | Status and discriminator | Retry guidance |
|---|---|---|
| Invalid selector | 400, field reason INVALID_CREDENTIAL_SELECTOR | Correct the request |
| Unsupported create/replace payload | 400, existing field validation reasons | Correct the request |
| Absent, foreign or wrong-parent item | 404, no distinguishing reason | Fix provisioning or access |
| Operation permission denied | 403, existing permission reason | Fix the grant |
| Unsupported policy constraint | 403, AUTHZ_SCOPE_NOT_SUPPORTED | Use a supported scope |
| Disabled credential | 503, CREDENTIAL_DISABLED | Wait for an operator change |
| Suspended credential | 503, CREDENTIAL_SUSPENDED | Wait for explicit resume |
| Incompatible parent auth, enforced override or unapproved destination | 403, CREDENTIAL_POLICY_DENIED | Fix configuration or policy |
| Different CredStore record ID | 403, CREDENTIAL_PIN_MISMATCH | Repair provisioning; no automatic repin |
| Secret missing or inaccessible to this caller | 503, CREDENTIAL_SECRET_UNAVAILABLE | Repair provisioning or access; no blind retry |
| CredStore dependency outage/timeout | 503, CREDENTIAL_STORE_UNAVAILABLE | May retry with backoff |
| Live credential read unavailable/timeout | 503, CREDENTIAL_CONFIG_UNAVAILABLE | May retry with backoff |
| Name conflict | 409, CREDENTIAL_NAME_CONFLICT | Resolve ownership/name conflict |
| Pending resume or write lost its validated version | 409, CREDENTIAL_VERSION_CONFLICT | Read again; issue a new operation |
| Gateway request limit exhausted | 429, existing quota details | Respect Retry-After |

Disabled and suspended responses use the same administrative `Retry-After` as a [disabled upstream](../../oagw/src/api/rest/error.rs); it is a polling hint, not a promise of automatic recovery. These reasons and status choices apply to named credentials only. They do not reuse the legacy auth-plugin mapping that can report a dependency failure as 401. Management validation uses the same pin/dependency reasons; secret-reference syntax errors remain 400. Routing failures otherwise follow the existing proxy contract.

Before forwarding, every failure produces zero provider calls. Denials before rate admission make no secret read or debit; secret failures occur after admission and retain its debits. Error bodies never contain secret values, references, record pins, auth headers or raw selector input. Pin and missing/inaccessible-secret responses do not reveal the underlying record's existence.

Log `credential_id` with upstream, route and request IDs only after authorized selection. Keep metric labels to `credential_mode=default|named`; IDs, names and secret references are not labels. Extend [configuration audit](../features/0008-cpt-cf-oagw-feature-observability.md#audit-log-emission) to create, replace, delete, suspend and resume, including repeated suspends and registry changes. Record actor/source, tenant, credential UUID, action, UTC time and result, with no secrets or reference values.

Keep the distinction between a gateway 429 and a provider 429. Preserve the provider's error source and retry guidance. The consumer decides whether to retry or select another account; OAGW never does so automatically.

### Request limits

An optional `rate_limit.requests_per_minute` is a positive u32. Without it, existing upstream and route limits still apply. Each replica shares the credential's bucket across its callers, as in [ADR-0004](0004-rate-limiting.md). A rate of 60 starts with capacity 60 and refills at one token per second. One admitted request costs one token.

After authorization, eligibility and preflight handling, check the effective upstream bucket (including shared `pool_owner_id`), then credential, then route, before CredStore. Stop at the first rejection. Earlier buckets retain their debits, even if a later limiter or secret read fails. This intentionally counts attempts and prevents exhausted keys from loading CredStore.

On success report the smallest remaining count; the first bucket wins ties. Send rate headers if any participating policy enables them; the credential limit enables them. On rejection the rejecting bucket supplies its retry and rate headers.

Add `RateLimitResource::Credential` to the [key builder](../../oagw/src/domain/rate_limit.rs). Use `oagw:ratelimit:credential:{credential_uuid}:{limit_generation}:global:minute`, with rate and generation from the same live credential row. Allocate a fresh generation UUID when a limit first takes effect, changes rate or returns after removal. An unchanged rate keeps the active generation across rotation, suspension/resumption and registry replay. Removing a limit clears its active generation.

Commit the rate and generation together. Clean up only the retired pair locally, then append `credential_limit_retired { tenant, credential, generation }` to the [ADR-0020 change log](0020-proxy-config-cache.md#the-change-log). Final deletion, including parent cascade, may append `credential_deleted { tenant, credential }` and remove all generations of that never-reused UUID. These additive variants are part of the coordinated rollout. They do not evict routing entries. When the deployment has no cluster change log, use local cleanup and the idle sweep; do not add a cluster dependency solely for counter reclamation.

Duplicate, delayed or reordered events cannot reset the current bucket. A missed event affects memory reclamation only; idle sweep removes abandoned counters. A request already admitted under an old row may finish against that generation. Keying only by credential UUID would allow alternating old/new requests to rebuild full buckets; generations avoid that reset.

### Storage and provisioning

Use a child table with a UUID primary key, composite foreign key `(tenant_id, upstream_id)` and `ON DELETE CASCADE`, plus unique `(tenant_id, upstream_id, name)`. Store name in case-sensitive VARCHAR(63), normalized secret reference in VARCHAR(255) without `cred://`, and tags in a child table with VARCHAR(128), following upstream tag storage. Apply the schema's byte/control-character checks before storage. Store the pin, both flags, limit/generation, approved endpoints, management source, optional registry source UUID, UTC timestamps and an internal monotonic row version. See [storage conventions](0009-storage-schema.md) and [persistence](0018-optional-persistence.md).

Use the enforcer's scope in both database and memory repositories. On create, first check permission and read CredStore outside the write transaction. Then lock and revalidate the authoritative parent, capture its endpoints and insert the child atomically. A changed parent requires fresh validation outside the transaction. No database or memory write lock may span CredStore, PDP or provider calls. Concurrent same-name creates produce one 201 and one 409.

PUT, registry update and resume read and validate outside write locks, then compare UUID and version at commit. Configuration changes increase the version. Repeat validation outside the transaction on a conflicting configuration write; return 409 for a conflicting resume or a replaced UUID. A no-op registry replay preserves the version. Every successful suspend increases it even when already suspended.

Compare reference and rate transitions with the latest row, never the initial read. An unchanged reference must match the current pin; an unchanged rate keeps the current generation. Retire only the generation actually replaced. PUT and registry updates preserve the latest suspension. A stale write cannot restore an earlier pin, version, generation or suspension flag. A changed reference that still needs pin capture requires another external validation before retry.

Only a registry-managed upstream may host a registry-managed credential, following the [route ownership rule](../../oagw/src/domain/services/management/mod.rs). Verify stored parent tenant/source in the same critical section as child create/update. Reject declarations under API parents without writing a child. API credentials may belong to either kind of local parent. On parent deletion, the transaction/cascade either removes concurrent new children or prevents their create. Old API binaries can delete API parents without leaving orphans or hitting a restrictive foreign key.

The registry envelope identifies the declaration's source UUID; its payload supplies the parent's canonical UUID and optional tenant using the existing default. The API takes tenant from context and parent from path. Reapply a live declaration to the same credential UUID. Removal and later reappearance allocate a new UUID. Name/parent changes require removal and recreation. The registry owns configuration, while operators own suspension.

Reconciliation normalizes declarations, rejects duplicate claims and preserves last valid rows. A declaration colliding with an API credential reports `credential_name_conflict`, keeps the API row intact and retries on later passes; it must not adopt or delete that row. Inaccessible secrets are retryable provisioning dependencies. A pin mismatch is a configuration error requiring operator repair, not an automatic repin.

Create parents before children; delete missing registry children before parents. Before parent removal, lock it and retain it while an API child or retained registry child depends on it. Fence old reconcilers before enabling credentials on registry parents because those binaries do not know the new child type. Registry provisioning retains its existing cross-tenant authority.

The operator or provisioning process owns onboarding: create the CredStore secret, create the local upstream credential as the intended writer/registry actor, then publish its name in the consumer's account configuration. These are separate systems with no distributed transaction. Keep the consumer mapping inactive until provisioning succeeds; failures return the documented error and never fall back to another key. Rotate values in place when possible. A recreated secret record requires deliberate credential recreation and may cause a provisioning gap.

### Revocation and deployment

Named proxy and preview resolve upstreams, routes and parent policies through [ADR-0020](0020-proxy-config-cache.md), including its TTL, stale-serving and change-log rules. They then perform one current point lookup for the credential using tenant, resolved local upstream UUID and name. Do not cache credential rows or misses. Read the primary database, not a lagging replica; the memory repository returns one current row. Check runtime permissions per invocation, subject to the policy service's own propagation guarantees.

A successful suspension, disabling, deletion or credential configuration change applies to requests whose live row read starts after that commit, including on other replicas sharing the primary. A failed or overdue credential read returns 503 with no stale-row fallback even when routing remains cached. Requests that already passed admission, including open streams, can finish.

Parent endpoint, plugin, enabled-state and sharing changes follow ADR-0020's cache freshness instead. A missed event or failed reload can keep the previous routing policy in use within its documented stale window. In particular, a newly enforced ancestor may block later than a credential suspension. This proposal does not promise immediate revocation of the entire hierarchy. Credential endpoint approvals still constrain every server resolved from that cache.

This trades a live point lookup per named request for prompt credential revocation without reloading the whole hierarchy. A routing cache hit does not guarantee service during a credential-database outage. Each memory process remains independent. Secret value and policy propagation belong to CredStore and the PDP. Limit cleanup notifications are optional for correctness; parent/alias/route invalidation remains unchanged.

Use this release order:

1. Deploy schema, capable proxies, management clients and registry/change-log workers. Keep credential writes and caller selectors off.
2. Verify every REST/SDK target, autoscaling image, failover pool and registry writer. Only then enable provisioning, grants and selectors.
3. Before rollback, block new named requests, drain existing ones and freeze all credential writes, including operational PATCH. Fence old registry writers or remove their credential dependencies before restarting them. Keep the new tables.

An old replica may strip the selector and use its default key. It must never receive a named request. No-selector calls retain their existing routing, auth and limit behavior.

### Consequences

Consumers store an alias/name mapping without receiving the key value. The separate credential adds one provisioning resource between the secret and that mapping, plus a live read per named request. Operators must coordinate provisioning, grants and mixed-version rollout. There is no atomic update across the three systems. Recreating a secret under the same name costs credential recreation; rotation within the same record avoids it.

The design keeps routing available through ADR-0020's cache but depends on the credential store database for named admission. Parent policies may be stale while credential state is current. Runtime users need no ancestor management grant. Per-replica limits retain their existing burst and scaling trade-offs; they are not provider-wide quotas or monetary budgets.

### Confirmation

The [scenario index](../../scenarios/INDEX.md#named-upstream-credentials-planned) links numbered setup/request/response specifications. Use mock providers, a CredStore spy and a controlled clock. Cover each error before/after admission, policy and endpoint checks, cached parents with live credential state, repeated suspend versus pending resume, registry ownership and delayed cleanup. Run storage cases on the same backends as the [four-level alias tests](../../oagw/src/domain/services/management/tests.rs). Future implementation checks must cover REST/SDK round trips and existing V1 implementations. Phase 1 runs documentation, link and schema validation only.

## Pros and Cons of the Options

### A. Alternate hostname alias

Existing CRUD can represent each account, but policy resolution follows the alias. Another alias could evade an ancestor's enforced policy. Safe use would require redefining destination identity.

### B. Route authentication

Equal method/path/priority can conflict. Different priorities still select one winner rather than let the caller choose an account on the same route. Route authentication would also move credential authority away from the upstream.

### C. Allowlisted secret reference

This is a small wire change, but exposes CredStore naming to consumers and still needs authorization, pinning, lifecycle, audit and per-key limits elsewhere.

### D. Named upstream credential

A UUID unifies lifecycle, use permission, suspension, record pin, endpoint approval and request limits. It preserves the route and upstream policy chain. Its costs are another provisioned resource and a live credential read.

### E. Consumer supplies the key

A consumer can fetch CredStore itself and inject auth through a suitably configured upstream. This remains valid for existing integrations and avoids another provisioning row. It exposes key values to each consumer and leaves per-key policy, suspension, rate accounting and lifecycle split between consumers. It does not supply the general OAGW-managed capability requested here.

### F. Named map in upstream auth config

A map such as `credential.research: cred://provider-research` is simple for lookup and keeps the existing upstream shape. Per-name pins, rate generations, independent ownership, approved endpoints and suspension that survives registry replay would need a separate state store anyway. A first-class resource makes those boundaries explicit.

## Open Questions

- Can every serving target and registry writer follow the release/rollback sequence?
- Does the existing per-replica burst policy meet the intended deployment's request profile?

The proposal fixes tenant-local API keys, runtime `use` without `override_auth`, cached parent policy with live credential state, and attempt counting before CredStore. Named OAuth and descendant credential sharing need a later decision.

## Traceability

- `cpt-cf-oagw-fr-upstream-credentials` in [PRD](../PRD.md)
- [Credential resolution](../features/0004-cpt-cf-oagw-feature-proxy-engine.md#credential-resolution) and [alias resolution](../features/0004-cpt-cf-oagw-feature-proxy-engine.md#alias-resolution)
- [Hierarchical config merge](../features/0005-cpt-cf-oagw-feature-tenant-hierarchy.md#hierarchical-config-merge)
- [System design](../DESIGN.md) and [planned work](../DECOMPOSITION.md)
