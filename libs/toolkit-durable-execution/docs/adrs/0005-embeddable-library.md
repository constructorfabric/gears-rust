---
status: accepted
date: 2026-10-25
---

# Embeddable durable-execution library

**ID**: `cpt-cf-durable-execution-adr-embeddable-library`


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Public API](#public-api)
  - [Authorization Port](#authorization-port)
  - [Errors](#errors)
  - [Storage](#storage)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep the system gear](#keep-the-system-gear)
  - [Module in toolkit-db behind a feature](#module-in-toolkit-db-behind-a-feature)
  - [Separate libs crate](#separate-libs-crate)
- [More Information](#more-information)
  - [Superseded Text](#superseded-text)
  - [Follow-ups](#follow-ups)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Durable execution was a system gear with a separate SDK crate.
The gear forced one shared host for the journal of all consumer gears.
Consumers resolved its traits through ClientHub.
The gear owned service credentials.
It exchanged client credentials through the AuthN resolver for each service operation.
It read the client secret and the queue database URL from environment variables.
Its YAML configuration used boolean flags such as `execute_activities` and `delivery_enabled`.

The transactional Outbox is already a library in [toolkit-db](../../../toolkit-db/src/outbox/README.md).
Each gear embeds it in its own database and lifecycle.
Durable execution builds on that Outbox and has the same embedding needs.
A consumer gear must run durable workflows in its own database, under its own identity and lifecycle.

## Decision Drivers

The [PLID guidelines](../../../../guidelines/DNA/public-interface/PLID.md) supply these drivers:

- Minimal public surface (PLID-10.06).
  Expose concrete handles, not object-safe traits for a service registry.
- Typed configuration and safe defaults (PLID-41.01, PLID-41.03).
  Settings are typed values with validation on entry.
- Modes instead of boolean flags (PLID-11.05).
  One process mode replaces combinations of flags.
- Lifecycle in the types (PLID-11.06).
  Build, prepare, and run are separate phases.
- An own error taxonomy (PLID-50.01, PLID-50.06).
  The library defines its error kinds and stable codes.
- No environment variable reads in a library (PLID-10.01).
  The application layer reads configuration and secrets.
- No gear-SDK dependencies in a library (PLID-30.01).
  The library does not depend on AuthN, AuthZ, ClientHub, or gear lifecycle crates.

## Considered Options

1. Keep the system gear and its SDK.
2. A module in `toolkit-db` behind a feature.
3. A separate crate in `libs/`.

## Decision Outcome

Use a separate crate in `libs/`.
The package is `cf-gears-toolkit-durable-execution`.
The library name is `toolkit_durable_execution`.
The crate replaces the gear and the SDK together.
The gear, its ClientHub registration, `gear.gdl`, its YAML `Config`, and the example-server feature are deleted.
There is no backward compatibility.
The stability tier is experimental.

### Public API

The engine lifecycle has three phases:

1. `Engine::builder(db, authorizer)` returns `EngineBuilder`.
   Its setters are `mode`, `lease`, `heartbeat`, `dispatch_interval`, `shutdown_timeout`, `control_plane_failure_limit`, and `authorization_timeout`.
   `build` returns `Result<Engine, ConfigError>` and does no I/O.
2. `Engine` supplies `client`, `registry`, `inspector`, `readiness`, and `shutdown_timeout`.
   `Engine::prepare` checks bindings.
   With a queue, it connects Apalis to PostgreSQL and starts the delivery Outbox.
   It returns `Result<Runner, LifecycleError>`.
3. `Runner::run(cancel)` serves until cancellation and drains within the shutdown timeout.
   Readiness is true while it serves.

`ExecutionMode` replaces the `execute_activities` and `delivery_enabled` flags:

| Mode | Behavior |
|---|---|
| `ControlPlane` (default) | Observation, cancellation, and catalog reconciliation. Start, Retry, and Resume return `FailedPrecondition` with code `DELIVERY_DISABLED`. |
| `Dispatcher(QueueConfig)` | Also admits and schedules runs and publishes due work. |
| `Worker(QueueConfig)` | Also executes activities with the registered handlers. |

`QueueConfig::new(dsn, queue)` takes the DSN as a `SecretString` and a required default queue name, unique among the engines on that database.
`concurrency`, `queue`, and `route` replace `queue_database_url_env`, `default_queue`, `queues`, and `definition_queues`.
`Engine::prepare` returns a `PrepareError` that hands the engine back, so a transient queue failure can be retried.
The library never reads environment variables.

The handles are concrete types:

- `ExecutionClient` replaces `DurableExecutionClient`, without `resolve(&ClientHub)`.
- `WorkflowRegistry` is a struct with `register_workflow`, `register_contract`, `registration`, `unregister`, and `activate`.
  `WorkflowRegistryExt` is removed.
- `ExecutionInspector` is a struct with `inspect` and `list`.

The object-safe traits `DurableExecution`, `WorkflowRegistry`, and `ExecutionInspector` are removed from the public API.
`migrations()` returns the schema migrations.
The consumer gear appends them to its own `DatabaseCapability::migrations`.

### Authorization Port

The library defines the `Authorizer` trait:

- `caller_scope(&SecurityContext, CallerAction)` returns an `AccessScope` or an `AuthorizationError`.
- `service_scope(ServiceAction)` returns an `AccessScope` or an `AuthorizationError`.

`CallerAction` contains create, get, update, manage, inspect, and list.
`ServiceAction` contains execute, heartbeat, recover, dispatch, and cancel_definition.
It also contains the definition actions register, get, unregister, activate, list, and release.
Each action has a stable PDP name from `as_str`.
`resource` returns `durable_execution.run` or `durable_execution.definition`.
`constraints` returns `Required` or `Optional`.

The embedding gear implements the port with its `PolicyEnforcer` and its own cached service identity.
The `service_client_id`, `service_client_secret_env`, and `service_scopes` settings are removed.

The engine keeps the checks that do not depend on policy:

- It rejects a nil subject or tenant.
- It applies `ensure_owner` to owner operations and hides runs of other owners.
- It falls back from `update` to `manage` for control of another owner's run.
- It rejects an unconstrained scope for actions whose constraints are `Required`, as `PolicyEnforcer` did, and accepts one for catalog actions and `cancel_definition`.
- It limits each call with `authorization_timeout`; a timeout returns `Unavailable`.
- It logs the action, resource, rejection reason and, for callers, subject and tenant ids, never the authorizer's error text.

### Errors

Public operations return the library `Error`.
`Error::kind` returns `ErrorKind`.
The kinds are `InvalidArgument`, `NotFound`, `AlreadyExists`, `PermissionDenied`, `FailedPrecondition`, `Aborted`, `Unavailable`, and `Internal`.
`ErrorKind` is non-exhaustive.
`Error::code` returns a stable string:
`EXECUTION_ACCESS_DENIED`, `DELIVERY_DISABLED`, `RUN_NOT_FOUND`, `DEFINITION_NOT_FOUND`, `LEASE_LOST`, `CONCURRENT_UPDATE`, `INVALID_STATE`, `DEFINITION_INACTIVE`, `DEFINITION_MISMATCH`, `DEFINITION_CONFLICT`, `REQUEST_KEY_CONFLICT`, `INVALID_DEFINITION`, `INVALID_FORMAT`, `PAYLOAD_TOO_LARGE`, `OUT_OF_RANGE`, `UNAVAILABLE`, or `INTERNAL`.
`is_retryable`, `field`, `run_id`, `definition`, and `request_key` supply structured data.

`Error::resource` and `Error::source` complete the structured data; the constants are public in `codes`.
The library has no conversion into `CanonicalError`, like other runtime libraries such as `toolkit-db`.
The embedding gear maps errors to its own canonical errors and resource types.
The SDK `reason` and `gts` modules and the GTS resource types of the gear are removed.
`ConfigError` supplies `setting` and `reason`.
`LifecycleError::kind` returns `IncompleteBindings`, `QueueUnavailable`, `Storage`, `ShutdownTimedOut`, or `Worker`.

### Storage

One squashed, forward-only migration `m0001_durable_execution__init` creates all journal tables.
`migrations()` returns it with the ToolKit outbox migrations as separate entries, so each is recorded in the gear's history.
Table names follow the [object namespacing ADR](../../../../docs/arch/database/ADR/0001-cpt-cf-database-adr-object-namespacing.md):

| Former name | Current name |
|---|---|
| `durable_runs` | `durable_execution__runs` |
| `durable_outbox` | `durable_execution__delivery_intents` |
| `durable_start_keys` | `durable_execution__start_keys` |
| `durable_coalescing` | `durable_execution__coalescing` |
| `durable_activities` | `durable_execution__activities` |
| `durable_attempts` | `durable_execution__attempts` |
| `durable_epochs` | `durable_execution__epochs` |
| `durable_events` | `durable_execution__events` |
| `durable_definitions` | `durable_execution__definitions` |
| `durable_delivery_outbox` prefix | `durable_execution__delivery_outbox` prefix |

The prefix is fixed.

### Consequences

Each consumer gear writes an `Authorizer` adapter.
The adapter maps actions to its policy enforcer and caches its service identity.

One database or schema hosts one engine until a caller-supplied prefix exists.
The Apalis tables live in a shared `apalis` schema.
Queue names must be unique across all engines that use one database.

The schema is squashed.
There is no migration from the tables of the former gear.
Use a clean development or test database.

Consumers keep the handles in their own gear and expose their own API.
The library does not register anything in ClientHub.
The embedding gear owns configuration parsing, secrets, and lifecycle.
Journal, fencing, Outbox, and saga decisions in ADR 0001 through ADR 0004 stay accepted.

### Confirmation

- [Engine tests](../../tests/unit/engine_tests.rs) check settings validation, queue routes, readiness, and the control-plane lifecycle.
- [Authorization tests](../../tests/unit/authorization_tests.rs) check stable action names, identity guards, timeouts, and error kinds.
- [Client tests](../../tests/unit/client_tests.rs) check progress streams of `ExecutionClient`.
- [Definition error tests](../../tests/unit/definition_error_tests.rs) check error kinds, codes, resources, and causes.
- [Worker authorization tests](../../tests/unit/worker_authorization_tests.rs) check tenant isolation, outages, and revocation through the real `Authority`.
- [Migration tests](../../tests/unit/mod_tests.rs) check separate recording, catch-up on existing databases, and object names.
- [Lifecycle tests](../../tests/integration/lifecycle_tests.rs) check runtime failure, readiness, and bounded shutdown against PostgreSQL.
- The [consumer gear example](../../examples/consumer_gear.rs) shows the complete embedding.

Run `make test-durable-execution-pg` against a disposable PostgreSQL database.
See the [Makefile](../../../../Makefile) and [CI workflow](../../../../.github/workflows/ci.yml).

## Pros and Cons of the Options

### Keep the system gear

Consumers keep the current integration.
One shared host serves the journal of all consumer gears.
ClientHub couples consumers to the gear.
The gear keeps service credentials and environment variable reads.
Boolean flags and gear configuration keys stay in the contract.

### Module in toolkit-db behind a feature

This option reuses the `toolkit-db` crate and its Outbox directly.
It adds Apalis, SQLx adapter, and workflow dependencies to a basic database crate.
The feature couples the release cycle and stability tier of both components.
The experimental workflow API would affect the stable database API.

### Separate libs crate

This option follows the Outbox embedding model.
It has its own stability tier, features, and dependencies.
It keeps gear-SDK dependencies out of the library.
Each consumer gear writes an `Authorizer` adapter.
This is the selected option.

## More Information

### Superseded Text

[ADR 0001](0001-apalis-with-transactional-outbox.md), [ADR 0002](0002-task-execution-framework-selection.md), and [ADR 0003](0003-typed-sdk-and-journal-boundary.md) refer to the gear hosting model.
Their notes identify the superseded parts.
Their decisions stay accepted.

### Follow-ups

- A caller-supplied table prefix.
  It needs secure-ORM support in `toolkit-db`, because SeaORM table names are `&'static str`.
- A shared `Authorizer` adapter based on `PolicyEnforcer` for consumer gears.

## Traceability

- [PRD](../PRD.md): operational concept, interfaces, authorization, and lifecycle requirements.
- [DESIGN](../DESIGN.md): engine lifecycle, authorization port, errors, and storage.
- [Engine](../../src/engine.rs): builder, modes, and lifecycle.
- [Authorization port](../../src/authorization.rs): actions, resources, and engine checks.
- [Errors](../../src/error.rs) and [codes](../../src/error/codes.rs): error kinds and stable codes.
- [Migrations](../../src/store/migrations/init.rs): squashed schema.
- [Examples](../../examples/README.md): embedding scenarios.
