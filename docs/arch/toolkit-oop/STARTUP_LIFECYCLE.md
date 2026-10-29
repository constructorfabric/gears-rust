# Gear Startup Lifecycle and the Path Forward

Everything about how gears start up today - every phase in both profiles
(in-process and out-of-process), what each phase receives and is for, how
inter-gear calls work, the constraints that follow, and the concrete options
for evolving the lifecycle - including the known types-registry issues in
out-of-process mode. Traced to `libs/toolkit/src/runtime/host_runtime.rs` and
related sources.

> **TL;DR**
> - `init()` is the only hook that runs for every gear - it is for **wiring**
>   (register your own client, capture handles), not calls, and never blocking.
> - Remote client **handles don't exist during `init()`** today (proxy-wiring
>   runs after it); calls to remote gears can 503 until the provider registers.
> - **Prefer `@consumes` + lazy resolve** - a gear must run in/out of process
>   with no code changes, and `deps` orders `init()` in-process only, so never
>   rely on it across profiles (§3).
> - **Startup calls belong in the gear's lifecycle task** (`start()` → the
>   entry fn, conventionally `serve`) before `ready.notify()` - but note
>   `ready.notify()` only flips the gear's *internal* status, not `/readyz`,
>   which is a separate deps-plus-healthchecks aggregate (§2.3).
> - You can't have all three of: startup-time calls, profile parity, no
>   blocking/deadlock - the constraint in §4.
> - The evolution path (§5): wire handles before `init()`, split writes from
>   reads for foundational gears (runtime-owned publication), add startup
>   deadlines, keep readiness best-effort by default, and wire `ready.notify()`
>   into the probe (§5.6).

## 1. The phases

Both profiles - **in-process** (Profile 1) and **out-of-process** (Profiles 2/3) -
run the same relative order. The `init → proxy-wiring → post_init` segment is a
single shared method (`run_init_wiring_post_init`) so the paths cannot drift.

| # | Phase | Whose code runs | Context given | What it's for / limits |
|---|-------|-----------------|---------------|------------------------|
| 1 | `pre_init` | `SystemCapability::pre_init` on each `system` gear | `SystemContext` | Wire runtime internals (gear manager, gRPC installer store) into system gears |
| 2 | `db` | No gear code - runtime runs each DB-capable gear's declared migrations | - | Runtime resolves `Db` itself; the gear never sees a connection |
| 3 | `init` | `Gear::init` on **every gear**, one at a time (topo order, system first) | `GearCtx` | DI/wiring: register your own client, capture handles. **No required calls to other gears; never block** - sequential & awaited, so a hang freezes the process |
| 4 | `proxy-wiring` | No gear code - runtime replays link-time `@consumes` registrations | `ClientHub` only | Register a resolving proxy per remote dep; co-located impl wins; remote deps become `/readyz` gates |
| 5 | `post_init` | `SystemCapability::post_init` on each `system` gear | `SystemContext` - **no `ClientHub`** | "All gears finished init" barrier - e.g. types-registry flips to ready mode. Structurally cannot do client calls |
| 6 | `rest` | `RestApiCapability::register_rest` on each REST gear, against the one host | `GearCtx` | Compose the Axum router (exactly one host required) |
| 7 | `grpc` | `GrpcServiceCapability` on each gRPC-service gear, against the one hub | `GearCtx` | Collect services and register them on the single hub |
| 8 | `start` | `RunnableCapability::start` on each `stateful` gear | `CancellationToken` (+`ReadySignal` inside the entry fn) | Spawn the long-running task (`serve`); **returns immediately even with `await_ready`** - heavy/cross-gear work belongs inside the spawned task |
| 9 | `directory-register` | No gear code - runtime advertises each in-process REST provider | `ClientHub` | Advertise providers in the directory once the gateway binds (in-process only) |
| 10 | `oop_spawn` | No gear code - runtime spawns configured OoP binaries | spawn config | Spawn configured OoP child gears (in-process only) |
| 11 | wait | No gear code - the runtime blocks until shutdown | root token | Await the root cancellation token; the boundary between startup and shutdown |
| 12 | `stop` | `RunnableCapability::stop` on each `stateful` gear, reverse order | fresh deadline token per gear | Two-phase shutdown: graceful request, then hard-stop after the deadline; watchdog force-exits on a hang |

