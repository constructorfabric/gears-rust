# Usage Emitter

> **Emitter library** — three-layer Runtime/Factory/Emitter model: process-wide runtime, module-scoped factory, per-call-site authorized emitter, transactional outbox enqueue, and async delivery to the usage collector.

A plain library crate (no `#[modkit::module]`). Each host module (`usage-collector`, `usage-collector-rest-client`, future gRPC client) calls `UsageEmitterRuntime::build(config, db, authz, collector)` in its own `init()` and registers the result as `dyn UsageEmitterRuntimeV1` in `ClientHub`.

## Why there is no `usage-emitter-sdk` crate

The rest of the workspace follows an "impl crate + thin SDK crate" pattern (`usage-collector` /
`usage-collector-sdk`, `tenant-resolver` / `tenant-resolver-sdk`, `account-management` /
`account-management-sdk`), where the SDK crate publishes only the trait + DTOs and consumers
depend on it instead of the impl crate. `usage-emitter` intentionally diverges and ships as a
single crate. Reasons:

- **The public trait is not separable from the impl.** `UsageEmitterRuntimeV1::factory`
  returns a concrete `UsageEmitterFactory`, which in turn produces a concrete `UsageEmitter`
  and `UsageRecordBuilder`. These types carry private state (`db`, `outbox`, `issued_at`,
  `allowed_metrics`) that is what makes the security invariant below enforceable — they cannot
  be reduced to opaque trait objects without losing the type-state authorization handle.
  Consumers therefore always use the trait *and* the concrete types together.
- **`UsageCollectorClientV1` is deliberately not in `ClientHub`.** Other modules publish a
  client trait that any module can resolve; here the collector client is supplied to
  `UsageEmitterRuntime::build` and stays private. There is no "SDK-only consumer" shape — a
  consumer either builds and registers the runtime (then it transitively depends on
  `modkit-db`, the outbox runtime, PDP wiring, and `tokio` anyway) or it resolves
  `dyn UsageEmitterRuntimeV1` from the hub, in which case it only needs the trait *plus* the
  concrete `UsageEmitterFactory`/`UsageEmitter`/`UsageRecordBuilder` chain to do anything
  useful.
- **Test consumers don't need a thinner crate.** Mocks for `UsageEmitterRuntimeV1` wrap a real
  `UsageEmitterRuntime` against an in-memory SQLite (see
  `usage-collector/tests/common/mod.rs`) because every meaningful test path exercises the
  outbox enqueue. A trait-only SDK crate would not remove that requirement.

If a future consumer appears that only depends on the trait shape and a small set of DTOs (e.g.
a gRPC façade re-publishing the API), revisit this decision and extract the trait into
`usage-emitter-sdk` at that point.

## Security invariant

`UsageCollectorClientV1` is **never** registered in `ClientHub`. It is supplied to `UsageEmitterRuntime::build` as a constructor argument and stays private inside the runtime. Only `dyn UsageEmitterRuntimeV1` is published to the hub, so the sole path a source module has to the collector is through a PDP-authorized, tenant/resource-bound `UsageEmitter::enqueue*`. The PDP resource type and action constants (`gts.x.core.usage.record.v1 / create`) are `pub(crate)` in this crate and cannot be referenced externally. `UsageRecordBuilder` is publicly constructible via `UsageRecordBuilder::new()` and performs no authorization checks — it is plain data, and the invariant is preserved because `enqueue` / `enqueue_in` re-validate every field of the resulting `UsageRecord` against the `UsageEmitter` handle (module, tenant, resource, subject, allowed-metrics, value/idempotency rules) before any outbox write.

## API

The three layers in order:

| Layer | Item | Description |
|-------|------|-------------|
| 1 | `UsageEmitterRuntimeV1` | Process-wide trait. Obtain from `ClientHub`. Call `runtime.factory(MODULE_NAME)` once (e.g. in `init()`) to get a module-scoped `UsageEmitterFactory`. |
| 1 | `UsageEmitterRuntime` | Concrete implementation of `UsageEmitterRuntimeV1`; built with `UsageEmitterRuntime::build`; owns the outbox worker, the gateway client, the PDP resolver, and shared `Arc`s. |
| 2 | `UsageEmitterFactory` | Module-scoped, cheaply cloneable. Module name is fixed at `runtime.factory(name)` and immutable thereafter. Apply per-call scope overrides via `.with_tenant(...)`, `.with_subject(...)`, `.with_subject_id(...)`, `.without_subject()`, or `.with_subject_opt(id, ty)`; then call `.authorize(ctx, resource_id, resource_type)` to run PDP authorization and obtain a `UsageEmitter`. |
| 3 | `UsageEmitter` | Per-call-site, authorized handle returned by `factory.authorize(...)`. Call `enqueue` / `enqueue_in` with a pre-built `UsageRecord`, or `usage_record_builder(metric, value)` to obtain a prefilled `UsageRecordBuilder`. No further PDP calls in the hot path. |
| — | `UsageRecordBuilder` | Plain builder. Construct via `UsageRecordBuilder::new()` or `UsageEmitter::usage_record_builder(metric, value)?` (which prefills `module`, `tenant_id`, `resource_id` / `resource_type`, `subject_id` / `subject_type`, `metric` + `kind`, and `value` from the authorized handle). Setters: `with_module`, `with_tenant_id`, `with_metric(name, kind)`, `with_value`, `with_resource(id, type)`, `with_subject(id, type)` / `with_subject_id(id)`, `with_idempotency_key`, `with_timestamp`, `with_metadata`. Call `.build()` to obtain a `UsageRecord`. |
| — | `UsageEmitterConfig` | Tunable authorization TTL, outbox queue name, partition count. Embedded in the host module's own config struct. |
| — | `CanonicalError` | Error type returned by all emitter methods. Re-exported from `modkit-canonical-errors`. |

