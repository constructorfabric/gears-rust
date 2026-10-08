# Orders Lifecycle

The S2 foundation milestone is delivered (S2-01–S2-12 plus the early S6-01/S6-04 draft reads; see [docs/implementation/README.md](docs/implementation/README.md)): **authorized draft capture and draft inspection** run through the real engine, PEP, sealed audit, idempotency registry and the bound Event Broker producer on PostgreSQL. Preview, submit, amendments, holds, cancel and the Workflow seams are registered contracts whose routes are not mounted and whose SDK methods answer `unavailable` until their packages deliver them. The optional example-server feature is `bss-orders-lifecycle` (it links Event Broker, the standalone cluster plugin and the SQLite event-log backend); it is absent from the server's default features.

| Package | Library | Purpose |
|---|---|---|
| `cf-gears-bss-orders-lifecycle-sdk` | `bss_orders_lifecycle_sdk` | Shared typed boundary, reason/state/trigger/event identities and planned operation catalog |
| `cf-gears-bss-orders-lifecycle` | `bss_orders_lifecycle` | Gear wiring, config, prerequisites, permission/type registration, REST boundary adapters and cooperative lifecycle |

The local `OrdersLifecycleV1` provider and the REST surface expose the five draft authoring operations (`create`, `patch_order`, `add_line`, `patch_line`, `remove_line`) and the three draft reads (`get`, `list`, `list_lines`); every other catalog operation (including `submit`) answers canonical 503 `unavailable` from the SDK and is unrouted over HTTP (the S2-12 census, `api::rest::DELIVERED_OPERATIONS`). The migration hook applies the S2-02 schema (24 tables under `bss_orders__`), the role and grant migrations and migration 10 (runtime history read). Additional SDK methods, complete commercial response models, native owner codecs, event schemas and business handlers arrive with their implementation packages. The [25-operation catalog](docs/implementation/contracts/CATALOG.md) describes that planned surface; its presence does not enable those operations.

## Configuration and lifecycle

The gear requires an explicit `config` section:

```yaml
gears:
  bss-orders-lifecycle:
    config:
      lock_route: direct
      idempotency_lease_seconds: 30
      dependency_timeout_ms: 2000
      service_principals:
        - role: workflow          # at least one; subscriptions and billing are optional
          subject_id: "<authenticated subject UUID>"
          tenant_id: "<its subject tenant UUID>"
      events:
        producer_subject_id: "<Orders producer subject UUID>"
        producer_tenant_id: "<its subject tenant UUID>"
        broker_partitions: 4      # must equal the broker's Orders topic setting (D-205)
      audit_minimization:
        key_id: "2026-10"         # recorded in every value; change it on rotation
        key: "${ORDERS_AUDIT_MINIMIZATION_KEY}"  # secret reference, 32..1024 bytes
      maintenance:                # optional: without it no worker is scheduled
        actor_subject_id: "<maintenance service subject UUID>"
        actor_tenant_id: "<its subject tenant UUID>"
        tasks: [retention_purge, idempotency_cleanup, audit_verification, audit_checkpoint]
        connections:              # one restricted LOGIN per class the tasks need (secret references)
          retention: "${ORDERS_RETENTION_DSN}"
          maintenance: "${ORDERS_MAINTENANCE_DSN}"
          verifier: "${ORDERS_VERIFIER_DSN}"
          checkpoint: "${ORDERS_CHECKPOINT_DSN}"
        schedule: {}              # design baselines; each value may be tightened, never exceeded
```

The five Orders-owned workers (S2-11; DESIGN Foundation contract §3.8) run under `maintenance`: per-state TTL expiry and draft auto-void (bodies arrive with Stage 5 and report `unavailable` until then), idempotency-window cleanup with the D-188/D-198 recovery continuation (bodies S3/S5), the daily retention purge, and per-namespace audit verification/checkpointing. Each takes its advisory key (`expiry`, `draft-auto-void`, `idempotency-cleanup`, `retention-purge`, `audit/<namespace>`) on the host connection with one non-blocking attempt and runs only through its class connection: a `LOGIN` member of the matching migration-08 group role (`bss_orders_discovery`, `_maintenance`, `_retention`, `_verifier`, `_checkpoint`), provisioned by the deployment. Provisioning contract (the E2E launcher of S2-12 and production alike): for each class in `maintenance.tasks`, create one `LOGIN` role with `INHERIT` that is a member of **exactly** that one group role and of nothing else (not a superuser, not a member of another `bss_orders_*` role, no direct table grants), and hand its DSN to the gear only as the secret reference named in `maintenance.connections.<class>` (the E2E fragment uses `ORDERS_MAINTENANCE_DSN`, `ORDERS_RETENTION_DSN`, `ORDERS_VERIFIER_DSN`, `ORDERS_CHECKPOINT_DSN`); the host connection stays on its own login and remains the only advisory-lock session. Startup attests every configured connection with zero-row probes and refuses one with missing or excess privilege (a superuser or a wrong-class login never starts a worker; the error names the class, never the DSN). The request path never reaches these connections: they are held only by the worker scheduler. Metrics are on the process meter under `bss-orders-lifecycle` (`orders_worker_*`, `orders_retention_*`, `orders_idempotency_*`, `orders_execution_recovery_*`, `orders_audit_*`). See [MAINTENANCE](docs/implementation/MAINTENANCE.md).

