# Types Registry P0 — Task List

Plan: [`plan.md`](./plan.md) · Spec: [`SPEC.md`](./SPEC.md)

Standing bar for every task, on top of its own acceptance criteria:
`make ci` green, no regression in other gears, behaviour verified at runtime, docs updated.
Code organisation follows `docs/toolkit_unified_system/` — **not** `guidelines/DNA/languages/RUST.md`.

`TR/` abbreviates `gears/system/types-registry/types-registry/`, `TR-SDK/` abbreviates
`gears/system/types-registry/types-registry-sdk/`. Other paths are from the repository root.

**No new ADRs.** The no-PDP deviation is recorded in SPEC §9 (ceiling C6) and §12; the two wire
breaks in SPEC §10.2 — `POST /entities` (D10) and the paged content-free `GET /entities` (D12).

---

## Phase 0 — Upgrade

### - [x] T1: Upgrade to `gts-rust` 0.12.0 via `[patch.crates-io]`

**Description:** Point the workspace at the local `gts-rust` checkout so the tri-state
compatibility verdict, `ContentModel` classification and `compare_documents` become
available, then prove the corrected semantics break nothing already declared. This task's
failure mode is other gears, which is why it runs first and alone.

Outcome and full evidence: [`t1-gts-0.12.0-upgrade-report.md`](./t1-gts-0.12.0-upgrade-report.md).

