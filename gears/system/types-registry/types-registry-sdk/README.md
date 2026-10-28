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

`TypesRegistryApiRestClient` serves the same read models, projections and validators
on tenant routes. Each call forwards its `SecurityContext` bearer for revalidation;
it retains no caller context and never uses the internal token. Missing bearer
returns 401. Both reads retry under `ClientConfig::retry`; paths are client-internal.

`PlatformTypesRegistryApiRestResolvingClient` and `TypesRegistryApiRestResolvingClient`
wrap the two clients in `DirectoryResolvingClient`, where `#[toolkit::consumes]` looks for
them: each call resolves the registry from the directory, follows it when it moves, and
fails `ServiceUnavailable` while none is registered or the directory cannot answer. The
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

## Publishing from a gear

A gear publishes the documents it owns from its own `init()` through the in-process
platform API, then reads them back. Its `Cargo.toml` depends on this SDK and on the
registry gear crate, `cf-gears-types-registry`: `deps = [types_registry]` re-exports that
crate to keep its registration linked, and orders the two `init()` calls.

Ordinary gears can omit the registry dependency: its system-gear init runs first.
The host must then link a database-backed registry or consumer init fails.
This [consumer-migration branch](https://github.com/aviator5/gears-rust/pull/1) moves
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
    PlatformTypesRegistryApiExt, Projection, PublisherContext, ReadOptions,
    publish_startup_entities,
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
        let desired = vec![(
            GtsId::try_new("gts.cf.example.events.user_created.v1~")?,
            serde_json::json!({
                "$id": "gts://gts.cf.example.events.user_created.v1~",
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object"
            }),
        )];
        // Returns only once every desired document is admitted or already up to date.
        publish_startup_entities(
            registry.as_ref(),
            &publisher,
            &desired,
            ctx.cancellation_token(),
        )
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
admission. Neither helper requires a second check of successful outcomes.

`publish_startup_entities` uses the platform outbound marker and the shared 120-second
`STARTUP_PUBLICATION_DEADLINE`. It awaits reconciliation in the current task, without
spawning or changing the gear's lifecycle. Pass the owning gear's publisher name and version;
`env!("CARGO_PKG_VERSION")` must be evaluated in that gear, not in the SDK.

On failure, `PublicationError` names the publisher and each rejected or pending identifier,
with the stop reason and machine-readable cause. `canonical_error()` and the error's
`source()` retain the original `CanonicalError`; `report()` exposes the decoded
`ReconcileReport`, when present. Propagate it with `?` or add `anyhow::Context` so the source
chain stays intact. Cancellation still names the requested identifiers even without a report.
For custom budgets, batching or security contexts, call `reconcile_entities_and_await` directly.

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

- `deps` orders `init()`; remote clients are wired afterwards. T41/T42 move
  publication from local init to a post-wiring hook.
- `PublisherContext` is required but only forwarded/enforced in T44/T45;
  mixed-version publishers have no earlier guarantee.
- Local clients trust both context types without validation or tenant scope (C2/C6).
- Reads hit the database until the SDK cache (T36).
- Legacy and new catalogues share seeds but no later writes until T38. Dependencies
  registered only through legacy cannot support publication through the new API.
- Shared-database outbox polling can delay admission by one minute; keep
  reconciliation deadlines and `worker.operation_timeout` above it.

## Reading

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

    // Document-free by default: identity, kind, lifecycle and origin.
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

`resolve_type_uuid` / `resolve_instance_uuid` map registered IDs to Registry
References; `resolve_type_id` / `resolve_instance_id` reverse them. Plural forms
follow `batch_get_*` rules. They take no options, select mandatory fields through
normal caching, and return snapshot values for active or deleted entities.
Unknown or other-kind references return `NotFound` singly, `None` in batches;
invalid/wrong-kind identifiers return `InvalidArgument` before reading.
Use `GtsId::to_uuid` for deterministic UUID derivation without registration or a read.

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
end the call as `stalled` once a pass admits nothing and only dependencies remain. A permanent submission refusal stops the call immediately:
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
  (`seed`, `Seed`, `seed_inventory`) or write through the contract; Type Schema documents
  resolve with `gts-rust` as admission does. `Fault` delays or fails calls, `reject` and
  `depends_on` steer admission, `strict()` panics on an unexpected call, and `calls`,
  `reads` and `submissions` record what happened. `install` registers it in a `ClientHub`
  under both contracts.
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
