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

[Core gears](00_gear_overview.md#gear-categories) means all gears under `gears/system/`;
repository placement does not select a startup group.

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
`clients.consumes` settings; `--registry` overrides only the endpoint.

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
- `ready_warn_after`: Non-fatal readiness **warning** period (e.g., "90s"); requires `await_ready`. See the deadline table below.

## Runtime deadlines

Three bounds protect startup and shutdown — two fatal, one a warning:

| Deadline | Default | Scope | On expiry |
| --- | --- | --- | --- |
| `init_deadline` | 10m (`DEFAULT_INIT_DEADLINE`) | per-gear context build + `init()` | A stuck `.await` returns `RegistryError::InitTimeout` (recoverable); a *synchronous* worker block is caught by an OS-thread watchdog (longer backstop) that force-exits with code 1. |
| `shutdown_deadline` | 35s (`DEFAULT_SHUTDOWN_DEADLINE`) | per-gear graceful stop | Set each gear's `stop_timeout` *below* this; if the stop phase wedges, the watchdog force-exits with code 1. |
| `ready_warn_after` | 60s (`DEFAULT_READY_WARN_AFTER`) | readiness signal (ready-mode only) | **Warning only:** logs an error; the gear stays `Starting` and a late `notify()` still promotes it. |

Force-exit diagnostics are logged at `error` and mirrored to `stderr`, with a grace timer guaranteeing the exit even if that write blocks.

### Configuring the deadlines

`init_deadline` and `shutdown_deadline` are set under the `server` section of the app config (both optional; absent fields keep the defaults) and threaded into `RunOptions` by both bootstrap entry points:

```yaml
server:
  init_deadline_secs: 600
  shutdown_deadline_secs: 35
```

`ready_warn_after` is configured per-gear via the `lifecycle(..., ready_warn_after = "...")` macro attribute, not through config.

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
impl MyGear {
    async fn serve(
        self: std::sync::Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
        ready: toolkit::lifecycle::ReadySignal,
    ) -> anyhow::Result<()> {
        // Bind sockets/resources before flipping to Running
        let listener = tokio::net::TcpListener::bind(&self.bind_addr).await?;
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

- The process has one root token. `toolkit::run(RunOptions)` takes the shutdown strategy in
  `RunOptions::shutdown`: `ShutdownOptions::Signals` (Ctrl+C / SIGTERM), `ShutdownOptions::Token(token)`
  or `ShutdownOptions::Future(..)` (`libs/toolkit/src/runtime/runner.rs`). The bootstrap entry points
  (`libs/toolkit/src/bootstrap/run.rs`, `bootstrap/oop.rs`) hook OS signals themselves and pass
  `ShutdownOptions::Token`.
- The `lifecycle(entry = ...)` method receives a child of the root token as `cancel`.
- `GearCtx::cancellation_token()` returns a child of the root token; derive further child tokens from
  it (`child_token()`) for work spawned in `init` or `post_init`.
- `stop_timeout` (default 30s) bounds how long `WithLifecycle` waits for the entry method to return
  after cancellation; keep it below `shutdown_deadline` (35s), which is the hard backstop.

Prefer running background work inside the lifecycle entry method (or a `WithLifecycle<T>`) so the
runtime tracks its state and stop timeout. A bare `tokio::spawn` is invisible to the runtime; if you
need one, select on a child token and keep the `JoinHandle`. With `lifecycle(entry = ...)` /
`WithLifecycle` there is no user stop hook (`WithLifecycle` implements `RunnableCapability::stop`
itself, `libs/toolkit/src/lifecycle.rs`), so await the handle before the entry method returns. With a
[custom lifecycle](#custom-lifecycle), await it in your `RunnableCapability::stop`.

## Custom lifecycle

A gear that declares `stateful` without `lifecycle(...)` must implement
`toolkit::contracts::RunnableCapability` itself; the macro registers the gear as that capability.

```rust
#[async_trait]
pub trait RunnableCapability: Send + Sync {
    /// Start the gear's background task. `cancel` is a child of the runtime's root token.
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()>;

    /// Stop the gear's background task.
    async fn stop(&self, deadline_token: CancellationToken) -> anyhow::Result<()>;
}
```

`stop` receives a fresh **deadline token**, not the root token: it is not cancelled when `stop` is
called (begin graceful shutdown) and is cancelled when the runtime's `shutdown_deadline` expires
(abort now). The trait documentation in `libs/toolkit/src/contracts.rs` shows the recommended
`tokio::select!` between graceful completion and `deadline_token.cancelled()`.

For a reusable task, implement `toolkit::lifecycle::Runnable`
(`async fn run(self: Arc<Self>, cancel: CancellationToken) -> anyhow::Result<()>`) and wrap it in
`WithLifecycle<T>`, which implements `RunnableCapability` and handles the stop timeout
(`with_stop_timeout`, `with_stop_timeout_fn`) and ready mode.

## Testing lifecycle

Call the entry method directly with a token you control. A `ReadySignal` is built from a oneshot
sender with `ReadySignal::from_sender`:

```rust
#[tokio::test]
async fn serve_stops_on_cancel() {
    let cancel = CancellationToken::new();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let gear = Arc::new(MyGear::default());

    let task = tokio::spawn(gear.serve(cancel.clone(), ReadySignal::from_sender(ready_tx)));
    ready_rx.await.expect("gear became ready");

    cancel.cancel();
    task.await.expect("task joined").expect("serve returned Ok");
}
```

## Quick checklist

- [ ] Add `lifecycle(entry = "...")` and the `stateful` capability to `#[toolkit::gear(...)]` for background tasks.
- [ ] Return from the entry method when `cancel` is cancelled; set `stop_timeout` below `shutdown_deadline`.
- [ ] Derive child tokens from `cancel` or `GearCtx::cancellation_token()` for spawned work.
- [ ] Call `ready.notify()` after setup when using `await_ready`; notify only when the gear can serve.
- [ ] Do not wait for remote services in `pre_init`, `init`, `post_init` or `start`.
- [ ] Publish GTS documents with `gts_publication(...)`.
- [ ] Test the entry method with a manual `CancellationToken`.