**`MigrateOnly` mode.** The same runtime can be driven in a reduced mode
(`RunMode::MigrateOnly`, via `run_migration_phases`) that runs only phases 1-2
(`pre_init` + DB migrations) and then exits before `init`. This is for cloud
deploy workflows that run migrations as a separate step. Everything below
describes the default `Full` mode.

OoP differences: the probe server (`/healthz`, `/readyz`) binds **before** the
phases so liveness passes immediately (gear routes return 503 until `start`);
no REST-host/directory-register/oop_spawn phases - `oop_serve` owns router,
presence, and drain.

## 2. The mechanics behind the table

### 2.1 The three contexts

- `SystemContext` - instance id + gear manager + gRPC installer store.
  **Deliberately has no `ClientHub`**: the system hooks wire runtime internals,
  so they *structurally* cannot make inter-gear calls - not a convention, the
  context doesn't expose the mechanism.
- `GearCtx` - gear config (`ctx.config()`), the shared `ClientHub`, a per-gear
  DB provider, and a child `CancellationToken`. What `init()`/`rest`/`grpc` get;
  the only context from which a client *could* be fetched.
- `CancellationToken` - all `start()` receives (the entry fn also gets
  `ReadySignal` when `await_ready` is set). Deliberately thin: `start()` is for
  running, not wiring - anything it needs it captured during `init()`.

### 2.2 `start` means "run a task", not "begin startup"

`start` launches the gear's **long-running task** (a server loop, a consumer, a
background worker) in a spawned `tokio` task that lives until shutdown. Gears
that only handle requests when invoked have nothing to spawn - the HTTP serving
lives inside the gateway's task, not theirs - which is why the phase is gated
on the `stateful`/`RunnableCap` capability rather than running for everyone.
`serve` is just the conventional name for the entry fn declared via
`lifecycle(entry = "...")`.

### 2.3 Readiness: lifecycle status vs the `/readyz` probe

Two separate mechanisms carry the word "ready", and they are **not wired
together today** - a distinction the rest of this document depends on.

**(a) Lifecycle status (`ReadySignal` / `await_ready`).** A `stateful` gear's
`WithLifecycle` wrapper has an internal state machine
(`Stopped → Starting → Running → Stopped`). With `await_ready`, the entry fn
receives a `ReadySignal` and `ready.notify()` flips `Starting → Running`;
without it the gear is marked `Running` the instant the task is spawned.
**`start()` returns immediately in every case** - the wait for `notify()`
happens in a spawned task, so a slow warm-up never blocks the start phase (or
any later gear). This status is used for stop coordination and is observable by
the gear's own code; **the runtime does not bridge it to `/readyz`.**

**(b) The `/readyz` aggregate.** Computed independently of (a), in both
profiles, from:

- **Dependency gates** - every `@consumes` dep, green once its endpoint
  resolves (sticky). In-process, a `ReadinessHealthcheck` leaf bridges these
  into the gateway's healthcheck registry; OoP reads them directly.
- **Per-gear healthchecks** - each gear's `RestApiCapability::healthcheck()`.
- **(OoP only) `startup_complete`** - set by the framework when the composed
  routes are published (right after the `start` phase returns), *not* when any
  gear calls `ready.notify()`.
- **draining flag** - set on shutdown (§2.5), forcing 503.

The aggregate renders as a four-state lifecycle on `/readyz`:

| State | HTTP | When |
|-------|------|------|
| `starting` | 503 | startup not complete, a dep unresolved, or a healthcheck `Unhealthy` |
| `ready` | 200 | all gates green and all healthchecks healthy |
| `degraded` | 200 | a healthcheck reports `Degraded` - kept in rotation (e.g. optional backend down but a fallback is fine) |
| `draining` | 503 | shutdown began - upstreams should stop routing |

**The consequence (a real gap, not just a detail).** Because (a) and (b) are
decoupled, a gear cannot express "up but not functionally ready" for free: a
`stateful` gear warming up in `serve()` before `ready.notify()`, or a
request-only gear with a still-unresolved lazy client, reports `/readyz` 200
(and OoP `startup_complete` true) regardless. To make `/readyz` reflect warm-up
the gear must express it as a **healthcheck** that reports `Unhealthy` until
warm. That asymmetry is what the §5.5/§5.6 readiness options are about.

### 2.4 Two ordering asymmetries that matter

- **Migrations run *before* `init()`** - `init()` may assume its own schema.
- **Proxy-wiring runs *after* `init()`** - a remote client *handle* does not
  exist yet during `init()`.

