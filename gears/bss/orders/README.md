# Orders

The S2 foundation milestone is delivered (S2-01–S2-12 plus the early S6-01/S6-04 draft reads): **authorized draft capture and draft inspection** run through the real engine, PEP, sealed audit, idempotency registry and the bound Event Broker producer on PostgreSQL. Preview, submit, amendments, holds, cancel and the Workflow seams are registered contracts whose routes are not mounted and whose SDK methods answer `unavailable` until their packages deliver them. The optional example-server feature is `bss-orders` (it links Event Broker, the standalone cluster plugin and the SQLite event-log backend); it is absent from the server's default features.

| Package | Library | Purpose |
|---|---|---|
| `cf-gears-bss-orders-sdk` | `bss_orders_sdk` | Shared typed boundary, reason/state/trigger/event identities and planned operation catalog |
| `cf-gears-bss-orders` | `bss_orders` | Gear wiring, config, prerequisites, permission/type registration, REST boundary adapters and cooperative lifecycle |

The local `OrdersV1` provider and the REST surface expose the five draft authoring operations (`create`, `patch_order`, `add_line`, `patch_line`, `remove_line`) and the three draft reads (`get`, `list`, `list_lines`); every other catalog operation (including `submit`) answers canonical 503 `unavailable` from the SDK and is unrouted over HTTP (the S2-12 census, `api::rest::DELIVERED_OPERATIONS`). The migration hook applies the S2-02 schema (24 tables under `bss_orders__`), the role and grant migrations, migration 10 (runtime history read) and migration 11 (complete item-key evidence). Additional SDK methods, complete commercial response models, native owner codecs, event schemas and business handlers arrive with their implementation packages. The 25-operation catalog describes that planned surface; its presence does not enable those operations.

## Configuration and lifecycle

The gear requires an explicit `config` section:

```yaml
gears:
  bss-orders:
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

The five Orders-owned workers (S2-11; DESIGN Foundation contract §3.8) run under `maintenance`: per-state TTL expiry and draft auto-void (bodies arrive with Stage 5 and report `unavailable` until then), idempotency-window cleanup with the D-188/D-198 recovery continuation (bodies S3/S5), the daily retention purge, and per-namespace audit verification/checkpointing. A `maintenance` block must list `retention_purge`, because every refusal is audited; startup refuses one that does not (D-239). Each takes its advisory key (`expiry`, `draft-auto-void`, `idempotency-cleanup`, `retention-purge`, `audit/<namespace>`) on the host connection with one non-blocking attempt and runs only through its class connection: a `LOGIN` member of the matching migration-08 group role (`bss_orders_discovery`, `_maintenance`, `_retention`, `_verifier`, `_checkpoint`), provisioned by the deployment. Provisioning contract (the E2E launcher of S2-12 and production alike): for each class in `maintenance.tasks`, create one `LOGIN` role with `INHERIT` that is a member of **exactly** that one group role and of nothing else (not a superuser, not a member of another `bss_orders_*` role, no direct table grants), and hand its DSN to the gear only as the secret reference named in `maintenance.connections.<class>` (the E2E fragment uses `ORDERS_MAINTENANCE_DSN`, `ORDERS_RETENTION_DSN`, `ORDERS_VERIFIER_DSN`, `ORDERS_CHECKPOINT_DSN`); the host connection stays on its own login and remains the only advisory-lock session. Startup attests every configured connection with zero-row probes and refuses one with missing or excess privilege (a superuser or a wrong-class login never starts a worker; the error names the class, never the DSN). The request path never reaches these connections: they are held only by the worker scheduler. Metrics are on the process meter under `bss-orders` (`orders_worker_*`, `orders_retention_*`, `orders_idempotency_*`, `orders_execution_recovery_*`, `orders_audit_*`). See MAINTENANCE.

`service_principals` is required (D-115). The audit `actor_class` derives only from the authenticated `(subject_id, subject_tenant_id)`: the configured maintenance actor (`maintenance.actor_subject_id`/`actor_tenant_id`) is `system`, a configured principal is `service`, and every other subject (including the same subject ID in another tenant) is `user`. The class grants nothing. A missing Workflow principal, nil or duplicate identities, an unknown role or overlap with the maintenance actor fails startup. [config/e2e-orders.yaml](config/e2e-orders.yaml) is the gear block for the live E2E host, with distinct system/service identities.

Audit minimization (D-96, keyed by D-204): administrative-edit before/after values follow a closed allowlist. `external_reference` is stored verbatim (bounded and credential-free). `display_label` and `internal_notes` are stored only as `hmac-sha256:v1:<key_id>:<hex>`, an HMAC-SHA256 tag under the required `audit_minimization.key`. The key is a secret reference (`"${ENV_VAR}"`, resolved at load time through the toolkit's `#[expand_vars]`), never committed, logged or echoed; a missing, unresolved or short (< 32 bytes) key, or a malformed `key_id` (1–64 of `A-Z a-z 0-9 . _ -`), fails startup. Rotation installs a new `key_id` with a new key; stored rows keep the ID they were written with. Audit verification hashes the stored text and never needs the key. A tag is pseudonymization, not anonymization: Privacy/Legal approval remains open (`cpt-cf-bss-orders-upreq-audit-identity-lifecycle`). Delegation proof references are refused if they embed credentials.

