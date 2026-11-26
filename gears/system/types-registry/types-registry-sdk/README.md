# Types Registry SDK

Transport-agnostic contracts, models and helpers for the Types Registry gear, which stores
GTS (Global Type System) Type Schemas and Instances.

## Contracts

| Contract | Context | For |
|---|---|---|
| `PlatformTypesRegistryApi` | `PlatformSecurityContext` | the gear's own work: batch reads, discovery, registration, deletion, operation reads |
| `TypesRegistryApi` | a tenant's `SecurityContext` | reads made on a tenant's behalf |

Both are resolved from `ClientHub` as trait objects. Every method returns
`Result<_, CanonicalError>`; project an error with `TypesRegistryError::from` for typed
matching, and an operation item's failure with `AdmissionFailure::from_canonical`.

`PlatformTypesRegistryApiExt` and `TypesRegistryApiExt` add kind-narrowed reads
(`get_type_schema`, `get_instance`, their `_by_uuid` and `batch_get_*` variants),
identifier and Registry Reference resolution (`resolve_type_uuid`, `resolve_type_id`,
`resolve_instance_uuid`, `resolve_instance_id` and their plural forms) and
complete listings (`list_type_schemas`, `list_instances`). The platform extension also
reconciles explicit documents with `reconcile_entities_and_await`. Import the extension
trait to call them; with both imported, choose one by UFCS.

### Both APIs over REST (`rest-client`)

The current base paths are `/types-registry/v2/platform` and `/types-registry/v2`.
After legacy v1 is removed in T38, both promote atomically to
`/types-registry/v1/platform` and `/types-registry/v1`; the SDK bindings and callers
move together in that cutover.

`PlatformTypesRegistryApiRestClient` calls `/types-registry/v2/platform/`, resolving
`ClientConfig::internal_token_provider` afresh per attempt for
`X-ToolKit-Internal-Token`. It sends no bearer or `PlatformSecurityContext`.
Safe reads and keyed mutations retry under `ClientConfig::retry`. Mutations read
back their operation after `Retry-After`, capped by `retry.max_delay`, while running.
Failed read-back returns `Aborted` / `OPERATION_READ_FAILED` with the operation ID;
replay the same key to recover it.

The uncached `TypesRegistryApiRestClient` uses the same read models, projections and validators. It forwards the caller's bearer on tenant reads, retains no caller context and never uses the internal token. A missing bearer returns 401. Both reads use `ClientConfig::retry` and client-internal paths.

`#[toolkit::consumes]` uses `PlatformTypesRegistryApiRestResolvingClient` and `TypesRegistryApiRestResolvingClient`. Cache hits skip directory lookup and HTTP.
Forwarded calls resolve the current endpoint or return `ServiceUnavailable`.
`new` uses SDK defaults. Use `with_cache_settings` to override them.
Set both entry limits to zero to disable retention. The
`rest-server` feature supplies `platform_types_registry_api_rest_http_binding` and
`types_registry_api_rest_http_binding`, the descriptors `#[toolkit::provides]` validates
against `platform_types_registry_api_ir` and `types_registry_api_ir`.

The registry gear declares `#[toolkit::provides(transports = [local, rest])]` for both
contracts and publishes them, locally, only past its seed barrier. A gear that may run
without the registry in its process declares, beside `#[toolkit::gear]`:

```rust,ignore
#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::consumes(contract = types_registry_sdk::TypesRegistryApi, from = "types-registry")]
```

with this SDK's `rest-client` feature. Where the registry is linked, its local clients win;
elsewhere the resolving clients are wired after every `init`, so a remote consumer cannot
call the registry from its own `init()`.

The `otel` feature adds trace propagation and RED client metrics to both clients.

The GTS identifier types the models use (`GtsId`, `GtsTypeId`, `GtsInstanceId`,
`GtsIdPattern`, `GtsIdSegment`) are re-exported, so building a request needs no direct
`gts` dependency.

## Payload validation

`TypeSchema::compile_payload_validator()` compiles a reusable `gts::PayloadValidator`
from the registry's materialized schema. Select `content` and `resolved_schema`;
traits, origin and provenance are not needed. The SDK neither builds a `GtsStore`
nor resolves schemas again, and compilation and validation make no registry calls.

