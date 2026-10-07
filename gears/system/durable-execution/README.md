# Durable execution

A system gear for background Rust activities and workflows. It persists inputs,
checkpoints and execution state, applies retries and recovers after process loss.
Handlers run in process. **The full runtime requires PostgreSQL; SQLite is used
for journal tests.**

Consumer gears use `cf-gears-durable-execution-sdk` and depend on the
`durable_execution` gear. Link `cf-gears-durable-execution` into the host; its
lifecycle starts and stops the runtime. Execution and delivery default to disabled.
Enable `execute_activities` on workers and `delivery_enabled` on submission-only
hosts. Cooperating hosts share the journal and queue names.

## Typed workflow

```rust
use std::time::Duration;
use durable_execution_sdk::{prelude::*, observation::RunState};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ConfirmationEmail { order_id: String, to: String }

// The mailer client lives in the handler; inputs never carry credentials.
let workflow = WorkflowBuilder::<ConfirmationEmail>::new("notifications.order-confirmation.v1")
    .then(Step::new("send_confirmation", move |ctx, email: ConfirmationEmail| {
        let mailer = mailer.clone();
        // At-least-once: the provider deduplicates a repeated send by this key.
        async move { mailer.send(&ctx.idempotency_key, &email).await }
    })
    .timeout(Duration::from_secs(30)))
    .build()?;
let reference = workflow.reference();
let registry = ctx.client_hub().get::<dyn WorkflowRegistry>()?;
registry.register(workflow.into_definition()).await?;

let execution = DurableExecutionClient::resolve(ctx.client_hub())?;
let email = ConfirmationEmail { order_id: "ord-1001".into(), to: "buyer@example.com".into() };
let started = execution.start(&user_context, &reference, email, StartOptions {
    // A retried checkout request returns the same run instead of a second email.
    idempotency_key: Some("confirm:ord-1001".into()),
    ..Default::default()
}).await?;
let progress = execution.get(&user_context, started.run_id).await?;
if matches!(progress.state, RunState::Succeeded { .. }) {
    let message_id: String = execution.result(&user_context, started.run_id, &reference).await?;
}
```

`.then` passes the preceding stage's result. `.parallel` accepts heterogeneous
step tuples or a homogeneous step list and joins in declaration order. Capture
an earlier stage with `.checkpoint()` and declare `.uses(&checkpoint)` on steps
that read it. Each step requires an explicit timeout; retry policy has a default.
Build validates IDs, policies and checkpoint dependencies before database access.

`get` returns payload-free progress and a consistent event cursor. Read results,
step results and execution history explicitly. Events notify clients to reread
progress. `ExecutionInspector` has separate administrative authorization and does
not grant access to results. Cancel, Retry and Resume require the expected epoch.

## Configuration

Use the host's toolkit-db provider and queue adapter against the same PostgreSQL
database. Configure AuthN/PDP permissions and keep the service secret outside
workflow inputs:

```text
DATABASE_URL=postgres://user:password@localhost/gears
DURABLE_SERVICE_CLIENT_SECRET=<service secret>
```

```yaml
gears:
  durable-execution:
    database:
      dsn: "${DATABASE_URL}"
    config:
      execute_activities: true
      queue_database_url_env: DATABASE_URL
      service_client_id: durable-worker
      service_client_secret_env: DURABLE_SERVICE_CLIENT_SECRET
      service_scopes: [durable-execution]
```

Applications restore local handlers after restart; catalog entries survive.
Dynamic registration supports `Retain`, `CancelAndRelease` and explicit `activate`.
External effects are **at least once**. Use request and activity idempotency keys,
honor cancellation and finish cleanup of owned external work before returning.
Dropping a future does not stop a subprocess, blocking thread or external request.
Shutdown preserves checkpoints and does not cancel the workflow as a business action.

[Runnable examples](durable-execution/examples/README.md) show concrete scenarios:
- order confirmation email and invoice emails sent from separate API and worker roles;
- an order saga (payment, stock, subscription) and an order status page;
- a fan-out monthly report;
- VM provisioning with cloud throttling, Retry and Resume;
- a long image build that survives cancellation and host shutdown;
- a CRM sync that is paused, disconnected and reconnected.

They use PostgreSQL and explicit local AuthN/PDP adapters.

[Configuration and operation](docs/configuration.md) | [Requirements](docs/PRD.md) |
[Design](docs/DESIGN.md) | [Tests and coverage](docs/testing.md)

- [Apalis + transactional Outbox](docs/adrs/0001-apalis-with-transactional-outbox.md)
- [Framework selection](docs/adrs/0002-task-execution-framework-selection.md)
- [Typed SDK and persisted journal](docs/adrs/0003-typed-sdk-and-journal-boundary.md)
