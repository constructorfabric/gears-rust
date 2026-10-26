# toolkit-durable-execution

Typed durable workflows that a ToolKit gear embeds.
A workflow survives process restarts: sequential steps, fixed parallel branches, runtime fan-out, retries and saga compensation.
Checkpoints live in the gear's own database; work is delivered through a PostgreSQL queue (Apalis) and the transactional ToolKit Outbox.

**Stability:** experimental. The API, the stored format and the error codes may change in any release.

External effects run at least once.
Activities must deduplicate with `ActivityContext::idempotency_key`; the library does not supply a distributed transaction.

## Getting started

Add `toolkit-durable-execution.workspace = true` to the gear and wire it like the outbox:

```rust,ignore
use toolkit_durable_execution::{Engine, ExecutionMode, QueueConfig, migrations};

// DatabaseCapability::migrations
fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
    let mut all = self.own_migrations();
    all.extend(toolkit_durable_execution::migrations());
    all
}

// Gear::init: settings only, no I/O.
let queue = QueueConfig::new(config.queue_dsn.clone(), "orders-durable");
let engine = Engine::builder(ctx.db_required()?.db(), Arc::new(OrdersAuthorizer::new(hub)?))
    .mode(ExecutionMode::Worker(queue))
    .build()?;
let client = engine.client();      // keep for the gear's API
let registry = engine.registry();  // trusted catalog management

// Lifecycle entry (`#[toolkit::gear(..., lifecycle(entry = "serve", stop_timeout_fn = ...))]`):
// register workflows, prepare, then serve until the host cancels.
let orders = registry.register_workflow(orders_workflow()?).await?.reference;
let runner = engine.prepare().await?;   // a PrepareError hands the engine back for a retry
runner.run(cancel).await?;              // a LifecycleError ends the lifecycle
```

Use `Engine::shutdown_timeout` as the gear's stop timeout and `Engine::readiness` for its health check; readiness turns off as soon as shutdown starts.
[`examples/consumer_gear.rs`](examples/consumer_gear.rs) shows the complete wiring with the `#[toolkit::gear]` macro.

### Execution modes

| Mode | Observes and cancels | Starts and schedules runs | Executes activities |
|------|----------------------|---------------------------|---------------------|
| `ControlPlane` (default) | yes | no (`FailedPrecondition`, code `DELIVERY_DISABLED`) | no |
| `Dispatcher(QueueConfig)` | yes | yes | no |
| `Worker(QueueConfig)` | yes | yes | yes |

Until `Engine::prepare` starts delivery, `Dispatcher` and `Worker` engines reject scheduling with retryable `Unavailable`.
`QueueConfig::new(dsn, queue)` takes the PostgreSQL DSN as a `SecretString` and the default queue name; the library never reads environment variables.
Queues live in the shared `apalis` schema, so every engine on a database needs its own queue names; name them after the gear.
A delivery for a run the engine cannot see is logged at `warn` and left to its owner's recovery.

### Authorization

The library defines the `Authorizer` port; the gear implements it.
`caller_scope` authorizes a caller's `CallerAction`, `service_scope` authorizes background and catalog work.
Each action exposes its stable PDP name (`as_str`), its `Resource` and whether row constraints are required.

```rust,ignore
use authz_resolver_sdk::{PolicyEnforcer, pep::{AccessRequest, ResourceType}};
use toolkit_durable_execution::{AuthorizationError, Authorizer, CallerAction, ConstraintPolicy, Resource, ServiceAction};

struct OrdersAuthorizer { enforcer: PolicyEnforcer, service: ServiceIdentity }

#[async_trait]
impl Authorizer for OrdersAuthorizer {
    async fn caller_scope(&self, ctx: &SecurityContext, action: CallerAction) -> Result<AccessScope, AuthorizationError> {
        self.scope(ctx, action.resource(), action.as_str(), action.constraints()).await
    }
    async fn service_scope(&self, action: ServiceAction) -> Result<AccessScope, AuthorizationError> {
        // Cache the service context: this runs before every worker step.
        let ctx = self.service.context().await.map_err(|e| AuthorizationError::unavailable().with_source(e))?;
        self.scope(&ctx, action.resource(), action.as_str(), action.constraints()).await
    }
}

impl OrdersAuthorizer {
    async fn scope(&self, ctx: &SecurityContext, resource: Resource, action: &str, constraints: ConstraintPolicy)
        -> Result<AccessScope, AuthorizationError> {
        let resource = ResourceType::from_static(resource.name(), resource.properties());
        let request = AccessRequest::new().require_constraints(constraints == ConstraintPolicy::Required);
        self.enforcer.access_scope_with(ctx, &resource, action, None, &request).await.map_err(|e| match e {
            EnforcerError::Denied { .. } => AuthorizationError::denied(),
            other => AuthorizationError::unavailable().with_source(other),
        })
    }
}
```

