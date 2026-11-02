# Lifecycle and Stateful Tasks

ToolKit provides lifecycle management for gears with background tasks, graceful shutdown, and cancellation support.

## Core invariants

- **Rule**: Use `CancellationToken` for coordinated shutdown across the entire process tree.
- **Rule**: Use `WithLifecycle<T>` for stateful gears with background tasks.
- **Rule**: Pass child tokens to background tasks for cooperative shutdown.
- **Rule**: Implement `RunnableCapability` for custom lifecycle (rare).
- **Rule**: Never wait for a remote service in a lifecycle hook (`init`, `post_init`, `start`).
- **Rule**: A gear that publishes GTS documents declares them with `gts(...)`; the runtime handles publication retries and the admission barrier.

## Runtime startup

For application gears, use the default startup stage. The runtime runs DB migrations and
waits for admission of all declared GTS Type Schemas and Instances before application `init`.
Do not implement a publication retry loop or wait for admission inside a lifecycle hook.

The runtime starts gears in two groups:

- **Infrastructure** (`stage = infrastructure`): Types Registry, Service Discovery,
  Cluster, API Gateway and gRPC Hub. These provide the services needed during startup.
- **Application** (default): all other gears, including resolvers and plugins.

```text
Collect GTS declarations → Start infrastructure → Admit schemas and instances → Start applications
```

1. **Validate.** Collect GTS Type Schemas and Instances from declarations and configuration.
   Reject conflicting GTS ownership or invalid startup dependencies before initializing any gear.
2. **Start infrastructure.** Run its lifecycle and expose probes and platform APIs.
   Spawn configured OoP children without waiting for their readiness.
3. **Publish.** Prepare application gears (`pre_init` and DB migrations), wire registry
   clients, then run all publishers concurrently. Every declared GTS Type Schema and Instance
   must be admitted before any application `init` runs.
4. **Start applications.** Run `init`, remaining client wiring, legacy registry finalization
   and `post_init`; install final REST/gRPC services, call `start`, then register providers
   in the directory. Only then clear the `application/preparation` readiness gate.

Infrastructure cannot depend on an application gear in the same process or declare `gts(...)`.
A non-empty publication cannot target an application provider in the same process either:
that provider has not initialized yet.

On shutdown, call `stop` for gears whose `start` was called: applications first, infrastructure
last, in reverse order within each group. Migration-only mode runs just `pre_init` and migrations.
See [`HostRuntime`](../../libs/toolkit/src/runtime/host_runtime.rs) for the full hook and wiring order.

### Pending publication and failures

- **Retryable publication failure:** keep retrying, including after an individual attempt's
  deadline. Infrastructure continues serving; application initialization waits.
- **Terminal publication failure:** leave applications uninitialized and serve infrastructure
  until shutdown. Shutdown also cancels pending publications.
- **Invalid publication input or duplicate GTS owner:** fail before infrastructure starts.
- **Other startup errors:** return an error and stop gears already started.

### Routes and readiness

`/healthz` reports liveness. `/readyz` stays `503` until application startup completes and
its dependency gates and gear healthchecks pass. While waiting for GTS admission, the bootstrap
router serves probes and platform calls; tenant requests receive `503` with `Retry-After`.

Routes and readiness are separate: in-process REST routes go live before application `start`.
The `application/preparation` gate remains set until the entire startup sequence finishes.
An E2E launcher may wait for just this gate to clear, leaving other readiness checks to its suite;
[`readiness.py`](../../tools/scripts/lib/readiness.py) implements this check and rejects draining servers.

**OoP:** the same admission barrier applies, but the unit binds its probe server before lifecycle
hooks. It attaches infrastructure routes after infrastructure `start` and final routes after
application `start`. Its HTTP server manages directory presence; it does not spawn child processes.