Configure the database using the host's standard gear DB configuration. Initialization requires PostgreSQL, `AuthZResolverApi`, and `TypesRegistryClient`. No default/allow-all PDP is supplied. Missing config/dependencies, unknown settings, an invalid duration, an unsupported backend, rejected/incomplete schema registration or registry timeout fails initialization before publishing the local client.

`lock_route` is explicitly `direct` or `session_pool`; transaction pooling is not supported. This is an operator declaration about the toolkit advisory-lock connection, not proof of the deployed pool's behavior. S1-03 capability proofs demonstrate direct PostgreSQL locking and custom-property enforcement locally. The deployed route and real authorization policy remain unverified. The lease range is 1–86400 seconds and dependency timeout 1–60000 milliseconds. These configuration bounds do not change a commercial hold deadline or state TTL.

The lifecycle uses the runtime cancellation token and releases its retained dependency state on shutdown. `/healthz` remains the host's shallow liveness probe. The gear's readiness check (S2-12, DESIGN §3.8) is real: the instance is ready only while its lifecycle task runs, the managed Event Broker producer is bound, the engine and the read parts are mounted and the store answers a scoped read of the mandatory platform date-policy default within 2 s. Losing any of these makes the instance **not ready** (`orders-store-unavailable`, the producer's bind-failure code, or `orders-not-ready`) without stopping the lifecycle task, so traffic stops and nothing restarts the process into the same unavailable dependency; readiness returns when the dependency does. Production enablement still requires the deployment conditions recorded in docs/UPSTREAM_REQS.md (provider policy, broker partition equality, identity provisioning, gateway zones).

Pre-engine throttling (Foundation §3.7, D-185): every caller-facing engine-entering route binds the api-gateway identity-keyed zone `rl_orders_caller_write` (the E2E fragment pins `3/s`, burst 20) and the workflow-only operations will bind `rl_orders_workflow_write`; a deployment must configure both zones or the gateway refuses to start. The per-(caller, order) limit (20 per minute) is the gear-local REST-edge fallback `throttling.per_order_per_minute` / `throttling.per_order_max_keys` until the gateway's path-parameter key exists ([UPSTREAM_REQS §2.12](docs/UPSTREAM_REQS.md#212-api-gateway)). A throttled attempt answers the canonical 429 before the engine and writes no audit row.

## Shared contract and authorization declarations

The SDK catalog and the gear's REST wire bindings are generated from the reviewed S1-02 inventory by `orders-sdk/generate_catalog.py`. Its enums include 11 states, 21 triggers, 11 event identifiers and 92 reason mappings. Catalog tests compare every operation and reason against the reviewed JSON. Positive commercial versions and nonnegative draft revisions have checked constructors and checked deserialization; idempotency keys preserve exact bytes and redact Debug output.

The REST boundary applies the explicit HTTP 428 override for missing/malformed expected version while retaining the canonical type/domain/code/title. Error diagnostics contain no arbitrary upstream or caller fields. The SDK error carries no Problem, HTTP status or route: the gear's REST error gear (`api/rest/error.rs`) and the generated wire bindings (`api/rest/wire.rs`) own them, and a canonical error Orders does not model is kept whole in `OrdersError::Other` (D-219).

The GTS bootstrap registers four permission-resource labels, the category type with `new_sale`/`change` instances, the reason base and 92 reason identities, and 19 separately grantable permissions. Registering `change` is not permission to admit it. Full event/payload schemas are delivered by their owning packages. Registration results must acknowledge exactly the requested identifiers; partial success cannot install the SDK client.

Design action tokens such as `approval-reflection` and `force-fail-unreconciled` are **logical names**. `gts::PERMISSIONS` binds them explicitly to toolkit `snake_case` action names, such as `approval_reflection` and `force_fail_unreconciled`. Future PEP calls must use the registered `action` and `resource` in this same catalog, not transform caller input or independently spell the permission. The aggregate ResourceType advertises the three named business axes plus the standard resource ID; it does not advertise `owner_tenant_id`. This is a declaration, not delivered authorization enforcement.

## Development delivery

Preview and submit are **not delivered in production**: `OrdersV1::preview` and `submit` answer canonical 503 `unavailable`, the Preview and submit routes are unmounted and the example server never enables the feature. For development, build with the `gate-dev-stubs` cargo feature and set `gate.dev_stubs: true` with `gate.dev_stub_fixture` (see "Development stubs" below; both are required, and a build without the feature rejects the flag at config validation). Then both flows run end to end against the fixture:

- **Preview** (`OrdersV1::preview`, `POST /bss-orders/v1/orders/preview`): one batched call per port, the gate assessment and a standalone diagnostics run; nothing is reserved, no audit row is written and a line lacking an authored term or billing cycle yields `tcvWithheld` instead of a TCV. The result carries `assessment_id`, per-line results, optional `resolved_total` (including optional producer-declared `exclusions`), indicative tax and fulfillment time/deferral. Optional `tcv_basis` copies the producer's `finite_term`, `rolling_annualized` or `mixed` token when TCV is present; it is omitted with an unavailable or withheld TCV. Orders derives neither exclusions nor a valuation horizon. Preview runs no admission guard and has no consumed-item-cap annotation.
- **Submit** (`OrdersV1::submit`; REST `POST /bss-orders/v1/orders/{orderId}/submit`, operation `bss_orders.submit`, mounted beside Preview only in this development delivery and outside the delivered-route census, D-234): assess, reserve the durable commercial attempt, check each line under its frozen Pricing key (the development acceptance double replays a key's first receipt), then commit through the engine with the pin, totals and `OrderSubmitted`. A successful submit returns the frozen run's `assessment_id` (the design's assessmentId, spelled `snake_case` on the wire); same-key replay preserves that identity. The optional member is absent on legacy responses and other transitions. A same-key retry recovers a lapsed attempt; there is no maintenance recovery body yet (D-232).

A failed local in-flight count or tuple lookup retains the completed gate port diagnostics in their stable log/signal order (D-236 A26 amendment), then returns the existing sanitized 503. It produces no assessment or durable gate-outcome vector, creates no commercial attempt and starts no acceptance work. Cancellation still prevents the next dependency wave.

**Subscription-side cardinality is advisory at order time (D-180).** Neither a gate nor an activation re-check pass guarantees admission. Subscriptions must enforce cardinality atomically at activation; the submit/activation path is not production-ready until `cpt-cf-bss-orders-upreq-overlap-activation-atomicity` is agreed and delivered. Workflow must handle later `overlap-collision` through failure acknowledgement and compensation. Orders' transactional one-in-flight-order claim does not supply Subscriptions' atomic admission.

The stubs are never evidence: readiness reports each stubbed port as `stubbed (development only)`, a stub answers only what the fixture states, and a stubbed answer proves nothing about a delivered producer. Decisions: D-231 (delivery switch, engine diagnostics hook, submit protocol bindings), D-232 (recovery scope, open), D-234 (the submit route and the e2e development build), D-236 (milestone-3c behavior, wire additions, evidence limits and deferrals), D-237 (residual implementation and current runtime evidence).

## PR5516 semantic adoption (D-238)

[The proposed Orders / Subscriptions contract](docs/SUBSCRIPTIONS_CONTRACT.md) adopts protected
active, retained-suspended and confirmed-order reservation semantics. The target requires
reservation before capture and protected own conversion/resume after a limit decrease.
Orders-local development delivery in `a81090675` and `cd82261cf`: complete recurring item-key
collections now survive frozen recovery, JSONB persistence, transactional all-key exclusion and
multi-scope recheck. The separate `protected_full_tuple_v1` proposal validates three counts,
versioned evidence and exact reserved own-membership; legacy active-only evidence stays separate.
Only a proven own reservation exempts initial activation from another slot. Production providers,
atomic Subscriptions reservation/conversion, financial fences and Workflow adoption remain open.
See the contract for concrete local DTOs and the provider checklist.

At source `39648a630`, 33 verification commands passed, with 583 tests passed, zero failures
and 224 ignored across 34 summaries. An earlier expanded HTTP run had 46 passes and two failures
on malformed protected evidence (500 instead of canonical 503); the source fix has independent
review approval. Final verification on that source passed all 224 PostgreSQL tests, rebuilt the
HTTP host and passed all 48 suite cases (43 HTTP scenarios and five sidecar helpers), including
both former failures. Task 2's 43-case HTTP pass is historical.

## Residual delivery and owner contracts

New submit attempts freeze the exact request passed to the evaluation port before acceptance dispatch. It includes the available native Pricing bindings, authorized tenant axes, assessment instant, plan identity, actual start date, resolved BillingTerms and known policy versions. Recovery reuses that snapshot without evaluation. Complete new line evidence stores each returned product identity independently of its scope in `overlap_item_keys`; legacy scalar rows and attempts retain their original absent fields rather than acquiring inferred facts.

Legacy occupancy in the development mirror requires count, limit, limit provenance and explicit `count_scope` (`per_payer` or `full_tuple`). Missing/malformed fields fail decoding; explicit `null` retains the local unlimited encoding. Predicate 7 adds other-resource in-flight lines only to a per-payer count; predicate 9 always checks same-tuple claims. The scope tag does not establish producer provenance or Subscriptions agreement on unlimited versus unknown. The new `item_keys_v1` collection supports distinct keys locally; real SUB-G1 mapping and producer adoption remain outstanding.

The proposed evaluation mirror accepts producer-declared recurring/one-time committed components and an explicit order/recurring TCV carrier for baskets without a recurring component. Orders validates coverage, kind uniqueness, currency and representability and copies the declared figures; usage remains uncommitted. Absent component lists retain the legacy single-component format. Orders does not split aggregates, sum money or invent a carrier, exclusions or horizon. Rating must still adopt the request/result schema, canonical request digest, profile/model and promotion-policy contract; production providers, grants and Workflow decoder/conformance adoption remain outstanding. These local changes do not close D-236's approved deferrals (see D-237).

## Observability

The gate reports on the existing meter `bss-orders` (D-223 naming; no tenant, subject or order attribute except the seller's `catalog_tenant`, bounded by `gate.commercial_identities[]`). Port signals are per call; run signals count once per run, never once per line, and a cancelled run or an integration error emits none:

- `orders_gate_port_calls_total{port, outcome}` (`ok`, `deadline`, `transient`, `permanent`, `backpressure`, `cancelled`, `subrequest_cap`, `breaker_open`)
- `orders_gate_port_call_duration_ms{port}` (only calls that reached the callee, retry included)
- `orders_gate_port_retries_total{port}`
- `orders_gate_breaker_state{port}` (0 closed, 1 half-open, 2 open)
- `orders_gate_breaker_transitions_total{port, to}`
- `orders_gate_port_unavailable_total{port, kind}` (`outage`, `denied`, `deadline`, `malformed`, `missing`)
- `orders_gate_catalog_denials_total{catalog_tenant}` (runs with a denied catalog read, once per run per seller)
- `orders_gate_runs_total{flow, outcome}` (`preview`/`submit`; `passed`/`blocked`)
- `orders_gate_refusals_total{flow, predicate, origin, reason}`
- `orders_gate_predicates_unevaluable_total{flow, predicate, origin, reason}`
- `orders_gate_runs_unevaluable_total{flow}`
- `orders_gate_pins_total{flow, kind}` (`unresolvable`, `unevaluable`; runs with such a pin, not lines)
- `orders_gate_occupancy_incomplete_total{flow}`
- `orders_gate_resolution_duration_ms{flow}`
- `orders_gate_resolution_over_ceiling_total{flow}` (above 2500 ms Preview, 2250 ms submit)
- `orders_gate_requests_total{flow}` (before the per-caller throttle, whose rejections stay on `orders_throttle_rejections_total{kind="per_caller"}`)

Alerts: **page** on any `orders_gate_breaker_transitions_total{to="open"}` increase in 5 min and on any `orders_gate_pins_total{kind="unresolvable"}` increase in 5 min; **warn** when `orders_gate_runs_unevaluable_total / orders_gate_runs_total` exceeds 0.05 over 15 min and when p95 `orders_gate_resolution_duration_ms{flow}` is above the flow's ceiling over 15 min. Every number is an unratified baseline (D-233).

Gate deadlines (D-240, unratified like D-227 item 5): `gate.deadline_ms` is keyed by port token (`current-revision`, `resolve`, `sku-read`, `assessment`, `billing-terms`, `acceptance-policy`, `identity-profile`, `contract-resolution`, `overlap-key`, `occupancy`, `evaluation`, `indicative-tax`; an unknown token fails at parse), and it, `gate.authority_exchange_deadline_ms` and `gate.acceptance_deadline_ms` each accept `1..=60000` ms, the bound of `dependency_timeout_ms`. Each `gate.commercial_identities[]` entry needs a non-blank `client_id`, no blank scope, and a non-empty `client_secret` that is not an unresolved `${VAR}` reference; boot refuses otherwise, naming the seller and never the secret.

Each port has a gear-wide circuit breaker (gear-local until the platform ships one, `…-upreq-shared-circuit-breaker`): `gate.breaker` with `enabled` (true), `failure_rate` (0.5), `window_seconds` (30), `open_seconds` (10) and `min_calls` (10, unratified), these defaults applying when the block is absent. A deadline or transient failure counts against it (at most one deadline per run per port); an open breaker answers the port unavailable (`outage`) without a call. Readiness shows each port's `breaker` state (`closed`, `open`, `half-open`) in the manifest and names non-closed breakers in the health summary.

The activation re-check (03 §3.6) is a pure decision in `domain/gate/recheck.rs` with 22 frozen vectors in `orders/tests/fixtures/activation-recheck-v1.json`, which Orders Workflow adopts as its conformance set (D-233 item 9).

## Verification

Run from the repository root. The unit tier needs no Docker. The in-crate gate, storage and gear-boot tests now compile and run through an Orders-owned test harness assembled from the current real Event Broker and committed Types Registry APIs; the four historical D-235 compile errors no longer block execution. Its fault wrapper delegates to the real broker. A simulated persisted response is not evidence of broker synchronous persistence, and the broker partition-equality limitation remains open.

The Postgres tier (D-224) runs tests marked `#[ignore = "requires Docker (testcontainers)"]`, each with isolated PostgreSQL containers. Enable `gate-dev-stubs` to include the development gate/submit storage tests. The pure `gate_*` integration tests and SDK Preview tests remain runnable without storage. The historical D-237 local verification on 2026-10-27 passed all 32 recorded commands and 559 tests across the feature-enabled library, integration and SDK tiers; the separate Docker PostgreSQL run passed all 220 ignored tests. Formatting, production-library clippy, toolkit-library clippy, generated catalog/schema inventory and CFS validation passed. These results supersede the earlier six-fixture-failure PostgreSQL checkpoint; they do not claim all-target clippy or live-provider conformance.

```sh
cargo test -p cf-gears-bss-orders -p cf-gears-bss-orders-sdk --features cf-gears-bss-orders/gate-dev-stubs   # unit tier
make test-integration LANE=orders-pg   # standard Postgres tier (Docker)
cargo test -p cf-gears-bss-orders --features gate-dev-stubs --lib -- --ignored --test-threads=1   # includes development gate/submit PG tests
cargo clippy -p cf-gears-bss-orders -p cf-gears-bss-orders-sdk --all-targets --no-deps
cargo fmt -p cf-gears-bss-orders -p cf-gears-bss-orders-sdk --check
cargo gears lint --dylint -P cf-gears-bss-orders -P cf-gears-bss-orders-sdk
cargo check -p cf-gears-example-server --no-default-features --features bss-orders
python3 gears/bss/orders/orders-sdk/generate_catalog.py --check
make e2e-orders   # live HTTP/PostgreSQL E2E: own PostgreSQL container, migrate as the schema owner, server under the restricted runtime login (Docker)
```

The end-to-end suite selects the test-host `bss-orders-test-authz-plugin` (D-235; example-server feature `bss-orders-test-authz`, block key `bss-orders-test-authz-plugin`) as its PDP. The development build is `cargo build --bin cf-gears-example-server --no-default-features --features bss-orders,bss-pricing,bss-orders-test-authz,static-tenants,static-authn,static-authz,bss_orders/gate-dev-stubs`. Run `make e2e-orders` with Docker. The suite owns its PostgreSQL sidecar (`testing/e2e/suites/bss/orders/postgres_sidecar.py`), using `postgres:18-alpine` with `GEARS_TEST_PG_TAG` override and a dynamic loopback port; it no longer waits for shared sidecar PR #5376. Admin migration precedes server boot under restricted runtime and worker logins. Toolkit now reads existing PostgreSQL migration history without attempting schema DDL; missing history and pending migrations still require their normal privileges.

The suite seeds real Products and Pricing, writes a per-run fixture and boots twice to exercise development Preview and submit over HTTP. Its collection includes sidecar checks as well as the HTTP scenarios; it is neither a production PDP nor live-provider evidence. After the invalid-assessment classification and response replay fixes, the final HTTP run exposed two stale non-recurring fixtures; those now explicitly supply producer component declarations and the required TCV carrier. Both affected cases passed, then the full suite passed all 43 cases (38 HTTP scenarios and 5 sidecar checks) on 2026-10-27. D-237 records the local verification evidence and the remaining owner dependencies.

Development stubs (Task 8b): to exercise the gate end to end before the nine undelivered providers ship, build with the `gate-dev-stubs` cargo feature (`cargo run -p cf-gears-example-server --features bss-orders,bss_orders/gate-dev-stubs`; `bss_orders` is the example server's dependency name for `cf-gears-bss-orders`; never on by default, and the example server's `bss-orders` feature never enables it) and set `gate.dev_stubs: true` with `gate.dev_stub_fixture` naming a JSON fixture; init fails on a missing, unreadable or invalid fixture, and a build without the feature rejects `gate.dev_stubs` at config validation. Each stubbed port answers only what the fixture states and `unavailable (missing)` otherwise, the catalog ports are never stubbed, a warning on `orders.gate.dev_stubs` names the fixture, and readiness reports every stubbed port as `stubbed (development only)` with `dev_stubs: true` in the manifest: stubbed answers are never readiness evidence. The fixture is one JSON object of optional entries keyed as each port asks (payer profiles by payer tenant, contracts by id, overlap scope keys by paid recurring SKU, occupancy by scope key, terms by plan revision, line evaluations by line plus the order totals and TCV as given, and single acceptance-policy, assessment-profile and tax entries); `gears/bss/orders/config/gate-dev-stubs.example.json` is a minimal example.