The engine keeps the policy-independent checks: it rejects callers without a subject or tenant, rejects an unconstrained scope for actions whose `constraints()` are `Required`, limits owner operations to the calling subject, hides other owners' runs and bounds every call with `EngineBuilder::authorization_timeout`.
Rejections are logged with the action, resource and reason (`denied`, `unavailable`, `timeout`, `unconstrained_scope`, `missing_identity`) and, for callers, the subject and tenant ids; never with the authorizer's error text.
Both `Authorizer` futures may be dropped when the timeout expires, so they must be cancel-safe.

## Authoring

```rust
use toolkit_durable_execution::prelude::*;
use std::time::Duration;

let builder = WorkflowBuilder::<Vec<u32>>::new("orders.reserve.v1").saga()
    .for_each(
        Step::new("reserve", Duration::from_secs(30), |ctx, item: u32| async move {
            // The provider must deduplicate with ctx.idempotency_key().
            Ok(item.to_string())
        }).compensate(Compensation::new(Duration::from_secs(30),
            |ctx, original_item: u32, receipt: String| async move {
                // Compensate the confirmed receipt; ctx carries the separate reverse key.
                Ok(())
            })),
        FanOutOptions::default(),
    );
let reservations = builder.checkpoint();
let workflow = builder
    .then(Step::with_deps("summarize", Duration::from_secs(5), &reservations,
        |_, receipts: Vec<String>, all: Vec<String>| async move { Ok(receipts.len() + all.len()) }))
    .build()?;
# Ok::<(), toolkit_durable_execution::DefinitionError>(())
```

- `.then` receives the preceding output; `.parallel` supplies the current input to fixed tuple or list branches and joins in declaration order.
- `.for_each` consumes `Vec<Item>` and joins `Vec<Output>` in input order; an empty list completes without handler calls.
- Capture earlier checkpoints with `.checkpoint()` and pass them to `Step::with_deps`.
- Use the `Activity` trait for steps that own a provider client.
- Timeouts must be positive and at most 24 hours. Build errors report a `DefinitionErrorKind` and the offending step.
- Return `ActivityError` codes of 1..=64 bytes of `[a-z0-9_]`; `or_retryable` and `or_permanent` map provider errors.

The contract fingerprint covers stages, data sources, policies, compensation and fan-out limits, not Rust functions.
Use a new definition version for changed payload formats or business behavior.

## Runs

- `ExecutionClient::start` admits a run and returns a typed `StartedRun`; use `StartedRun::handle()` for `result` and `step_result`.
- `wait` and `watch` read progress until the run settles; bound `wait` with `tokio::time::timeout`.
- `WorkflowRef::attach(run_id)` restores a saved handle without reading storage or granting access.
- Result reads check the expected fingerprint before decoding the payload.

`Cancel` pauses forward work and `Resume` continues from saved checkpoints.
`Abort` stops forward work permanently; with `.saga()`, permanent failure or `Abort` runs declared compensators in reverse stage and branch order.
`CompensationFailed` keeps the original cause; manual `Retry` continues the failed compensators.

## Errors

| Type | Returned by | Use |
|------|-------------|-----|
| `Error` | client, registry and inspector calls | `kind()` for recovery, `code()` as a stable value, `is_retryable()` |
| `DefinitionError` | `WorkflowBuilder::build` and step constructors | `kind()` and `step()` |
| `ConfigError` | `EngineBuilder::build` | `setting()` and `reason()` |
| `LifecycleError` | `Engine::prepare`, `Runner::run` | return it from the gear lifecycle |

`Error::source` keeps the underlying storage, authorization or decoding failure.
The library does not depend on the platform error model: like `toolkit_db` errors, the gear maps `Error` to its canonical errors at its REST or gRPC boundary.
`Error::kind` gives the category, `Error::resource` whether a run or a definition is concerned, and `Error::code` (one of `codes::*`) a stable reason.

| `ErrorKind` | Canonical category |
|-------------|--------------------|
| `InvalidArgument` | invalid argument |
| `NotFound` | not found |
| `AlreadyExists` | already exists |
| `PermissionDenied` | permission denied |
| `FailedPrecondition` | failed precondition |
| `Aborted` | aborted |
| `Unavailable` | service unavailable |
| `Internal` | internal |

## Storage

`migrations()` returns the journal migration `m0001_durable_execution__init` and the ToolKit outbox migrations for `durable_execution__delivery_outbox` as separate entries.
Each is recorded under its own name in the gear's migration history, so migrations added by later releases are applied to existing databases.
The prefix is fixed, so one database or schema hosts one engine.
There is no reader for formats of the former durable-execution gear.

## Testing

Enable the `test-util` feature in `dev-dependencies` to run workflows in memory with `testing::TestRuntime`.
It runs handlers, retries, fan-out and saga compensation without PostgreSQL.
Recovery, concurrency and control commands need the PostgreSQL suites:

```sh
cargo test -p cf-gears-toolkit-durable-execution --features test-util
make test-durable-execution-pg   # Docker required
```

## Documents

- [Executable examples](examples/README.md)
- [Requirements](docs/PRD.md)
- [Design](docs/DESIGN.md)
- [Embeddable library decision](docs/adrs/0005-embeddable-library.md)
