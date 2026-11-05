# Lifecycle and Stateful Tasks

ToolKit manages gear startup, background tasks and cooperative shutdown.

## Core invariants

- Use `CancellationToken` and child tokens for coordinated shutdown.
- Use `WithLifecycle<T>` for background tasks; implement `RunnableCapability` directly only for custom lifecycle needs.
- Never wait for a remote service in `pre_init`, `init`, `post_init` or `start`.
- Declare GTS documents with `gts_publication(...)`; the runtime owns publication retries and admission.

## Runtime startup

The runtime starts two groups in dependency order within each group:

- **System gear group**: gears declaring `system` in `capabilities = [...]`; currently
  Types Registry, Service Discovery, Cluster and gRPC Hub.
- **Application gear group**: all gears without `system`, including the API Gateway,
  resolvers and plugins.

[Core gears](00_gear_overview.md#gear-categories) means all gears under `gears/system/`
(possibly `gears/core/` in the future); repository placement does not select a startup group.

### The system capability

1. **Early startup.** The system gear group completes startup before application preparation.
2. **Availability during admission.** It keeps running while application GTS admission
   is pending. Refusal or expiry stops started gears and returns an error for supervisor restart.
3. **Dependency restrictions.** Its gears cannot declare `deps` or `consumes` on a gear
   in the application gear group in the same process.
4. **No managed publication.** Its gears cannot declare `gts_publication(...)`.
5. **Runtime internals.** `wire_runtime(&SystemContext)` runs before `Gear::pre_init`:
   Service Discovery takes `GearManager`; gRPC Hub takes the installer store and process ID.
   The default implementation is a no-op.
6. **Shutdown ordering.** The application gear group stops first, then the system gear group,
   each in reverse dependency order. Only gears whose `start` was called are stopped.

### Startup sequence

1. **Validate.** Build GTS manifests and publication policies; reject invalid input,
   duplicate GTS owners and invalid startup dependencies before any gear initializes.
   Bind framework probes early in both embedded and OoP hosts.
2. **Start the system gear group.** Run `pre_init`, migrations, `init`, client wiring,
   `post_init`, initial gRPC installation and `start`. Attach its bootstrap REST routes
   and advertise opted-in system providers before application preparation.
3. **Prepare applications.** Run their `pre_init` and migrations, wire registry clients,
   then publish declared GTS Type Schemas and Instances concurrently. All must be admitted
   before any application `init`. Publication cannot depend on a local application provider
   or an unstarted child; an explicit external endpoint remains valid.
4. **Start the application gear group.** Run `init`, remaining client wiring and
   `post_init`; attach the complete REST router and tenant authenticator, install late
   gRPC services and call `start`.
   The in-process host advertises REST providers, clears `application/preparation`, then
   spawns children without waiting for their readiness. Parent publication must use a
   local or already available external registry and cannot depend on its children.

Every gear may implement `Gear::pre_init` and `Gear::post_init`, once per gear in its group.
`pre_init` has no DB handle or admitted manifest; no gear in the group has initialized.
`init` can use its migrated DB and register clients. Ordinary consumed clients are wired
between the group's `init`s and `post_init`s; resolve them in `post_init` or lazily,
or declare `deps` when a local client must be available during `init`.

The temporary `legacy_finalize_after_application_init` hook validates legacy registrations
between application client wiring and `post_init`. It will be removed with the legacy
registry's in-memory ready mode.

Migration-only mode runs runtime wiring, `pre_init` and migrations, system gear group first;
no `init`, `post_init` or publication. See [`HostRuntime`](../../libs/toolkit/src/runtime/host_runtime.rs)
for the implementation.

### gRPC service ownership during preparation

Configure Directory ownership before embedded application or remote OoP providers register:

```yaml
gears:
  service-discovery:
    config:
      grpc_service_owners:
        example.orders.v1.OrdersService: orders
```

Service Discovery loads this map during system `init`; application installers add their names later.
Until then, unconfigured names use first-registration ownership. Remote metadata is not propagated
into Directory. Registration identity authorization is independent.

### Failures, routes and readiness

- Publication retries share a 120-second startup budget. Refusal or expiry stops started gears
  and returns an error for supervisor restart. Shutdown cancels pending publication.
- Both hosts serve probes before preparation, system routes after system startup, and tenant
  routes after application initialization. Early routes need no AuthN resolver and reject tenant
  calls with `503`. OoP serving spawns no children. All providers honor `advertised_in_directory()`.
- `application/preparation` clears after application startup and REST provider registration,
  without waiting for children. Other dependencies and gear healthchecks still gate `/readyz`.
- A gear's `await_ready` / `ready.notify()` changes its lifecycle state to `Running`,
  without blocking `start`, other gears, route publication or the application readiness gate.
  For asynchronous business bootstrap, work in `serve` and provide a REST healthcheck
  to keep `/readyz` at `503` until ready.

Publication failure does not undo migrations or partial admission. Use retryable preparation and
expand/contract migrations compatible with old pods.

The barrier permits synchronous plugin binding in `init`, but one slow publisher delays all
applications. See [ADR-0005](../arch/toolkit-oop/ADR/0005-cpt-cf-adr-eventual-readiness.md).

## Publishing GTS documents

```rust,ignore
#[toolkit::gear(
    name = "my-plugin",
    gts_publication(crates = [crate, my_sdk], entities = Self::configured_instances),
)]
pub struct MyPlugin { /* ... */ }
```

`crates` collects static declarations; optional `entities` builds config-only documents.
`init` reads admitted documents through `ctx.gts_publication_manifest()` and binds plugin
clients. An instance becomes selectable only after its plugin binds its client.

Listed crates call `declare_gts_inventory!()` and use `toolkit_gts::local::*`; remaining root
macros cause a manifest error. Deployment publication shares startup validation and per-publisher
`consumer_wiring` settings; `--registry` overrides only the endpoint.

See the [SDK publication guide](../../gears/system/types-registry/types-registry-sdk/README.md#publishing-from-a-gear)
for collectors, publisher setup, manifest binding, retry/timeout/batch configuration and registry-client wiring.
See the [deployment design](../../gears/system/types-registry/docs/DESIGN.md#deployment-preparation)
for deployment preparation; the current runtime still runs migrations and reconciliation after a deployment job.

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
- [ ] Publish GTS documents with `gts_publication(...)`; notify `ReadySignal` only when the gear can serve.