For deployment preparation and the proposed deployment-managed mode, see
[Types Registry deployment design](../../gears/system/types-registry/docs/DESIGN.md#deployment-preparation).
The current runtime still runs migrations and publication reconciliation after a deployment job.

## Publishing GTS documents

Declare `gts(...)`; the runtime handles publication, retries and admission. Use `crates` for
static declarations and `entities` for documents built from configuration:

```rust
#[toolkit::gear(
    name = "static-authz-plugin",
    gts(
        crates = [crate, my_sdk],          // crates with `toolkit_gts::declare_gts_inventory!()`
        entities = Self::plugin_instance,  // optional: config-only documents
    ),
)]
pub struct StaticAuthZPlugin { /* ... */ }

impl StaticAuthZPlugin {
    fn plugin_instance(inputs: &GtsPublicationInputs<'_>) -> anyhow::Result<Vec<(String, Value)>> {
        let cfg: Config = inputs.config_or_default()?;
        /* build the instance from cfg */
    }
}

#[async_trait]
impl Gear for StaticAuthZPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let service: Arc<dyn Client> = /* ... */;
        // All declared schemas and instances were admitted before `init`.
        toolkit::plugins::bind_plugin_instances::<Spec, _>(
            &ctx.client_hub(), ctx.gts_publication_manifest()?.documents(), &service)
    }
}
```

The default publisher, `types_registry_sdk::gts_publisher`, requires `types-registry-sdk`
with the `rest-client` feature and wires its registry client automatically. Add `#[toolkit::consumes]` if your own code also uses that contract.
A gear with no GTS Type Schemas or Instances to publish needs no registry for publication.

In `init`, `ctx.gts_publication_manifest()` returns a `GtsManifest`: the publisher name/version
and authored GTS Type Schemas and Instances, all admitted before `init`. This ToolKit data
structure is available only to gears declaring `gts(...)`. Plugins bind their scoped clients
from these documents.
`choose_registered_plugin_instance` selects the best-ranked instance with a bound client;
a plugin whose `init` has not run is not selectable. Declare `deps` when initialization order matters.

## A gear's own readiness

For work such as AM bootstrap, use `lifecycle(..., await_ready)`: perform the work in `serve`
and call `ready.notify()` when done. This changes that gear's lifecycle state to `Running`.
It does **not** block `start`, other gears, route publication or the application readiness gate.

Callers must handle a gear that is still bootstrapping. To expose this condition through
`/readyz`, provide a REST healthcheck; `Unhealthy` keeps the response at `503`, with or without
`gts(...)`.

## Declarative lifecycle with `#[toolkit::gear(...)]`

### Gear with lifecycle

```rust
#[toolkit::gear(
    name = "api-gateway",
    capabilities = [rest_host, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct ApiGateway {
    /* ... */
}
```

### Lifecycle parameters

- `entry`: Method name to run as the background task
- `stop_timeout`: Graceful shutdown timeout (e.g., "30s", "1m")
- `stop_timeout_fn`: Method name returning `Duration` from `&self`, evaluated at stop after initialization. Mutually exclusive with `stop_timeout`. Available in `#[gear(..., lifecycle(...))]` and `#[lifecycle(...)]`.
- `await_ready`: Wait for ready signal before marking as Running

## WithLifecycle states and transitions

```
Stopped ── start() ── Starting ──(await_ready? then ready.notify())──▶ Running
   ▲                                  │
   │                                  └─ if await_ready = false → Running immediately
   └──────────── stop()/cancel ────────────────────────────────────────────────┘
```

## Lifecycle entry method

### Accepted signatures

```rust
// 1) Without ready signal
async fn serve(
    self: std::sync::Arc<Self>,
    cancel: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()>

// 2) With ready signal
async fn serve(
    self: std::sync::Arc<Self>,
    cancel: tokio_util::sync::CancellationToken,
    ready: toolkit::lifecycle::ReadySignal,
) -> anyhow::Result<()>
```

### Implementation example

```rust
impl ApiGateway {
    async fn serve(
        self: std::sync::Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
        ready: toolkit::lifecycle::ReadySignal,
    ) -> anyhow::Result<()> {
        // Bind sockets/resources before flipping to Running
        let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
        let app = self.build_app();

        // Notify that we're ready
        ready.notify();

        // Run until cancelled
        axum::serve(listener, app)
            .with_graceful_shutdown(cancel.cancelled())
            .await?;

        Ok(())
    }
}
```

## CancellationToken usage

### Root token propagation

```rust
// In main()
let root_cancel = CancellationToken::new();
let mut runtime = GearRuntime::builder()
    .with_cancellation_token(root_cancel.clone())
    .build();

// Gears receive child tokens automatically
```

### Child tokens for background tasks

```rust
impl MyService {
    pub fn new(db: Arc<DbHandle>, cancel: CancellationToken) -> Self {
        let child = cancel.child_token();

        // Spawn background task
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = child.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {
                        // Do periodic work
                    }
                }
            }
        });

        Self { db, cancel }
    }
}
```

### Cooperative shutdown

```rust
pub async fn process_with_shutdown(
    cancel: CancellationToken,
) -> Result<Vec<TaskResult>, DomainError> {
    let mut tasks = Vec::new();

    for item in items {
        tokio::select! {
            result = process_item(item) => {
                tasks.push(result?);
            }
            _ = cancel.cancelled() => {
                // Graceful shutdown: return partial results
                return Ok(tasks);
            }
        }
    }

    Ok(tasks)
}
```

## Background task patterns

### Periodic task

```rust
pub struct PeriodicTask {
    cancel: CancellationToken,
}

impl PeriodicTask {
    pub fn spawn(cancel: CancellationToken) {
        let child = cancel.child_token();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                tokio::select! {
                    _ = child.cancelled() => break,
                    _ = interval.tick() => {
                        // Do periodic work
                        if let Err(e) = do_periodic_work().await {
                            tracing::error!("Periodic work failed: {}", e);
                        }
                    }
                }
            }
        });
    }
}
```

### Event listener

```rust
pub struct EventListener {
    receiver: broadcast::Receiver<Event>,
    cancel: CancellationToken,
}

impl EventListener {
    pub fn spawn(
        mut receiver: broadcast::Receiver<Event>,
        cancel: CancellationToken,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    event = receiver.recv() => {
                        match event {
                            Ok(event) => handle_event(event).await,
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!("Event listener lagged, skipped {} events", skipped);
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }
}
```

### Work queue processor

```rust
pub struct WorkQueueProcessor {
    queue: Arc<Mutex<VecDeque<WorkItem>>>,
    cancel: CancellationToken,
}

impl WorkQueueProcessor {
    pub fn spawn(
        queue: Arc<Mutex<VecDeque<WorkItem>>>,
        cancel: CancellationToken,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        let work = {
                            let mut q = queue.lock().await;
                            q.pop_front()
                        };

                        if let Some(work) = work {
                            if let Err(e) = process_work(work).await {
                                tracing::error!("Work processing failed: {}", e);
                            }
                        }
                    }
                }
            }
        });
    }
}
```

## Custom lifecycle (advanced)

### Implement RunnableCapability

```rust
use toolkit::lifecycle::{RunnableCapability, RunnableHandle};

pub struct CustomGear {
    // Fields
}

#[async_trait]
impl RunnableCapability for CustomGear {
    async fn run(
        self: Arc<Self>,
        cancel: CancellationToken,
    ) -> Result<RunnableHandle, anyhow::Error> {
        let handle = tokio::spawn(async move {
            // Custom lifecycle logic
            self.run_with_cancel(cancel).await
        });

        Ok(RunnableHandle::new(handle))
    }
}
```

## Graceful shutdown patterns

### Clean shutdown sequence

```rust
impl MyGear {
    async fn shutdown_gracefully(&self) -> Result<(), DomainError> {
        // 1. Stop accepting new work
        self.accepting_work.store(false, Ordering::SeqCst);

        // 2. Wait for in-flight work to complete
        self.wait_for_in_flight().await?;

        // 3. Close connections
        self.close_connections().await?;

        // 4. Flush buffers
        self.flush_buffers().await?;

        Ok(())
    }
}
```

### Shutdown timeout handling

```rust
impl MyGear {
    async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        // Setup
        self.setup().await?;
        ready.notify();

        // Main loop
        tokio::select! {
            result = self.run_main_loop() => result,
            _ = cancel.cancelled() => {
                // Graceful shutdown with timeout
                tokio::select! {
                    result = self.shutdown_gracefully() => result,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {
                        tracing::warn!("Graceful shutdown timeout, forcing exit");
                        Ok(())
                    }
                }
            }
        }
    }
}
```

## Testing lifecycle

### Test with manual cancellation

```rust
#[tokio::test]
async fn test_lifecycle_shutdown() {
    let cancel = CancellationToken::new();
    let gear =  Arc::new(MyGear::new());

    // Start the gear
    let handle = tokio::spawn({
        let gear =  gear.clone();
        let cancel = cancel.clone();
        async move {
            gear.serve(cancel, ReadySignal::new()).await
        }
    });

    // Let it run a bit
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Trigger shutdown
    cancel.cancel();

    // Wait for graceful shutdown
    let result = handle.await.unwrap();
    assert!(result.is_ok());
}
```

### Test background task cancellation

```rust
#[tokio::test]
async fn test_background_task_cancellation() {
    let cancel = CancellationToken::new();
    let counter = Arc::new(AtomicU32::new(0));

    let task_counter = counter.clone();
    let task_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(10));

        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => break,
                _ = interval.tick() => {
                    task_counter.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    });

    // Let it run a bit
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel and verify it stops
    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let count = counter.load(Ordering::SeqCst);
    assert!(count > 0, "Task should have run");
    assert!(count < 20, "Task should have stopped quickly");
}
```

## Quick checklist

- [ ] Add `lifecycle(entry = "...")` to `#[toolkit::gear(...)]` for background tasks.
- [ ] Use `CancellationToken` for shutdown coordination.
- [ ] Pass child tokens to background tasks.
- [ ] Call `ready.notify()` after setup when using `await_ready`.
- [ ] Use `tokio::select!` for cooperative shutdown.
- [ ] Implement graceful shutdown with timeout handling.
- [ ] Test lifecycle with manual cancellation.
- [ ] Publish GTS documents with `gts(...)`; notify `ReadySignal` only when the gear can serve.