### 2.5 Shutdown flips readiness before teardown

Right after the `start` phase, the runtime spawns a background watcher on the
root cancellation token. The moment shutdown begins, it sets the draining
flag so `/readyz` reports 503 *before* any gear's `stop()` runs. This lets the
orchestrator pull the instance out of the load balancer and drain in-flight
requests while gears are still fully up, rather than tearing down under live
traffic. `/healthz` (liveness) is unaffected - a draining pod is still alive.

## 3. How inter-gear calls work today

Three mechanisms:

| Mechanism | What it does | Ordering guarantee |
|-----------|--------------|--------------------|
| `deps = [...]` | Topo-sort (Kahn's) of `init()` order, system gears first; cycles are a build-time error | In-process only - in OoP it is just a spawn/readiness hint |
| Manual registration | Provider calls `ctx.client_hub().register::<dyn Api>(impl)` in its own `init()` | Only ordered if a `deps` edge or init order happens to place it first |
| `@consumes(contract, from)` | Inventory registration replayed in proxy-wiring: local impl wins, else a directory-resolving proxy is registered that resolves lazily per call and gates `/readyz` | **None** - deliberately adds no topo edge; wired after `init()` |

Because a gear must run in- or out-of-process with no code changes, **prefer
`@consumes`**: it resolves lazily per call and behaves identically in both
profiles. `deps` orders `init()` **in-process only** - rely on it for anything
observable and the gear breaks the moment it is split out. Use `deps` solely for
in-process `init()` ordering (recipe 6.2), never as an inter-gear-call enabler
or cross-profile guarantee; §5.1 removes even that need.

So, can you call another gear in `init()`?

| Case | Handle in `init()`? | Safe to call? |
|------|---------------------|---------------|
| Co-located provider, `deps` declared | Yes - provider's `init()` ran first | Fragile - provider's `start()` has not run; "registered ≠ ready" |
| Co-located, no `deps` | Racy - discovery order | No |
| Remote (OoP) | No - proxy wired after `init()` | No - handle absent; later calls can 503 until the provider registers |

And regardless of case: **never block `init()`** - the loop is sequential, so
one blocked gear stalls every later gear (process alive at `/healthz` 200,
`/readyz` 503 forever).

## 4. The constraint that shapes everything

Pick at most two of: **startup-time calls**, **identical behavior in both
profiles**, **no blocking startup / no undetectable deadlock**. Full parity
forces the weakest guarantee to win: OoP can never promise "B exists when A
starts", so a uniform model can't promise it in-process either.

**This project chooses the latter two** - in/out-of-process parity with no code
changes, and no blocking/deadlock - and therefore gives up *guaranteed*
startup-time calls. That is the root reason the patterns above lean on lazy
resolution and `start()`-time work rather than `init()`-time calls or `deps`
ordering.

And a required startup call is a **readiness decision, not just a function
call**: `/readyz` currently means "deps *resolvable*" (endpoint exists), not
"the dep is Ready". A gear that must call B to function isn't truly ready until
B is Ready - transitive readiness - which the current gate doesn't compute, and
which deadlocks on `@consumes` cycles that can't be detected at build time.

## 5. The path forward: how the phases could change

### 5.1 Handle parity - wire before `init()`

`@consumes` registrations come from `inventory` (link-time data), not gear
code, so proxy-wiring does not depend on `init()` having run. Moving it ahead
of `init()` makes `client_hub().get::<dyn Api>()` return a handle in `init()`
**identically** in both profiles. One fix required: the local-impl-wins check
is evaluated once at wire time today; it must become lazy/per-call so a
co-located provider registering during its own `init()` is still found.
Semantic-neutral: a handle exists, but calling it early is still governed by
readiness.

### 5.2 Where calls belong - `start()`, not `init()`

`start()` spawns the gear's entry task and returns immediately; the task runs
concurrently - the correct home for anything heavy or dependency-dependent
(recipe 6.3). With `await_ready`, `ready.notify()` flips the gear's internal
status but not `/readyz` today (§2.3); to gate the probe on warm-up, pair it
with a healthcheck.

A universal ready-gated hook ("all gears in this process wired") is possible
but is a **per-process barrier only** - it says nothing about other processes,
so it can't deliver cross-process readiness. Optional Profile-1 sugar, not a
coordination mechanism.

### 5.3 Foundational gears - split registration (write) from lookup (read)

types-registry already embodies this: a **config phase** where
`register(validate=false)` accepts types into a staging buffer while reads fail,
then a **flip** (`post_init → switch_to_ready()`) that validates the whole
closure. Consumers register in `init()` (write) and resolve lazily on first use
(read).

This is **reported to break OoP today, and the mechanism shows why it would**:
the flip fires on the registry's *own* `post_init` ("all gears in *this*
process finished init"), which in OoP happens almost immediately, before
consumers in other pods have registered; their pushes then hit strict
validation or a client that isn't wired yet. (Symptoms inferred from the
mechanism, not traced end-to-end.) The parity-correct shape:

- Registration becomes an **eventual, retried push** (like directory
  self-registration) **owned by the runtime** - the gear declares the
  publication, the runtime retries it; no `stateful` task required (§6.4).
- The "validate all at once" flip becomes **process-independent**: either
  incremental/eventual validation (push rejects until the referenced type
  arrives; caller retries), or an explicit bootstrap window/publish signal.
- Lookups stay lazy/first-use - already parity-correct.

Note: types-registry's own `p0` plan independently arrives at the same shape -
deleting `switch_to_ready`/`post_init` for DB-backed immediate admission and
moving consumers to `#[consumes]`, dropping `deps` with each gear's last
startup call.

### 5.4 Startup time bounds

Shutdown is rigorously bounded (per-gear deadline + watchdog thread that
force-exits); startup is not. Two backstops would close the gap:

- **Per-gear `init()` deadline** - `init()` is awaited with no timeout; a hang
  freezes everything. Mirror the stop watchdog: cancel, then force-exit, naming
  the gear. Most valuable in-process, where no kubelet exists.
- **Readiness deadline for `await_ready`** - a gear that never calls
  `ready.notify()` sits in `Starting` forever with only a debug log; bound it
  so it fails loudly.

These are backstops for bugs, never a license to block.

### 5.5 Readiness model options

- **Best-effort**: `/readyz` gates on deps resolvable; calls tolerate
  503/retry. Cannot deadlock; industry standard (K8s + gRPC `wait_for_ready`).
- **Transitive**: a gear that *must* call B before serving declares
  `require_ready`; readiness propagates along the `@consumes` graph, with a
  mandatory timeout - accepting cycle-deadlock risk.

The transitive model only needs to exist if a real gear proves to need a hard
startup call. (types-registry is a write case - it does **not** need it.)

### 5.6 Wire lifecycle readiness into the probe

The §2.3 gap - `ready.notify()` doesn't gate `/readyz` - is a silent trap: a
gear warming up before it signals is already reporting 200. The fix is cheap
and parity-neutral: when `await_ready` is set, hold the `/readyz` aggregate
not-ready until `ready.notify()` fires (OoP: `mark_startup_complete` on notify
instead of on route-attach; in-process: a runtime-registered leaf healthcheck
`Unhealthy` until the signal). This makes recipe 6.3 behave as authors expect,
with the §5.4 readiness deadline as the only new backstop. Orthogonal to §5.5:
it is about a gear signaling its *own* readiness, not waiting on other gears.

### 5.7 Prevent calls in `init()`, don't just document them

Recipe 6.6 shows the anti-pattern (a synchronous cross-gear call in `init()`)
and relies on authors knowing not to write it. It could instead be made hard or
impossible, three tiers strongest first:

- **Compile-time:** `init()` gets a phase-scoped context whose client accessor
  returns a non-callable `ClientHandle<dyn Api>` - a storable token, not an
  `Arc<dyn Api>`. Recipe 6.6's BAD call then does not compile; a later hook
  receives the context that *resolves* a handle into an `Arc`.
- **Runtime:** generated `@consumes`/SDK clients carry the current phase and
  fail loudly on any call while `phase == init` - catches raw `get()` escape
  hatches.
- **Lint:** a `cargo gears lint` advisory rule flagging awaited calls on
  hub-derived values inside `init()`.

## 6. Code recipes

Each recipe is tagged **Today** (works against the current runtime) or
**Target** (depends on a §5 change not yet built).

### 6.1 Provider: register your own client in `init()` · *Today*

```rust
impl Gear for MyGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let api: Arc<dyn MyGearApi> = Arc::new(MyService::new(ctx.config()?));
        ctx.client_hub().register::<dyn MyGearApi>(api);   // publish, don't call
        Ok(())
    }
}
```