**Acceptance criteria:**
- [x] `[patch.crates-io]` overrides `gts`, `gts-id`, `gts-macros` with paths into `~/dev/gts-rust`, and the three workspace requirements name **`0.12.0`**. The original criterion said not to edit any version requirement, on the premise that the local crates still declared `0.11.0`; that premise expired when the checkout bumped itself, and a `0.11.0` requirement then made the patch **unused** — a cargo warning, not an error — silently building against the published 0.11.0. Requirements at `0.12.0` make the patch load-bearing (SPEC §7)
- [x] All 202 declared GTS identifiers in the repository still admit; any that do not are fixed or explicitly waived in writing — 118/118 admitted at runtime under the e2e feature set, 797 doc/JSON files validate under the 0.12.0 validator, and every macro literal compiles. **The figure 202 itself is not reproducible** and is replaced by three measured populations in the report §3
- [x] Every difference in `#[gts_type_schema]`-generated schema documents versus 0.11.0 is enumerated and accounted for — none silent. 9 of 118 documents differ, all by one change (a doc-commented GTS-identifier field's subschema preserved under `allOf` instead of having its `description` overwritten); shown semantically inert, including `x-gts-ref`, which `XGtsRefValidator` still enforces by recursing through `allOf` (report §2)
- [x] `Cargo.lock` is **not** committed while the patch is present

**Verification:**
- [ ] `make ci` — **partial**: `fmt` clean, `clippy --workspace --all-targets --all-features -D warnings -D clippy::perf` clean, workspace tests and `gts-docs` green; `test-db`, `test-users-info-pg` and `test-usage-collector-pg` not run (Docker daemon down) and `dylint` not run (`cargo-gears` not installed). None of the four links `gts` in a way this upgrade touches; recorded in report §4
- [x] `make gts-docs` — 797 files, 0 errors, both with the installed validator and with one built from the local checkout
- [x] `cargo test --workspace` — 10213 passed, 368 skipped, 0 failures; baseline was 10206 passed + 368 skipped, and the difference is exactly the 6 new capability tests
- [x] Manual: generated schema documents captured before and after by booting the example server and dumping `GET /cf/types-registry/v1/entities`, then diffed field by field

**One source break, fixed:** `GtsIdSegment::ver_major() -> u32` became
`ver_major_opt() -> Option<u32>` (the v0-versus-wildcard fix). One call site,
`TR/src/api/rest/dto.rs`, now reads `ver_major_opt().unwrap_or(0)` — byte-identical to
0.11.0, which returned `parts().map_or(0, ..)` — so the REST response shape is unchanged.
Surfacing the distinction belongs to the major-0 quarantine slice (T18).

**Added:** `TR/tests/gts_012_semantics_tests.rs`, 6 tests pinning the tri-state verdict,
`ContentModel` including `Partial`, `GtsStore::compare_documents` and the provenance
versions. Under 0.11.0 the file does not compile — none of those symbols exists in the
published crate — so it is a genuine RED→GREEN.

**Dependencies:** None
**Files likely touched:** `Cargo.toml`, plus test fixtures the corrected semantics reject
**Scope:** M (mechanical change, large verification surface)

**Update, 2026-08-18 — O1 closed.** `gts` / `gts-id` / `gts-macros` 0.12.0 published to
crates.io; the `[patch.crates-io]` block is deleted from `Cargo.toml` and `Cargo.lock`
re-resolved against the registry. Re-verified, not assumed: `cargo check --workspace`
clean, `cargo test -p cf-gears-types-registry` 209/0 (this task's 208 + the 6
`gts_012_semantics_tests.rs` tests), `make gts-docs` 798/0. See report §6.

---

### Checkpoint 0
- [ ] `make ci` green — **partial, see T1**: everything that does not need Docker or `cargo-gears` is green
- [x] Every declared GTS identifier admits under 0.12.0
- [x] Generated-schema diff reviewed and accounted for — report §2, one class of change in 9 of 118 documents
- [ ] **Human review before any registry code is written**

---

## Phase 1 — One global Type Schema, persisted, async, end to end

Exercised by fixtures and REST only. Consumers stay on the in-memory path until T24, so
nothing in this phase can regress another gear.

### - [ ] T2: Migration for the 9 tables

**Description:** One initial SeaORM migration creating the P0 subset of `database.sql`:
`version_family`, `entity`, `type_schema_revision`, `instance_revision`, `type_schema`,
`instance`, `dependency`, `operation`, `operation_item`. Tenant columns and their CHECK
constraints are created exactly as specified and never populated with tenant scope, so P1
tenancy needs no migration. `source_claim` and `routing_config` are not created.

**Acceptance criteria:**
- [ ] All 9 tables, their PKs, FKs, UNIQUE and CHECK constraints, and the 5 indexes from `database.sql` are created
- [ ] Identifier columns are `varchar(1024)` with binary collation and ASCII charset where the backend default is multi-byte
- [ ] Enumerations stored as smallint with CHECKs enumerating allowed values
- [ ] `DatabaseCapability::migrations()` returns the Migrator; outbox tables come from `outbox_migrations_with_prefix("types_registry_outbox")`, not from this migration
- [ ] Raw SQL appears only here (`11_database_patterns.md` invariant)

**Verification:**
- [ ] `make test-db` — migration up and down on SQLite, PostgreSQL, MySQL
- [ ] Test: each CHECK constraint rejects the shape it names (e.g. `ck_tr_entity_owner` with a global row lacking `owning_gear`)
- [ ] Test: `ck_tr_operation_item_state` rejects a `succeeded` non-dry-run registration item with no `result_revision_no`

**Dependencies:** T1
**Files likely touched:**
- `TR/src/infra/storage/migrations/mod.rs`
- `TR/src/infra/storage/migrations/m20260817_000001_initial.rs`
- `TR/src/gear.rs`
- `TR/Cargo.toml`
**Scope:** M — one long DDL file; deliberately not split, because splitting it orders FKs across tasks

---

### - [ ] T3: SeaORM entities for the core six

**Description:** Entity structs for `version_family`, `entity`, `type_schema_revision`,
`type_schema`, `operation`, `operation_item`, each with `#[derive(Scopable)]` and
`#[secure(unrestricted)]` plus a comment recording the P1 switch to
`tenant_col = "owner_tenant_id"`.

**Acceptance criteria:**
- [ ] One file per entity under `TR/src/infra/storage/entity/` (`02_gear_layout_and_sdk_pattern.md`)
- [ ] Every entity declares its security dimensions; none omits the attribute
- [ ] `ponytail:`-style comment on the `#[secure(unrestricted)]` attributes records ceiling C6 (no PDP — authenticated but not authorized) and its upgrade path (`tenant_col` + `PolicyEnforcer`). This is the point where `unrestricted` is chosen, so it is where the ceiling binds
- [ ] Enumeration columns map to typed Rust enums with explicit smallint conversion, storage-only — the SDK and REST expose the string vocabulary

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry`
- [ ] Test: enum ↔ smallint round-trip for every vocabulary, asserting the exact numbers in `database.sql` (they are per-column and deliberately not aligned)
- [ ] `make dylint`

**Dependencies:** T2
**Files likely touched:** `TR/src/infra/storage/entity/{mod,version_family,entity,type_schema_revision,type_schema,operation,operation_item}.rs`
**Scope:** M — 7 files, each mechanical; grouped because they are one DDL mirror

---

### - [ ] T4: Repositories on `DBRunner`

**Description:** Repository methods for the core six, taking `runner: &impl DBRunner` and
`scope: &AccessScope` so the same method works inside and outside a transaction. Keyed
reads, insert-if-absent for `version_family`, compare-and-swap on
`entity.resource_version`, and canonical-order locking helpers.

**Acceptance criteria:**
- [ ] Every method takes `runner: &impl DBRunner`, never `&SecureConn`
- [ ] No raw SQL; all queries go through the typed builder
- [ ] Compare-and-swap on `resource_version` is a single statement whose affected-row count is the success signal
- [ ] Family create-then-locked-read works on all three backends
- [ ] Read primitives for the database read path (SPEC D2, §8.2): a keyed exact read, and a list read that prefilters in SQL on the stored columns and then applies `GtsIdPattern::matches` in Rust. **GTS identifier matching is never translated into SQL** — that would be a local approximation of GTS semantics (`constraint-gts-implementation`)
- [ ] The list read is a **keyset page**: `gts_id > :after ORDER BY gts_id LIMIT :n`, excluding deleted rows, so a page boundary cannot drift or duplicate (D12). It reports whether more remains, and never loads the whole match set to slice it in memory
- [ ] A dependency-closure read: given candidate identifiers, return them plus the transitive closure of what they consume, walking `dependency` edges (D5), `gts_id`-sorted. This is what T5 builds its store from

**Verification:**
- [ ] `make test-db`
- [ ] Test: concurrent `version_family` creation yields exactly one row
- [ ] Test: CAS with a stale version affects zero rows and is reported as such
- [ ] Test: list read with a wildcard pattern returns exactly what `GtsIdPattern::matches` accepts, including a case the SQL prefilter admits but the pattern rejects
- [ ] Test: keyset paging over a set larger than one page yields every row exactly once, and a row inserted mid-traversal neither duplicates an earlier row nor hides a later one
- [ ] Test: closure read over a chain returns the whole chain and nothing outside it

**Dependencies:** T3
**Files likely touched:**
- `TR/src/infra/storage/repo.rs`
- `TR/src/infra/storage/mapper.rs`
- `TR/src/domain/repo.rs`
- `TR/tests/common/mod.rs`
**Scope:** M

---

### - [ ] T5: Transient `gts-rust` store built from database rows

**Description:** One function that builds a `GtsStore` from a set of database rows, for use by
a single admission unit and dropped with it (SPEC D2, §8.2; `plan.md` P6). It takes the
candidates plus the transitive closure of what they consume — obtained from the `dependency`
table — reads them `gts_id`-sorted so a derived schema never loads before its base, and
returns an owned store. Nothing is cached, published or shared: **no `ArcSwap`, no snapshot,
no process-lifetime store.** Reads are served from the database, not from here. The old
in-memory repository keeps serving the old trait until T24 and is not touched.

**Acceptance criteria:**
- [ ] Signature takes a row set (or a closure query) and returns an owned `GtsStore`; it stores nothing in `self` and registers nothing globally
- [ ] Load order is `gts_id`-sorted, so a derived schema never loads before its base
- [ ] The store is built from the unit's dependency closure, not from the whole `entity` table — a whole-table load must fail the closure test below
- [ ] No lock of any kind: the store is owned by one caller, so `GtsOps` not being `Sync` is irrelevant rather than worked around
- [ ] Closure lookup uses the `dependency` edges (D5), and a candidate with no dependencies yields a store containing only the candidate

**Verification:**
- [ ] `make test-db`
- [ ] Test: store built from a chained fixture resolves the derived schema's base
- [ ] Test: closure containment — a document not reachable from the candidate's dependency closure is **absent** from the store
- [ ] Test: rows are consumed in `gts_id` order given deliberately shuffled input
- [ ] Test: two sequential builds after a committed revision each observe the new revision, with no invalidation step between them

**Dependencies:** T4
**Files likely touched:**
- `TR/src/domain/gts_store.rs`
- `TR/src/domain/mod.rs`
- `TR/tests/gts_store_test.rs`
**Scope:** S

---

### - [ ] T6: Typed configuration

**Description:** Extend `TypesRegistryConfig` with `allow_compatibility_force`, `limits.*`
(including P0's `activation_write_set`), `registration_policy` and `worker.*`, keeping the
existing keys. The `local_client.cache.*` keys stay live — the cache is kept (SPEC §8.3) — and
their reshaping into `freshness_window` / `store_bound` belongs to T30, not here.

**Acceptance criteria:**
- [ ] Absent config and `config: {}` both yield the SPEC §10.3 defaults via `ctx.config_or_default()`
- [ ] `registration_policy` keys are validated at startup; an invalid GTS pattern fails startup rather than being skipped
- [ ] `tenant_ownable` is **parsed and validated but inert** (SPEC §10.3): P0 rows are always `ownership_scope = 1`, so the parameter has nothing to decide. It is neither rejected — a P1-ready deployment carries it — nor silently read as enabling tenant ownership
- [ ] Per-parameter resolution implemented: longest literal prefix wins, an exact key beats any pattern, entries omitting a parameter are skipped, closed default otherwise
- [ ] Global `cf` vendor is implicitly admitted; nothing else is

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry`
- [ ] Table-driven test over the four policy entries in SPEC §10.3 plus the resolution rules, manual `vec![]` + loop (no `rstest`)
- [ ] Test: a more specific `allowed_vendors` **replaces** a less-specific set rather than extending it
- [ ] Test: an entry that omits `allowed_vendors` is skipped, and a less-specific entry supplies it
- [ ] Test: a config carrying `tenant_ownable` starts cleanly and does not admit a tenant-owned candidate
- [ ] Test: invalid pattern in `registration_policy` fails startup with the region named

**Dependencies:** T2
**Files likely touched:**
- `TR/src/config.rs`
- `TR/src/domain/policy.rs`
- `TR/tests/config_test.rs`
**Scope:** M

---

### - [ ] T7: Acceptance path and operation records

**Description:** The synchronous half of admission: envelope and batch bounds, canonical
identifier check, registration policy, identifier profile, Draft-07 dialect gate, `force`
gate, request fingerprint, `Idempotency-Key` resolution, then one transaction inserting the
operation, its items and the outbox message. Reads no entity state.

**Acceptance criteria:**
- [ ] Checks run in SPEC §8.1 order; policy precedes any existence lookup so a refusal cannot probe the namespace
- [ ] Policy gates **declared creation only** — absent `expected_resource_version` — so a revision or deletion in a region that has since closed still proceeds; a refusal names the region and the parameter
- [ ] Fingerprint covers canonical body, operation kind, owner, preconditions and each `force` flag
- [ ] Replay with a matching fingerprint returns the stored operation (`202` non-terminal, `200` terminal); a different fingerprint under the same key returns `409`
- [ ] Concurrent acceptance on one key resolves via the unique constraint, loser returns the winner after fingerprint verification
- [ ] `plane = 1`, `tenant_id = NULL`, `principal_id` the named P0 constant with a `TODO`; ceiling C2 (global idempotency namespace) commented
- [ ] Ceiling C5 (no operation-retention sweep — terminal operations accumulate) commented where operation rows are inserted, with the §3.2 sweep as its upgrade path
- [ ] Literal `expected_resource_version: 0` rejected; absent means must-not-exist

**Verification:**
- [ ] `make test-db`
- [ ] Tests: replay, fingerprint conflict, concurrent acceptance, each refusal reason, `0` rejection
- [ ] Test: acceptance issues no read against `entity`

**Dependencies:** T4, T6
**Files likely touched:**
- `TR/src/domain/admission/acceptance.rs`
- `TR/src/domain/admission/fingerprint.rs`
- `TR/src/domain/admission/mod.rs`
- `TR/src/domain/error.rs`
- `TR/tests/operation_idempotency_test.rs`
**Scope:** M

---

### - [ ] T8: Admission worker — one dependency-free candidate

**Description:** The worker as a plain function of `(operation_id, runner)`: build the unit's
transient store (T5), evaluate one acyclic, reference-free Type Schema candidate against it,
then commit family, entity, revision, current-state projection with materialized artifacts,
`resource_version` and the item outcome. No dependencies, no compatibility, no batching yet.

**Acceptance criteria:**
- [ ] Entry point is directly callable and returns a result; no `sleep`, timer or polling anywhere in it or its tests
- [ ] Evaluation happens outside the transaction; the transaction contains only rechecks and writes
- [ ] `type_schema` row is populated at admission — `resolved_schema`, `effective_traits`, `effective_traits_schema`, `resolution_fingerprint` (D3)
- [ ] `resolution_fingerprint` is computed over canonical bytes, independent of map iteration order
- [ ] Creation requires the identifier absent; the outcome records `gts_uuid` and `resource_version`
- [ ] The transient store is built inside the invocation and dropped with it; nothing is retained on the worker, the service or the gear between invocations, and there is no post-commit rebuild step

**Verification:**
- [ ] `make test-db`
- [ ] Test: registering a schema writes exactly one row in each of the five affected tables
- [ ] Test: `resolution_fingerprint` is stable across two computations of identical artifacts
- [ ] Test: creation against an existing identifier fails terminally with no revision written
- [ ] Test: a second invocation after a committed revision sees it — the store is rebuilt from the database, not carried over

**Dependencies:** T5, T7
**Files likely touched:**
- `TR/src/domain/admission/worker.rs`
- `TR/src/domain/admission/unit.rs`
- `TR/src/domain/artifacts.rs`
- `TR/tests/admission_worker_test.rs`
**Scope:** M

---

### - [ ] T9: REST — `POST /entities`, `GET /operations/{id}`, `GET /entities/{key}`

**Description:** The first complete REST path: submit a registration and get `202` +
operation, poll the operation, read the entity. `POST /entities` breaks its old `200`
shape (D10).

**Acceptance criteria:**
- [ ] `POST /entities` returns `202` with operation `Location` and advisory `Retry-After`; `200` only on terminal replay
- [ ] `Idempotency-Key` is required; absence is a synchronous refusal
- [ ] Errors are RFC-9457 problem details via `.standard_errors(openapi)`, never raw status tuples
- [ ] Routes are `/types-registry/v1/...` and `.authenticated()`; DTOs live only in `api/rest/dto.rs`
- [ ] `GET /entities/{key}` accepts a GTS identifier or a `gts_uuid`
- [ ] These routes are the **platform-plane** API for global entities (SPEC §8.4, `plan.md` P8): they keep the authentication they have today, stay `.exposed()` so e2e can reach them, and no handler assumes a tenant scope. `.anonymous()` is **not** used — without a platform identity to replace the current gate it would be a regression. Ceiling C8 is commented where the routes are registered
- [ ] **Handlers are mapping steps only.** No business logic in `api/rest/`; the domain service's public surface must already be sufficient for a future `api/grpc` adapter without adding domain methods (SPEC §8.4). `Idempotency-Key` is read from the header and passed as a parameter, never interpreted in the handler

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry` — `api_rest_test.rs` via `Router::oneshot`
- [ ] `make dylint` — DE0201, DE0801 clean
- [ ] Manual: `curl` the three routes against `make example`; `/cf/docs` renders them

**Dependencies:** T8
**Files likely touched:**
- `TR/src/api/rest/routes.rs`
- `TR/src/api/rest/dto.rs`
- `TR/src/api/rest/handlers.rs`
- `TR/src/api/rest/error.rs`
- `TR/tests/api_rest_test.rs`
**Scope:** M

---

### Checkpoint 1 — proves the architecture
- [ ] A fixture Type Schema registers over REST, the operation reaches `completed`, the entity and its resolved artifacts are readable
- [ ] Entity and artifacts survive a process restart byte-identically
- [ ] Consumers untouched: the old `TypesRegistryClient` is still served from its existing in-memory repository; full workspace tests pass
- [ ] The new path holds no entity state between admissions: the store is built per unit and dropped, and the entity read in the first item above comes from the database
- [ ] `make test-db` green on SQLite, PostgreSQL, MySQL
- [ ] **Human review — everything after this widens the path rather than reshaping it**

---

## Phase 2 — Instances, revisions, concurrency

### - [ ] T10: Registered Instances

**Description:** Extend admission to registered Instances: `instance_revision`, `instance`
current pointer, conformance to the Type Schema identified by the identifier prefix through
the last `~`, and the immutable schema-revision pair.

**Acceptance criteria:**
- [ ] An Instance records the exact Type Schema revision that validated it
- [ ] An Instance whose conforming schema is absent fails retryably, not terminally
- [ ] A minor or major 0 in the Instance identifier's last segment is refused at acceptance
- [ ] `instance` carries only the current-revision pointer — no derived artifact

**Verification:**
- [ ] `make test-db`
- [ ] Test: Instance value violating its schema is refused with the offending path
- [ ] Test: `fk_tr_instance_revision_schema` prevents dangling schema-revision references

**Dependencies:** Checkpoint 1
**Files likely touched:** `TR/src/infra/storage/entity/{instance,instance_revision}.rs`, `TR/src/domain/admission/unit.rs`, `TR/src/infra/storage/repo.rs`, `TR/tests/instance_test.rs`
**Scope:** M

---

### - [ ] T11: Content revisions and compare-and-swap

**Description:** Second and later revisions of a logical entity: `expected_resource_version`
preconditions, immutable revision insert, current-state pointer move, and the `unchanged`
outcome for authored content equal to current.

**Acceptance criteria:**
- [ ] Update requires `entity.resource_version == expected_resource_version`; mismatch is terminal `precondition_failed` with no silent rebase
- [ ] Equal authored content yields `unchanged`, creating no revision and not advancing `resource_version`
- [ ] `unchanged` is impossible for a create or a delete, enforced in code as well as by the CHECK
- [ ] Content hash is a prefilter only; effective artifacts are excluded from equality

**Verification:**
- [ ] `make test-db`
- [ ] Tests: stale version, equal content, content equal to an *older* non-current revision (must create a new revision, ADR-0005)
- [ ] Test: revision numbers are contiguous per entity

**Dependencies:** Checkpoint 1
**Files likely touched:** `TR/src/domain/admission/unit.rs`, `TR/src/infra/storage/repo.rs`, `TR/tests/revision_test.rs`
**Scope:** M

---

### - [ ] T12: Version-family kind, shape and contiguity rules

**Description:** The three non-stored rules enforced under the family lock: kind must match
the family, minor shape must be uniform within a major, and minors must be contiguous from
`M.0`. All three are keyed lookups, not scans.

**Acceptance criteria:**
- [ ] `vM.n~` refused while `vM~` exists; `vM~` refused while `vM.0~` exists
- [ ] `vM.n~` with `n > 0` refused unless `vM.(n-1)~` exists
- [ ] A `DELETED` predecessor still counts; the predecessor test is re-asked inside the commit transaction
- [ ] Family ownership is write-once; the entity's owner columns are a projection maintained under the lock
- [ ] The predecessor is excluded from `dependency` and from the revision vector

**Verification:**
- [ ] `make test-db`
- [ ] Table-driven test over shape and contiguity combinations
- [ ] Test: concurrent first registration under two owners yields one winner
- [ ] Test: family key derivation maps `v1~`, `v1.4~`, `v2~` to one row, and a preceding-segment minor survives verbatim

**Dependencies:** T10, T11
**Files likely touched:** `TR/src/domain/family.rs`, `TR/src/domain/admission/unit.rs`, `TR/src/infra/storage/repo.rs`, `TR/tests/family_test.rs`
**Scope:** M

---

### Checkpoint 2
- [ ] Instances register and conform; revisions and CAS behave; family rules hold under concurrency
- [ ] `make test-db` green on three backends
- [ ] Human review

---

## Phase 3 — Dependencies and materialization

### - [ ] T13: Dependency edge extraction and writes

**Description:** Extract the four edge kinds from authored content — `$ref`, `x-gts-ref`
target, immediate derivation base, Instance conformance — and replace the admitted entity's
outgoing rows on each admission. Both endpoints are always managed entities. This is also
the same extractor the worker uses for in-batch ordering.

**Acceptance criteria:**
- [ ] `x-gts-ref` edge targets the exact identifier, or the pattern's longest valid identifier prefix; a pattern naming nothing valid (`gts.*`) and a GTS §9.6 relative pointer create no edge
- [ ] Admission replaces only the admitted entity's outgoing rows
- [ ] Derivation and conformance are materialized even though derivable from the identifier
- [ ] Extraction uses `gts-rust`'s extractor, never a local scan
- [ ] Extraction is exposed as a pure function over authored content, callable without a database — required for unit testing without a fixture DB

**Verification:**
- [ ] `make test-db`
- [ ] Table-driven test over edge-kind fixtures, including the no-edge cases
- [ ] Test: re-admission removes an edge the new revision dropped

**Dependencies:** Checkpoint 2
**Files likely touched:** `TR/src/domain/dependency.rs`, `TR/src/infra/storage/repo.rs`, `TR/src/infra/storage/entity/dependency.rs`, `TR/tests/dependency_test.rs`
**Scope:** M

---

### - [ ] T14: Reverse-impact worklist and artifact refresh

**Description:** The iterative worklist over direct reverse edges (D5 — a recursive CTE is
not available, since raw SQL is forbidden outside migrations), with visited-set
deduplication, fingerprint-stability early stop, the `activation_write_set` bound, and
refresh of every affected dependent's effective artifacts in the same transaction.

**Acceptance criteria:**
- [ ] Traversal terminates on a cyclic graph
- [ ] A dependent whose recomputed artifacts are identical stops the branch and does not move `resource_version`
- [ ] Every affected dependent's artifacts become current in the same transaction as the new revision — no mixed current state
- [ ] Exceeding `limits.activation_write_set` fails the candidate with a structured reason and commits nothing partial
- [ ] `ponytail:` comment names the measured max fan-out (27), the bound (512) and the staging upgrade path

**Verification:**
- [ ] `make test-db`
- [ ] Test: revising a base with N dependents refreshes exactly N `type_schema` rows
- [ ] Test: cyclic dependency graph terminates
- [ ] Test: over-bound case commits nothing

**Dependencies:** T13
**Files likely touched:** `TR/src/domain/dependency.rs`, `TR/src/infra/storage/repo.rs`, `TR/src/domain/admission/unit.rs`, `TR/tests/dependency_repo_test.rs`
**Scope:** M

---

### - [ ] T15: Revision-vector guard and bounded retry

**Description:** The multi-pod correctness guard (D4): record a revision vector for every
correctness-relevant dependency and dependent during evaluation, then under the target's
entity lock re-derive the reverse-impact set from the database and compare both membership
and the full vector, rolling back and revalidating within a bounded retry policy.

**Acceptance criteria:**
- [ ] Vector carries `resource_version` and, where effective content was consumed, `resolution_fingerprint`
- [ ] A new, removed or moved dependency/dependent rolls the transaction back
- [ ] Retries are bounded by `worker.max_revalidation_attempts`; exhaustion terminalizes the item as `failed`
- [ ] Lock order is family → entity/current rows, in canonical identifier order, everywhere

**Verification:**
- [ ] `make test-db`
- [ ] Test: a dependency mutated between evaluation and commit causes exactly one rollback and one successful retry
- [ ] Test: a phantom dependent created after the initial scan is detected
- [ ] Test: two pods against one database — a commit on one is visible to the other's first post-commit read

**Dependencies:** T14
**Files likely touched:** `TR/src/domain/admission/unit.rs`, `TR/src/domain/admission/vector.rs`, `TR/src/infra/storage/repo.rs`, `TR/tests/concurrency_test.rs`
**Scope:** M

---

### - [ ] T16: Observability for the admission path

**Description:** Instrument admission so production behaviour is diagnosable: structured
spans per operation and per admission unit, and counters for the outcomes and bounds that
matter.

**Acceptance criteria:**
- [ ] One span per operation and one per admission unit, carrying `operation_id`, `gts_id`, kind and dry-run mode
- [ ] Counters: candidates by terminal status, refusals by reason, revalidation retries, activation-set size, worker duration
- [ ] Every refusal reason in the acceptance path is countable and distinguishable — including `Unknown` compatibility once T17 lands
- [ ] Structured fields only; no print macros (DE13xx)

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry` using `tracing-test` to assert emitted fields
- [ ] Manual: register over REST against `make example`, confirm spans and counters appear

**Dependencies:** T8 (may run parallel with T14, T15)
**Files likely touched:** `TR/src/domain/admission/worker.rs`, `TR/src/domain/admission/acceptance.rs`, `TR/src/observability.rs`, `TR/tests/observability_test.rs`
**Scope:** S

---

### Checkpoint 3
- [ ] Dependent refresh is atomic with the new revision; identical recomputation is a no-op
- [ ] Activation bound refuses rather than partially commits
- [ ] Multi-pod read-after-commit holds
- [ ] Admission emits spans and metrics
- [ ] Human review

---

## Phase 4 — Compatibility

### - [ ] T17: Compatibility against one baseline

**Description:** Baseline selection — the entity's current revision for a major-only
candidate, or the `ACTIVE`/`DELETED` definition of `vM.(n-1)~` for a minor-bearing one —
compared through `GtsStore::compare_documents`, which resolves both sides. `Unknown` is
rejected with its own reason, never collapsed into `Incompatible`.

**Acceptance criteria:**
- [ ] `compare_documents` is the only comparison entry point; `is_minor_compatible` is not used
- [ ] `CompatibilityVerdict::Unknown` fails the candidate with a reason distinct from `Incompatible` (`principle-fail-closed`)
- [ ] Every admitted revision records `gts_spec_version`, `gts_impl_version` and `compat_forced`
- [ ] `force` waives exactly one cross-minor check, only where the deployment enabled it and the candidate has such a check to waive
- [ ] Major-0 candidates get no baseline and no verdict

**Verification:**
- [ ] `make test-db`
- [ ] Compatibility matrix: optional property added at a `Closed` level (compatible), at `Open` (incompatible), at `Partial` (`Unknown`)
- [ ] Test: provenance columns match `GTS_SPECIFICATION_VERSION` and the crate version
- [ ] Test: `force` refused when `allow_compatibility_force` is off, including on Dry Run

**Dependencies:** Checkpoint 3
**Files likely touched:** `TR/src/domain/compat.rs`, `TR/src/domain/admission/unit.rs`, `TR/src/domain/error.rs`, `TR/tests/compat_test.rs`
**Scope:** M

---

### - [ ] T18: Derivation chain and major-0 quarantine

**Description:** Identifier-derived chain validation against every managed base, the
Draft-07 dialect pin across a major, and the ADR-0015 quarantine: a stable candidate may not
reference a major-0 identifier, and a major-0 schema may not carry a registered Instance.
Includes the preflight assertion (O4).

**Acceptance criteria:**
- [ ] Chain bases are reconstructed with `chain_ids()`, not stored or re-derived locally
- [ ] A stable candidate whose immediate base, `$ref` or `x-gts-ref` targets include a major-0 identifier is refused
- [ ] A registered Instance conforming to a major-0 schema is refused, even though the marker is in a preceding segment
- [ ] Dialect is pinned at initial admission and cannot change across revisions of a major
- [ ] Preflight asserts the `dependency` ⋈ `entity.gts_id` join finds no stable subject referencing a major-0 target

**Verification:**
- [ ] `make test-db`
- [ ] Tests: each quarantine path, and the preflight against a deliberately violating fixture
- [ ] Test: dialect change across revisions is refused

**Dependencies:** T17
**Files likely touched:** `TR/src/domain/derivation.rs`, `TR/src/domain/admission/acceptance.rs`, `TR/src/infra/storage/migrations/m20260817_000002_quarantine_preflight.rs`, `TR/tests/quarantine_test.rs`
**Scope:** M

---

### Checkpoint 4
- [ ] Compatibility matrix passes including the `Unknown` tier
- [ ] Provenance persisted on every revision
- [ ] Quarantine and dialect rules hold; preflight passes on the real dataset
- [ ] Human review

---

## Phase 5 — Batching, deletion, dry run

### - [ ] T19: Dependency-aware partial admission

**Description:** Batch admission: build the candidate graph from authored references between
candidates plus the implicit `vM.(n-1)~ → vM.n~` edge, condense into SCCs, process in
topological order, treat each acyclic candidate as one unit and each cyclic component as one
atomic unit, and record an outcome for every candidate. The SCC condensation and topological
order stay pure functions over a candidate set.

**Acceptance criteria:**
- [ ] Independent passing branches commit despite failures elsewhere
- [ ] In-batch references resolve against the candidate overlay, never a previously committed revision
- [ ] A failed selected dependency yields `blocked_by_dependency`; a failed lower minor yields `blocked_by_predecessor`
- [ ] A cyclic component with one invalid member commits nothing
- [ ] The implicit predecessor edge is not written to `dependency`
- [ ] Condensation and ordering are exposed as pure functions over a candidate set, usable without a database — required for unit testing without a fixture DB

**Verification:**
- [ ] `make test-db`
- [ ] Tests: partial commit, blocked dependent, blocked predecessor, atomic cycle
- [ ] Test: batch over `limits.batch_candidates` refused synchronously

**Dependencies:** Checkpoint 4
**Files likely touched:** `TR/src/domain/admission/graph.rs`, `TR/src/domain/admission/worker.rs`, `TR/tests/partial_admission_test.rs`
**Scope:** M

---

### - [ ] T20: Deletion and Dry Run

**Description:** The short deletion protocol — positive `expected_resource_version`, family
and entity locks, recheck `ACTIVE` with no direct registered dependents, lifecycle to
`DELETED`, version increment, outcome — and Dry Run as a mode of both registration and
deletion, running every check in a rollback-only transaction.

**Acceptance criteria:**
- [ ] Deletion with a live direct registered dependent is refused, reporting a count without identities
- [ ] A transitive-only dependent does not block
- [ ] A deleted entity is still exact-readable as deleted, and absent from lists
- [ ] Dry Run commits nothing, moves no `resource_version`, and its mode is part of the fingerprint
- [ ] Dry-run `succeeded` omits `resource_version`; dry-run `unchanged` reports the existing one

**Verification:**
- [ ] `make test-db`
- [ ] Tests: blocked deletion, transitive non-blocking, tombstone readability, dry run for both kinds
- [ ] Test: reusing one key for dry run then commit is a fingerprint mismatch, not a replay

**Dependencies:** T19
**Files likely touched:** `TR/src/domain/admission/deletion.rs`, `TR/src/domain/admission/worker.rs`, `TR/src/api/rest/routes.rs`, `TR/tests/deletion_test.rs`
**Scope:** M

---

### Checkpoint 5
- [ ] Partial admission, atomic cycles, deletion safety and Dry Run all behave
- [ ] `make test-db` green
- [ ] Human review

---

## Phase 6 — Dispatch and the new contract

### - [ ] T21: Outbox dispatch wiring

**Description:** Wire the `toolkit-db` leased outbox with prefix `types_registry_outbox` as
a thin `LeasedMessageHandler` shell over the worker function, mapping its result to
`Ok`/`Retry`/`Reject`. Messages carry only the operation UUID.

**Acceptance criteria:**
- [ ] Handler contains no admission logic — it resolves the operation UUID and calls the worker
- [ ] Delivery is at-least-once and commits are idempotent; duplicate delivery is a no-op
- [ ] Transient database failure returns `Retry`; `Reject` only for a permanently invalid message
- [ ] Candidate content never enters an outbox or dead-letter payload
- [ ] **Worker is started at the end of types-registry's `init()`**, not in the stateful `start` (plan decision P3), wired to `ctx.cancellation_token()`; the `OutboxHandle` is retained and `stop()`ed on shutdown
- [ ] Started **after** inline seeding, so seed operations — which are never enqueued — cannot be leased concurrently
- [ ] An operation submitted from any consumer's `init()` is admitted without that consumer waiting for the `start` phase

**Verification:**
- [ ] `make test-db`
- [ ] Test: duplicate delivery of one operation UUID changes nothing
- [ ] Test: an operation submitted immediately after `init()` returns reaches `completed` without the `start` phase running
- [ ] Test: shutdown drains or cancels cleanly — no task leak after `stop()`
- [ ] Manual: submit over REST against `make example` and observe the operation reach `completed` without direct worker invocation

**Dependencies:** Checkpoint 5
**Files likely touched:** `TR/src/infra/outbox.rs`, `TR/src/gear.rs`, `TR/Cargo.toml`, `TR/tests/outbox_test.rs`
**Scope:** M

---

### - [ ] T22: `toolkit-gts` — `owning_gear` on inventory records

**Description:** Add `owning_gear` to `InventoryTypeSchema` and `InventoryInstance`, derived
at macro-expansion time from the declaring crate's gear name, so the SDK can filter the
process-wide inventory down to what each gear owns (plan decision P4). Without this field
there is no way for a gear to know which inventory records are its own.

**Acceptance criteria:**
- [ ] Both record structs carry `owning_gear: &'static str`
- [ ] `#[gts_type_schema]` and `gts_instance!` populate it without the declaring crate passing anything explicitly
- [ ] `toolkit-gts` exposes a filter — records for one `owning_gear` — beside the existing aggregators
- [ ] Existing aggregators keep working; nothing that reads all records breaks
- [ ] Generated schema documents are byte-identical to before (this change adds metadata, not schema content)

**Verification:**
- [ ] `cargo test -p cf-gears-toolkit-gts`
- [ ] `cargo test --workspace` — every declaring crate still compiles
- [ ] Test: the filter returns exactly one gear's records for a fixture with two declaring crates
- [ ] Manual: diff generated schema documents against the T1 baseline — no change

**Dependencies:** T1 (may run parallel with T21)
**Files likely touched:**
- `libs/toolkit-gts/src/lib.rs`
- `libs/toolkit-gts-macros/src/lib.rs`
- `libs/toolkit-gts/tests/`
**Scope:** M — a library and macro change; blast radius is every declaring crate

---

### - [ ] T23: New SDK trait and the reconciliation helper

**Description:** `TypesRegistryEntities` plus its models per SPEC §10.1, and the
reconciliation workflow of DESIGN §3.3 as an SDK helper so no gear hand-rolls batching,
idempotency or retry (plan decision P4). The old trait is **not** kept — it is deleted in
T26 once every consumer has moved.

**Acceptance criteria:**
- [ ] Trait is object-safe: `hub.get::<dyn TypesRegistryEntities>()` compiles
- [ ] Models are field-for-field the SPEC §10.1 shapes, with out-of-scope fields absent rather than renamed
- [ ] No serde, no utoipa, no HTTP types in the SDK crate
- [ ] Models stay **gRPC-expressible** for future out-of-process use (plan decision P5): flat structures, no `Arc`-linked object graphs, no trait objects. The old trait's `Arc<GtsTypeSchema>` parent chain is exactly what must not be reproduced
- [ ] No security-context parameter; a doc comment records that planes will add one as a deliberate breaking change, and that out-of-process use requires it
- [ ] **Convenience read helpers as provided methods** over the two required primitives, so consumers keep familiar call shapes and the trait stays object-safe (DESIGN: *"single reads and the kind-narrowed `get_type_schema` / `get_instance` are provided methods over it"*): `get_type_schema`, `get_instance`, `get_type_schemas`, `get_instances`, the `_by_uuid` variants, `list_type_schemas`, `list_instances`. Kind narrowing costs no round trip — the kind is the trailing `~` of the identifier, so a kind-mismatched argument fails locally
- [ ] `EntitySnapshot` carries small accessors for the materialized groups (`effective_traits`, `effective_schema`, `authored_value`, `segments`) so the ~40 call sites using the old models' computed methods become field reads rather than rewrites
- [ ] **No `effective_*` recomputation exists in the SDK** — the old `GtsTypeSchema::effective_schema` / `effective_properties` / `effective_required` / `effective_traits` / `effective_traits_schema` are not reproduced. They resolved only the parent `$ref` and left non-parent references unresolved, and `effective_traits` was an admitted approximation (`TODO(#1723)`), so reproducing them would reintroduce both a wrong answer and a `constraint-gts-implementation` violation (SPEC §10.1)
- [ ] `EntityQuery` carries `limit` and `cursor`, and `EntityPage` carries the next cursor — the trait already declared `EntityPage` in SPEC §10.1, and without these it is a page in name only (D12)
- [ ] `list_instances` / `list_type_schemas` **hydrate a content-free page through `batchGet`**, so the ~87 existing call sites keep reading payloads from the result. The doc comment states the trade: complete with respect to the traversal, not to an instant, and one extra round trip per page which the client cache absorbs
- [ ] **The validator field is in the models from this task**, and `BatchGet` accepts a validator per requested key, even though T29 computes them and T30 consumes them. Adding either later would break the SDK contract after ~50 call sites have moved onto it (SPEC §8.5, `plan.md` P9). A result variant for `unchanged` is part of the same shape
- [ ] **Reconciliation helper** implements DESIGN §3.3's five steps: batch-read the desired identifiers, omit content equal to current, set `expected_resource_version` from the read for differing ones and leave it unset for missing ones, return `UpToDate` with no POST when nothing remains, otherwise submit once under one idempotency key and poll to terminality
- [ ] The helper filters the process inventory by `owning_gear` (T22) and batches within `limits.batch_candidates`
- [ ] **Retry lives here, not in gears:** a candidate failing because a dependency is not yet registered is retried a bounded number of times; failure names the gear and identifier
- [ ] One generated idempotency key spans an invocation's retries and polling
- [ ] Callable from a consumer's `init()` (P3). Its doc comment states the one requirement: declare `deps = [types_registry]`, or `init` ordering is not guaranteed

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry-sdk`
- [ ] Test: a mock consumer round-trips submit → poll → read through the new trait
- [ ] Test: helper returns `UpToDate` without submitting when everything already matches
- [ ] Test: helper converges when a base type is admitted only on the second attempt
- [ ] Test: helper against an operation nothing will drain fails on its deadline with a diagnosable error, not a hang
- [ ] `make dylint`

**Dependencies:** T22 (contract shape fixed by SPEC; may start at Checkpoint 4)
**Files likely touched:**
- `TR-SDK/src/entities.rs`
- `TR-SDK/src/entity_models.rs`
- `TR-SDK/src/reconcile.rs`
- `TR-SDK/src/lib.rs`
- `TR/src/domain/local_client.rs`
**Scope:** M

---

### Checkpoint 6
- [ ] An operation submitted through the outbox reaches `completed` with no direct worker call
- [ ] Inventory records carry `owning_gear`; generated schema documents unchanged
- [ ] New trait and reconciliation helper pass against a mock consumer
- [ ] Nothing is cut over yet — consumers still on the old path
- [ ] Human review

---

## Phase 7 — Cutover and migration

### - [ ] T24: Cutover — registry seeds only what it owns; ready mode and in-memory repository out

**Description:** types-registry stops pulling the whole process inventory. It seeds only what
it owns — `toolkit-gts` base types and its own control-plane types — inline at `init()` (P2),
then starts the outbox worker (P3). Delete `switch_to_ready`, the `temporary`/`persistent`
split, `SystemCapability::post_init` and the in-memory repository. From here on reads are
served from the database (SPEC D2, §8.2) — this is the task where the old in-memory read path
is switched off, so it is where that must be true. The `local_client` cache is **not** deleted
(SPEC §8.3, `plan.md` P7): its old model-typed implementation goes when the old models go, and
T30 lands its replacement. Between here and T30 reads are uncached.

**Acceptance criteria:**
- [ ] Seeding covers exactly the entities types-registry owns; no other gear's declarations are pulled
- [ ] Seeding is idempotent — a second start admits nothing new and reports `unchanged`
- [ ] Seeding runs **before** the outbox worker starts (P3) and enqueues nothing — it invokes the worker inline
- [ ] `init()` never waits on a registrant and never blocks on the outbox (`constraint-boot-path`)
- [ ] Its own seed set fits one batch; if it ever exceeds `limits.batch_candidates`, startup fails loudly rather than silently splitting
- [ ] Ready mode and the in-memory repository are gone; `ready_mode_tests.rs` deleted. The old model-typed cache goes with the old models, and the four `local_client.cache.{type_schemas,instances}.{capacity,ttl}` keys become accepted-and-ignored with a warning naming their T30 replacements
- [ ] `owning_gear` comes from T22's inventory field, not a constant — ceiling C3 is struck from SPEC §9 in this task
- [ ] No entity-derived state survives `init()` — no `ArcSwap`, no entity map, no `GtsOps` field on the gear or the service. Grep-checkable, and the ceilings C1/C4 struck by D2 depend on it

**Verification:**
- [ ] `make test-db`
- [ ] Test: second `init()` against a populated database seeds nothing
- [ ] Test: the seed set contains no entity owned by another gear
- [ ] Test: a read issued after an entity is written directly to the database (not through the service) returns it — proving the read path holds no process-local copy. This is the single-process form of SPEC §13's two-pod criterion
- [ ] `make quickstart` — server boots with only registry-owned types present
- [ ] Manual: restart, confirm entities and artifacts byte-identical

**Dependencies:** T19, T21, T23
**Files likely touched:**
- `TR/src/gear.rs`
- `TR/src/domain/seeding.rs`
- `TR/src/domain/service.rs`
- `TR/src/infra/storage/in_memory_repo.rs` (deleted); `TR/src/infra/cache/` retyped in T30, not deleted
- `TR/tests/seeding_test.rs`, `TR/tests/ready_mode_tests.rs` (deleted)
**Scope:** M

---

### - [ ] T25: Migrate system gears and plugins onto the new trait

**Description:** Move every system gear and plugin off `TypesRegistryClient`: reads to the new
trait, and the ~13 explicit `register(...)` sites to the reconciliation helper called from
`init()`. Each gear gates its own readiness on its own registration — DESIGN's *"Each gear
gates only its own readiness"*.

Covers `account-management` (+ static-idp-plugin), `authn-resolver` (+ static and oidc
plugins), `authz-resolver` (+ static and tr plugins), `tenant-resolver` (+ static,
single-tenant and rg plugins), `resource-group`, `usage-collector` (+ plugins), `cluster`,
`credstore` (+ static plugin), `oagw`.

**Acceptance criteria:**
- [ ] No system gear or plugin references `TypesRegistryClient`
- [ ] Read sites move mechanically: T23's provided helpers keep the call shapes, so a read migration is a `use` change plus field reads where a computed method was used
- [ ] Every gear declaring GTS types calls the reconciliation helper once, in `init()`, and declares `deps = [types_registry]`
- [ ] `RegisterResult::ensure_all_ok` sites are replaced by the helper's terminal result — no site treats `pending` as success
- [ ] Registration failure fails that gear's startup naming the gear and identifier, and does not affect other gears

- [ ] Where a materialized `effective_*` field differs from what the deleted client-side method returned, the **materialized value is accepted** — the difference is the old approximation being wrong (unresolved non-parent `$ref`, trait-default order), and `gts-rust` is authoritative. A failing assertion is updated to the new value, never "fixed" back
**Verification:**
- [ ] `cargo test --workspace`
- [ ] `make quickstart` and `make example` — server boots, `/health` green, every migrated gear's types present
- [ ] Test per gear group: registration is idempotent across two starts
- [ ] `make dylint`

**Dependencies:** T24
**Files likely touched:** `gear.rs` and the types-registry call sites of each gear above
**Scope:** L — committed incrementally, one commit per gear; do not batch the whole set

---

### - [ ] T26: Migrate domain gears; delete the old trait

**Description:** The remaining consumers — `bss/ledger`, `bss/rate-provider` (its shared
`registration.rs` helper), `mini-chat` (+ static-audit and static-model-policy plugins),
`llm-gateway`, `model-registry` — then delete `TypesRegistryClient`, its models and
`testing::MockTypesRegistryClient`.

**Acceptance criteria:**
- [ ] No crate in the workspace references `TypesRegistryClient`, `RegisterResult`, `RegisterSummary`, `TypeSchemaQuery` or `InstanceQuery`
- [ ] `rate-provider-sdk`'s shared `register_rate_provider_plugin` uses the new trait, so every rate-provider plugin migrates with it
- [ ] Old trait, its models and its test mock are deleted from the SDK crate
- [ ] `types-registry-sdk` exports only the new surface
- [ ] No consumer recomputes effective artifacts locally — every site reads the materialized group (D3)

- [ ] Where a materialized `effective_*` field differs from what the deleted client-side method returned, the **materialized value is accepted** — the difference is the old approximation being wrong (unresolved non-parent `$ref`, trait-default order), and `gts-rust` is authoritative. A failing assertion is updated to the new value, never "fixed" back
**Verification:**
- [ ] `cargo test --workspace`
- [ ] `grep -r TypesRegistryClient` finds nothing outside history
- [ ] `make ci`, `make dylint`
- [ ] `make example` — boots with every gear's types present

**Dependencies:** T25
**Files likely touched:** the domain-gear call sites above, `TR-SDK/src/api.rs` (deleted), `TR-SDK/src/models.rs`, `TR-SDK/src/testing.rs`
**Scope:** L — split per gear, same rule as T25

---
### - [ ] T27: REST completion, OpenAPI, QUICKSTART

**Description:** The remaining routes — `POST /entities:batchGet`, `POST /entities:delete`,
`GET /entities` — plus OpenAPI completeness, the changelog entry for the `POST /entities`
break, and `QUICKSTART.md`.

**Acceptance criteria:**
- [ ] `batchGet` returns one explicit result per requested key, including absence; duplicate keys collapse
- [ ] `GET /entities` excludes deleted entities and sorts by canonical identifier
- [ ] `GET /entities` returns **one bounded page and a cursor** (D12): `limit` defaults to `limits.page_size_default` (100) and a request above `limits.page_size_max` (1000) is **refused, not clamped**; cursors come from `toolkit-odata` and an unknown cursor version is rejected rather than reinterpreted
- [ ] The page is **content-free**: the default field set is identity and metadata; `content`, `resolved_schema` and `effective_traits` are absent, and a page carries no validator (§8.5)
- [ ] One default set **per surface**: the page is content-free, while `GET /entities/{key}` and `batchGet` return the full representation with D3's artifacts
- [ ] A request carrying **`$select` is refused** with an RFC-9457 problem naming the parameter — never answered with the default representation (§10.2). Accept-and-ignore is wrong here: the caller would get up to 1MB it did not ask for and would build on behaviour P1 changes
- [ ] Changelog records the `GET /entities` shape change beside the `POST /entities` one — two breaks, same release
- [ ] Both read routes go through T4's database read primitives; pattern filtering is `GtsIdPattern::matches` in Rust over prefiltered rows, never SQL that reimplements identifier matching
- [ ] All six routes appear in the OpenAPI document with RFC-9457 error responses registered
- [ ] `QUICKSTART.md` exists per `02_gear_layout_and_sdk_pattern.md` — description, features, link to `/docs`, one or two working `curl` examples
- [ ] OpenAPI and `QUICKSTART.md` describe this as the platform-plane API for global entities, and say plainly that platform identity (`X-ToolKit-Internal-Token` / `PlatformIdentity`) and a separate platform listener are not yet enforced (C8) — a reader must not conclude the plane is authenticated as such
- [ ] Changelog records the `POST /entities` break
- [ ] No handler added in this task carries logic the domain service does not already expose — the REST surface stays a mapping layer, so a later gRPC surface cannot diverge from it (SPEC §8.4)

**Verification:**
- [ ] `make e2e-local` — register → poll → read → re-register unchanged → delete, plus replay and `409`
- [ ] `make ci`, `make dylint`, `make lychee`
- [ ] Manual: `/cf/docs` renders every operation

**Dependencies:** T21, T23
**Files likely touched:**
- `TR/src/api/rest/routes.rs`
- `TR/src/api/rest/handlers.rs`
- `TR/src/api/rest/dto.rs`
- `gears/system/types-registry/QUICKSTART.md`
- `testing/e2e/test_types_registry.py`
**Scope:** M

---

---

### - [ ] T28: Update e2e suites for the `202` contract

**Description:** The `POST /entities` break (D10) invalidates every e2e call site that
registers and reads the result synchronously. Those sites move to submit-then-poll: `202`,
then `GET /operations/{id}` until terminal, then assert on the per-candidate outcome.

Surface: `testing/e2e/gears/types_registry/` — six test files, ~95 references to
`/types-registry/v1/entities` — plus `testing/e2e/gears/account_management/conftest.py`,
whose registration helper is setup for another gear's suite.

**Acceptance criteria:**
- [ ] A shared polling helper lives in `testing/e2e/gears/types_registry/helpers.py` and is reused; no test open-codes a poll loop
- [ ] The helper has a bounded deadline and fails with the operation's per-candidate errors, never on a bare timeout
- [ ] `account_management`'s registration helper polls to terminality before returning, so that suite's setup stays synchronous from its own point of view
- [ ] Assertions move from the POST body to the operation's per-`gts_id` outcomes
- [ ] `GET /entities` call sites move to the paged, content-free shape (D12): the shared helper pages through the cursor, and any assertion that read `content` from a list result now reads it from `batchGet` or an exact read
- [ ] Tests that assert refusals still assert them **synchronously** — envelope, identifier, policy and idempotency failures stay pre-`202` (SPEC §8.1)

**Verification:**
- [ ] `make e2e-local` — full suite green, including `account_management` and `types_registry`
- [ ] `make e2e-docker`
- [ ] Manual: confirm a rejected candidate surfaces its reason through the polled operation, not as an opaque failure

**Dependencies:** T27
**Files likely touched:**
- `testing/e2e/gears/types_registry/helpers.py`
- `testing/e2e/gears/types_registry/test_types_registry_{register,get,list,validation,error_handling}.py`
- `testing/e2e/gears/types_registry/test_registration_debug_logging.py`
- `testing/e2e/gears/account_management/conftest.py`
**Scope:** L — split per test file; the `account_management` change is its own commit because it lands in another gear's suite

---

### - [ ] T29: Freshness validators and conditional reads

**Description:** The per-request validator of DESIGN §3.3 and the conditional reads it
enables (SPEC §8.5, `plan.md` P9). For a P0 managed platform-plane read the validator is a
versioned digest over `entity.resource_version`, `type_schema.resolution_fingerprint` (Type
Schemas only) and a default-projection marker — every other input in DESIGN's table is
`tenant plane only`, availability-conditional or external, so none of them applies here.
Ships the `ETag` / `If-None-Match` → `304` path on exact reads and per-key validators on
`batchGet`.

**Acceptance criteria:**
- [ ] Validator is **computed per request, never stored** (`cpt-cf-types-registry-principle-derive-not-store`) — no column, no cache entry holds one as authority
- [ ] Inputs are exactly `resource_version` + `resolution_fingerprint` (Type Schemas; Instances have no derived form) + normalized-projection marker. A `TODO` names the P1 additions: subject visibility-chain version, Context Tenant availability-chain version, routing generation
- [ ] Wire form per DESIGN: base64url of a **versioned** JSON object, identical bytes in `ETag` and in batch bodies; 128-bit digest for the managed case. The version field is what lets P1 add inputs without honouring a P0 token
- [ ] Comparison decodes fields — never compares encoded strings, so serialization differences cannot read as a change
- [ ] Projection is digested as the **normalized field set**, not the query string; absent `$select` equals the explicit default set (RFC 9110 §8.8.3), so a P1 narrow token cannot produce a false `unchanged` for a wider representation
- [ ] Exact read: response carries `ETag`; a matching `If-None-Match` returns a bodyless `304` declared through `no_content_response(StatusCode::NOT_MODIFIED, ..)`
- [ ] `batchGet`: validators travel **beside individual keys** because one header cannot represent them; a result may be `unchanged`, and the response stays `200` even when all are
- [ ] **Discovery pages carry no validator** and are never conditional (DESIGN: validators are for exact reads, *"never discovery pages"*) — `GET /entities` is unaffected
- [ ] A deleted entity still has a validator, and deletion moves it (deletion increments `resource_version`)
- [ ] Handlers stay mapping-only: the validator is computed in the domain service, so a future gRPC adapter gets it without new domain methods (SPEC §8.4)
- [ ] `ponytail:`-style comment where the digest is built records ceiling C7 — the fixed projection marker and absent chain versions — and names the version field as the upgrade path

**Verification:**
- [ ] `make test-db`
- [ ] Test: unchanged entity yields a byte-identical validator across two reads; a revision changes it
- [ ] Test: a dependent whose `resolved_schema` was refreshed gets a new validator **even when its own `resource_version` did not move** — this is why `resolution_fingerprint` is an input, and it is the case a `resource_version`-only digest gets wrong
- [ ] Test: `If-None-Match` with the current validator returns `304` with no body; with a stale one returns `200` and the document
- [ ] Test: `batchGet` with a mix of current and stale validators returns `200`, `unchanged` for the current ones, full snapshots for the rest
- [ ] Test: an Instance validator omits `resolution_fingerprint` and still changes on revision
- [ ] Test: decoding rejects a validator whose version field is unknown rather than treating it as a match
- [ ] Test: deletion changes the validator
- [ ] `make dylint` — DE0201, DE0801 clean

**Dependencies:** T23 (validator field in the models), T27 (routes exist)
**Files likely touched:**
- `TR/src/domain/validator.rs`
- `TR/src/domain/service.rs`
- `TR/src/api/rest/handlers.rs`, `TR/src/api/rest/routes.rs`
- `TR-SDK/src/entity_models.rs`
- `TR/tests/validator_test.rs`
**Scope:** M

---

### - [ ] T30: SDK client cache — freshness window, byte bound, `fresh` bypass

**Description:** Port the `local_client` cache onto `EntitySnapshot` and give it DESIGN §3.3's
contract (SPEC §8.3, `plan.md` P7). Reads have been uncached since T24; this restores caching,
and because T29 supplies validators the cache **revalidates** rather than merely expiring —
which is what `cpt-cf-types-registry-fr-client-cache` asks for. Deferred to P1 with tenancy:
only the projection / visibility / Context-Tenant key dimensions.

**Acceptance criteria:**
- [ ] Cache is typed on `EntitySnapshot`; the `GtsTypeSchema` / `GtsInstance` implementation is gone with the old models
- [ ] Bound is **bytes** (`store_bound`, default 64MB), LRU-evicted — not an entry count. §3.2 caps one resolved document at 1MB, so the old `capacity: 1024` permitted ~1GB
- [ ] Freshness window (`freshness_window`, default 30s per DESIGN); `0s` is meaningful and disables the window rather than being rejected
- [ ] `fresh` on a read bypasses the window for that call and revalidates unconditionally against the entry's validator
- [ ] **Expiry revalidates, it does not drop:** expired keys and their validators go out in **one** batched conditional `batchGet` — DESIGN's batch poll scheduling — and an `unchanged` result refreshes the confirmation instant while keeping the snapshot. Demand-driven, no timer
- [ ] A failed revalidation **propagates the error and never extends the window** (`principle-fail-closed`); an entry is never served while its revalidation is in flight past the window
- [ ] A terminal successful registration or deletion outcome invalidates every local entry for each returned identifier/UUID pair, under **both** key forms. A `202` acceptance invalidates nothing — the client has not observed the mutation yet
- [ ] Entries are indexed by identifier and by UUID, so either resolution direction hits one snapshot
- [ ] Never cached, each asserted separately: `NotFound`, a failed read, a discovery page or its items, an operation resource
- [ ] The key carries visibility context and projection as fixed P0 markers, so P1 adds dimensions without reshaping it
- [ ] The four old config keys are accepted-and-ignored with a warning naming `freshness_window` / `store_bound`; `ponytail:`-style comment records what is left of ceiling C7 (the key dimensions, not revalidation)

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry`
- [ ] Test: read inside the window after a direct database change returns the cached value; the same read with `fresh` returns the new one
- [ ] Test: `0s` window never serves a cached entry
- [ ] Test: terminal outcome invalidates under identifier **and** UUID; a bare `202` invalidates nothing
- [ ] Test: byte bound evicts on one 1MB document where a thousand small entries do not
- [ ] Test: each of the four never-cached cases
- [ ] Test: a failed read leaves no entry and does not extend an existing window
- [ ] Test: an expired entry whose content did not change is revalidated to `unchanged` and **kept**, not refetched — assert on the absence of a full-snapshot response, not only on the returned value
- [ ] Test: two expired keys produce **one** conditional `batchGet`, not two
- [ ] Test: a failed revalidation surfaces the error and leaves the window unextended
- [ ] `make e2e-local` — no staleness surfaces in the register → poll → read flow

**Dependencies:** T24, T26, T29
**Files likely touched:**
- `TR/src/infra/cache/cache.rs`, `TR/src/infra/cache/cache_tests.rs`
- `TR/src/domain/local_client.rs`
- `TR/src/config.rs`
- `TR/tests/client_cache_test.rs`
**Scope:** M

---

### Checkpoint 7 — ready for review
- [ ] The cutover holds: the real inventory seeds through the new path, every existing consumer works unchanged, the platform boots and stays healthy
- [ ] All 15 success criteria of SPEC §16 met
- [ ] `make ci`, `make test-db` (three backends), `make e2e-local`, `make e2e-docker`, `make dylint`, `make lychee` green
- [ ] Every ceiling in SPEC §9 has a comment at the point it binds
- [x] O1 resolved: `gts` 0.12.0 published and `[patch.crates-io]` removed — **blocks merge** (closed 2026-08-18, see T1 report §6)
- [ ] `TypesRegistryClient` is deleted and no crate references it (D6, T26)
- [ ] Conditional reads work end to end: an exact read carries a validator and honours `If-None-Match` with `304`, `batchGet` reports `unchanged` per key (T29)
- [ ] Discovery is bounded: no response is unbounded in items or bytes, and a cursor traverses the whole set exactly once (T27, D12)
- [ ] The client cache is in place on the new models with its window, byte bound, `fresh` bypass and batched conditional revalidation (T30) — P0 does not ship an uncached read path
- [ ] Human review