`service_principals` is required (D-115). The audit `actor_class` derives only from the authenticated `(subject_id, subject_tenant_id)`: the configured maintenance actor (`maintenance.actor_subject_id`/`actor_tenant_id`) is `system`, a configured principal is `service`, and every other subject (including the same subject ID in another tenant) is `user`. The class grants nothing. A missing Workflow principal, nil or duplicate identities, an unknown role or overlap with the maintenance actor fails startup. [config/e2e-orders-lifecycle.yaml](config/e2e-orders-lifecycle.yaml) is the gear block for the live E2E host, with distinct system/service identities.

Audit minimization (D-96, keyed by D-204): administrative-edit before/after values follow a closed allowlist. `external_reference` is stored verbatim (bounded and credential-free). `display_label` and `internal_notes` are stored only as `hmac-sha256:v1:<key_id>:<hex>`, an HMAC-SHA256 tag under the required `audit_minimization.key`. The key is a secret reference (`"${ENV_VAR}"`, resolved at load time through the toolkit's `#[expand_vars]`), never committed, logged or echoed; a missing, unresolved or short (< 32 bytes) key, or a malformed `key_id` (1–64 of `A-Z a-z 0-9 . _ -`), fails startup. Rotation installs a new `key_id` with a new key; stored rows keep the ID they were written with. Audit verification hashes the stored text and never needs the key. A tag is pseudonymization, not anonymization: Privacy/Legal approval remains open (`cpt-cf-bss-orders-lifecycle-upreq-audit-identity-lifecycle`). Delegation proof references are refused if they embed credentials.

Configure the database using the host's standard gear DB configuration. Initialization requires PostgreSQL, `AuthZResolverApi`, and `TypesRegistryClient`. No default/allow-all PDP is supplied. Missing config/dependencies, unknown settings, an invalid duration, an unsupported backend, rejected/incomplete schema registration or registry timeout fails initialization before publishing the local client.

`lock_route` is explicitly `direct` or `session_pool`; transaction pooling is not supported. This is an operator declaration about the toolkit advisory-lock connection, not proof of the deployed pool's behavior. S1-03 [capability proofs](docs/implementation/CAPABILITIES.md) demonstrate direct PostgreSQL locking and custom-property enforcement locally. The deployed route and real authorization policy remain unverified. The lease range is 1–86400 seconds and dependency timeout 1–60000 milliseconds. These scaffold configuration bounds do not change a commercial hold deadline or state TTL.

The lifecycle uses the runtime cancellation token and releases its retained dependency state on shutdown. `/healthz` remains the host's shallow liveness probe. The gear's readiness check (S2-12, DESIGN §3.8) is real: the instance is ready only while its lifecycle task runs, the managed Event Broker producer is bound, the engine and the read parts are mounted and the store answers a scoped read of the mandatory platform date-policy default within 2 s. Losing any of these makes the instance **not ready** (`orders-store-unavailable`, the producer's bind-failure code, or `orders-not-ready`) without stopping the lifecycle task, so traffic stops and nothing restarts the process into the same unavailable dependency; readiness returns when the dependency does. Production enablement still requires the deployment conditions in [docs/implementation/MILESTONE.md](docs/implementation/MILESTONE.md) (provider policy, broker partition equality, identity provisioning, gateway zones).

Pre-engine throttling (Foundation §3.7, D-185): every caller-facing engine-entering route binds the api-gateway identity-keyed zone `rl_orders_caller_write` (the E2E fragment pins `3/s`, burst 20) and the workflow-only operations will bind `rl_orders_workflow_write`; a deployment must configure both zones or the gateway refuses to start. The per-(caller, order) limit (20 per minute) is the gear-local REST-edge fallback `throttling.per_order_per_minute` / `throttling.per_order_max_keys` until the gateway's path-parameter key exists ([UPSTREAM_REQS §2.12](docs/UPSTREAM_REQS.md#212-api-gateway)). A throttled attempt answers the canonical 429 before the engine and writes no audit row.

## Shared contract and authorization declarations

The SDK catalog is generated from the reviewed S1-02 inventory by `orders-lifecycle-sdk/generate_catalog.py`. Its enums include 11 states, 21 triggers, 11 event identifiers and 86 reason mappings. Catalog tests compare every operation and reason against the reviewed JSON. Positive commercial versions and nonnegative draft revisions have checked constructors and checked deserialization; idempotency keys preserve exact bytes and redact Debug output.

The REST boundary applies the explicit HTTP 428 override for missing/malformed expected version while retaining the canonical type/domain/code/title. Error diagnostics contain no arbitrary upstream or caller fields. Unknown remote Problems retain their known envelope/context unchanged. These adapters are tested but not mounted as routes.

The GTS bootstrap registers four permission-resource labels, the category type with `new_sale`/`change` instances, the reason base/86 identities, and 19 separately grantable permissions. Registering `change` is not permission to admit it. Full event/payload schemas are delivered by their owning packages. Registration results must acknowledge exactly the requested identifiers; partial success cannot install the SDK client.

Design action tokens such as `approval-reflection` and `force-fail-unreconciled` are **logical names**. `gts::PERMISSIONS` binds them explicitly to toolkit `snake_case` action names, such as `approval_reflection` and `force_fail_unreconciled`. Future PEP calls must use the registered `action` and `resource` in this same catalog, not transform caller input or independently spell the permission. The aggregate ResourceType advertises the three named business axes plus the standard resource ID; it does not advertise `owner_tenant_id`. This is a declaration, not delivered authorization enforcement.

## Verification

Run from the repository root; the runtime test starts an isolated PostgreSQL container using the workspace's pinned image and removes it when finished:

```sh
cargo test -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk
cargo clippy -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk --all-targets --no-deps
cargo fmt -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk --check
cargo gears lint --dylint -P cf-gears-bss-orders-lifecycle -P cf-gears-bss-orders-lifecycle-sdk
cargo check -p cf-gears-example-server --no-default-features --features bss-orders-lifecycle
python3 gears/bss/orders-lifecycle/orders-lifecycle-sdk/generate_catalog.py --check
make e2e-orders-lifecycle   # live HTTP/PostgreSQL E2E: own PostgreSQL container, migrate as the schema owner, server under the restricted runtime login (Docker)
```

See the [execution ledger](docs/implementation/SCAFFOLD.md) for observed results and the [implementation handoff](docs/implementation/README.md) for the next work. The registry/PDP used in bootstrap tests are explicitly test doubles; no live policy or real-registry conformance is claimed.

The [S1-03 capability ledger](docs/implementation/CAPABILITIES.md) records real PostgreSQL constraint/role tests, compile-fail discovery barriers and the bundled authorization provider limitation. These test-only prototypes do not enable business operations.

The [S1-04 commercial ledger](docs/implementation/COMMERCIAL.md) documents the native schema-2 receipt codec, exact term conversion, independent digest vectors and proposed owner contracts. Run the `commercial/build_vectors.py --check` and `commercial/check_contracts.py` commands listed there alongside the Rust suites.

The [S1-05 process ledger](docs/implementation/PROCESS.md) records proposed owner/receiver contracts and 151 specification cases, including the mandatory event-consumer corpus. Run `python3 gears/bss/orders-lifecycle/docs/implementation/process/check_contracts.py`. Missing owner SDKs and real receiver conformance remain production blockers.

The [S1-06 conformance ledger](docs/implementation/CONFORMANCE.md) records independent audit/request/cursor/native digest vectors, 200-line receipt construction and deterministic PostgreSQL fault probes. Its test-only encoders/hooks do not enable business operations.

The [S1-07 readiness ledger](docs/implementation/READINESS.md) maps all upstream requirements and implementation packages to owners, existing capabilities, evidence gaps and an acyclic dispatch queue. The next assignment is S5-01 contract corrections before final grant/control migrations.

S5-01 local receiver contract amendments and the semantic operation/alias matrix are in [RECEIVER_CONTRACTS](docs/implementation/RECEIVER_CONTRACTS.md). Next implementation package: S2-02 schema and secure repositories.