```rust,no_run
use toolkit_security::SecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsTypeId, Projection, ReadOptions,
    TypesRegistryApi, TypesRegistryApiExt,
};

async fn validate_records(
    registry: &dyn TypesRegistryApi,
    caller: &SecurityContext,
    type_id: &GtsTypeId,
    payloads: &[serde_json::Value],
) -> anyhow::Result<()> {
    let schema = registry.get_type_schema(caller, type_id, ReadOptions {
        projection: Projection::Select(FieldSelection::with(&[
            EntityField::Content,
            EntityField::ResolvedSchema,
        ])),
        ..ReadOptions::default()
    }).await?;
    let validator = schema.compile_payload_validator()?;
    for payload in payloads {
        validator.validate(payload)?;
    }
    Ok(())
}
```

`CompilePayloadValidatorError::MissingField` identifies an unselected required field;
`InvalidContent` rejects a selected non-object authored document, including JSON null.
`Upstream` preserves `gts::CompilePayloadValidatorError` and its error source; it is
also exported as `GtsCompilePayloadValidatorError` for typed matching without a direct
`gts` dependency. `PayloadValidator` and `PayloadValidationError` are re-exported too.
Only the authored leaf's boolean `x-gts-abstract: true` makes it abstract; an inherited
flag in `resolved_schema` does not. Other schema-admission checks are not repeated.

Validation uses native GTS dialect, format, regex and reference-keyword rules. It checks
reference syntax/patterns (`GtsRefValidation::None`), not target presence or validity.
Every referenced schema resource must be present in the materialized schema; unresolved
external resources fail compilation. Lossy reference inlining can change verdicts relative
to store-based validation. Selected null `resolved_schema` is an upstream compilation
error, while boolean schemas retain their native meaning. Payloads are not modified and
defaults are not inserted. Consumers choose freshness, lifecycle and availability policy,
and construct their own payload envelopes; existing consumers migrate separately in T38.

**Intermediate AM topology.** An embedded `legacy_only` registry serves the
legacy client and v1 routes. It skips seeding, admission and service-directory
advertisement. New API calls go to the remote registry. AM publishes through
`gts_publication(...)`, including its RG declarations. RG reads definitions
through `TypesRegistryApi` with the caller's context and stores only type keys
and their identifiers locally.

Application readiness waits for GTS Type Schema and Instance publication
before `init`. The server's `publish-gts --registry <url>` subcommand publishes
the same documents during deployment. The dedicated AM remote host, its two
configs and `make e2e-am-remote` are deferred to T41. See [the P0 spec](../docs/p0/SPEC.md).

## Publishing from a gear

A gear publishes the documents it owns from its own `init()` through the in-process
platform API, then reads them back. Its `Cargo.toml` depends on this SDK and on the
registry gear crate, `cf-gears-types-registry`: `deps = [types_registry]` re-exports that
crate to keep its registration linked, and orders the two `init()` calls.

Ordinary gears can omit the registry dependency: the registry initializes in the system gear group first.
The host must then link a database-backed registry or consumer init fails.
This consumer-migration branch moves
AM and AuthZ/TR publishers onto this local-only path. AM request-path reads use
`TypesRegistryApi` with the caller’s context; `AuthN` plugins consume the platform
contract and publish in `post_init`. T41/T42 retain full remote publication and
readiness work; the remaining legacy consumers move in T38.

```rust,no_run
# // This doctest cannot depend on the registry gear; stand the SDK in for it.
# pub extern crate types_registry_sdk as types_registry;
use toolkit::{Gear, GearCtx};
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsId, GtsTypeId, Origin, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, Publication, PublisherContext, ReadOptions,
};

// `deps` makes this gear's `init()` run after the registry's.
#[toolkit::gear(name = "example-events", deps = [types_registry])]
#[derive(Default)]
pub struct ExampleEvents;

#[async_trait::async_trait]
impl Gear for ExampleEvents {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Fails when the registry has no database bound: there is no fallback.
        let registry = ctx.client_hub().get::<dyn PlatformTypesRegistryApi>()?;
        let platform = PlatformSecurityContext::outbound_marker();
        let publisher = PublisherContext {
            name: "example-events".to_owned(),
            version: env!("CARGO_PKG_VERSION").parse()?,
        };
        let desired = [(
            GtsId::try_new("gts.cf.example.events.user_created.v1~")?,
            serde_json::json!({
                "$id": "gts://gts.cf.example.events.user_created.v1~",
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object"
            }),
        )];
        // Returns only once every desired document is admitted or already up to date.
        Publication::new(registry.as_ref(), publisher, desired)
            .publish(ctx.cancellation_token())
            .await?;

        let type_id = GtsTypeId::try_new("gts.cf.example.events.user_created.v1~")?;
        let read = registry
            .get_type_schema(
                &platform,
                &type_id,
                ReadOptions {
                    projection: Projection::Select(FieldSelection::with(&[
                        EntityField::Content,
                        EntityField::Origin,
                    ])),
                    ..ReadOptions::default()
                },
            )
            .await?;
        if let Some(Origin::Managed { resource_version, .. }) = read.origin {
            tracing::info!(resource_version, "user_created is published");
        }
        Ok(())
    }
}
```