### 6.2 Consumer: declare `@consumes`, capture the handle, call later · *Today*

Capturing in `init()` is reliable in-process today but only parity-safe in both
profiles once §5.1 lands; for parity *now*, prefer lazy resolution (recipe 6.5).

```rust
#[toolkit::consumes(contract = billing_sdk::BillingApi, from = "billing")]
#[toolkit::gear(name = "orders", capabilities = [db, rest, stateful],
               lifecycle(entry = "serve", await_ready))]
pub struct OrdersGear {
    billing: OnceLock<Arc<dyn BillingApi>>,   // captured, not called
}

impl Gear for OrdersGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Reliable for a co-located provider today; present in BOTH profiles
        // only once §5.1 lands (a remote handle is wired after init() today).
        self.billing.set(ctx.client_hub().get::<dyn BillingApi>()?)?;
        Ok(())
    }
}
```

### 6.3 Startup-dependent work: `serve()` before `ready.notify()` · *Today*

```rust
async fn serve(self: Arc<Self>, cancel: CancellationToken, ready: ReadySignal) -> Result<()> {
    // Cross-gear calls belong here, not in init().
    self.warm_cache().await?;
    ready.notify();                // flips the internal lifecycle status Starting -> Running
    self.run_until(cancel).await
}
```

Caveat (§2.3): `ready.notify()` does **not** by itself keep `/readyz` at 503
while `warm_cache()` runs. If you need the probe to reflect warm-up, back it
with a healthcheck that reports `Unhealthy` until the cache is warm:

```rust
fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
    Some(Arc::new(CacheWarmCheck::new(self.state.clone())))  // Unhealthy until warm
}
```

### 6.4 Foundational write (types-registry pattern) · *Today + Target*

```rust
// Today (Profile 1 only): a synchronous cross-gear call inside init().
let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
registry.register(vec![schema_json]).await?;
```

"Eventual push" = directory self-registration's model (ADR-0005): the gear
declares the publication; the runtime retries it in the background until
accepted, tracked as a readiness component - not a synchronous `init()` call.

```rust
ctx.publish_gts(schema_json)?;   // declare; runtime owns the retry task
// a stateful gear may additionally await it before ready.notify()
```

Publication being runtime-owned means a request-only gear publishes without a
`stateful` capability. (types-registry's `p0` plan names this `publish_gts` +
"status joins Required readiness".)

### 6.5 Lazy read (the parity-safe default): resolve on first use · *Today*

Works identically in both profiles with no reliance on `init()` ordering - the
recommended way to access another gear today.

```rust
// AM/authn-resolver pattern: a wrapper that resolves the real client
// on first call instead of assuming it exists at init time.
let catalog = self.types_registry.get_or_init(|| {
    ctx.client_hub().get::<dyn TypesRegistryClient>()
}).await?;   // resolved+cached on first need, not during init()
```

### 6.6 What NOT to do · *Today*

```rust
// BAD: a synchronous call to another gear inside init(). The provider's
// start() has not run (Profile 1) and the handle may not even exist (OoP);
// worse, a call that blocks freezes every later gear's init().
async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
    let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
    let entity = registry.get("gts://cf.core.types.v1~...").await?;  // BAD
    Ok(())
}
```

**What to do instead** - resolve lazily on first use (recipe 6.5, parity-safe
today), or capture the handle in `init()` and call it in `serve()` (recipes
6.2/6.3; fully parity-safe once §5.1 lands). How this anti-pattern could be
*prevented* rather than just documented is §5.7.

## 7. Summary

Today's phases are: a single wiring hook (`init()`, sequential, system-first,
migration-safe but call-unsafe), a post-init proxy-wiring that only exists in
time for `post_init`/`start`, and a spawned `start()` that is the real home of
heavy work and cross-gear calls. To support "call gears at startup" uniformly
in both profiles: wire `@consumes` handles before `init()`, keep calls out of
`init()` (lazy-resolve on first use today, capture the handle once §5.1 makes
that parity-safe), do startup-dependent work in the lifecycle
entry task before `ready.notify()` (and wire that signal into `/readyz`, §5.6,
since it does not gate the probe today), convert foundational-gear registration
into eventual retried pushes with a process-independent flip, and bound
`init()`/`await_ready` with deadlines that mirror the existing stop watchdog.
The thing no reorder fixes: a remote provider is a separate process that may not
exist yet - calls must tolerate that, not assume it.
