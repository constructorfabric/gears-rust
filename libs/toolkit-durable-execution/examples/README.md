# Durable-execution examples

## Run an Example

Each executable uses the real engine lifecycle, journal, Outbox, and Apalis runtime.
Use a disposable PostgreSQL database.
Set `DURABLE_EXAMPLE_PG_URL` to its URL.
The examples read this variable; the library reads no environment variables.

```sh
export DURABLE_EXAMPLE_PG_URL=postgres://postgres:postgres@localhost:5432/postgres
cargo run -p cf-gears-toolkit-durable-execution --example single_activity
```

Replace `single_activity` with the necessary example name.
Run examples sequentially when they share a database.
Do not use production data.
Each example checks assertions and stops its engine.
A successful process exit confirms those assertions.

The [support module](support/mod.rs) applies `toolkit_durable_execution::migrations()` and builds the engine directly.
It uses `ExecutionMode::Worker` with a `QueueConfig` for the example database.
Its `ExampleAuthorizer` implements the `Authorizer` port.
It grants caller access to the example tenants and service access to both tenants explicitly.
A production gear implements `Authorizer` with its `PolicyEnforcer` and a cached service identity.
The providers are local fakes without paid external calls.
Most use [support/fake.rs](support/fake.rs).
The CRM example keeps credentials in its local `CrmClient`.
Providers deduplicate calls with the supplied idempotency key.

## Scenarios

| Example | Scenario and mechanism | Expected result |
|---|---|---|
| [single_activity](single_activity.rs) | Send an order email with `Activity` and idempotent start. | Repeated start returns the same run and one mail receipt. |
| [sequential_workflow](sequential_workflow.rs) | Pay, reserve stock, and fail activation in a saga. | Stock compensation fails, then manual Retry completes stock cleanup and payment refund. |
| [parallel_workflow](parallel_workflow.rs) | Join billing/usage and export a variable region list with `.for_each`. | Results keep input order; an empty list calls no export handler. |
| [retries_and_resume](retries_and_resume.rs) | Provision a VM with throttling, quota failure, and maintenance pause. | Retry/Resume keep the network checkpoint. |
| [cancellation_and_shutdown](cancellation_and_shutdown.rs) | Build an image through an owned external process. | Cancel reports `Cancelled`; shutdown restores `Queued`; both clean up the process. |
| [dynamic_registration](dynamic_registration.rs) | Manage CRM clients for two tenants with separate versioned names. | Tenant checks and separate pause, disconnect, and reconnect keep isolation. |
| [progress_projection](progress_projection.rs) | Read checkout progress as a caller and inspect it as support staff. | Progress excludes the card token and result payloads. |
| [contract_and_bindings](contract_and_bindings.rs) | Submit with a serialized contract before worker binding. | No attempt is consumed before binding; the worker then sends one email. |
| [consumer_gear](consumer_gear.rs) | An orders gear declared with `#[toolkit::gear]` embeds the engine: its migrations include `migrations()`, `init` builds the engine from the gear config, and the lifecycle entry `serve` registers, prepares, and awaits `Runner::run`; `WithLifecycle` provides the two-phase stop. | A retried confirmation joins the first run and sends one email. |

The contract/bindings example separates roles within one host.
Its test checks that `contracts/bindings.v1.json` matches the worker code:
`cargo test -p cf-gears-toolkit-durable-execution --example contract_and_bindings`.
Set `UPDATE_CONTRACTS=1` to rewrite the file after an intentional change.
PostgreSQL tests also check separate hosts and restoration after shutdown.
The order example checks compensation failure and Retry.
Separate provider subprocess tests check worker crashes before forward and compensation checkpoints.

## Authoring and Results

A workflow is a sequence of stages.
`.then` changes the current output type.
`.parallel` gives the same input to fixed branches.
Tuples support two through eight branches with different output types.
Lists support homogeneous static branches.
`.for_each` consumes a run-specific `Vec<Item>`.
Its output is `Vec<Output>` in input order.
Without saga compensation, a failed static branch does not stop eligible siblings.

Capture an earlier checkpoint with `.checkpoint()`.
Pass it to `Step::with_deps`; the handler receives its decoded value.
Build rejects foreign, current-stage, and future dependencies.
Static `.branches()` returns references directly.
Fan-out `.branch(index)` creates a selector whose existence is checked during a read.
Use typed handles with `result` and `step_result`.
Use `WorkflowRef::attach` to restore a handle from a saved run ID.
Contract identity is checked before payload hydration.

Supply timeouts to step constructors.
Use `Activity` for steps that own a provider client, and `?` with `or_retryable` or `or_permanent`.
Declare compensators with their own timeout and retry policy.
Use a new definition version when payload format or business behavior changes.
Keep credentials in provider clients and store only necessary business receipts.

## Progress and Control

Get returns progress and its committed event cursor without results.
`wait` and `wait_until` poll progress until the run settles or a predicate holds.
`watch` streams progress after each committed event until the run settles.
Read events after that cursor to detect changes.
Then read progress again.
Use result and history operations explicitly.
Every operation uses current authorization.
Administrative inspection does not grant result access.

Cancel pauses forward work.
Resume keeps completed checkpoints.
Abort permanently stops forward work and starts declared saga compensation.
Compensation Retry keeps successful compensation checkpoints.
`CancelAndRelease` keeps saga bindings until compensation completes.
Restore exact bindings after worker restart, including while the definition is `Stopping`.

See [README execution modes](../README.md#execution-modes), [README authorization](../README.md#authorization), [Makefile](../../../Makefile), and [CI workflow](../../../.github/workflows/ci.yml).
Use the [design](../docs/DESIGN.md) and its [technical terms](../docs/DESIGN.md#23-terminology).