Success is `UpToDate`, or `Reconciled` with every desired identifier `Admitted`. An accepted
operation, or an `Ok` from a single `register_entities`, is not: an item can still fail in
admission. Neither `Publication::publish` nor `reconcile_entities_and_await` requires a
second check of successful outcomes.

`Publication` takes the required registry, publisher and documents in `new` (typed) or
`from_manifest` (parsed GTS ids, unparsed version, rejected before any request); `deadline`,
`timeout`, `retry_backoff`, `batch_size` and `on_pending` are optional. `publish` uses the
platform outbound marker, unlimited passes, batches of 100 and the shared 120-second
`STARTUP_PUBLICATION_DEADLINE`, measured from the start of `publish`. An absolute `deadline`
`D` replaces that budget, may exceed it and is never renewed between retries. A `timeout`
`T` caps the budget: it ends at `start + T` without a deadline and at `min(D, start + T)`
with one, so `T` never extends `D`; a `T` no clock can represent fails before any request
without a deadline and leaves `D` in force with one. `retry_backoff(initial, max)` and
`batch_size` are the raw reconciliation values: an `initial` above `max` is clamped, a zero
pause retries at once, and a batch above the registry's `limits.batch_candidates` is
refused. The `gts_publication` runtime configuration rejects zero values and a maximum below
the initial pause before they get here. It awaits reconciliation in the current task, without
spawning or changing the gear's lifecycle, and returns the `Reconciliation`. Pass the owning
gear's publisher name and version; `env!("CARGO_PKG_VERSION")` must be evaluated in that
gear, not in the SDK.

On failure, `PublicationError` names the publisher and each rejected or pending identifier,
with the stop reason and machine-readable cause. `canonical_error()` and the error's
`source()` retain the original `CanonicalError`; `report()` exposes the decoded
`ReconcileReport`, when present. Propagate it with `?` or add `anyhow::Context` so the source
chain stays intact. Cancellation still names the requested identifiers even without a report.
For another security context or other reconciliation options (a pass limit, `stop_when_stalled`), call
`reconcile_entities_and_await` directly.

**Bounded publication.** `Publication::publish` calls reconciliation directly with one
publication budget. It supplies the platform context,
formats progress and failure diagnostics, and configures the reconciliation options.
Transient reads, dependencies and conflicts retry within that budget, with equal-jitter
exponential backoff (1 s up to 30 s). `on_pending` receives a `PublicationError` describing
what is still pending before each retrying pass (default: nothing). Admitted identifiers are never re-read or resubmitted, so another writer's
later changes are left alone. Rejections and call-level refusals stop retries;
cancellation returns `Cancelled`. Budget expiry reports `deadline`, retaining each
identifier's last known cause. Accepted writes continue after the caller stops waiting.

