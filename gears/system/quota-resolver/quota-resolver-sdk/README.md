# cf-gears-quota-resolver-sdk

Public, transport-agnostic contract of the `quota-resolver` gear: a stateless
gateway in front of one quota backend. A quota caps how much of a **metric**
(what is counted, for example tokens) a **quota subject** (whom the cap
applies to, for example a user or a tenant) may use. A **principal** is the
authenticated caller, identified by its type and its id.

## What this crate provides

- `PlatformQuotaResolverApi` — the platform plane: `debit`, `acquire_lease`,
  `commit_lease`, `release_lease`, `return_amount` and `quota_state`. Each takes
  a `PlatformSecurityContext`, the credential of the calling module, and names
  the tenant in the request. Its REST projection `PlatformQuotaResolverApiRest`
  is internal, under `POST /quota-resolver/v1/platform/*`.
- `QuotaResolverApi` — the tenant plane: `quota_state` only, for the caller's
  own principal and tenant, both taken from the `SecurityContext`. Its REST
  projection is exposed at the edge as `POST /quota-resolver/v1/quota-state`.
- `QuotaResolverPluginClient` — the in-process trait a quota backend
  implements; it receives requests whose subjects are all quota subjects.
- Request and result types: `PlatformDebitRequest`, `QuotaDecision`,
  `LeaseOutcome`, `QuotaState`, `SubjectInput` (a quota subject or a principal)
  and the rest of `models`.
- GTS types: the plugin spec `QuotaResolverPluginSpecV1`, the quota subject
  base `gts.cf.core.quota.subj.v1~`, the tenant subject type
  `gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~` and the metric base
  `gts.cf.core.quota.metric.v1~`. The `quota-resolver` gear publishes them.

Features `rest-client` and `rest-server` emit the REST clients and the routes.

## How a module charges a user

Writes exist on the platform plane only. The module authenticates with its own
credential and passes the user as data:

```rust
use quota_resolver_sdk::PlatformQuotaResolverApi;
use quota_resolver_sdk::models::{
    PlatformDebitRequest, PrincipalSubject, SubjectInput, caller_tenant_id,
};

let resolver = hub.get::<dyn PlatformQuotaResolverApi>()?;
let principal = PrincipalSubject::from_security_context(&ctx).ok_or(no_subject_type)?;

let decision = resolver
    .debit(
        platform_ctx,
        PlatformDebitRequest {
            tenant_id: caller_tenant_id(&ctx),
            subjects: vec![SubjectInput::Principal(principal)],
            items: vec![item],        // a metric `{type, id?, metadata}` and an amount
            idempotency_key: key,
        },
    )
    .await?;
if !decision.allowed {
    // decision.reason: quota_exceeded, no_applicable_quota or principal_not_mapped
}
```

A denial is a successful call. Only a failure to decide is an error. The gateway
maps the principal type to a quota subject type with the `principal_subject_types`
config and adds nothing else; a principal type with no mapping is denied as
`principal_not_mapped`, and its quota state is an empty page.

A lease holds capacity for `ttl_secs`: `acquire_lease` returns a token,
`commit_lease` charges the amounts actually used (omitted means the held amounts),
`release_lease` charges nothing, and `return_amount` gives back part of an earlier
debit or lease commit, named by its operation and idempotency key. Every write
carries an idempotency key.

## How a backend plugs in

1. Implement `QuotaResolverPluginClient`.
2. Register the client in `ClientHub` under the GTS id of a plugin instance of
   `QuotaResolverPluginSpecV1`, and publish that instance with a `vendor` and a
   `priority`.

The gateway lists the instances of the plugin type in the types registry, keeps
those with a registered client and the configured `vendor`, and picks the lowest
`priority`. The `quota-enforcement` gear is such a backend.

## Metric and subject types

Each module registers its metric types in the types registry, derived from the
metric base, with the traits `kind` (`counter` or `gauge`), `admitted_subjects`
and `enforcement` (`quota_gated` or `direct`). The operator registers the quota
subject types of its principals, derived from the subject base.

## Design

See `../docs/PRD.md`, `../docs/DESIGN.md` and `../docs/ADR/`. JSON Schemas of the
GTS types are in `../docs/schemas/`.
