# Durable-execution examples

Each executable uses the real gear lifecycle, journal, Outbox and Apalis runtime.
Use a disposable PostgreSQL database and a nonempty demo secret:

```sh
export DURABLE_EXAMPLE_PG_URL=postgres://postgres:postgres@localhost:5432/postgres
export DURABLE_EXAMPLE_SECRET=local-example-secret
cargo run -p cf-gears-durable-execution --example single_activity
```

The support module installs local AuthN/PDP adapters for a fixed example tenant
and checks the service credentials. These adapters demonstrate explicit grants;
production hosts use their normal AuthN/PDP integration. Do not point examples at
production data. Each scenario asserts its result and shuts down the lifecycle.
Run examples sequentially when sharing a database.

Each example is a concrete business scenario built around one SDK mechanism.
External systems (mailer, payment gateway, cloud API, CRM) are in-memory fakes in
[support/fake.rs](support/fake.rs); they deduplicate by `ctx.idempotency_key` the
way real providers do, because activities are at-least-once.

| Example | Business scenario | Mechanism | Expected result |
|---|---|---|---|
| [single_activity](single_activity.rs) | **Notifications.** Send an order confirmation email. | Associated-type `Activity`; idempotent start (a repeated request returns the same run). | One receipt; the mailer sends exactly once. |
| [sequential_workflow](sequential_workflow.rs) | **Order saga.** Authorize payment, reserve stock, activate the subscription. | Typed `.then` chain; a later step reads an earlier checkpoint through `.uses`. | Activation carries the payment authorization; payment authorized once. |
| [parallel_workflow](parallel_workflow.rs) | **Fan-out.** Monthly customer report from billing and usage, exported per region. | Heterogeneous parallel tuple, join, dynamic branch list, ordered join. | 240 rows in EU/US/APAC files; per-branch results readable. |
| [retries_and_resume](retries_and_resume.rs) | **Provisioning.** Create a VM through a throttling cloud API; quota failure; maintenance pause. | Automatic retry; epoch-checked Retry, Cancel and Resume; retained checkpoint. | VM handle; the network is allocated once; the cloud is called twice. |
| [cancellation_and_shutdown](cancellation_and_shutdown.rs) | **Provisioning.** Long image build in an external builder process. | Cooperative cancellation and cleanup of owned work; shutdown is not cancellation. | User cancel ends `Cancelled`; host shutdown returns the run to `Queued`; the builder is killed both times. |
| [dynamic_registration](dynamic_registration.rs) | **Integration.** Per-tenant CRM contact sync paused, disconnected and reconnected with new credentials. | `Retain`/`activate`, `CancelAndRelease`, rebinding, new generation. | Starts rejected while paused; the new generation uses the new credentials. |
| [progress_projection](progress_projection.rs) | **Order status page.** The UI polls checkout progress; support staff inspect runs. | Progress and cursor, events, history, separately authorized inspection. | Views never contain the card token or step results. |
| [contract_and_bindings](contract_and_bindings.rs) | **Notifications with split roles.** The public API accepts invoice emails; the worker with SMTP credentials sends them. | Serialized contract on the API host; late worker bindings. | No attempts are consumed until the worker binds; then one email is sent. |

Replace `single_activity` in the command with any example name. A successful
process exit means its assertions passed. The contract/bindings example separates
the API operations within one host; the PostgreSQL tests also verify independent
hosts and restoration after shutdown.

A workflow is a sequence of stages. `.then` changes the current output type;
`.parallel` supplies the same current input to every branch and returns a tuple
(two through eight branches) or `Vec<Output>`. Joining waits for successful
checkpoints from every branch and retains declaration order. A failed branch does
not stop eligible siblings; progress remains `Failing` until the stage settles.

Capture an earlier stage with `.checkpoint()`. Steps that call
`ctx.checkpoint(&reference)` must declare `.uses(&reference)`. Build rejects
foreign, current-stage and future dependencies. For a parallel checkpoint,
`.branches()` returns typed references to each branch in declaration order; use
them with `.uses()` or `step_result`. The serialized flow, step IDs,
timeouts and retry policies form the contract fingerprint. Changing payload
formats or handler semantics requires a new workflow version; Rust type names do
not define persisted schemas.

`get` returns progress and its cursor without results. Use `events` after that
cursor to detect changes, then reread progress. Use `result`, `step_result` and
`history` explicitly. Events do not reconstruct journal state. Every operation
performs fresh authorization; administrative inspection does not grant result access.

[Configuration](../../docs/configuration.md) | [Tests](../../docs/testing.md) |
[SDK design](../../docs/DESIGN.md)