## Emitting a usage record

```rust
use usage_emitter::{UsageEmitter, UsageEmitterFactory, UsageEmitterRuntimeV1};

// In init(): obtain the runtime from ClientHub and store a factory bound to this module.
let runtime = hub.get::<dyn UsageEmitterRuntimeV1>()?;
let emitter_factory: UsageEmitterFactory = runtime.factory(Self::MODULE_NAME);

// In a handler — authorize for this call site (single PDP call + allowed-metrics fetch;
// the returned UsageEmitter is valid for UsageEmitterConfig::authorization_max_age).
let emitter: UsageEmitter = emitter_factory
    .clone()
    .authorize(&ctx, resource_id, &resource_type)
    .await?;

// Build a record (the builder is prefilled from the authorized emitter — metric kind is
// resolved from the allowed-metrics list; an unknown metric returns an error here) and
// enqueue on the emitter's DB connection.
let record = emitter
    .usage_record_builder("requests", 1.0)?
    .build()?;
emitter.enqueue(record).await?;

// Enqueue inside a caller transaction (atomic with your write).
// let record = emitter.usage_record_builder("requests", 1.0)?.build()?;
// emitter.enqueue_in(&txn, record).await?;

// Optional: set idempotency key, timestamp, or metadata before calling .build().
// let record = emitter
//     .usage_record_builder("requests", 1.0)?
//     .with_idempotency_key("key123")
//     .with_timestamp(Utc::now())
//     .build()?;
// emitter.enqueue(record).await?;

// Emit with overrides — explicit tenant + no subject (e.g. system jobs).
// let emitter = emitter_factory
//     .clone()
//     .with_tenant(target_tenant)
//     .without_subject()
//     .authorize(&ctx, resource_id, "system_job")
//     .await?;

// Forwarder / REST ingest — explicit subject from the request body.
// let emitter = emitter_factory
//     .clone()
//     .with_tenant(req.tenant_id)
//     .with_subject_opt(req.subject_id, req.subject_type.as_deref())?
//     .authorize(&ctx, req.resource_id, &req.resource_type)
//     .await?;

// `UsageRecordBuilder::new()` can also be used standalone — every required field
// (`module`, `tenant_id`, `metric`, `value`, `resource_id`, `resource_type`) must
// then be set explicitly via the corresponding `with_*` setter; `.build()` returns
// `UsageEmitterError::InvalidArgument` listing any missing fields.
```

## Configuration

`UsageEmitterConfig` is embedded in the host module's own config struct with `#[serde(default)]`, so all fields are optional in YAML:

```yaml
modules:
  usage-collector:         # or usage-collector-rest-client
    config:
      emitter:
        authorization_max_age: "30s"   # default
        outbox_queue: "usage-records"  # default
        outbox_partition_count: 4      # default; power of 2 in 1–64
```

| Field | Default | Description |
|-------|---------|-------------|
| `authorization_max_age` | `30s` | Maximum age of a `UsageEmitter` handle before `enqueue*` rejects it with `AuthorizationExpired` |
| `outbox_queue` | `usage-records` | Outbox queue name |
| `outbox_partition_count` | `4` | Partition count (power of 2 in 1–64) |

## Error handling

```rust
use usage_emitter::CanonicalError;

match emitter.enqueue(record).await {
    Ok(()) => {}
    Err(CanonicalError::PermissionDenied { .. }) => { /* PDP denied or authorization expired */ }
    Err(CanonicalError::Internal { .. }) => { /* metric not allowed, non-finite value, or runtime error */ }
    Err(CanonicalError::ServiceUnavailable { .. }) => { /* outbox DB write failed or transient outage */ }
    Err(e) => { /* other canonical error */ }
}
```

## Background delivery

The outbox worker dequeues from the queue configured via `outbox_queue` (host overrides apply) and calls `UsageCollectorClientV1::create_usage_record` per message:

- **`Ok`** — message acknowledged
- **`DeadlineExceeded`** / **`ResourceExhausted`** / **`ServiceUnavailable`** — transient (plugin timeout, rate limit, HTTP 5xx, circuit breaker open, transport error); message is retried
- **Other errors** — permanent (`Unauthenticated`, `PermissionDenied`, `NotFound`, `Internal`); message is dead-lettered

Delivery is independent of the request that enqueued the record and survives process restarts through the durable outbox.

## Testing

```bash
devbox run cargo test -p cyberware-usage-emitter
```
