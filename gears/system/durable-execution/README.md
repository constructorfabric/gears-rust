# Durable execution

Durable execution is a system Gear for typed Rust workflows.
It supports durable checkpoints, runtime fan-out, retries, and saga compensation.
The full runtime uses PostgreSQL, transactional ToolKit Outbox, and Apalis.
Consumer gears depend on `cf-gears-durable-execution-sdk`.
The host links the implementation and controls its lifecycle.

## Getting Started

Add `durable-execution-sdk.workspace = true` to the consumer gear and import it as `durable_execution_sdk`.
Declare the `durable_execution` gear dependency so the host initializes it first.
In `init`, resolve `WorkflowRegistry` and `DurableExecutionClient::resolve` from ClientHub.
When the gear starts, build its workflows and call `register_workflow`.
Keep the returned typed `WorkflowRef` for the gear's own API.
The [consumer gear example](durable-execution/examples/consumer_gear.rs) shows the complete wiring.

## Authoring

```rust
use durable_execution_sdk::prelude::*;
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
let first_receipt = reservations.branch(0);
let workflow = builder
    .then(Step::with_deps("summarize", Duration::from_secs(5), &reservations,
        |_, receipts: Vec<String>, all: Vec<String>| async move { Ok(receipts.len() + all.len()) }))
    .build()?;
let reference = registry.register_workflow(workflow).await?.reference;
```

Submit input through `DurableExecutionClient::start`.
Use `StartedRun::handle()` for `result` and `step_result`.
`DurableExecutionClient::wait` reads progress until the run settles; bound it with `tokio::time::timeout`.
Use `reference.attach(run_id)` to restore a saved handle.
Attach does not read storage or grant access.
Result reads check the expected fingerprint before payload hydration.
Contract mismatch returns `FailedPrecondition`.
Typed decode failure returns `Internal`.

`.then` receives the preceding output.
`.parallel` supplies the current input to fixed tuple or list branches.
Its join keeps declaration order.
Its checkpoint's `.branches()` returns typed references directly.
`.for_each` consumes `Vec<Item>` and joins `Vec<Output>` in input order.
Its checkpoint's `.branch(index)` selects a run-specific branch.
Reads check that the branch exists and has a result.
An empty list completes without handler calls.

Capture earlier checkpoints with `.checkpoint()`.
Pass them to `Step::with_deps`; the handler receives their decoded values as a typed argument.
Use the `Activity` trait for steps that own a provider client.
Supply a timeout to `Step::new`, `Step::with_deps`, or `Step::activity`.
Timeouts must be positive and no greater than 24 hours.
Build errors report a `DefinitionErrorKind` and the offending step ID.
Return `ActivityError` codes of 1..=64 bytes of `[a-z0-9_]`; `or_retryable` and `or_permanent` map provider errors.

Enable the SDK `test-util` feature in `dev-dependencies` to run workflows in memory with `testing::TestRuntime`.
`DurableExecutionClient::watch` streams progress after each committed event until the run settles.
It runs handlers, retries, fan-out and saga compensation without PostgreSQL.
Recovery, concurrency and control commands still need the PostgreSQL tests.
The fingerprint includes stages, data sources, policies, compensation, and fan-out limits.
It excludes run-specific item counts and local Rust functions.
Use a new definition version for changed payload formats or changed business behavior.

## Saga and Control

`.saga()` enables compensation after permanent failure, exhausted retries, or Abort.
The runtime stops new forward claims and signals active handlers.
Compensation waits for confirmed cleanup or lease expiry.
It runs declared compensators for confirmed successful activities.
It uses reverse stage order and reverse branch-index order.
Only one compensator runs at a time.
Original inputs, successful outputs, and separate stable keys survive restart.

`CompensationFailed` keeps the original cause separately from the compensation error.
Manual Retry continues the failed and remaining compensators.
It keeps successful compensation checkpoints.
`Compensated` rejects forward Retry and Resume.
A new run is necessary for another business attempt.

Cancel pauses forward work.
Resume continues from saved checkpoints.
Abort stops forward work permanently for the run.
Cancel during compensation is rejected.
Shutdown keeps the current phase.
Saga `CancelAndRelease` keeps bindings until compensation completes.
Compensation failure keeps the definition `Stopping`.
Restore matching saga bindings after restart, including during `Stopping`.

External effects can repeat.
Compensation covers declared compensators and confirmed outputs only.
Providers must deduplicate or reconcile effects before a lost checkpoint.
The SDK does not supply a distributed transaction.
Keep credentials in provider clients.

## Observation and Operation

Get and inspect read only the stored progress projection in one scoped query.
List uses one count query and one paginated projection query.
Results and history use separate reads and authorization.
The cursor identifies the last committed event for the run.
Events tell clients when to read progress again.
For control commands, supply the expected epoch and current authorization.

Unsupported stored formats are rejected.
There is no legacy reader, fingerprint fallback, or automatic database deletion.

## Configuration

Execution and delivery are disabled by default.
Set `execute_activities=true` to run workers and delivery.
Set `delivery_enabled=true` to publish work on a host without workers.
Set the queue DSN through `queue_database_url_env` (default `DATABASE_URL`).
Set the service secret through `service_client_secret_env` (default `DURABLE_SERVICE_CLIENT_SECRET`).
Restore local handlers and compensators after restart, including saga bindings while `Stopping`.
Use a clean development or test database for the current format.
The [Config source](durable-execution/src/config.rs) defines all settings, defaults, and validation.

## Checks

Use the existing [Makefile](../../../Makefile) targets:

```sh
make gear-ci GEAR=durable-execution
make test-durable-execution-pg
```

The [CI workflow](../../../.github/workflows/ci.yml) runs the PostgreSQL and subprocess recovery target.

## Documents

- [Executable examples](durable-execution/examples/README.md)
- [Requirements](docs/PRD.md)
- [Design](docs/DESIGN.md)
- [Runtime decision](docs/adrs/0004-saga-fanout-and-progress.md)