`#[toolkit::gear(..., gts_publication(...))]` publishes through `publication::Publication`
during application preparation, before any application `init`. Ordinary startup uses a
120-second budget; a deployment job supplies its absolute deadline instead, shared by every
publisher. Each publishing gear can set its own policy under
`gears.<gear>.config.gts_publication` (hidden from the gear's typed configuration):

```yaml
gears:
  account-management:
    config:
      gts_publication:
        timeout: "120s"          # caps startup and deployment budgets; default: no cap
        retry_backoff: "1s"
        retry_backoff_max: "30s"
        batch_size: 100
```

Without `timeout`, startup keeps the 120-second default and a deployment job's deadline
replaces it, even when longer. Unknown fields, wrong types, zero values,
`retry_backoff_max < retry_backoff` and a `timeout` above 10 minutes (the same on every OS, and the same
maximum as the `publish-gts` deadline) fail startup before any publication writes, naming the
gear and the config path. The policy applies per publication, alike for a local and a
remote registry; the registry client's URL, HTTP timeout and retries stay in
`clients.consumes.types-registry`. Publication reuses the gear's own client view and settings.
Deployment jobs use the same validation and tuning; `--registry` changes only the endpoint
and must respect `require_tls`. Custom `publisher = ...` adapters provide
the same `Publication` shape: `from_manifest(registry, name, version, &[(GtsId, JsonDocument)])`,
`deadline(Option<tokio::time::Instant>)`, `timeout(Option<Duration>)`,
`retry_backoff(Duration, Duration)`, `batch_size(NonZeroUsize)`,
`on_pending(FnMut(&impl Display) + Send)` and `publish(cancel)`; see
`toolkit::gts_publication`.

```rust,ignore
#[toolkit::gear(
    name = "my-gear",
    gts_publication(
        crates = [crate, my_gear_sdk],      // crates with `declare_gts_inventory!()`
        entities = Self::configured,        // optional: config-only documents
    ),
)]
pub struct MyGear { /* ... */ }
```

`init` uses `GearCtx::gts_publication_manifest()` to bind plugin clients from declared instances
after admission. The returned `GtsManifest` holds publisher name/version and authored
GTS Type Schemas and Instances; it is a `ToolKit` data structure, not a GTS entity.
The gear crate depends on this SDK, as `types_registry_sdk`, with the `rest-client` feature.
See [GTS publication by gears](../docs/DESIGN.md#inventory-and-startup-reconciliation).

The examples use vendor `cf`, the only one a registry admits without configuration. Another
vendor needs a `registration_policy` entry for its region in the registry's configuration:

```yaml
gears:
  types-registry:
    config:
      registration_policy:
        "gts.acme.core.*":
          allowed_vendors: [acme]
```

**Local API availability.** Registry init admits linked Type Schemas
(`#[toolkit_gts::gts_type_schema(..)]`), Instances (`gts_instance!`,
`gts_instance_raw!`) and configured `entities` before publishing both contracts
in `ClientHub`. Missing database or failed seeding leaves neither registered;
`ClientHub::get` fails without legacy fallback.

- `deps` orders `init()`; remote clients are wired afterwards. AM, the `AuthN`, `AuthZ` and
  tenant resolvers, and the static `AuthN`, `AuthZ`, TR and `IdP` plugins and the OIDC `AuthN`
  plugin publish with `gts_publication(...)`; the remaining publishers move in T41/T42.
- `Publication::publish` above runs in its caller's task; it does not perform the
  lifecycle migration by itself.
- `PublisherContext` is required but only forwarded/enforced in T44/T45;
  mixed-version publishers have no earlier guarantee.
- Local clients trust both context types without validation or tenant scope (C2/C6).
- Local and resolving REST clients each cache up to 16 384 snapshots (`max_entries`) and 64 MiB (`max_size_bytes`). Either limit at zero disables snapshot retention.
  The budget counts compact JSON documents, GTS IDs, raw validators and 256 bytes per snapshot. Snapshots of one entity share byte-identical documents in the same field.
- Reads return the requested fields from a covering snapshot. Matching validators allow fields to merge within one scope. A changed validator replaces stale data.
  After `freshness_window` (30 s), expired keys revalidate in one batch. Use `fresh` to check earlier. Observed terminal successes invalidate affected platform snapshots.
  The cache skips absences, failed reads, discovery pages and operation resources.
- Any authenticated reader can reuse a fresh global snapshot. Tenant-owned snapshots are per subject tenant because `ownership` is relative to the reader. The registry enforces per-subject visibility, so each read context must also confirm access.
  Its first read can use the snapshot's `ETag`. The cache keeps up to `max_entries` confirmations for the freshness window, keyed by entity, subject ID, subject type, subject tenant and raw token scopes. Bearers with identical context fields share a confirmation. Anonymous calls bypass the cache. REST misses require the caller's bearer.
- A read returning `NotFound` drops a global snapshot. For other entities it revokes only that context's confirmation for the key and fences its pending confirmations.
  A refused request leaves prior confirmations in place.
- Each client's identity LRU retains up to `max_identity_entries` registry-issued ID/UUID pairs (16 384 by default). Reads, resolutions and discovery supply pairs.
  Discovery leaves snapshots and their freshness unchanged. Pairs survive expiry of snapshots and observed mutations because deleted entities still resolve.
  Recreate the client or restart its consumer after purge (ADR-0013, deferred) or when switching logical registries, since old pairs can still resolve offline.
- Legacy and new catalogues share seeds but no later writes until T38. Dependencies
  registered only through legacy cannot support publication through the new API.
- Shared-database outbox polling can delay admission by one minute; keep
  reconciliation deadlines and `worker.operation_timeout` above it.

## Reading

Every `Entity` carries `ownership` (`Global`, `ContextTenant` or `AncestorTenant`). Remote clients require it, so upgrade the registry before the gears that read it over REST.

Platform reads see global entities only. Read a tenant's own types under that tenant's service account through `TypesRegistryApi`; read platform contracts the gear uses for its own work through the platform client, which shares one cache across all callers.

```rust,no_run
use toolkit::client_hub::ClientHub;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsTypeId, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, ReadOptions,
};

async fn read(hub: &ClientHub) -> anyhow::Result<()> {
    let registry = hub.get::<dyn PlatformTypesRegistryApi>()?;
    let ctx = PlatformSecurityContext::outbound_marker();
    let type_id = GtsTypeId::try_new("gts.cf.example.events.user_created.v1~")?;

    // Document-free by default: identity, kind, lifecycle, ownership and origin.
    let light = registry.get_type_schema(&ctx, &type_id, ReadOptions::default()).await?;

    // Select documents explicitly.
    let with_schema = ReadOptions {
        projection: Projection::Select(FieldSelection::with(&[
            EntityField::Content,
            EntityField::ResolvedSchema,
        ])),
        ..ReadOptions::default()
    };
    let schema = registry.get_type_schema(&ctx, &type_id, with_schema).await?;

    // Bypass a client cache for this read.
    let fresh = registry.get_type_schema(&ctx, &type_id, ReadOptions {
        fresh: true,
        ..ReadOptions::default()
    }).await?;
    Ok(())
}
```

`Projection::Default` is the document-free field set on single and batch reads, while the
list helpers give it their documents (content, and a Type Schema's materializations). An
explicit selection is always kept as given.

Single and batch typed read helpers take `ReadOptions`. Its default uses
`Projection::Default` and `fresh = false`. The options preserve the projection
and apply `fresh` to every request, including each chunk of a batch read. `fresh`
controls the client cache and is not sent on the wire. On a client without a
cache, this flag has no effect.

### Conditional reads

To refresh a snapshot, use `BatchGetItem::new(key).revalidate(&previous_lookup)`.
Reuse it only for the tenant that read it. The SDK checks entity and field coverage, but a P0 `ETag` is the same across projections and does not distinguish tenants.

```rust,no_run
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetItem, EntityKey, EntityLookup, PlatformTypesRegistryApi,
};

async fn refresh(
    registry: &dyn PlatformTypesRegistryApi,
    ctx: &PlatformSecurityContext,
    key: EntityKey,
    held: &EntityLookup,
) -> anyhow::Result<()> {
    let item = BatchGetItem::new(key.clone()).revalidate(held);
    let answer = registry
        .batch_get_entities(ctx, BatchGetEntitiesRequest::new([item]))
        .await?;
    match &answer.0[&key] {
        EntityLookup::Unchanged { .. } => { /* `held` is still current */ }
        EntityLookup::Found { .. } => { /* hold this answer instead */ }
        _ => { /* absent, or a future variant */ }
    }
    Ok(())
}
```

Transports get the usable `ETag` from `item.if_none_match_for(&request.projection)`.
Keep the original `Found` after an `Unchanged` reply. Passing `Unchanged` or `NotFound` to `revalidate` makes the next read unconditional.

`resolve_type_uuid` and `resolve_instance_uuid` map registered IDs to Registry References. `resolve_type_id` and `resolve_instance_id` reverse the mapping.
Plural forms follow `batch_get_*` rules. These helpers take no read options.

Platform resolution checks the identity LRU, then fresh snapshots. Tenant resolution can use a pair that a read or discovery classified as global even from a stale snapshot. Other pairs require a read or resolution confirmed for the same context within the window. Discovery alone cannot confirm access. Setting `max_identity_entries` to zero skips the identity LRU. Both contracts send misses to `batch_resolve_identities` (`POST {base}/entities:batchResolve`), which returns stored identities for active and deleted entities without loading snapshots, validators or documents.

Clients reject incomplete, unexpected or contradictory results as `Internal`.
Deploy the registry with `:batchResolve` before remote SDK consumers. Test doubles with only batch reads can use `types_registry_sdk::ext::resolve_through_batch_get`, under the `test-util` feature.
Use the registry-issued UUID returned by `resolve_*_uuid` or already held in `EntityKey::GtsUuid` (ADR-0001).

Unknown or other-kind references return `NotFound` singly, `None` in batches;
invalid/wrong-kind identifiers return `InvalidArgument` before reading.

`EntityField::Provenance` selects a managed Type Schema's `provenance` group as a whole:
`TypeSchema::provenance` is `None` when unselected and `Some(Provenance { .. })` when
selected. Its member `compatibility_check_skipped` is a plain `bool`: `true` when
`skip_compatibility_check` waived an applicable cross-minor compatibility check, `false` when
it did not — not a claim that a check ran. `Instance` has no `provenance`.

## Listing

```rust,no_run
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};

async fn list(registry: &dyn PlatformTypesRegistryApi) -> anyhow::Result<()> {
    let ctx = PlatformSecurityContext::outbound_marker();
    let query = ListEntitiesRequest::matching(GtsIdPattern::try_new("gts.cf.example.*")?);
    let schemas = registry.list_type_schemas(&ctx, query).await?;
    Ok(())
}
```

Each `with_*` builder sets one field and keeps the others. `list_type_schemas` and
`list_instances` set the kind themselves and refuse the other one; `list_entities` takes
`with_kind`.

A listing helper follows every page; past `MAX_LIST_PAGES` it fails with
`ResourceExhausted`. A cursor resumes only the query that issued it.

## Reconciling

[Publishing from a gear](#publishing-from-a-gear) shows a complete call and its outcomes.
Reconciliation creates absent identifiers and updates differing ones; it never deletes. An
identifier listed twice with different documents is `Rejected` without being submitted.
`Ok` means every identifier is admitted. Otherwise `FailedPrecondition` carries a report;
decode it with `ReconcileReport::from_canonical(&error)` for the stop reason and every outcome,
including admitted identifiers. `Rejected` is terminal for the document; `Pending` may settle
on a later call. Caller cancellation and an unrepresentable deadline return `Cancelled` and
`InvalidArgument` without a report.

`ReconcileOptions::batch_size` must fit the registry's configured `limits.batch_candidates`
(both default to 100). Passes (default 5) re-read and resubmit what a conflict or a dependency in
a later batch left pending; a caller whose desired set is complete can set `stop_when_stalled` to
end the call as `stalled` once a pass admits nothing and only dependencies remain. Transient
read errors retry under the same deadline and pass limit; permanent read refusals and malformed
responses stop as `read_failed`. A terminal item rejection stops further passes after the current
pass's batches, reporting `rejected` when other identifiers remain pending. A permanent submission
refusal stops the call immediately:
`InvalidArgument`, `FailedPrecondition`, `Unauthenticated`, `PermissionDenied`, `NotFound`,
`AlreadyExists`, `OutOfRange`, `Cancelled`, or `ResourceExhausted` without a quota
`retry_after_seconds`. A `ResourceExhausted` with that hint resends the same request under the same
key after at least the longest hint, within the deadline; `Aborted`, `Internal`, `Unknown` and
`ServiceUnavailable` allow up to three attempts per batch. The canonical category and context
decide, the same for local and remote clients, never the HTTP status. Unjudged identifiers keep
earlier outcomes or become `Pending(Skipped)`; accepted writes continue.

## Operations

`register_entities` and `delete_entities` answer the accepted operation; poll it with
`get_operation` until `OperationStatus::Completed`. Each item carries an outcome with exactly
its state's data:

```rust,no_run
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{Operation, PlatformTypesRegistryApi, RegistrationOutcome};

async fn outcomes(
    registry: &dyn PlatformTypesRegistryApi,
    operation_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let ctx = PlatformSecurityContext::outbound_marker();
    if let Operation::Registration(operation) = registry.get_operation(&ctx, operation_id).await? {
        for item in operation.items {
            match item.outcome {
                RegistrationOutcome::Succeeded { resource_version } => {}
                RegistrationOutcome::Unchanged { resource_version } => {}
                RegistrationOutcome::WouldSucceed => {} // a dry run: nothing committed
                RegistrationOutcome::Failed { error } => {}
                RegistrationOutcome::Pending | RegistrationOutcome::Running => {}
            }
        }
    }
    Ok(())
}
```

A deletion item's `DeletionOutcome` has the same states except `Unchanged`.

## Errors

```rust,no_run
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsTypeId, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, ReadOptions,
    TypesRegistryError,
};

async fn classify(
    registry: &dyn PlatformTypesRegistryApi,
    type_id: &GtsTypeId,
) -> anyhow::Result<()> {
    let ctx = PlatformSecurityContext::outbound_marker();
    match registry.get_type_schema(&ctx, type_id, ReadOptions::default()).await {
        Ok(schema) => {}
        Err(error) => match TypesRegistryError::from(error) {
            TypesRegistryError::NotFound { .. } => {}
            TypesRegistryError::Validation { issues } => {}
            other => return Err(other.into()),
        },
    }
    Ok(())
}
```

## Observability

### Client cache metrics

Local and REST caches use the global OpenTelemetry meter under the `cf-gears-types-registry-sdk` scope. On `ToolKit` hosts built with `otel`, enable `opentelemetry.metrics.enabled`. Other hosts install their provider before constructing clients.

| Instrument | Labels in addition to `plane` | What it measures |
|---|---|---|
| `types_registry_client_cache_lookups` | `operation` (`batch_get`, `batch_resolve`), `result` (`hit`, `miss`, `revalidate`) | One lookup per distinct key in a batch that goes through the cache |
| `types_registry_client_cache_evictions` | `cache` (`snapshot`, `identity`) | Entries displaced by LRU to enforce a capacity limit |
| `types_registry_client_cache_entries` | `cache` (`snapshot`, `identity`) | Resident entries summed across live cache instances |
| `types_registry_client_cache_size` | | Total bytes charged against `max_size_bytes` across live clients |

`plane` is `platform` or `tenant`. Hits stay local. Forwarded keys count as revalidations if they carry the cache's `ETag`, otherwise as misses, even if the registry call fails. Hit rate divides hits by all lookups. Occupancy series appear once the cache retains entries. Replacements and invalidations affect occupancy.
Capacity-driven removals also count as evictions.

### Reconciliation tracing

`reconcile_entities_and_await` traces with `tracing` at INFO; nothing needs DEBUG to be
correlated. Every log record has a stable `log_kind` field. Codes only: no document content, and no
free text of an error (`detail`, a violation's description, an admission message). The full
error reaches the caller in the returned `ReconcileOutcome` or `Err`; an accepted operation's
item failures are also persisted and read with `get_operation`. A synchronous refusal, a
conflicting duplicate or a read fault need not have created an operation: when an error names
one (an idempotency conflict, a lost read-back, a deadline while it ran), the batch span
records its `operation_id`; otherwise the caller's error is the only full text.

| Span (INFO) | Fields |
|---|---|
| `reconcile` | `reconcile_id`, `publisher`, `publisher_version`, `inputs` (entries given), `desired` (distinct identifiers), `passes`, `batch_size`, `deadline_ms` |
| `reconcile_batch` | `batch`, `pass`, `size`, `idempotency_key`, `attempt`, `operation_id` (once known) |

| Level | `log_kind` | Meaning |
|---|---|---|
| INFO | `reconcile_started` | the call began |
| INFO | `batch_submitted` | a new payload is about to be sent, under its `idempotency_key`; written before any answer |
| INFO | `batch_accepted` | the registry accepted the batch as `operation_id` |
| WARN | `candidate_refused` | one candidate refused synchronously (`gts_id`, `category`, `reason`, `violation_fields`); the call stops |
| WARN | `batch_refused` | the request as a whole was refused; its candidates stay pending and the call stops |
| WARN | `transport_exhausted`, `incomplete_read`, `item_unreported` | transport or protocol trouble |
| INFO / WARN | `reconcile_finished` | `stop`, `elapsed_ms`, `desired`, `admitted`, `rejected`, `pending`, `undecided`, `passes` |
| WARN | `entity_unsettled` | one per identifier not admitted, on a normal return: `outcome`, `cause`, `category`, `reason`, `dependency_id`, `dependency_kind`, `diagnostic_code`; `rejected` is final for that document, `pending` may settle in a later call |
| DEBUG | `transport_retry` | a transport failure retried under the same key (`category`, `reason`, `backoff_ms`) |

`reconcile_finished` is written exactly once — also when the call is cancelled, its future
dropped (`stop = "dropped"`, with what it had observed and the rest `undecided`) or its
deadline unrepresentable. It is INFO only for `up_to_date`, a `completed` call that admitted
everything, and cancellation by the caller's token; anything else is WARN. `stop` is
`up_to_date`, `completed`, `passes_exhausted`, `deadline`, `read_failed`, `invalid_input`, `stalled`, `cancelled`,
`invalid_deadline`, `dropped` or `panicked`.

The registry writes INFO `operation_accepted` once an operation is durable — `operation_id`,
`kind`, `dry_run`, `items`, `replayed` — with `idempotency_key` when the key is a canonical
UUID, as the SDK generates (any other key is logged as `idempotency_key_kind` and
`idempotency_key_len` only).

**Investigating a call.** Filter by `reconcile_id`. `reconcile_finished` and the
`entity_unsettled` rows say what ended how. Each `batch_submitted` names its batch's
`idempotency_key`, before any answer; the registry's `operation_accepted` with that key gives the
`operation_id`, even when the acknowledgement never reached the client (a replay is
`replayed = true`). The registry's `types_registry.admission.operation` and `unit` spans carry
that `operation_id` and each candidate's outcome, and `get_operation` reads an admission
refusal's full text. Rows from a synchronous refusal (`candidate_refused`,
`batch_refused`), a conflicting duplicate or `read_failed` may name no operation; their full
error is the one the caller received. With the local client the registry's acceptance runs inside the caller's spans, so its
event carries `reconcile_id` directly.

## Testing (`test-util`)

- `testing_platform::MockTypesRegistry` implements both contracts in memory. Seed it
  (`seed`, `Seed`, `seed_inventory`) or write through the contract. Changed Type Schemas
  pass native GTS schema, derivation and trait validation before content or versions change;
  malformed candidates return `InvalidSchema`. Unordered batches wait for selected schema
  prerequisites; refused prerequisites block their dependents without supplying their old
  revisions. Independent candidates may still succeed, and dry runs validate against a
  disposable copy. Managed schemas must fully materialize, matching the real registry's
  rejection of unresolved/circular schema resources. `Fault` delays or fails calls (`Call::BatchResolve` for identities, `Call::BatchGet` for snapshots).
  `reject` and `depends_on` steer admission, `strict()` panics on an unexpected call, and `calls`,
  `reads` and `submissions` record what happened. `install` registers it in a `ClientHub`
  under both contracts.
- `seed` and `Seed` deliberately bypass admission for fixtures, including invalid schemas.
  Explicit call faults and admission refusals retain precedence; lifecycle, precondition,
  unchanged and idempotent-replay behavior is preserved. An unrelated invalid seed cannot
  poison valid admission. The members of an in-batch schema cycle are refused as
  `invalid_schema` and the candidates waiting on them are `blocked_by_dependency`, as in the
  registry. The mock does not reproduce publisher guards, database locking,
  compatibility/history checks or every production graph failure classification.
- Instances are admitted without validating their payload against their Type Schema and
  without waiting on, or being blocked by, in-batch schema candidates. A batch the registry
  refuses (an invalid payload, an instance of a type refused in the same batch) can succeed
  on the mock: assert instance admission against the registry's local client.
- `testing_platform::conformance::run` holds the contract checks that the mock, the
  registry's local client and the REST client over TCP all pass.

## Legacy API

**Removed at T38.** Existing consumers still use it until they migrate; new code uses the
contracts above. `TypesRegistryClient` reads and registers through the registry's in-memory
catalogue, which is independent of the database the new contracts serve. Its models
(`GtsTypeSchema`, `GtsInstance`, `AncestorIter`, `RegisterResult`, `RegisterSummary`, …) and
`testing::MockTypesRegistryClient` remain at the crate root until then.

```rust,no_run
use toolkit::client_hub::ClientHub;
use types_registry_sdk::TypesRegistryClient;

async fn legacy(hub: &ClientHub) -> anyhow::Result<()> {
    let client = hub.get::<dyn TypesRegistryClient>()?;
    let schema = client
        .get_type_schema("gts.cf.example.events.user_created.v1~")
        .await?;
    Ok(())
}
```

## License

Apache-2.0
