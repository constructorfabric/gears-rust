# Types Registry P0 — Task List

Plan: [`plan.md`](./plan.md) · Spec: [`SPEC.md`](./SPEC.md)

**Execution is sequential, in numeric order:**

- **Phase 7 — both clients, real local AM, every gear on the persistent registry:** T31 → T38. Contracts/adapters are complete (T29 + T30); T31 makes them usable through ClientHub after database seeding, with a new gear publishing in `init()`. T32 then migrates the complete local AM/IdP/TR group and proves it through existing AM REST requests, with only the new registry client inside AM. Then both APIs over REST, the platform cache and generic lifecycle are ready (Checkpoint 7A, local application/SDK-contract handoff); then one atomic remaining-fleet/REST cutover (Checkpoint 7). T33 is implemented on its own toolkit branch and must reach `main` before T34's route move.
- **Phase 8 — publication after wiring and out-of-process operation:** T39 → T40 → T41 → T42 → T43. Collectors, ownership and selection; Account Management after wiring and remotely (Checkpoint 8A); the remaining fleet and the end of the pull; the out-of-process run (Checkpoint 8).
- **Phase 9 — mixed-version rollout (publisher-version guard), last:** T44 → T45. Final P0 deployment is gated by Checkpoint 9; Checkpoint 7A is a development/integration handoff.

Standing bar for every task, on top of its own acceptance criteria: `make fmt`, `make clippy` and gear tests green, no regression in other gears, behaviour verified at runtime, docs updated. **The full `make ci` is a checkpoint gate, not a per-task one** — it ends in `dylint` and pulls in four container-backed targets, which is P13's whole point. Naming `make ci` per task is what made T1–T9 record it as *partial* every time. Checkpoints 7A, 7, 8A, 8 and 9 retain full CI/backend gates. The real AM remote target joins CI at T41; Checkpoint 7A claims local/SDK-contract coverage only (P31). Tasks are feature-sized like Phases 1–6: each lists the commits it lands as, and each commit clears the standing bar. Code organisation follows `docs/toolkit_unified_system/` — **not** `guidelines/DNA/languages/RUST.md`.

`TR/` abbreviates `gears/system/types-registry/types-registry/`, `TR-SDK/` abbreviates
`gears/system/types-registry/types-registry-sdk/`. Other paths are from the repository root.

**No new ADRs.** The no-PDP deviation is recorded in SPEC §9 (ceiling C6) and §12; the two wire breaks in SPEC §10.2 — `POST /entities` (D10) and the paged content-free `GET /entities` (D12). P23's decisions are SPEC D18–D22 and ceilings C11–C13; P26 changes scheduling, granularity and numbering, and amends D11's dependent configured entities (T40). P28 moves seeding/local publication to T31; P29 adds the full local AM/IdP/TR group in T32. Both permit bounded coexistence until the remaining T38 cutover. P30 merges five later task IDs; P31 limits Checkpoint 7A to local AM and SDK contracts and assigns the full process proof to real AM in T41. Open question O5 and the P1-deferred O6 are in SPEC §17.

---

## Commands

Every task below that says **gear tests** means both halves:

```bash
# SQLite — the default backend; every unit and integration test runs here
cargo nextest run -p cf-gears-types-registry

# PostgreSQL + MySQL, one testcontainers container per test (Docker required)
make test-integration LANE=types-registry-db
```

**Per-task versus checkpoint.** Per task: `make fmt`, `make clippy`, gear tests. At the checkpoint: the full `make ci`, which adds `dylint` (whole-workspace build — P13), `deny`, `lychee`, `gts-docs` and the container targets.

---

## Phase 0 — Upgrade

### - [x] T1: Upgrade to `gts-rust` 0.12.0 via `[patch.crates-io]`

**Description:** Point the workspace at the local `gts-rust` checkout so the tri-state
compatibility verdict, `ContentModel` classification and `compare_documents` become
available, then prove the corrected semantics break nothing already declared. This task's
failure mode is other gears, which is why it runs first and alone.

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] `gts`, `gts-id` and `gts-macros` are all three at **`0.12.0`** from crates.io — they move together, and `gts-dylint` / `gts-macros-cli` must not lag (SPEC §7, D8)
- [x] Every declared GTS identifier still admits; any that does not is fixed or explicitly waived in writing. **The figure "202" is not reproducible** and is replaced by three measured populations: 118/118 entities admitted at runtime under the e2e feature set (34 Type Schemas + 84 Instances), 797 doc/JSON files validated by the 0.12.0 validator, and every macro literal compiled by the workspace test run
- [x] Every difference in `#[gts_type_schema]`-generated schema documents versus 0.11.0 is enumerated and accounted for — none silent. 9 of 118 documents differ, all by one change (a doc-commented GTS-identifier field's subschema preserved under `allOf` instead of having its `description` overwritten); shown semantically inert, including entity references (`x-gts-type-ref` / `x-gts-instance-ref`), which `XGtsRefValidator` still enforces by recursing through `allOf`

**Verification:**
- [x] `make gts-docs` — 797 files, 0 errors
- [x] `cargo test --workspace` — 10213 passed, 368 skipped, 0 failures; baseline was 10206 passed + 368 skipped, and the difference is exactly the 6 new capability tests
- [x] Manual: generated schema documents captured before and after by booting the example server and dumping `GET /cf/types-registry/v1/entities`, then diffed field by field

**Added:** `TR/tests/gts_012_semantics_tests.rs`, 6 tests pinning the tri-state verdict,
`ContentModel` including `Partial`, `GtsStore::compare_documents` and the provenance
versions. Under 0.11.0 the file does not compile — none of those symbols exists in the
published crate — so it is a genuine RED→GREEN.

**Dependencies:** None
**Files likely touched:** `Cargo.toml`, plus test fixtures the corrected semantics reject
**Scope:** M (mechanical change, large verification surface)

---

### Checkpoint 0
- [x] `make ci` green — **partial, see T1**: everything that does not need Docker is green
- [x] `make dylint` — full workspace, once for the phase (P13). Satisfied by the workspace-wide run recorded at Checkpoint 1: Phase 0 is one task and that run included its changes
- [x] Every declared GTS identifier admits under 0.12.0
- [x] Generated-schema diff reviewed and accounted for — one class of change in 9 of 118 documents
- [x] **Human review before any registry code is written**

---

## Phase 1 — One global entity of each kind, persisted, async, end to end

Exercised by fixtures and REST only. Consumers stay on the in-memory path until T38, so nothing in this phase can regress another gear.

### - [x] T2: Migration for the 9 tables

**Description:** One initial SeaORM migration creating the P0 subset of `database.sql`:
`version_family`, `entity`, `type_schema_revision`, `instance_revision`, `type_schema`,
`instance`, `dependency`, `operation`, `operation_item`. Tenant columns and their CHECK
constraints are created exactly as specified and never populated with tenant scope, so P1
tenancy needs no migration. The database scope, stated once: `coordination_state` exists
(the second migration), only `entity_write_order` is seeded and used in P0,
`source_claim` is not created, the `routing` state row arrives with federation, and a
standalone `routing_config` table is never created in any phase.

**Acceptance criteria:**
- [x] All 9 tables, their PKs, FKs, UNIQUE and CHECK constraints, and the 4 indexes from `database.sql` are created — `idx_tr_operation_status`, `idx_tr_entity_family`, `idx_tr_entity_visibility`, `idx_tr_dependency_to`. Conformance was **measured**, not argued: the Postgres list reproduced `database.sql`'s P0 constraint set 48 for 48 and all three dialects declared the same columns in the same order. That was a one-time measurement — the standing guard behind it was removed after Checkpoint 1
- [x] Identifier columns are `varchar(1024)` with binary collation and ASCII charset where the backend default is multi-byte — `family_key`, `entity.gts_id`, `operation_item.gts_id`: `varchar(1024) COLLATE "C"` on Postgres, `VARCHAR(1024) CHARACTER SET ascii COLLATE ascii_bin` on `MySQL`, `TEXT COLLATE BINARY` on `SQLite` (its default, stated so a later `COLLATE NOCASE` cannot creep in)
- [x] Enumerations stored as smallint with CHECKs enumerating allowed values. Two forms, both present and both tested: an explicit `IN` list for `kind`, `status`, `entity_kind` and `dependency.kind`; and the branch CHECK for `ownership_scope`, `plane` and `lifecycle_status`, where no branch matches a third value — `ck_tr_version_family_owner`, `ck_tr_entity_owner`, `ck_tr_operation_plane` and `ck_tr_entity_lifecycle` already close those domains, so adding an `IN` list would have been a constraint `database.sql` does not have
- [x] `DatabaseCapability::migrations()` returns the Migrator; outbox tables come from `outbox_migrations_with_prefix("types_registry__outbox")`, not from this migration. Both halves are tested: one test asserts the initial migration alone creates **no** outbox table, another applies the gear capability's full set and asserts the 9 managed tables and the prefixed outbox tables all exist
- [x] Raw SQL appears only in migration infrastructure (`11_database_patterns.md` invariant) — the three statement lists plus the drop list live in `m20260817_000001_initial.rs`, and `m20260904_000002_coordination_state.rs` carries its own three lists plus drop list the same way; no gear code gained SQL

**Verification:**
- [x] Migration up and down on **SQLite** — `tests/migration_test.rs`, 41 tests, in-memory `SQLite` with `PRAGMA foreign_keys = ON`, including a full `up` → `down` → `up` roundtrip
- [x] Migration up and down on **PostgreSQL and MySQL**. `tests/migration_backends_test.rs` (behind `--features integration`, per the `account-management` precedent) brings up a Postgres and a `MySQL` 8.1 container, applies the migration, re-checks `ck_tr_entity_owner` / `ck_tr_operation_item_state` / FK `RESTRICT` against the real engines, and rolls back. Written here, first **run** at Checkpoint 1 once Docker was up — which is where it found the uuid-binding defect
- [x] Test: each CHECK constraint rejects the shape it names — all 19 CHECKs of the Postgres set, and the 3 boolean-domain CHECKs the `SQLite` / `MySQL` lowering adds, have a test. Two needed isolation work rather than a hedge: `ck_tr_operation_item_dry_run_bool` fires on a row that also breaks the composite FK, so its test runs against a database with `PRAGMA foreign_keys = OFF` and a control insert proving the switch-off took; `ck_tr_instance_revision_no` needs the whole entity → item → type-schema-revision chain seeded first. Three (`ck_tr_operation_item_kind`, `..._status`, and the third-plane / third-scope / third-kind cases) are shown to reject the *shape* rather than attributed to one named constraint, because a second CHECK matches the same row — stated in the test comments, not claimed away
- [x] Test: `ck_tr_operation_item_state` rejects a `succeeded` non-dry-run registration item with no `result_revision_no` — plus the positive case, the dry-run success that wrongly allocated a resource version, and `unchanged` on a first admission
- [x] Manual, at runtime: booted `cf-gears-example-server` with a temporary config binding a `SQLite` database to types-registry. Both migrations applied (`applied=2 skipped=0`) and the file holds the 9 managed tables, the 4 indexes and the 7 prefixed outbox tables. The boot then fails in `oagw` post-init (`Failed to resolve root tenant: no plugin available`) — reproduced identically with the **pristine** `config/quickstart.yaml`, so it pre-dates this task

**Dependencies:** T1
**Files touched:**
- `TR/src/infra/storage/migrations/mod.rs` — NEW, Migrator
- `TR/src/infra/storage/migrations/m20260817_000001_initial.rs` — NEW, three statement lists + drop list
- `TR/src/infra/storage/migrations/m20260817_000001_initial_tests.rs` — NEW, 12 in-source tests
- `TR/tests/migration_test.rs` — NEW, 41 `SQLite` schema tests
- `TR/tests/migration_backends_test.rs` — NEW, 2 container-backed tests behind `integration`
- `TR/src/gear.rs` — capabilities `[system, db, rest]`, `DatabaseCapability`
- `TR/src/infra/storage/mod.rs`, `TR/src/infra/mod.rs`, `TR/src/lib.rs` — re-export `Migrator`
- `TR/Cargo.toml` — `sea-orm`, `sea-orm-migration`, `toolkit`, `toolkit-db` feature
  `sqlite`, `integration` feature, `testcontainers` dev-deps
**Scope:** M — one long DDL file; deliberately not split, because splitting it orders FKs across tasks

---

### - [x] T3: SeaORM entities for the core six

**Description:** Entity structs for `version_family`, `entity`, `type_schema_revision`,
`type_schema`, `operation`, `operation_item`, each with `#[derive(Scopable)]` and
`#[secure(unrestricted)]` plus a comment recording the P1 switch to
`tenant_col = "owner_tenant_id"`.

**Acceptance criteria:**
- [x] One file per entity under `TR/src/infra/storage/entity/` (`02_gear_layout_and_sdk_pattern.md`). `entity/entity.rs` is module inception and deliberately so — these files are a DDL mirror, so each is named after its table; renaming would put the mirror out of step with the schema it tracks. One targeted `#[allow(clippy::module_inception)]` on the `mod` declaration, with that reason
- [x] Every entity declares its security dimensions; none omits the attribute. The `Scopable` derive already refuses to compile without an explicit decision per dimension, so the criterion is met by construction — but *which* decision was made is what a future edit could change silently, so it is also a test: `every_core_entity_is_declared_unrestricted_while_ceiling_c6_stands` asserts `IS_UNRESTRICTED` and all four dimension columns `None` for all six
- [x] `ponytail:`-style comment on the `#[secure(unrestricted)]` attributes records ceiling C6 and its upgrade path. Three variants, because the tables differ in what they *could* be scoped by: `version_family` / `entity` own `owner_tenant_id` and name the switch to `tenant_col` directly; `operation` owns `tenant_id` and additionally records ceiling C8, since the plane is expressed by that column rather than enforced by the transport; `operation_item`, `type_schema_revision` and `type_schema` own **no** owner column at all — ownership is the parent entity's — so their note says `unrestricted` is the only honest marker today and leaves the copy-versus-scoped-read choice with the `PolicyEnforcer` work
- [x] Enumeration columns map to typed Rust enums with explicit smallint conversion, storage-only. Seven `DeriveActiveEnum` vocabularies over `rs_type = "i16", db_type = "SmallInteger"` with explicit `num_value`. **No `Serialize` / `Deserialize` / `ToSchema` is derived on any of them**, so the integers cannot reach the wire by accident — that is the mechanism behind "storage-only", not just a convention

**Verification:**
- [x] `cargo test -p cf-gears-types-registry` — 153 lib + 41 migration + 7 entity + the pre-existing suites, 0 failures
- [x] Test: enum ↔ smallint round-trip for every vocabulary, asserting the exact numbers in `database.sql` — 10 tests. Every case is written out literally rather than derived from variant order, because deriving it from the order would restate the bug it guards. One test pins the *count* per vocabulary, so a variant added without a case here fails; one asserts every out-of-vocabulary integer fails to parse, so a row written by a future version is a clean read error rather than a silent misinterpretation; and one exists purely to stop a future reader "unifying" `OperationStatus::Completed = 3` with `OperationItemStatus::Succeeded = 3`, which `database.sql` says is coincidence and MUST NOT become a contract
- [x] **Known open gap, by decision.** `every_core_entity_declares_exactly_the_columns_database_sql_defines` was written, mutation-checked by deleting `operation.request_fingerprint`, then removed after Checkpoint 1 along with the shared `database.sql` parser. The gap it closed is therefore open: the round-trip tests in `tests/entity_test.rs` prove the columns an entity *names* exist, and cannot notice one it **omits**, because `SeaORM` simply never selects it and the read succeeds. `every_core_entity_binds_to_its_table_in_the_migration` survives in `entity/columns_tests.rs` — it needs no DDL parser
- [x] Added: `tests/entity_test.rs`, 7 tests writing and reading every core entity against the real migrated schema. The timestamp round-trip is the specific risk worth covering — `timestamptz` lowers to `TEXT` on `SQLite`, so an `OffsetDateTime` through `SeaORM` is a real conversion — and two tests write shapes `ck_tr_entity_lifecycle` and `ck_tr_operation_item_state` constrain, so a successful write is itself evidence the mapping agrees with the DDL

**Three of the tables have no entity yet, deliberately.** `instance` and
`instance_revision` arrive with Registered Instances (T10), `dependency` with edge extraction
(T13). An entity with no reader is code the compiler cannot check against the DDL, which is
exactly the drift these mirrors exist to prevent.

**No relations are declared** on any entity. `entity.family_id`,
`type_schema_revision.operation_item_id` and the composite current-state pointers are real
foreign keys, but nothing joins across them yet — the T4 repositories read the family by key
under its own lock. An unused `has_many` would be code with no reader.

**Dependencies:** T2
**Files touched:**
- `TR/src/infra/storage/entity/mod.rs` — NEW
- `TR/src/infra/storage/entity/enums.rs` — NEW, 7 storage vocabularies
- `TR/src/infra/storage/entity/enums_tests.rs` — NEW, 10 tests
- `TR/src/infra/storage/entity/{version_family,entity,type_schema_revision,type_schema,operation,operation_item}.rs` — NEW
- `TR/src/infra/storage/entity/columns_tests.rs` — NEW, 2 conformance tests
- `TR/src/infra/storage/normative_schema.rs` — NEW, shared `database.sql` reader + 3 self-tests (**deleted after Checkpoint 1**)
- `TR/src/infra/storage/migrations/m20260817_000001_initial_tests.rs` — drift tests rewired onto the shared parser
- `TR/src/infra/storage/mod.rs` — declare `entity`, `normative_schema` (the latter since removed)
- `TR/tests/entity_test.rs` — NEW, 7 DB-backed tests
- `TR/Cargo.toml` — `toolkit-db-macros`, `time` (+ `macros` for `datetime!` in tests)
**Scope:** M — 7 files, each mechanical; grouped because they are one DDL mirror

---

### - [x] T4: Repositories on `DBRunner`

**Description:** Repository methods for the core six, taking `runner: &impl DBRunner` and
`scope: &AccessScope` so the same method works inside and outside a transaction. Keyed
reads, insert-if-absent for `version_family`, compare-and-swap on
`entity.resource_version`, and canonical-order locking helpers.

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] Every method takes `runner: &impl DBRunner`, never `&SecureConn` — and one test passes a transaction to the same methods, so a signature that quietly narrowed to `DbConn` would fail
- [x] No raw SQL; all queries go through the typed builder
- [x] Compare-and-swap on `resource_version` is a single statement whose affected-row count is the success signal. A stale precondition is `Ok(false)`, not an error — an ordinary concurrent-writer outcome the caller turns into `412`
- [x] Family create-then-read works on all three backends. **The "locked read" half of the original criterion is not achievable:** `DBRunner` hides the raw executor and the secure builder exposes no lock clause, so a repository cannot take `SELECT … FOR UPDATE`. `create_or_get` makes `uq_tr_version_family_key` the serialization point instead — the loser's conflict is **absorbed** (`ON CONFLICT DO NOTHING`), not raised, and then re-read. Serializing the *validation* window needs the toolkit advisory lock on the `Db` handle, which is service-layer (T12); `lock_order` is the ordering half. Now **run** on both container backends, inside a transaction as well as on a pooled connection — see the correction below
- [x] Read primitives for the database read path (SPEC D2, §8.2): a keyed exact read, and a list read decided entirely in SQL over stored columns (SPEC D14)
- [x] The list read is a **keyset page**: `gts_id > :after ORDER BY gts_id LIMIT :n + 1`, excluding deleted rows, so a page boundary cannot drift or duplicate (D12). It returns a cursor exactly when another row matches and never loads the whole match set
- [x] A dependency-closure read: given candidate identifiers, return them plus the transitive closure of what they consume, walking `dependency` edges (D5), `gts_id`-sorted. Candidates with no entity row are **reported** in `missing_roots` rather than failing the read, because a first admission's own candidate is exactly that case

**Verification:**
- [x] Gear tests (see [Commands](#commands)) — 161 lib + 149 integration tests, of which 18 are `repo_test.rs`
- [x] Test: concurrent `version_family` creation yields exactly one row — 8 tasks against a file-backed pool; `SQLITE_BUSY` is retried rather than pretended away, and the assertion is on the end state
- [x] Test: CAS with a stale version affects zero rows and is reported as such
- [x] Test: list read with a wildcard pattern returns exactly what `GtsId::matches_pattern` accepts, including siblings in the same identifier range. A chain segment is a full `vendor.package.namespace.type.vMAJOR`, and a bare segment is an implicit derived-type envelope (GTS spec §3.6), so `…v1~` and `…v1~*` accept the same set, base included
- [x] Test: keyset paging over a set larger than one page yields every row exactly once, and a row inserted mid-traversal neither duplicates an earlier row nor hides a later one
- [x] Test: closure read over a chain returns the whole chain and nothing outside it — plus termination on a row that contradicts acyclicity. The relation is a DAG (ADR-0012), so the `seen` set is what keeps the walk linear in entities rather than in converging paths; termination on a contradicting row is defence in depth, retitled at T14 when the invariant gained a test of its own
- [x] `cargo test --workspace` (excluding the two macro crates, as `make test-no-macros` does) — passes, so no regression in any other gear
- [x] PostgreSQL / MySQL repository primitives — **run, and they found two defects.** `tests/repo_backends_test.rs` covers the properties `SQLite` cannot demonstrate: the unique-conflict handling, the keyset cursor's binary collation, and now the same two races **inside a transaction**, which is the only shape production uses. Both container suites pass; see the correction below. Run: `cargo test -p cf-gears-types-registry --features integration --test repo_backends_test`

**Added beyond the acceptance criteria:**
- **Sparse matches cost no extra pages.** 2100 rows share the match's identifier prefix and sort ahead of it; the match still arrives on the first page, with no cursor after it
- **`replace_outgoing` treats its edge list as a set** — `(from, kind, to)` is the primary key, so a schema that `$ref`s one base twice would otherwise be a PK violation mid-admission. Mutation-checked
- **A second deletion is proved to be a no-op** — `mark_deleted` requires `Active`, so a repeated call reports failure and leaves `deleted_at` where it was; the read-back also pins the `LifecycleStatus` enum lowering through `Expr::value`, which no `ActiveModel` covers
- **Two pre-existing T3 clippy failures fixed** (`make clippy` runs `--all-features`, so both would have failed CI): `entity_test.rs`'s bare-connection reads, allowed at file scope with the reason that the file tests the entity rather than the scope; and an unbackticked `PostgreSQL` in `migration_backends_test.rs`'s module header

**A correction, found by running the container suites.** `create_or_get` caught the loser's
unique violation and re-read *through the same runner* — which in production is always `&DbTx`,
and the two backends disagree about what a raised violation does to one. **`PostgreSQL` aborts
it**, so the recovery could never have worked on the backend it was written for; the `SQLite` test
passed only because it uses a pooled connection. **`MySQL` hides the winner** — with the violation
absorbed instead, the loser's re-read returned nothing under `InnoDB`'s default `REPEATABLE READ`,
the row having been committed after its snapshot opened. Fixed at the layer that owns each:
`repo::conflict_do_nothing` absorbs the conflict so nothing is ever aborted, and
`ports::commit_write` asks the commit transaction for `READ COMMITTED` so a recheck sees what
another admission just committed — the mirror image of `snapshot_read`. `insert_entity` got the
same treatment and now returns `Option`, where `None` is a lost race the worker records as
`already_exists` instead of the `500` a raised violation produced. Each half is pinned by an
in-transaction backends test, and each was confirmed to fail with its fix reverted.

**Dependencies:** T3
**Files touched:**
- `TR/src/infra/storage/repo.rs` — NEW, three repositories (**split into `repo/` — one file per repository — after Checkpoint 1**, see the Phase 2 preamble)
- `TR/src/infra/storage/mod.rs` — `pub mod repo`
- `TR/tests/repo_test.rs` — NEW, 17 `SQLite` tests against the migrated schema
- `TR/tests/repo_backends_test.rs` — NEW, 2 container-backed tests behind `integration`
- `TR/tests/common/mod.rs` — `SQLite`/DSN provider harness, `allow_all()` scope
- `TR/tests/entity_test.rs`, `TR/tests/migration_backends_test.rs` — the two T3 clippy fixes
**Scope:** M

---

### - [x] T5: Transient `gts-rust` store built from database rows

**Description:** One function that builds a `GtsStore` from a set of database rows, for use by a single admission unit and dropped with it (SPEC D2, §8.2; P6). It takes the candidates plus the transitive closure of what they consume — obtained from the `dependency` table — reads them `gts_id`-sorted so a derived schema never loads before its base, and returns an owned store. Nothing is cached, published or shared: **no `ArcSwap`, no snapshot, no process-lifetime store.** Reads are served from the database, not from here. The old in-memory repository keeps serving the old trait until T38 and is not touched.

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] Signature takes a row set (or a closure query) and returns an owned `GtsStore`; it stores nothing in `self` and registers nothing globally. Two layers: `build_store(Vec<UnitDocument>)` is pure — no database, no clock, no global state — and `load_unit_store(runner, scope, candidates)` reads the closure and delegates to it. Both are free functions; there is no `self` to store anything in. The store is returned inside `UnitStore`, which owns it outright: no `Arc`, no lock, and no way to clone it out
- [x] Load order is `gts_id`-sorted, so a derived schema never loads before its base — **but the stated mechanism is wrong and is corrected here.** `register_schema` inserts into a map and validates nothing; it is `validate_schema` / `resolve_schema_refs` that walk `chain_ids()` and fail on an absent base. Registering a derived schema first therefore does **not** fail in `gts` 0.12.0. The sort is kept because it makes the store complete before anything is asked of it, which turns a latent ordering bug into an impossible one — not because registration needs it
- [x] The store is built from the unit's dependency closure, not from the whole `entity` table — a whole-table load must fail the closure test below. **Mutation-checked rather than argued:** substituting `EntityRepo::list_page(None, 1000)` for the closure read fails exactly two tests and leaves the other eight green
- [x] No lock of any kind: the store is owned by one caller, so `GtsOps` not being `Sync` is irrelevant rather than worked around. `GtsStore` is `Send + !Sync` because `GtsReader` has no `Sync` supertrait — the exact reason the old in-memory repository holds a `Mutex<GtsOps>`. Nothing here holds one, and the store still crosses an `.await`, which is all an admission unit needs
- [x] Closure lookup uses the `dependency` edges (D5), and a candidate with no dependencies yields a store containing only the candidate — the first-admission case, whose candidate is reported in `missing_candidates` (T4's `missing_roots`) rather than failing the read

**Verification:**
- [x] Gear tests (see [Commands](#commands)): 170 lib + 159 integration tests, of which 9 are the in-source builder tests and 10 are `gts_store_test.rs`
- [x] Test: store built from a chained fixture resolves the derived schema's base — a candidate whose document `$ref`s `gts://<base>`, with the base supplied by the closure; the resolved document carries no `$ref` and does carry the base's own property. Its `$id` stays a `gts://` URI, because `resolve_schema_refs` inlines references and does not rewrite identity
- [x] Test: closure containment — a document not reachable from the candidate's dependency closure is **absent** from the store. The stranger is committed, active and in the same family, so only reachability distinguishes it
- [x] Test: rows are consumed in `gts_id` order given deliberately shuffled input — asserted through `UnitStore::load_order()`, because a `HashMap` keeps no trace of insertion order and the criterion is otherwise untestable
- [x] Test: two sequential builds after a committed revision each observe the new revision, with no invalidation step between them — the second revision is committed directly, so the builder learns of it only by re-reading, and the old content is asserted absent as well as the new one present
- [x] Added: the candidate overlay beats the committed document under the same identifier (D19's in-batch rule, and what T11 needs); a tombstoned base still loads, because it stays the compatibility baseline until purge; a document-less entity and a stored non-JSON document are each named rather than surfacing later as `UnresolvedRefs`; and the same builder runs inside a transaction, so T8 can build its store inside the transaction it commits in
- [x] Added: a dialect-less document is refused by name, and an empty `$schema` separately. Without `$schema`, `register_schema` registers the document as an **Instance** — `GtsEntity::new` overwrites the `is_schema: true` it was passed — and every `$ref` at it then stays unresolved with no error anywhere
- [ ] PostgreSQL / MySQL `current_documents` — **written, not run: Docker daemon down.** One case in `tests/repo_backends_test.rs` for the two properties `SQLite` cannot show: the exact-pair disjunction binds two parameters per entity, and `raw_schema` is `text` / `LONGTEXT`, so a document past any `varchar` bound is evidence about the column type. Run: `cargo test -p cf-gears-types-registry --features integration --test repo_backends_test`

**Dependencies:** T4
**Files touched:**
- `TR/src/domain/gts_store.rs` — NEW, pure builder + DB loader + `UnitStore` / `UnitDocument` / `StoreBuildError`
- `TR/src/domain/gts_store_tests.rs` — NEW, 9 in-source tests, no database
- `TR/src/domain/mod.rs` — declare `gts_store`
- `TR/src/infra/storage/repo.rs` — NEW `TypeSchemaRepo::current_documents`, `CurrentDocument`, `PAIR_CHUNK`
- `TR/tests/gts_store_test.rs` — NEW, 10 `SQLite` tests
- `TR/tests/common/mod.rs` — shared managed-state fixtures (operation → item → revision → pointer)
- `TR/tests/repo_backends_test.rs` — one container-backed case for `current_documents`
**Scope:** S — as estimated for the builder; the content read is the unplanned half

---

### - [x] T6: Typed configuration

**Description:** Extend `TypesRegistryConfig` with `allow_compatibility_force`, `limits.*` (including P0's `activation_write_set`), `registration_policy` and `worker.*`, keeping the existing keys. The `local_client.cache.*` keys stay live — the cache is kept (SPEC §8.3) — and their reshaping into `freshness_window` / `store_bound` belongs to T36, not here.

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] Absent config and `config: {}` both yield the SPEC §10.3 defaults via `ctx.config_or_default()` — asserted as the *same value* rather than each checked against the table, plus a second test pinning every default against §10.3 value for value
- [x] `registration_policy` keys are validated at startup; an invalid GTS pattern fails startup rather than being skipped. `TypesRegistryConfig::validate()` returns the **compiled** `RegistrationPolicy` rather than `()`, so the boot path and the acceptance path consult one compilation instead of two that could disagree
- [x] `tenant_ownable` is **parsed and validated but inert** (SPEC §10.3). `RegistrationPolicy::tenant_ownable` resolves it by the same rules as the vendor set and is asserted to return the configured value — which is what makes it inert rather than dropped — while `admits` refuses any candidate asking for tenant ownership whatever the entry says. §9 makes that request shape unreachable, so it is a fail-closed assertion, not a feature
- [x] Per-parameter resolution implemented: longest literal prefix wins, an exact key beats any pattern, entries omitting a parameter are skipped, closed default otherwise. Specificity is `(is_exact, literal_prefix_len)` — the exact-beats-pattern rule is a separate tuple element rather than left to fall out of prefix lengths, because DESIGN states it separately
- [x] Global `cf` vendor is implicitly admitted; nothing else is. Keyed on the **last** segment's vendor, so a `cf`-rooted identifier with an `acme` derivation is an `acme` candidate — tested, since reading the first segment would open the whole platform namespace to derivations
- [x] ~~`worker.write_lock_timeout` is positive, defaults to 5s and is enforced by the admission worker~~ — **the key is gone.** It bounded the family advisory locks, which T15 retired; the `entity_write_order` claim's wait is the database's own (`lock_timeout`, `innodb_lock_wait_timeout`, `busy_timeout`) and a gear-side budget is not expressible — see SPEC §8.1 and the `TxConfig` ask in §4

**Verification:**
- [x] `cargo test -p cf-gears-types-registry` — 255 lib + 259 integration tests on SQLite; the operator-facing configuration boundary has 21 `config_test.rs` cases
- [x] Table-driven test over the four policy entries in SPEC §10.3 plus the resolution rules, manual `vec![]` + loop (no `rstest`)
- [x] Test: a more specific `allowed_vendors` **replaces** a less-specific set rather than extending it — including the pair of candidates that shows it, one excluded inside the narrow region and the same vendor still admitted outside it
- [x] Test: an entry that omits `allowed_vendors` is skipped, and a less-specific entry supplies it. Both parameters are therefore `Option`: collapsing absent onto `[]` / `false` would let a narrow entry silently close what a broad one opened
- [x] Test: a config carrying `tenant_ownable` starts cleanly and does not admit a tenant-owned candidate
- [x] Test: invalid pattern in `registration_policy` fails startup with the region named
- [x] Added: an unknown key is refused rather than ignored (`deny_unknown_fields` is the other half of a typed configuration — a misspelled limit must fail the boot, not silently never apply); byte sizes accept the documented `256KB` / `1MB` forms and a malformed one fails to parse rather than becoming zero

**Corrected at the Checkpoint 1 review.** "Has no reader" was true and
invisible: five keys read as enforced, and `CLOSURE_BOUND`'s comment claimed to *mirror*
`activation_write_set`, so an operator writing 1024 silently got 512. Three changes, no new
enforcement — the enforcement is scheduled and pulling it forward would be guessing at T14's and
T21's shapes:

- every field now says **enforced** or **accepted, not enforced in P0**, and the latter names the
  task that binds it (T14 for the write set, T15 for `max_revalidation_attempts`, T21 for
  `operation_timeout`, T22a for the page-size pair) — the honest
  shape `tenant_ownable` already had;
- `CLOSURE_BOUND` says it is *its own* bound over what a store build **reads**, not SPEC §8.1
  step 4.6's write set, and its error message no longer borrows the other bound's name;
- `config::inert_limit_keys()` collects the keys a deployment moved off their default that P0 does
  not act on, and `init` names them in one `warn!`. Not a boot failure: a P1-ready configuration
  legitimately carries every one of them. The test over it is exhaustive, so binding a key breaks
  that test — which is the reminder to take it out of the list.

**Resolution budgets:**

- [x] `resolution_closure` bounds each candidate or refreshed schema's distinct authored
  resolution graph, including its own document and the candidate overlay. Converging paths
  count once; entity references (`x-gts-type-ref` / `x-gts-instance-ref`) and unrelated documents in the shared refresh store do not count
- [x] `resolved_document` bounds the canonical UTF-8 bytes of each effective artifact,
  including the conforming schema used by an Instance. Exactly-at-bound passes; exceeding
  either budget refuses admission and rolls back any revision and dependent refresh
- [x] Both settings reject zero at startup and are removed from `inert_limit_keys()`

**Dependencies:** T2
**Files touched:**
- `TR/src/config.rs` — `allow_compatibility_force`, `Limits`, `PolicyEntry`, `WorkerSettings`, `ByteSize`, `ConfigError`, `validate()`
- `TR/src/domain/policy.rs` — NEW, `RegistrationPolicy` + `PolicyConfigError` + `PolicyRefusal`
- `TR/src/domain/policy_tests.rs` — NEW, 16 in-source tests
- `TR/src/domain/mod.rs` — declare `policy`
- `TR/src/gear.rs` — startup validation
- `TR/tests/config_test.rs` — NEW, 21 tests
**Scope:** M

---

### - [x] T7: Acceptance path and operation records

**Description:** The synchronous half of admission: envelope and batch bounds, canonical
identifier check, registration policy, identifier profile, Draft-07 dialect gate, `skip_compatibility_check`
gate, request fingerprint, `Idempotency-Key` resolution, then one transaction inserting the
operation, its items and the outbox message. Reads no entity state.

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] SPEC §8.1 ordering: `validate` has no database access, so policy precedes existence checks. Acceptance handles steps 1–6 and 8; T18 implements step 7 in the worker using its extracted dependency edges (see SPEC §8.1). T17 replaces the temporary `skip_compatibility_check` refusal with compatibility checks and provenance.
- [x] Policy gates **every accepted candidate**, because P0 accepts only declared creations: a positive `expected_resource_version` is refused (`AcceptanceError::RevisionNotAccepted`) and deletion is refused with the envelope. SPEC §8.1's revision/deletion bypass is deliberately *not* implemented yet — gating on the caller's declared kind while nothing verified the claim let a request name a version and skip the gate outright (found at the Checkpoint 1 review). The bypass returns at T11, together with the commit-side precondition that makes the claim checkable. A refusal names the region and the parameter
- [x] Fingerprint covers canonical body, operation kind, owner, preconditions and each `skip_compatibility_check` flag — plus dry-run mode, plane, tenant and principal, as one table-driven test over all nine inputs. Every field is **length-prefixed**, so a digest cannot confuse `("ab", "c")` with `("a", "bc")`, and the digest carries a version tag so a future change to its coverage cannot read as a matching replay
- [x] Replay with a matching fingerprint returns the stored operation (`202` non-terminal, `200` terminal); a different fingerprint under the same key returns `409`. The replay test submits a deliberately **reordered** body, because canonicalization is the thing that makes a replay a replay
- [x] Concurrent acceptance on one key resolves via the unique constraint, loser returns the winner after fingerprint verification. The loser re-reads **outside** the rolled-back transaction: on PostgreSQL a constraint violation poisons the transaction, so a re-read inside it would fail for a second, unrelated reason
- [x] `plane = 1`, `tenant_id = NULL`, `principal_id` the named P0 constant with a `TODO`; ceiling C2 (global idempotency namespace) commented on the constant and at the scope-hash call site
- [x] Ceiling C5 (no operation-retention sweep — terminal operations accumulate) commented on `OperationRepo::insert`, with the §3.2 sweep as its upgrade path
- [x] Literal `expected_resource_version: 0` rejected; absent means must-not-exist

**Verification:**
- [x] Gear tests (see [Commands](#commands)): 223 lib + 182 integration tests, of which 11 are `fingerprint_tests.rs`, 26 `acceptance_tests.rs` and 9 `operation_idempotency_test.rs`
- [x] Tests: replay, fingerprint conflict, concurrent acceptance (8 tasks on a file-backed pool), each refusal reason, `0` rejection — plus a dry run then a commit under one key, which is a conflict rather than a replay of the dry-run result
- [x] Test: acceptance issues no read against `entity` — tested by **removing the ability**, not by inspection: the `entity` table is dropped through a second connection to the same file database, a control probe asserts it really is gone, and acceptance still succeeds
- [x] Added: a dispatch failure rolls the whole acceptance back, and a synchronous refusal writes no operation and dispatches nothing

**Added beyond the criteria:** an over-long `Idempotency-Key` refused before the `varchar(255)`
column sees it; an empty batch refused; a negative precondition refused beside the literal `0`;
and T6's `limits.authored_document` enforced on the canonical bytes, which is what gets stored
and fingerprinted.

**Dependencies:** T4, T6
**Files touched:**
- `TR/src/domain/admission/mod.rs` — NEW, `SubmitRequest` / `Candidate` / `Accepted` / `OperationDispatch`
- `TR/src/domain/admission/acceptance.rs` — NEW, steps 1–6 and 8, the transaction, `AcceptanceError`
- `TR/src/domain/admission/acceptance_tests.rs` — NEW, 26 in-source tests
- `TR/src/domain/admission/fingerprint.rs` — NEW, canonical bytes, fingerprint, scope hash, `P0_PRINCIPAL_ID`
- `TR/src/domain/admission/fingerprint_tests.rs` — NEW, 11 in-source tests
- `TR/src/domain/policy.rs`, `TR/src/domain/policy_tests.rs` — vendor from the last named segment
- `TR/src/domain/mod.rs` — declare `admission`
- `TR/src/infra/storage/repo.rs` — NEW `OperationRepo`, `NewOperation`, `NewOperationItem`
- `TR/Cargo.toml` — `sha2`
- `TR/tests/operation_idempotency_test.rs` — NEW, 9 `SQLite` tests
**Scope:** M

---

### - [x] T8: Admission worker — one dependency-free candidate

**Description:** The worker as a plain function of `(operation_id, runner)`: build the unit's
transient store (T5), evaluate one acyclic, reference-free Type Schema candidate against it,
then commit family, entity, revision, current-state projection with materialized artifacts,
`resource_version` and the item outcome. No dependencies, no compatibility, no batching yet.

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] Entry point is directly callable and returns a result; no `sleep`, timer or polling anywhere in it or its tests. **The error boundary is the retry boundary:** `WorkerError` is infrastructure-only, while a candidate that is simply wrong is an `ItemFailure` recorded on its item — `Err(WorkerError)` versus `Ok(_)` with a failed item. Retrying a final decision would burn the outbox's attempt budget forever, so T21's handler becomes a two-line map
- [x] Evaluation happens outside the transaction; the transaction contains only rechecks and writes. In the type signatures, not by convention: `evaluate` takes a runner and opens nothing, `commit_creation` takes a `&DbTx<'_>`
- [x] `type_schema` row is populated at admission — `resolved_schema`, `effective_traits`, `effective_traits_schema`, `resolution_fingerprint` (D3). All three come from `validate_schema`, which is the **only** public route to them: `effective_traits` is `pub(crate)` in the library and `GtsOps::validate_schema` discards the `ResolvedType` it built
- [x] `resolution_fingerprint` is computed over canonical bytes, independent of map iteration order — the same canonicalization the request fingerprint uses, and asserted against the stored artifacts rather than against a literal
- [x] Creation requires the identifier absent; the outcome records `gts_uuid` and `resource_version`. `gts_uuid` is `GtsId::to_uuid()` — `gts-rust`'s deterministic derivation, never a locally reproduced UUIDv5, which is what T4's *tests* used as a stand-in
- [x] The transient store is built inside the invocation and dropped with it; nothing is retained on the worker, the service or the gear between invocations, and there is no post-commit rebuild step

**Verification:**
- [x] Gear tests (see [Commands](#commands)): 232 lib + 190 integration tests, of which 5 are `family_tests.rs`, 4 `artifacts_tests.rs` and 8 `admission_worker_test.rs`
- [x] Test: registering a schema writes exactly one row in each of the five affected tables — `version_family`, `entity`, `type_schema_revision`, `type_schema` and the `operation_item` outcome, plus the operation's own transition to `completed`. `dependency` is deliberately **not** among them: this candidate references nothing and edge extraction is T13
- [x] Test: `resolution_fingerprint` is stable across two computations of identical artifacts, and each of the three artifacts moves it
- [x] Test: creation against an existing identifier fails **terminally** with no revision written — recorded on the item, not returned as a worker error, and through the recheck *inside* the commit transaction, which is the same path a concurrent creation hits
- [x] Test: a second invocation after a committed revision sees it — **partly, and the test says so.** A derivation that consumes the base through a `$ref` needs the base in its closure, and the edges are T13's; so the derived candidate fails, with the reason asserted, and what is proved is that a fresh invocation reads the committed row and its authored document from the database with no carried-over copy. The store-level form is already covered by T5's `two_sequential_builds_each_observe_the_committed_revision`; the end-to-end form arrives with T13
- [x] Added: an unresolvable `$ref` and a document failing its meta-schema are item failures rather than worker errors; an unknown operation UUID *is* a worker error; a failed evaluation leaves all three affected tables empty while the operation still reaches `completed`; a second pass over a completed operation is a no-op

**Not done, deliberately:** `insert_current` is an insert rather than an upsert — moving an
existing pointer is a revision with its own preconditions (T11), and a silent overwrite would
hide a missing recheck. One item is one unit, in `item_no` order; in-batch references and
topological ordering are T19. `run_operation` takes no config, because nothing in T8 reads a
limit — T14 gives it `&Limits`.

**Duplicate delivery is already a no-op**, ahead of T21 asking for it: a completed operation is
recognized and its stored outcomes returned, and an item already terminal from an earlier pass is
skipped. Four lines, and the property at-least-once delivery makes load-bearing — leaving it to
T21 would have meant writing the worker twice.

**Dependencies:** T5, T7
**Files touched:**
- `TR/src/domain/admission/worker.rs` — NEW, `run_operation`, `WorkerError`, `ItemFailure`, outcomes
- `TR/src/domain/admission/unit.rs` — NEW, `evaluate` (no transaction) and `commit_creation` (transaction only)
- `TR/src/domain/artifacts.rs`, `artifacts_tests.rs` — NEW, D3 materialization + 4 tests
- `TR/src/domain/family.rs`, `family_tests.rs` — NEW, family-key derivation + 5 tests
- `TR/src/domain/mod.rs`, `TR/src/domain/admission/mod.rs` — declarations
- `TR/src/infra/storage/repo.rs` — revision / current-state inserts and the four status writes
- `TR/tests/admission_worker_test.rs` — NEW, 8 `SQLite` tests
**Scope:** M

---

### - [x] T9: REST — `POST /entities`, `GET /operations/{id}`, `GET /entities/{entity_key}`

**Description:** The first complete REST path: submit a registration and get `202` +
operation, poll the operation, read the entity. `POST /entities` breaks its old `200`
shape (D10).

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] `POST /entities` returns `202` with operation `Location` and advisory `Retry-After`; `200` only on terminal replay. The `Location` is built from the path the request **arrived on** and is therefore followable under api-gateway's `prefix_path` — the credstore precedent, with one correction to it: `OriginalUri`, not `Uri`, because `apply_prefix` mounts every gear with `Router::nest`, which rewrites away exactly the prefix that has to survive (found at the Checkpoint 1 review). The receipt reports the operation's **real** status: `pending` for a fresh submission, the stored value for a replay. It comes from `Accepted.status`, which `submit` already knows, rather than from the read-back this used to do: that read cost a second snapshot transaction over two statements and carried a `"pending"` fallback for an operation that had just been committed and so cannot be absent (found at the Checkpoint 1 review)
- [x] `Idempotency-Key` is required; absence is a synchronous refusal. **A toolkit gap:** `OperationBuilder` exposes `path_param` / `query_param` and no header equivalent, so the requirement cannot be declared as an OpenAPI parameter from this gear. The route description states it *and* states that it is undeclared; fixing the builder is toolkit work outside this gear
- [x] Errors are RFC-9457 problem details via `.standard_errors(openapi)`, never raw status tuples — one match arm per `AcceptanceError` variant, exhaustive, so a new refusal reason cannot reach the wire as an opaque `500` by omission
- [x] Routes are `/types-registry/v1/...` and `.authenticated()`; DTOs live only in `api/rest/dto.rs`
- [x] Added at the review gate: the four wire vocabularies — operation status and kind, item status, entity kind, lifecycle status — are `#[api_dto]` **enums**, not `String` fields whose admissible values lived in a docstring. A `String` publishes an unconstrained `string` in the served `OpenAPI`, so a generated client gets no vocabulary and a value this gear never emits type-checks against it; the five `*_str` helpers are gone with it (found at the Checkpoint 1 review)
- [x] `GET /entities/{entity_key}` accepts a GTS identifier or a `gts_uuid` — classified by `EntityKey::parse` in the **domain**, so a future gRPC adapter with the same field gets it for free
- [x] Interim global API retains authenticated routes without tenant scope; C8 is documented at registration. v2 omits `.exposed()` but remains in OpenAPI: visibility controls gateway discovery, not spec inclusion. T38 promotes paths without exposing mutations while C8 remains open
- [x] **Handlers are mapping steps only.** `RegistryService` has three methods taking and returning domain values with no `StatusCode`, `HeaderMap` or `Json`. The identifier-versus-UUID classification stays in the domain. The request's `expected_resource_version` is persisted as the worker precondition (`0` means must-not-exist) but is not echoed by the public operation outcome. The created `revision_no` likewise remains internal admission provenance; only the resulting `resource_version`, which future writes accept as their precondition, crosses that response boundary. `Idempotency-Key` is read from the header and passed as a parameter

**Verification:**
- [x] `cargo test -p cf-gears-types-registry` — `api_rest_test.rs` via `Router::oneshot`, 10 cases, driving the **real** `register_routes` with a stub `OpenApiRegistry` rather than a hand-built router: a bare router would pass while a route was registered at the wrong path or without its auth stage. 226 lib + 195 integration tests pass
- [x] Manual at Checkpoint 1: `make example` with `E2E_ARGS` (`config/e2e-features.txt`, including `static-tenants`) boots 25 gears; health is `200`, all three routes work and `/cf/docs` renders them. Bare cargo startup omitted the tenant-resolver plugin, explaining the earlier invocation failure. The missing `/cf` prefix in receipt `Location` was fixed and pinned by a nested-router follow-through test. `config/e2e-local.yaml` retains the old store until T38

**Superseded by T9a.** This task repointed the two existing v1 routes at the database path; T9a restores v1 and moves this surface to `/types-registry/v2/`, for the reasons in P12. The DTOs, handlers and tests below are the ones T9a re-registers under v2 — nothing here is discarded, only re-addressed.

**Two interim shapes, each marked in code.** The scope is `allow_all` (ceiling C6).
Ceiling C8 is commented at the routes.

**The database is optional.** `ctx.db()` rather than `db_required()`: `no-db.yaml` and `--mock`
bind none, and failing their boot for a path they do not use would be a regression. Where none is
bound the routes are registered and answer `503` with a problem document naming the cause — a
`404` would suggest the API changed — and `init()` logs the config key to set.
`config/quickstart.yaml` and `config/e2e-local.yaml` now bind one: this is the binding T2
deferred to *"the first slice that reads these tables"*, and before this task the migration ran
in no deployment at all.

**Dependencies:** T8
**Files touched:**
- `TR/src/domain/registry_service.rs` — NEW, `RegistryService`, `EntityKey`, record types, `ServiceError`
- `TR/src/api/rest/routes.rs` — the three routes, ceiling C8 comment
- `TR/src/api/rest/handlers.rs` — three new handlers; the two replaced ones deleted
- `TR/src/api/rest/dto.rs` — the submit-then-poll DTOs and mappings; the old shape's four DTOs deleted
- `TR/src/api/rest/error.rs` — `From<ServiceError>` / `From<WorkerError>` / `From<AcceptanceError>`
- `TR/src/infra/storage/repo.rs` — `EntityRepo::find_by_gts_uuid`, `TypeSchemaRepo::find_current`
- `TR/src/gear.rs` — wire the database-backed service when a database is bound
- `TR/Cargo.toml` — `tower` dev-dependency
- `TR/tests/api_rest_test.rs` — NEW, 10 route tests
- `TR/tests/registration_tests.rs`, `TR/tests/query_tests.rs` — tests of the deleted handlers removed
- `config/quickstart.yaml`, `config/e2e-local.yaml` — bind a database to types-registry
**Scope:** M

---

### - [x] T9a: Restore the v1 REST contract; the async surface moves to `/types-registry/v2/`

**Description:** T9 **repointed** the two existing v1 routes instead of adding new ones, which breaks the invariant this plan set for itself (the historical risk-table assumption was no database consumers before the legacy-fleet cutover, now T38, and no dual write; P28/P29 later added the early local paths in T31/T32). Three consequences at that time:

1. `POST /v1/entities` changed its **body shape** and gained a required `Idempotency-Key`. Every existing caller gets `400`/`422`, so this is not the "expects `201`, gets `202`" break T38 was scoped for.
2. **Cross-gear registration over REST is functionally broken.** `testing/e2e/gears/oagw/helpers.py:83` and `testing/e2e/gears/account_management/conftest.py:182` register over REST and then resolve through `TypesRegistryClient` (`account-management/src/infra/types_registry/`, `usage-collector/src/domain/service.rs`). Those gears now **write to the database and read from process memory**. No e2e change fixes this — only T38 does.
3. One resource, two sources of truth: `GET /v1/entities` (list) answers from memory,
   `GET /v1/entities/{entity_key}` from the database.

Restore both v1 routes from `main` verbatim and register T9's surface under `/types-registry/v2/`. Everything v1 needs is still in the crate — `TypesRegistryService::{register_validated, get, is_ready}` (`domain/service.rs:66,108,144`), `InMemoryGtsRepository`, `GtsEntityDto` — only two handlers and four DTOs were deleted, and they come back unchanged. v2 is **interim by design**: T38 deletes v1 with the repository it reads, and T38 promotes v2 onto the v1 paths, so P0 ends on one version.

**Acceptance criteria:**
- [x] `POST /types-registry/v1/entities` is contract-identical to `main`: `operation_id` `types_registry.register`, body `{"entities":[…]}`, `200`, `RegisterEntitiesResponse` with its summary, served from the in-memory service behind `is_ready()`
- [x] `GET /types-registry/v1/entities/{gts_id}` restored: `operation_id` `types_registry.get`, in-memory, unchanged DTO
- [x] The four deleted DTOs — `RegisterEntitiesRequest`, `RegisterEntitiesResponse`, `RegisterSummaryDto`, `RegisterResultDto` — are **restored from `main`**, not re-derived: a re-derived DTO is a second wire change nobody asked for. Lifted out of `git show main:` rather than retyped, so the wire shape cannot drift by a field name
- [x] `GET /types-registry/v1/entities` (list) is untouched; it never moved
- [x] The async surface is `POST /v2/entities`, `GET /v2/operations/{operation_id}`, `GET /v2/entities/{entity_key}`, keeping T9's `operation_id`s — `submit_entities`, `get_operation`, `get_entity`, distinct from v1's `register`, `list`, `get`
- [x] **No route straddles the two stores:** no v1 handler takes `RegistryService`, no v2 handler takes `TypesRegistryService`. Grep-checked — three handlers take `Extension<Arc<TypesRegistryService>>` (`register_entities`, `list_entities`, `get_entity`) and three take `Extension<Option<Arc<RegistryService>>>` (`submit_entities`, `get_operation`, `get_entity_by_key`); no handler takes both
- [x] **No dual-write and no fallback read.** Pinned by `a_v1_registration_is_absent_from_v2_and_the_reverse`: a v1 registration reads `200` on v1 and `404` on v2, a v2 admission `200` on v2 and `404` on v1. The two directions run against the *same* routes, so the `404` cannot be a missing route rather than an honest miss — the test would fail on the companion `200` first
- [x] `RegistryService` stays per-database-optional as T9 left it: with no database bound, v2 answers `503` and v1 works. Both halves are tested — `without_a_database_the_routes_report_service_unavailable` for v2, and `without_a_database_the_v1_routes_still_serve` for v1's register, get and list
- [x] Route paths come from one constant per version, so T38's promotion is a constant change rather than a sweep. `routes::V1` / `routes::V2` are `pub` and the test imports **the same two constants** the routes are built from — not its own copies, which would drift at exactly the moment the promotion happens
- [x] The tests T9 deleted from `registration_tests.rs` and `query_tests.rs` return with the routes they cover — three and two respectively, and both files now `diff` clean against `main`
- [x] SPEC §10.2 is amended: the v1 break is **withdrawn** for the T9a–T38 window and reinstated at T38, naming both tasks. The route table there is labelled as the post-T38 surface, so it is not read as describing today

**Verification:**
- [ ] `make e2e-local` green **without editing a single e2e file** — that is the criterion, because the task's whole point is that the suite did not need migrating yet. **Blocked by a pre-existing T1 defect, not by this task** (see the Checkpoint 1 item below): the run never reaches pytest, because types-registry's `post_init` fails first. No e2e file was edited, which is the half of the criterion this task owns and which holds
- [x] Attribution done rather than assumed: `make e2e-local` was run on a **stashed tree at HEAD**, without any T9a change, and failed with `MAKE_EXIT=2` and the byte-identical signature — same two identifiers, same `Ready commit failed with 2 errors`. The suite was already red at Checkpoint 1
- [x] `cargo test -p cf-gears-types-registry` — **443 passed, 0 failed** across 19 suites (426 before this task; +15 in `api_rest_test`, +5 restored, and the arithmetic differs by the two suites whose counts moved with the restores)
- [x] The v1/v2 split is **asserted at router level instead of checked by hand** — `a_v1_registration_is_absent_from_v2_and_the_reverse` drives the real `register_routes`, so it covers the same ground as the manual pass and cannot rot. The manual criterion is satisfied by it plus `make e2e-local`, which boots the real server over the unmodified suite
- [x] **Both versions reach the generated document with six distinct operation ids** — also a test rather than a manual look, because the failure mode is invisible in the router: `OperationBuilder` registers two routes under one `operation_id` without complaint, and the second silently replaces the first in the document a generated client is built from. `TestOpenApi` now records `(method, path, operation_id)` and `both_versions_are_declared_with_distinct_operation_ids` asserts the whole set. **Mutation-checked:** giving v2's read v1's `types_registry.get` fails it with the colliding pair named
- [x] Added, not in the original criteria: a pre-existing `clippy::doc_markdown` error in `domain/admission/fingerprint.rs:73` (unbackticked `UUIDv5`, from the Checkpoint 1 commit) blocked `-D warnings` for the whole gear. Fixed here — one word of backticks — because a slice cannot be verified against a red gate it did not cause

**Dependencies:** T9
**Files likely touched:**
- `TR/src/api/rest/routes.rs` — v1 restored, T9's three routes moved to v2
- `TR/src/api/rest/handlers.rs` — `register_entities` / `get_entity` restored from `main`
- `TR/src/api/rest/dto.rs` — the four DTOs restored
- `TR/tests/api_rest_test.rs`, `TR/tests/registration_tests.rs`, `TR/tests/query_tests.rs`
- `docs/p0/SPEC.md` §10.2
**Scope:** S — a revert plus a prefix move; no domain code changes

---

### - [x] T10: Registered Instances

**Moved into Phase 1** from Phase 2. Two reasons. Instances are what the platform actually pushes today — P4 counts *"roughly eleven plugin gears"* already registering their well-known Instances from their own `init()` — so they are on the critical path to T38 and are the longest pole, not a widening. And T9's surface accepts an Instance and then fails it in the **worker** (`StoreBuildError::UnsupportedKind` → `WorkerError::StoreBuild` → opaque `500`), which is a retryable class for a decision that is final; building the feature closes that hole instead of adding a refusal for it.

**Description:** Extend admission to registered Instances: `instance_revision`, `instance`
current pointer, conformance to the Type Schema identified by the identifier prefix through
the last `~`, and the immutable schema-revision pair.

**Three companions come with the move, and each is load-bearing rather than tidy-up.**

1. **The closure gains an identifier-derived component.** `GtsStore::validate_instance` resolves
   the instance's `type_id` *out of the store*, so the parent Type Schema must be loaded — and
   today `DependencyRepo::closure` walks the `dependency` table only, which nothing writes until
   T13. But a derivation base is not an edge: `GtsId::chain_ids()` and `get_type_id()` are pure
   functions of the identifier, needing no table at all. So the closure seeds its worklist with
   the candidate's chain **and** its edge targets. This is why the move is cheap, and it also
   repairs a Phase 1 defect: `admission_worker_test.rs::a_second_invocation_sees_the_first_ones_committed_revision`
   currently asserts that a derived Type Schema **fails**, and the comment blames T13's missing
   edges — half right, since `validate_schema` walks `chain_ids()` and would have found the base
   with no edge table involved. T13 supplies the `$ref` targets needed by the forward closure and
   the three direct edge kinds needed by T14's reverse walk. An entity reference (`x-gts-type-ref` / `x-gts-instance-ref`) is neither resolved nor
   represented by an edge.
2. **`instance` and `instance_revision` acquire their four edits** — entity, repository with its
   mapper, port trait and types, and the forwarding block in `store.rs`. T2's migration already
   created both tables, so no migration changes.
3. **T12's *kind* rule comes with T10; shape and contiguity stay in T12.** A Type Schema
   `…ns.thing.v1~` and an Instance `…ns.thing.v1` derive the **same** `family_key`
   (`family.rs` normalizes the trailing `~` away — see `family_tests.rs`), so the first Instance
   can land in a family whose members are Type Schemas. Unguarded, that produces a family row a
   later task's invariant assumes cannot exist.

**Acceptance criteria:**
- [x] An Instance records the exact Type Schema revision that validated it — `(type_schema_entity_id, type_schema_revision_no)` on `instance_revision`, read in **the same snapshot** as the transient store so the recorded pair is the one that actually validated; a second read could see the schema revised in between
- [x] An Instance whose conforming schema is absent fails immediately with `dependency_not_found` and structured dependency ID/kind, before value validation. The refusal is recorded on the item and acknowledged by the outbox.
- [x] A minor or major 0 in the Instance identifier's last segment is refused at acceptance — **already built and tested in T7**; `an_instance_identifier_must_name_a_stable_major_without_a_minor` covers both halves plus the Type Schema contrast, so this task adds nothing
- [x] `instance` carries only the current-revision pointer — no derived artifact. The asymmetry with `type_schema` is documented on the entity so it is not "fixed" later: an Instance has no derived state, its value is authored and its schema revision is immutable and pinned by `ON DELETE RESTRICT`, so there is nothing that could change without a new revision and nothing to fingerprint
- [x] `entity_kind` is derived from the identifier, not passed in. Stronger than removing the literal: `EvaluatedOutcome` is an **enum** whose variant carries the kind-specific payload (artifacts for a schema, the revision pair for an Instance), and `entity_kind()` reads it off the variant. Two `Option` fields beside a `kind` discriminant would have made a mismatch representable; here it is a compile error
- [x] The transient store carries both kinds: both `UnsupportedKind` refusals are deleted and the dialect gate is keyed on the identifier's `~`. `type_id` is passed to `GtsEntity::new` **explicitly** from `GtsId::get_type_id()` with a `None` config — letting content name the conforming type would allow a value to claim conformance to a type it was not registered under, which is a validation bypass, not a convenience
- [x] The closure reaches a candidate's derivation chain with **no `dependency` row present** — `closure` seeds its worklist with `GtsId::chain_ids()`, and `missing_roots` is still computed over the original roots so a chain member the seed added is not reported as one. The Phase 1 test is inverted, and its old comment was half wrong: it blamed T13's missing edges, but a derivation base is a pure function of the identifier
- [x] A family holds one kind — `family_kind_conflict`, checked under the family lock and only for a family that already existed. Distinct from `already_exists` because the identifier *is* free; saying otherwise sends a caller looking for a conflicting entity that does not exist. Both orders are tested
- [x] No `$ref` target is expected to resolve in T10; document-reference support belongs to T13 and is verified there by `a_ref_outside_the_chain_is_admitted`. An entity-reference (`x-gts-type-ref` / `x-gts-instance-ref`) target is never resolved or seeded because validating the keyword reads no target document.
- [x] **An admitted Instance reads back with its value.** *Added after the task was marked done — see the correction below.* `GET /v2/entities/{entity_key}` returns either kind's authored document under the single public field `content`. The immutable `revision_no` remains an internal current-pointer and provenance detail; the public entity exposes `resource_version`, which is the token future writes accept. The three `effective_*` artifacts stay absent for an Instance, and that is the contract rather than the same omission: an Instance has no derived state, so there is nothing to materialize

**The read path was missed, and the task's criteria are why.** Every criterion above is about
admission — the write path, the transient store and the family rule — and none names the public
read. `RegistryService::entity()` was built at T9, when only Type Schemas existed, and kept asking
`TypeSchemaStore::{find_current_schema, current_documents}` for **both** kinds. An Instance has no
row in either table, so an admitted Instance answered `200` with `content: null` while its
operation reported `succeeded` — the value was durable, correct, and unreachable through the
API. `InstanceStore::current_values` already existed and was already
correct; it had one caller, `gts_store.rs`, which is the **transient validation** store and not a
read path. `api_rest_test.rs` contained no Instance case at all, which is why 454 green tests said
nothing about it.

The read now branches on the row's kind into a `CurrentState` enum — the same shape
`EvaluatedOutcome` has on the write side, so "an Instance carrying a resolved schema" is not a
representable value rather than a mismatch a later edit could introduce. One statement, not two:
`current_values` returns the pointer's revision number with the value it points at; the service
uses that pairing internally and does not publish the revision number.

**Consequence for later tasks, since it would have surfaced there as a regression.** T29's `get_instance` and T22a's `batchGet` both sit on this method; the SDK helpers that hydrate a content-free page through `batchGet` would have returned pages of null values.

**Verification:**
- [x] Gear tests, all three backends — 454 on `SQLite`, and `make test-integration LANE=types-registry-db` green: 4 container tests, migration and repository primitives on both `PostgreSQL` and `MySQL`
- [x] Test: Instance value violating its schema is refused — `invalid_value`, distinct from `invalid_schema`: the schema is fine, the value is not
- [x] Test: `fk_tr_instance_revision_schema` prevents dangling schema-revision references — `instance_revision_cannot_reference_a_missing_schema_revision`. Foreign keys are enabled explicitly on that connection, because `SQLite` does not enforce them by default and the test would otherwise pass while proving nothing
- [x] Test: an Instance admits against a Type Schema committed by an **earlier** operation, with the `dependency` table asserted empty
- [x] Test: a derived Type Schema admits against a committed base, with the `dependency` table empty
- [x] Test: Type Schema then Instance under one `family_key` — second is refused; and the reverse order
- [x] Test: an admitted Instance reads back over the **real routes** with its authored value under `content`, without exposing internal `revision_no`, and with the three `effective_*` artifacts asserted absent — `api_rest_test.rs::an_instance_reads_back_with_its_authored_value`, which polls the operation first so a refused candidate cannot be mistaken for a value the read failed to reach. Plus the same Instance by its Registry Reference, pinning that the kind branch is chosen by the row and not by how it was found. Both are genuine RED→GREEN: they failed on `content: null` before the fix. Re-run on all three backends — 457 on `SQLite`, `make test-integration LANE=types-registry-db` green

**Interim, and stated rather than discovered later:** *"fails retryably"* is exercisable only as a
unit call on `run_operation`, because there is no outbox to redeliver until T21 and inline
admission surfaces a `WorkerError` to the caller as a `500`. That is the existing behaviour of
every `WorkerError` today, not something this task introduces; T21 makes it a redelivery.

**Dependencies:** T8 (commit path), T9a (so the surface it lands on is v2)
**Files likely touched:** `TR/src/infra/storage/entity/{instance,instance_revision}.rs`, `TR/src/infra/storage/repo/{instance_repo,dependency_repo}.rs`, `TR/src/domain/admission/unit.rs`, `TR/src/domain/gts_store.rs`, `TR/src/domain/family.rs`, `TR/src/domain/ports.rs`, `TR/src/infra/storage/store.rs`, `TR/tests/instance_test.rs`
**Scope:** M — at the top of M with the three companions; if the closure change grows past the chain seed, split it out rather than letting this become an L

---

### Checkpoint 1 — proves the architecture

Outcome and evidence: the criteria below.

- [x] A fixture Type Schema registers over REST, the operation reaches `completed`, the entity and its resolved artifacts are readable — as a test (`api_rest_test.rs::a_registration_is_accepted_polled_and_read_back`, driving the real `register_routes`) **and now at runtime**: `POST /cf/types-registry/v1/entities` `202` → operation `completed` with one `succeeded` item → entity `active`, `rv=1`, all four artifacts materialized, readable by `gts_id` and by `gts_uuid`. **T9's boot blocker was the invocation, not the code, and is retracted:** `oagw` is a non-optional dependency of the example server while every tenant-resolver plugin sits behind a cargo feature, so a bare `cargo run` compiles the resolver with no plugin and `oagw` is the first to notice. With `--features "$(cat config/e2e-features.txt)"` — which is what `make example` passes — all 25 gears boot and `/cf/docs` renders the four routes
- [x] `TR/tests/restart_persistence_test.rs` (2 tests): reopening SQLite preserves whole models across eight tables after admitting a schema and Instance; fresh reads and idempotency replay work without writes. A persisted non-terminal operation also completes after reopen. This proves pool reopen, not process restart; real init/seeding/process restart remains e2e/manual work
- [x] Consumers untouched: the old `TypesRegistryClient` is still served from its existing in-memory repository; full workspace tests pass — **10593 passed, 368 skipped, 0 failures** (`cargo nextest run --workspace` minus the two macro crates, as `make test-no-macros` does). Structurally, the branch touches four files outside the gear — `Cargo.lock`, `Cargo.toml` (gts 0.11.0 → 0.12.0) and the two configs — and **not one file in `types-registry-sdk`**; the gear still holds `service` and `local_client` beside the new `registry`
- [x] The new path holds no entity state between admissions: the store is built per unit and dropped, and the entity read in the first item above comes from the database — `RegistryService` has no store field and `grep ArcSwap src/gear.rs` finds nothing; `build_store` / `load_unit_store` are free functions returning an owned `UnitStore`, so there is no `self` to retain it in. T5's `two_sequential_builds_each_observe_the_committed_revision` proves the consequence, and the `503`-without-a-database case shows the read really is a database read
- [x] Gear tests green on SQLite, PostgreSQL and MySQL (see [Commands](#commands)) — 423 tests on SQLite, and the **first ever** container run of the two backend suites, Docker having been down for T1–T9. It found a real defect: `sqlx` binds `Uuid` as 16 raw bytes on both non-native backends, so every uuid write failed on MySQL's `CHAR(36)` and was silently stored as a blob in `SQLite`'s `TEXT`. Fixed to `BINARY(16)` / `BLOB` + `ck_tr_*_uuid_len`
- [x] `make dylint` clean — green for this gear **and for the whole workspace**. This is the phase-end run for Phase 0 and Phase 1 (P13), and the first one: 26 violations were standing across T1–T9, and because `file-storage`, `mini-chat`, `chat-engine` and `oagw` reach types-registry through `authz-resolver`, they could not be linted past it either. DE0708 (2) — `sha2` replaced by inline FNV-1a where the input is server-side and by `aws-lc-rs` SHA-256 where it is client-controlled; no allow-list entry spent. DE1302 (10) — wire details composed from variant fields, infrastructure causes logged and answered opaquely. DE0301 (14) — the domain now names `domain::ports` and `domain::enums`, never `crate::infra`
- [x] **The T1 defect that made `make e2e-local` red is fixed.** types-registry's `post_init` used to fail before pytest started:

  ```
  GTS validation error gts_id=gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~
    ... is not compatible with base 'gts.cf.core.am.tenant_type.v1~':
    Schema at '$' adds required properties: ["id"]
  ```

  Reproduced identically on a stashed tree at HEAD, so it predated T9a. **T1's failure mode arriving late** — SPEC §7 named the class, and T1's three measured populations (118 runtime entities, 797 doc/JSON files, macro literals) cover no schema embedded in a YAML config or posted by an e2e fixture.

  **Root cause, and it was not what the error suggested.** AM's abstract envelopes `tenant_type.v1~` and `tenant_metadata.v1~` declared a closed root with **no extension point at all** — only an inert `id` field present to satisfy the `gts-macros` base-struct contract, which being non-`Option` landed in `required`. Their derived types are authored as runtime JSON, so they never composed the base either. Under gts 0.12.0's corrected directional check a derived type must be *included in* its base, and a derived that omits a required property accepts documents the base rejects.

  **Two config-level fixes were tried and both are wrong**, which is worth recording because each looks right:
  - `required: ["id"]` on the derived passes the derivation check and **silently weakens validation** — with no `properties.id` in scope, `{"id": 123}` and `{"id": "not a gts id"}` both validate. Measured, not argued.
  - `allOf: [{$ref: base}]` composes correctly and then **breaks real data**, because the base's `required: ["id"]` and `additionalProperties: false` reach the payload: a metadata value `{"environment": …, "owner": …}` is refused.

  **Fixed at the root, per GTS spec §4.4.1** *"closed envelope with designated open containers"*. Both envelopes now close their root and declare an open `payload` container, and the inert `id` is no longer `required` — no instance can supply a meaningful value for a field that exists to satisfy a macro. The canonical shape is `gts-macros-cli`'s `BaseEventV1<P>`.

  **The wire is unchanged.** AM stores and exposes the payload content, so `MetadataSchemaRegistry::validate_value` composes the envelope (`{"payload": value}`) at the one seam that validates a whole metadata document. `gts_validation::validate_property_value` checks individual properties and is unaffected. AM's REST contract, its domain models and the e2e suite keep their shapes
- [x] `make dylint` **re-run** after T9a and T10 — workspace-wide, exit 0, zero findings. Unlike the first run at Checkpoint 1, nothing had accumulated: a phase is a short enough window that the two tasks since carried no layering debt (P13)
- [x] **v1 is intact and the new surface is additive (T9a).** Both v1 routes are restored verbatim from `main` and the async surface sits under `/v2/`; three handlers take `TypesRegistryService`, three take `Option<Arc<RegistryService>>`, and neither falls back to the other. `make e2e-local` is green with **no e2e file edited for T9a** — the one e2e file this branch touches, `account_management/conftest.py`, belongs to the envelope fix above and would have been needed with or without T9a
- [x] **Review follow-up: unsupported dry-run fails synchronously, and replays are explicit.** Before T20, unsupported `dry_run: true` was rejected during admission with a canonical `400` field violation, before an operation can be created or stranded in `running`. Successful idempotency replays now include `Idempotency-Replayed: true`; first submissions omit it. Domain and REST regression tests pin both contracts
- [ ] **Human review — everything after this widens the path rather than reshaping it.** Five open items, none of them a failing check:
  - **Another gear owns part of `/types-registry/v1/*`.** `resource-group` registers five routes — `POST|GET /types`, `GET|PUT|DELETE /types/{code}` — inside this gear's service namespace, from `gears/system/resource-group/.../api/rest/routes/types.rs`. T20a, T22a and T38 widen that namespace, so a collision waits for whichever gear registers a conflicting path first. Decide: report to the resource-group owners now, or carry it as a known hazard into T20a/T22a/T38. **T20a's widening did not collide** — it added two `/v2/entities*` routes, and `resource-group` owns only `/v1/types*`; see T20a's *Resolved hazard*. The decision for T22a and T38 is still open
  - ~~**`Idempotency-Key` cannot be declared in OpenAPI.**~~ **Retracted — the claim was false and is now fixed.** `ParamLocation::Header` exists and `openapi_registry.rs:200` already maps it onto utoipa's `ParameterIn::Header`; the generic `OperationBuilder::param(ParamSpec)` declares it. What misled us is that there is no `header_param` convenience beside `path_param` / `query_param`, so the capability is discoverable only by reading the enum. `POST /v2/entities` now declares the header as a required parameter, pinned by `the_idempotency_key_header_is_declared_as_a_required_parameter` and mutation-checked. The remaining toolkit gap is the missing convenience method — filed upstream as constructorfabric/gears-rust#4614. Narrowed by the #4828 review follow-up: `ParamSpec` is now built through `ParamSpec::header` / `::query` / `::path` rather than a field literal, so the location no longer has to be found by reading `ParamLocation`, and the struct is `#[non_exhaustive]` so the next schema keyword added to it cannot repeat this PR's mechanical `format: None, minimum: None` across 12 declaration sites
  - **T2's three lowering decisions were flagged *worth review* and never signed off:** MySQL `DATETIME(6)` rather than `TIMESTAMP(6)`; three extra boolean-domain CHECKs on SQLite and MySQL; MySQL's four indexes declared inline as `KEY`
  - **The migration changed after T2 was marked done** (the uuid binding). Nothing to migrate forward — no deployment had run it — but the "done" marker moved
  - **T10's marker moved too, and the reason generalizes.** The public read returned `content: null` for an admitted Instance; fixed, with the correction written into T10. What is worth deciding rather than just noting: T10's criteria covered admission exhaustively and never named the read, and the same asymmetry stands wherever a task extends the write path — T11, T13 and T20 each add state that `RegistryService::entity()` must then be able to return. Consider a standing criterion for those three: *whatever this task makes storable is readable through the public surface, with a test on the route*. **T11 adopted it** — the criterion is written into T11's verification and discharged by `a_revision_is_readable_through_the_entity_route`; T13 and T20 still need the same, and the decision to make it standing rather than per-task is still yours
  - **Five gear groups have their GTS declarations proven at compile time only**, never admitted into a live registry: `bss-rate-provider` and its plugins, `usage-collector` and its TimescaleDB plugin, `tr-authz`, the OoP calculator example, and `chat-engine` (which `make gts-docs` excludes by policy). They are outside `config/e2e-features.txt`, so the runtime dump covered 34 of the repository's ~40 Type Schema declaration sites. Cheap follow-up once the configs they need exist: repeat the dump with those features enabled

---

## Phase 2 — Revisions and concurrency

**What a new table costs, from here on.** Refactored after Checkpoint 1, so it is not what
T2–T9 did: repositories in `infra/storage/repo/` take and return the row and input types in
`domain/ports.rs` and map their own `SeaORM` models at the edge — one file per repository. A
new table is therefore four edits, not two: the entity, the repository (with its mapper), the
port trait and its types, and a forwarding block in `infra/storage/store.rs`, which holds no
mapping and exists only because the domain wants one `Arc<dyn Stores>`. `entity::Model` must
not leave `infra/storage/repo/`; the pairs of identical `repo::New*` / `ports::New*` structs
that shape removed are the failure mode to avoid re-creating.

**Every port method takes `&DbTx<'_>`.** Not a runner, and not a `&dyn DBRunner` — a dyn
runner can be *coerced* to (sealing prevents implementing the trait, not coercing to it,
which is why mini-chat's `OutboxEnqueuer` takes one) but cannot be *queried* with:
`SecureSelect::one` carries an implicit `Sized` bound that `toolkit_db::outbox` opted out of
(`&(impl DBRunner + Sync + ?Sized)`) and the secure query API did not. So the domain opens the
transaction and hands it down. A new port method that wants a pooled connection is a sign the
call belongs outside the ports.

**A multi-statement read opens `ports::snapshot_read()`; a single-statement one opens a plain
`transaction()`.** The isolation level is the substance, not the transaction: PostgreSQL
defaults to `READ COMMITTED`, where every statement takes a fresh snapshot, so wrapping two
reads in a bare `transaction()` leaves the tear exactly where it was. `snapshot_read` is keyed
on `Db::db_engine()` and returns `TxConfig::default()` for SQLite — the toolkit's doc says
SQLite *"maps other levels to `Serializable`"*, but `SeaORM` does not map them, it logs `WARN`
and ignores them, so an unconditional request put two `WARN` lines in the log per read. SQLite
loses nothing: its reader holds a WAL snapshot or a shared lock for the transaction's duration.
`commit_write()` is the mirror image — `READ COMMITTED`, so a recheck sees what another
admission just committed.

### When a domain concept becomes a directory

`domain/` is one file per concept. `admission/` is a directory because the operation pipeline
has six modules; nothing else has earned one yet, and a directory holding one file is the
premature half of this decision rather than the tidy half.

The grouping axis is **the concept** — which is also its table in `database.sql` — and the
tasks below are where three of them acquire a second file. Take the directory then, not before
and not later; moving three files at T12 is cheaper than moving seven at T22d, and leaving them
flat is how the root reaches 25 files.

| Concept | Directory at | Contents then |
|---|---|---|
| `family/` | **T10 + T12** | `key.rs` (today's `family_key`), `rules.rs` (kind at T10, shape and contiguity at T12), tests |
| `dependency/` | ~~T13 + T14~~ | **Not taken.** The traversal turned out to be SQL, so there was no second file to put beside `extraction.rs`: `domain/dependency.rs` stays one pure file and T14's admission step went to `domain/admission/refresh.rs`. See T14's *Not `TR/src/domain/dependency/`* |
| `compat/` | **T17 + T18** | `baseline.rs` (selection + resolved comparison), `derivation.rs` (major-0 quarantine + the ADR-0014 dialect pin), tests |

Staying flat: `policy.rs`, `artifacts.rs`, `validator.rs`, `gts_store.rs`, `enums.rs`,
`error.rs`, `ports.rs`, `registry_service.rs`, `seeding.rs` — one concept each, the way
`credstore` and `account-management` keep `authz.rs` flat beside their aggregate directories.

**A `rules/` bucket was considered and rejected.** The candidates were `policy`, `family`,
`artifacts`, `compat`, `dependency`, `derivation`, `validator` — but only three of those are
rules (`policy`, `compat`, `derivation`); the rest are derivations: `family_key` is identifier
arithmetic, `artifacts` materializes and digests, `dependency` walks a graph, `validator`
builds a freshness token. The only thing all seven share is having no database and no state,
which is a technical property, not a concept. A directory whose name promises a domain concept
it does not have is worse than a flat list, because the next module that is merely *pure* gets
filed there too.

### - [x] T11: Content revisions and compare-and-swap

**Description:** Second and later revisions of a logical entity: `expected_resource_version`
preconditions, immutable revision insert, current-state pointer move, and the `unchanged`
outcome for authored content equal to current.

**No migration.** T2's `ck_tr_operation_item_state` already carries the `unchanged` arm —
`status = 4` requires `result_revision_no IS NULL` beside a non-null `result_resource_version`
and `expected_resource_version >= 1` — and `OperationItemStatus::Unchanged` already existed in
all three vocabularies (domain, storage, DTO). So the CHECK that makes `unchanged` impossible
for a creation was written before there was any code that could reach it; this task is where
the first row lands in that state.

**Acceptance criteria:**
- [x] Acceptance stops refusing a positive `expected_resource_version` — `AcceptanceError::RevisionNotAccepted` is deleted, along with its REST mapping — **and** the SPEC §8.1 step 3 bypass is restored in the same change: the gate is now `if expected == Precondition::MustNotExist`. What makes that safe is the other half of the same commit: `worker::process_item` dispatches on the item's **stored** precondition, and `commit_revision` refuses an identifier the registry does not hold. The caller's declared kind is therefore *enforced*, not trusted — which is exactly what Checkpoint 1's refusal stood in for
- [x] Update requires `entity.resource_version == expected_resource_version`; mismatch is terminal `precondition_failed` with no silent rebase. It goes through `EntityRepo::compare_and_swap_version` — written and unit-tested at T4 with no caller until now — which became a port method here (its first domain caller, per `store.rs`'s own rule)
- [x] The compare-and-swap returns `Some(next_resource_version)` from the repository and `None` on a lost race. The repository computes `next` with `checked_add` and writes that exact value; the domain never reconstructs the database result or saturates at the numeric ceiling
- [x] A positive precondition on a minor-bearing Type Schema is refused during acceptance: ADR-0004 makes that published contract content-immutable, so a change is registered as the next minor rather than appended as a revision
- [x] Equal authored content yields `unchanged`, creating no revision and not advancing `resource_version`. Both kinds: the rule is shared, the tables are not
- [x] `unchanged` is impossible for a create or a delete, enforced in code as well as by the CHECK. In code it is **structural**: `commit_creation` returns `CommittedUnit` and both revision commit paths return `RevisionCommit`; the early `unchanged` proof is constructed only for `Precondition::Version`. A creation of existing content is `already_exists`, whatever the content
- [x] Equality is exact comparison of canonical authored bytes; effective artifacts are excluded. `CurrentDocument` and `CurrentInstanceValue` carry the bytes, and the decision is `bytes == bytes`. (A stored FNV-1a `content_hash` prefilter was used here first; it was later removed with its column, see T22b.)

**The concurrency shape, because it is not the obvious one.** The commit transaction runs at
`READ COMMITTED` (`ports::commit_write`), so a concurrent admission can commit between reading
the entity and reading the current document. For a real revision the compare-and-swap closes
that by construction — the precondition is in the `WHERE`. For `unchanged` there is no write to
put a `WHERE` on, so the precondition is **re-asked** immediately before the item write. Without
it, a pass that read revision N's content while another admission committed N+1 would answer
`unchanged` against content that is no longer current, and never notice: it runs no CAS. With
it, both interleavings are serializable — a re-read that still sees `expected` means the other
admission had not committed yet.

**Verification:**
- [x] Gear tests, all three backends (see [Commands](#commands)) — 514 on `SQLite`, and `make test-integration LANE=types-registry-db` green on `PostgreSQL` and `MySQL`
- [x] Tests: stale version, equal content, content equal to an *older* non-current revision (must create a new revision, ADR-0005) — `TR/tests/revision_test.rs`, eleven tests
- [x] Test: revision numbers are contiguous per entity — three admissions yield `1, 2, 3` with `resource_version` at 3
- [x] Test: a revision in a region the policy has since **closed** is admitted (the restored bypass), while a creation there is still refused — `a_revision_survives_a_region_the_policy_has_since_closed` drives both halves against two compiled policies, and the acceptance unit tests pin the pure half
- [x] **The standing read criterion, applied.** Checkpoint 1's open item asked that whatever a write-path task makes storable be readable through the public route. `api_rest_test.rs::a_revision_is_readable_through_the_entity_route` reads `resource_version = 2`, the new `content` **and** the re-materialized `resolved_schema` over the real router; `unchanged_content_reports_unchanged_on_the_operation` pins the other outcome on the wire
- [x] **New repository primitives covered on the container backends**, not only on `SQLite`. `TypeSchemaRepo::update_current` rebinds `resolution_fingerprint` — a binary column, the exact class of defect the uuid binding was at Checkpoint 1 — in an `UPDATE` rather than the `INSERT` T3 covered; and `mark_item_unchanged` writes the one `ck_tr_operation_item_state` arm no other test reaches. Both added to `repo_backends_test.rs`

**Two design choices worth naming.**

- **`update_current` is separate from `insert_current`, not one upsert.** An insert that finds a
  row means a missing existence recheck; an update that finds none means a missing first
  admission. One upsert makes both bugs silent, so each returns its own miss and the revision
  commit turns an unexpected one into `WorkerError::CurrentStateMissing` — infrastructure, not a
  statement about the candidate.
- **The Type Schema pointer and its artifacts move in one statement.** D3's artifacts are outputs
  of resolving *that* revision, so a row carrying revision N+1 beside revision N's
  `resolved_schema` is a state no reader should see — and two statements would create it.

**Interim implementation window (SPEC C9).** T11 makes database-backed revisions executable before T14's reverse-impact refresh and T17's compatibility comparison exist. No consumer or v1 cutover may use this path before those checkpoints (the DB path remains internal until T38). During the window, minor-bearing Type Schema revisions are rejected structurally, and `skip_compatibility_check` is rejected whenever it would have a real check to skip; T17 removes that temporary refusal only when it can record `compat_forced` truthfully. Major-only Type Schema revisions remain staging-only until T14 and T17 close the dependent-refresh and compatibility gaps.

**Dependencies:** Checkpoint 1
**Files touched:** `TR/src/domain/admission/{acceptance,acceptance_tests,errors,unit,worker}.rs`, `TR/src/domain/ports.rs`, `TR/src/infra/storage/repo/{entity_repo,type_schema_repo,instance_repo,operation_repo}.rs`, `TR/src/infra/storage/store.rs`, `TR/src/api/rest/{error,dto}.rs`, `TR/tests/{revision_test,api_rest_test,repo_backends_test,repo_test,common/mod}.rs`
**Scope:** M

---

### - [x] T12: Version-family shape and contiguity rules

**Description:** The remaining non-stored rules enforced under the family lock: minor shape must
be uniform within a major, and minors must be contiguous from `M.0`. Both are keyed lookups, not
scans.

**The kind rule landed with T10** (P13) — but *inline in the commit path*, not in a `rules.rs`.
This task's plan said it would "take over the file it opened"; there was no such file, so T12
created `family/rules.rs` and **moved** the kind rule into it. Worth recording because the
correction is the point: all three rules now read as one list, rather than one rule buried in
`commit_creation` and two beside it.

**No migration, and no new port.** Both rules are exact lookups through
`uq_tr_entity_gts_id` on an identifier the key module derives, so `EntityStore::find_by_gts_id`
— which already returns tombstones — is the only primitive either needs.

**Acceptance criteria:**
- [x] `vM.n~` refused while `vM~` exists; `vM~` refused while `vM.0~` exists — `family_shape_conflict`
- [x] `vM.n~` with `n > 0` refused unless `vM.(n-1)~` exists — `missing_predecessor`
- [x] A `DELETED` predecessor still counts; the predecessor test is re-asked inside the commit transaction. Both fall out of one primitive: `find_by_gts_id` returns tombstones and every rule runs inside `commit_creation`'s transaction, so there is no way for the two rules to disagree about what a tombstone means
- [x] Family ownership is write-once; the entity's owner columns are a projection maintained under the lock. `NewEntity` now takes `family.ownership_scope` / `family.owner_tenant_id` instead of re-reading the request — a copy taken from the row it is verified against **cannot** disagree with it, which is stronger than verifying two independent readings. Write-once is structural: `create_or_get` has no update path, so this is the only writer of either column
- [x] The predecessor is excluded from `dependency` and from the revision vector — a **negative** criterion, and the only discharge available now: T13 does not write edges yet and T15's vector does not exist. `a_predecessor_is_not_a_dependency_edge` asserts the table stays empty after `v1.0~` then `v1.1~`, so T13 inherits a failing test if it adds the edge
- [x] ~~Family locks use the configured 5s retry budget; timeout is a retryable `503` with `Retry-After`~~ — **retired at T15 with the family locks.** `worker.write_lock_timeout` is gone, and so is the `503`: the write path serializes on the `entity_write_order` row, whose wait only the backend bounds (SPEC §8.1, and the `TxConfig` ask in §4)

**Both rules are scoped to one MAJOR**, as the compatibility chain is, so a family may hold a
major-only `v1~` beside a minor-bearing `v2.0~` (`database.sql`). Three of the twelve table rows
exist only to pin that, because "uniform within a family" is the plausible misreading.

**The pure/impure split is where the risk is.** `version_probe` returns *which identifiers decide*
this candidate — three variants for the three shapes a last segment can have, so "a first minor
with a predecessor" is not representable. `sibling_id` lives beside `family_key` because it is that
function run backwards, and a rule that spells a sibling differently from the way the registry
stores it is a rule that **silently never fires** rather than one that fails loudly. That is what
`rules_tests.rs` is for: six pure tests, including that every probe stays inside the candidate's
own family and that an Instance probes Instance spellings.

**Verification:**
- [x] Gear tests, all three backends (see [Commands](#commands)) — 514 on `SQLite`; `make test-integration LANE=types-registry-db` green on `PostgreSQL` and `MySQL`. No new backend cases: the rules add no new SQL shape, only more `find_by_gts_id` calls, which the suite already covers
- [x] Table-driven test over shape and contiguity combinations — twelve rows, each its own database, `shape_and_contiguity_over_the_combinations`. Genuine RED→GREEN
- [x] Test: family key derivation maps `v1~`, `v1.4~`, `v2~` to one row, and a preceding-segment minor survives verbatim — the pure half in `key_tests.rs` (unchanged from T8), the *one row* half in `one_family_row_holds_every_version_and_owns_its_members`, which also pins the owner projection
- [x] ~~Test: concurrent first registration under two owners yields one winner~~ — **already covered, and deliberately not duplicated on `SQLite`.** `repo_backends_test.rs::family_race_yields_one_row` (eight concurrent callers, exactly one `created`) and `family_race_inside_a_transaction_yields_one_row` (the loser keeps reading in the same transaction) are T4's, on both container backends. A `SQLite` version cannot exist: a second concurrent writer fails the whole transaction with `database is locked` rather than losing a unique-key race — measured, not assumed. `family_test.rs` carries a named placeholder test pointing at the two that do cover it. *"Two owners"* is P1 language; P0 fixes every row to `ownership_scope = 1` (ceiling C6)
- [x] ~~Tests: a creation holds the family lock across its transaction; configured contention returns a refusal~~ — **retired at T15 with the family locks themselves.** The `entity_write_order` claim makes the whole commit transaction exclusive, so the family rules those locks serialized are already serialized; keeping them also inverted the lock order against ADR-0013's purge protocol

**Family-lock gap closed, and later retired.** T12's `worker::lock_families` took toolkit advisory locks in canonical
`family::lock_order` before opening the commit transaction and holds them through every family
rule and entity insert. Acquisition used the then-enforced `worker.write_lock_timeout`; contention
returns retryable `503 Service Unavailable` with `Retry-After`, while any guards accumulated
before either timeout or a fatal acquisition error are explicitly released through the same
helper as the successful commit path. `SQLite` keeps its toolkit-defined DSN-keyed lock scope, so
tests use unique database files where contention matters rather than assuming row-lock semantics
that SQLite does not provide. A timeout leaves the operation non-terminal; recovery is a replay
under the same `Idempotency-Key`, which `submit`'s `!terminal` branch re-drives.

**Dependencies:** T10, T11
**Files touched:** `TR/src/domain/family.rs` → `TR/src/domain/family/{mod,key,key_tests,rules,rules_tests}.rs`, `TR/src/domain/{mod,enums}.rs`, `TR/src/domain/admission/{unit,worker}.rs`, `TR/src/config.rs`, `TR/src/api/rest/{error,routes}.rs`, `TR/tests/{family_test,config_test,common/mod}.rs`
**Scope:** M

---

### Checkpoint 2
- [x] Revisions and CAS behave; family shape and contiguity hold under concurrency (the kind rule landed with T10)
- [x] Gear tests on three backends (see [Commands](#commands))
- [x] `make dylint` — full workspace, once for the phase (P13)
- [x] Human review

**The one decision this checkpoint owed: the family lock is wired, not deferred.** T12 first
raised it as an open gap and left the choice between T15 and a task of its own; the answer was
neither — it landed straight away, in `5240378e4` and `6b836e247`, which is why T12's note above
reads as closed. Family validation and creation are serialized under
`Db::try_lock` with a retry on transient contention, so the two-different-new-members race T12
described is closed for T10's kind rule at the same time. The same work stops a revision
resurrecting a tombstoned entity and separates corruption from contention in the worker's error
vocabulary. T15 therefore inherits bounded retry over the revision vector only — the lock and
its ordering are already there.

---

## Phase 3 — Dependencies and materialization

### - [x] T13: Dependency edge extraction and writes

**Description:** Extract the edge kinds from authored content — `$ref`, immediate derivation
base, Instance conformance — and replace the admitted entity's outgoing rows on each admission.

An entity reference (`x-gts-type-ref` / `x-gts-instance-ref`) is not a dependency edge. Validation matches the value string against the pattern
without consulting the registry, the target is not inlined into an effective artifact, and the
keyword gives no deletion-safety guarantee. The managed–external boundary will classify candidate
content when federation is introduced. The three stored edge kinds are `$ref`, immediate
derivation base, and Instance conformance.

**Unchanged by T10's move** (P13). T10 made the *forward* closure reach a candidate's
derivation chain from the identifier, so derived schemas and Instances no longer wait on this
task. What still needs rows here: `$ref` targets, which are not derivable from any identifier,
and derivation/conformance, which stay materialized because T14's **reverse**
walk has no identifier to walk backwards from — the criterion below already says so. Both endpoints are always managed entities. This is also
the same extractor the worker uses for in-batch ordering.

**Writing the rows is not enough — the extractor also runs on the read side.** Rows are
replaced at commit, so they do not exist while the candidate that authored them is being
validated: on a first admission `load_unit_store` walks an edge set that says nothing about
this candidate, and a `$ref` to an entity outside the candidate's identifier chain never
reaches the store. `validate_schema` then refuses a target the registry holds. A revision has
the mirror problem — the stored edges are the *previous* revision's, so a reference the
candidate added is not followed and one it dropped still is. `load_unit_store`'s
"the candidate overlay wins" today covers documents but not edges. The fix is the same pure
extractor, called over the candidate document *before* the closure read, its targets seeded as
closure roots beside the candidate's `chain_ids()`. Found at the PR #4641 review.

**Acceptance criteria:**
- [x] An entity reference (`x-gts-type-ref` / `x-gts-instance-ref`) creates no edge and has no reader in the P0 dependency implementation
- [x] Admission replaces only the admitted entity's outgoing rows, through the existing `DependencyRepo::replace_outgoing` — written and unit-tested at T4, with no caller until this task
- [x] Derivation and conformance are materialized even though derivable from the identifier
- [x] `$ref` extraction uses `gts-rust`'s extractor, never a local scan
- [x] Extraction is exposed as a pure function over authored content, callable without a database — `domain::dependency::extract_edges(&GtsId, &Value)`, no clock and no ports in its signature, which is what lets the same call serve both sides of the admission
- [x] `load_unit_store` seeds the closure with the `$ref` targets extracted from each candidate **document**, alongside the candidate's `chain_ids()` — a first admission has no stored edges of its own, so the identifier-derived seed is the only thing the closure would otherwise see. An entity-reference target is not seeded because validating the keyword never reads the target document
- [x] For a revision the roots come from the candidate document's references, never from the previous revision's stored edge set: the overlay wins for edges as it already does for documents. **With one honest limit,** stated at `load_unit_store`: the candidate's *stale* rows are still walked, because `closure` cannot tell a candidate root from any other, so a reference a revision dropped may still be loaded. That is inert — resolution follows the document, and an extra registered schema answers no question — while the direction that is not inert holds: a reference the candidate **added** resolves
- [x] A reference target that genuinely does not exist is distinguishable from a candidate that is not stored yet — `UnitStore::missing_references` beside `missing_candidates`. **No port change was needed.** `closure` already reports every root with no entity row; which of those roots is a candidate is `load_unit_store`'s own knowledge, so the split is one `partition` over the existing result rather than a second list crossing the seam. `missing_candidates` still has no production reader, so T19 inherits both lists and no ambiguity
- [x] The `CLOSURE_BOUND` accounting covers the added roots: `ensure_within_bound` charges `roots.len()` before the first hop, and the seed *is* that vector — reference targets are pushed into it, not passed beside it, so no accounting change was possible to forget

**The dependency reader is `gts-rust`.** `$ref` goes through `gts::extract_gts_refs`, the
canonical definition shared with the resolution that validates the candidate. No local scan
interprets either `$ref` or an entity reference as a dependency.

**An Instance carries exactly one edge, and that is a rule rather than an omission.** `$ref`
and the entity-reference keywords are schema keywords, so the same strings inside a *value* are data.
Extracting them would invent an edge from a coincidence, and a malformed one would refuse a
valid value — `an_instance_values_ref_shaped_data_is_data_and_not_an_edge` pins it.

**Where the edges travel.** Extracted in `evaluate`, where the document is already parsed and
no transaction is open, and carried on `EvaluatedUnit::edges` as target *identifiers*. The
commit resolves them to rows, because that answer changes between evaluation and commit. Every
edge target must resolve: an absent base, conforming schema or `$ref` target is a terminal
`dependency_not_found` candidate refusal. If an entity identity disappears between evaluation
and commit, `DependencyTargetAbsent` is a permanent system failure: P0 tombstones entities and
does not physically remove them. No incomplete edge set is written. A malformed `$ref` still
fails at extraction as `invalid_schema`.

**Verification:**
- [x] Gear tests, all three backends (see [Commands](#commands)) — 531 on `SQLite`, six consecutive full-suite runs green; `make test-integration LANE=types-registry-db` green on `PostgreSQL` and `MySQL`. No new backend cases: the writes go through `replace_outgoing` and the resolution through `find_by_gts_ids`, both already exercised on both container backends by T4's `closure_walks_a_chain`. T13 adds no new SQL shape
- [x] Table-driven test over edge-kind fixtures, including a derived schema carrying both schema edge kinds, a reference-free schema, and an Instance of a derived type; focused no-edge tests cover `x-gts-type-ref` and `x-gts-instance-ref`
- [x] Test: re-admission removes an edge the new revision dropped — `a_revision_removes_the_edge_it_dropped_and_adds_the_one_it_gained`, with `a_revision_that_keeps_its_reference_keeps_the_edge` for the other direction, because "replace" could otherwise be read as "clear and forget"
- [x] Test: a new schema whose `$ref` names an existing schema **outside its identifier chain** is admitted — the case that failed with `invalid_schema`. It was already pinned, inverted, as `admission_worker_test::a_ref_outside_the_chain_still_fails`; that test is now `a_ref_outside_the_chain_is_admitted` and its flip is the RED→GREEN record. `dependency_test.rs` pins the rows the same admission writes
- [x] Test: a revision that adds one reference and drops another resolves against the new set, not the stored one — `gts_store_test::a_revision_resolves_against_the_reference_it_now_carries`. All three new store tests were verified RED with the one seeding line disabled
- [x] Test: a reference to an identifier no entity carries is reported as a missing reference, distinct from the candidate's own absence — `a_reference_no_entity_carries_is_a_missing_reference_not_a_missing_candidate`, and end to end `an_entity_ref_writes_no_row_whether_its_target_exists_or_not` against `a_ref_naming_no_entity_fails_the_candidate`: same absent identifier, two outcomes, because a pattern is satisfiable before anything matches it while a `$ref` is not

**Two existing tests had to be inverted, and both were boundary markers rather than
regressions.** `admission_worker_test::a_ref_outside_the_chain_still_fails` asserted the gap
this task closes. `instance_test::an_instance_admits_against_a_type_from_an_earlier_operation`
asserted an **empty** `dependency` table to prove conformance is identifier-derived; it now
asserts exactly one row, from the Instance to its type, and reads the type's own empty
outgoing set to keep making the original point — the resolution above needed no row.

**SQLite family-lock tests use isolated family keys.** The `SQLite` lock backend is a marker
**file** at `<cache_dir>/cf-gears/locks/{database_scope}/{hash(gear, key)}`
(`toolkit-db/src/advisory_locks.rs`), and `database_scope` for `sqlite::memory:` is identical for
every test in the workspace. So contention is per *lock key*, across processes, however isolated
the databases are — and `nextest` runs each test in its own process. Sibling tests in
`family_test` all admit some version of
`cf.core.example.thing`, they all take that one family key, and the probe carries a 1 ms budget
with no retries.

The lock probe admits `cf.core.lockprobe.thing.v1~`, a family no other test touches. The Instance
and dependency suites likewise own `cf.core.inst.*` and `cf.core.dep.*`. This isolates lock keys
without bypassing the real marker-file lock mechanism. Eight consecutive full-suite runs are
green.

**Dependencies:** Checkpoint 2
**Files touched:** `TR/src/domain/{dependency,dependency_tests}.rs` (new), `TR/src/domain/{mod,enums}.rs`, `TR/src/domain/gts_store.rs`, `TR/src/domain/ports.rs`, `TR/src/domain/admission/{errors,unit}.rs`, `TR/src/api/rest/error.rs`, `TR/src/infra/storage/store.rs`, `TR/tests/dependency_test.rs` (new), `TR/tests/{gts_store_test,admission_worker_test,instance_test,family_test,revision_race_backends_test,common/mod}.rs`, `docs/{PRD,DESIGN}.md`, `docs/database.sql`, and `docs/p0/{SPEC,plan}.md`
**Scope:** M

---

### - [x] T14: Reverse-impact traversal and artifact refresh

**Description:** The reverse-impact read over `dependency` and the refresh of every
affected dependent's effective artifacts in the same transaction as the new revision, with
the fingerprint-stability stop and the `activation_write_set` bound (D5).

**The traversal is one scoped recursive CTE, and the refresh is a loop.** The task was
planned as a worklist for both halves on the premise that a recursive CTE needs raw SQL,
which `11_database_patterns.md` forbids outside migrations. That premise does not hold:
`toolkit-db`'s ADR-0001 (`SecureCteSelect::recursive_cte`) builds a scoped `WITH RECURSIVE`
entirely through `sea-query`, scope embedded in both the seed and the recursive member. So
the traversal became one statement — it runs inside the commit transaction, where round
trips are paid for in lock contention — and only the refresh stayed a loop, because the
fingerprint stop decides the write set by *recomputation*, which no closure query can
express. SPEC §D5 records the split.

**Acceptance criteria:**
- [x] A dependent whose recomputed artifacts are identical is not written and does not move `resource_version` — nor its `revision_no`, nor `updated_at`. Also verified as *examined*: the early-stop test asserts `examined == 2` beside an empty write set, so an empty impact set cannot make it pass
- [x] Every affected dependent's artifacts become current in the same transaction as the new revision — the refresh takes the caller's `&DbTx`, and `commit_revision` calls it after `replace_edges`, so a dependent sees this revision's reference set
- [x] Exceeding `limits.activation_write_set` fails the candidate with a structured reason (`activation_write_set_exceeded`) and commits nothing partial
- [x] `ponytail:` comment names the measured max fan-out (27), the bound (512) and the staging upgrade path — on `DependencyRepo::reverse_impact`
- [x] The traversal terminates on a row that contradicts acyclicity. **The criterion was "terminates on a cyclic graph", and an admitted graph cannot be cyclic** — admission is what keeps it acyclic (ADR-0012, rewritten here with PRD/DESIGN/`database.sql`): derivation strictly shortens the `~`-chain and nothing references an Instance, so those two cannot close a cycle at all, while `$ref` can — alone, or combined with derivation, where a base `$ref`s a schema derived from it. gts-rust refuses the first with `Circular $ref detected`, and T19 refuses both over the combined edge set. The termination property is kept as defence in depth, since a contradicting row would otherwise hang a commit transaction, and the invariant itself is now pinned where it is enforced

**What made a depth-capped CTE safe.** `recursive_cte` requires a `max_depth` and truncates
**silently** past it — a dependent left out keeps stale artifacts marked current, the one
failure this read must not have. The cap is therefore the write-set bound itself, which
makes truncation unreachable below the refusal: seed rows carry depth `0`, so a dependent at
shortest distance `d` appears at depth `d - 1`; a hidden dependent would need a path of at
least `bound + 2` edges, and every one of its `bound + 1` intermediate dependents is nearer
and so already in the set — which puts the set over the bound, where the refusal has already
fired. A returned set is complete; an incomplete walk is an error.

**A refusal after the writes began needed its own channel.** `commit_revision` returns
`Ok(Err(ItemFailure))` for every other candidate refusal, but those are all reached *before*
a write. This one cannot be: the refresh must see the committed revision. Returning it in the
`Ok` position would **commit** the revision it refuses, so it travels as
`WorkerError::RefusedAfterWrite(ItemFailure)` — an error, which rolls the transaction back —
and `process_item` unwraps it into the same `record_failure` path an evaluation-stage refusal
takes. Invisible past the worker; `an_over_bound_write_set_commits_nothing` is what would
have caught the mistake.

**`limits` now reach the worker.** `run_operation` takes `&Limits` and carries it to
`commit_revision`; `activation_write_set` moved off `inert_limit_keys` and gained a
zero-refusal in `validate()`, alongside the other enforced limits. `resolved_document` and
`resolution_closure` are also enforced, per document, during evaluation and refresh. A
refresh refusal uses the same rollback channel as the activation-write-set refusal.

**Two shapes came out of the rebase onto the family lock, not out of T14's design.** `Limits`
and `WorkerSettings` both travel per item, which puts `process_item` one argument over
Clippy's threshold, so T14 groups them as `ItemConfig` and `limits` rides on
`CommitRequest` from there — T15 then replaces that grouping with its own borrowed
`Tuning`, which does the same job for the same reason. And `commit_revision` crossed the
200-line lint once the refresh call joined the tombstone and lock rechecks, so its
`unchanged` branch became `commit_unchanged`: a re-read standing in for the
compare-and-swap a real revision carries in its `WHERE`. T15 had planned that same
extraction for its own clippy bound and inherits it verbatim.

**Verification:**
- [x] Gear tests, all three backends — `cargo nextest run -p cf-gears-types-registry`: **550 passed** (544 before the rebase; the six added are the family-lock suites this branch rebased onto); `--features integration` on `repo_backends_test`, `migration_backends_test` and `revision_race_backends_test`: **6 passed** (PostgreSQL + MySQL containers)
- [x] Test: revising a base with N dependents refreshes exactly N `type_schema` rows — `refresh_test.rs::revising_a_base_refreshes_every_dependent_schema`, over a derived type and an out-of-chain `$ref`erer, asserting the new property reaches the refreshed artifacts
- [x] Test: over-bound case commits nothing — the base's `resource_version`, its whole current row and both dependents' fingerprints are byte-identical after the refusal
- [x] Test: the walk terminates on a cyclic row — `dependency_repo_test.rs` and `repo_test.rs`, both writing the cycle straight through the repository, plus the `PostgreSQL`/`MySQL` case, where a non-terminating backend would hang rather than fail
- [x] Test: acyclicity is enforced, not assumed — `dependency_test.rs::a_revision_that_would_close_a_ref_cycle_is_refused` pins the `invalid_schema` refusal and that no edge is written. The in-batch half (two candidates referencing each other under one overlay) belongs to T19 and is listed there
- [x] `make fmt`, `make clippy` (`--all-targets --features integration`) — clean

**Added:** `TR/tests/dependency_repo_test.rs` (7 tests: transitive reach, root exclusion,
every edge kind, empty set, the bound, no-truncation-past-the-cap, cyclic-row termination),
`TR/tests/refresh_test.rs` (5 tests), `reverse_impact_walks_back_up_a_chain` in
`repo_backends_test.rs`, `a_revision_that_would_close_a_ref_cycle_is_refused` in
`dependency_test.rs`.

**Dependencies:** T13
**Files touched:** `TR/src/infra/storage/repo/{dependency_repo,mod}.rs`,
`TR/src/domain/admission/refresh.rs` (new), `TR/src/domain/admission/{mod,unit,worker,errors}.rs`,
`TR/src/domain/{ports,dependency}.rs`, `TR/src/infra/storage/store.rs`, `TR/src/config.rs`,
`TR/src/api/rest/error.rs`, `TR/src/domain/registry_service.rs`, `TR/tests/*`, plus the
document rewrite: `docs/{PRD,DESIGN,database.sql}`, `docs/ADR/0012-*`, `docs/p0/{SPEC,plan}.md`

**Not `TR/src/domain/dependency/`.** The plan called for splitting that file into
`extraction.rs` + `worklist.rs`. There is no worklist module: the traversal is SQL, and what
remained is an admission step that speaks `ItemFailure` / `WorkerError` and builds a transient
store — `unit.rs`'s neighbours, not extraction's. It went to
`domain/admission/refresh.rs`, and `domain/dependency.rs` stays one pure file.

---

### - [x] T15: Revision-vector guard and bounded retry

**Description:** The multi-pod correctness guard (D4): record a revision vector for every
correctness-relevant dependency and dependent during evaluation, then under the target's
entity lock re-derive the reverse-impact set from the database and compare both membership
and the full vector, rolling back and revalidating within a bounded retry policy.

**Membership is re-derived from the recorded *roots*, not re-read from the recorded rows.**
Comparing versions of the entities evaluation happened to see catches a dependency that
*moved* and misses one that *appeared* — a transitive dependency pulled in because some
intermediate entity gained an edge, and the phantom dependent, which is one of the
criteria. So `RevisionVector` carries the closure roots (the candidate identifier plus its
document's `$ref` targets, a pure function of the authored document) and the commit re-runs
`closure` and `reverse_impact` from them. One derivation function serves both sides, or the
comparison would measure the difference between two readers instead of two states.

**The vector is recorded inside the store-build transaction.** `evaluate` already opens one
`snapshot_read` around `load_unit_store`; the vector's reads join it. Anywhere later and the
comparison would be measuring a gap that opened *before* the evaluation rather than after
it. `UnitStore` grew two accessors for this: `roots()`, because the roots are the closure's
*question* and cannot be recovered from its answer, and `closure_entities()`, because the
vector's dependency half **is** that answer — so evaluation calls `vector::derive_from` with
the rows the store builder already walked, and only the commit side pays for the walk
(`vector::derive`). One function builds the vector on both sides regardless, which is the
property the comparison depends on.

**Step 4.2's row locks over the revision vector are the wrong tool, and the documents now say so rather than deferring them** (P15; DESIGN §4 corrected, no ceiling). A lock guarantees only that nothing moves *after* it is taken, and the movement that matters happens between evaluation and the lock — the phantom dependent appears before any lock could be held. So the comparison is required either way and the lock is purely additive: it buys waiting instead of rolling back, which is liveness. It costs one round trip per vector member inside the commit transaction on a set the bound allows to reach 512, and it is the only reason step 4.2's canonical ordering had to extend past families at all. What serializes the rows instead was already there: the candidate's own row by the compare-and-swap that writes it, and a dependency by the write-write conflict its own refresh creates on the candidate's `type_schema` row precisely when the move affects it — and where it does not, the fingerprint-stability stop is the proof the staleness is inert. That conflict *orders* the two writes and nothing more: a refresh computes its artifacts before it writes, so the resumed `UPDATE` re-evaluates its predicate, not its payload. The refresh's write therefore carries the revision and fingerprint its own read saw, and the loser rolls back rather than overwriting the winner. **Found in review after this task**, and corrected here rather than left as a claim the code did not make good on. The missing `FOR UPDATE` is corroboration, not the reason.

**One lock does survive the argument, and it orders commits.** The optimistic mechanism
reaches everything that meets on a row. It does not reach an edge committed *after* a mover's
reverse scan: adding an edge moves no `resource_version` and writes only `dependency`, so the
two commits write no row in common, both pass their own guards, and the dependant keeps an
artifact inlined from a revision that is no longer current with a fingerprint that matches it
— no drift to report and nothing later to repair it. The requirement is a **serialized write path**:
every commit claims the `entity_write_order` row of `types_registry__coordination_state` as its
transaction's first statement.
Either the edge is in `dependency` when the mover's scan runs after that claim, or the unit
writing it has committed nothing and its own guard sees the mover's revision when it does —
two cases, no third. A row rather than an advisory lock, because advisory keys live on a
session separate from the transaction's connection: losing it releases the key while the
transaction carries on, and the exclusion lapses silently.

Consequences for this task: the family lock is no longer taken for a new member only, since
the family advisory locks are gone entirely — the claim makes the transaction exclusive, so
the rules they serialized already are, and keeping them inverted the lock order against
ADR-0013's purge protocol. The vector guard stays load-bearing
for the window the lock does not cover — evaluation runs outside it, so a commit landing
between evaluation and lock acquisition is exactly what
`a_dependency_mutated_between_evaluation_and_commit_costs_one_rollback_and_one_retry` holds a
pass at its evaluation closure read to produce. Every other writer of entity state must take
the lock too, or the order is not total: deletion at T20, the purge job under ADR-0013.
**Found by review, not by a test** — which is the honest provenance;
`a_new_dependant_cannot_commit_while_its_dependency_is_being_revised` is the test it should
have had.

**The `unchanged` branch is not guarded, and that is the decision, not an oversight.** It
writes no revision, moves no version and refreshes no dependent, and the one thing it
decides concerns the current authored document. Guarding it could only turn a
genuine no-op re-submission into a revalidation because a *neighbour* moved, and after
`max_revalidation_attempts` of those, into a failure.

**The retry lives in `process_item`, not in `transaction_with_retry`.** A drift means the
evaluation is void rather than wrong, so what has to be redone is the whole of step 3 — a
fresh snapshot, a fresh transient store, fresh validation. Re-running the same transaction
would compare the same stale vector and drift identically, which is why
`RevalidationRequired` reads as `None` to `retryable_db_err`.

**Early `unchanged` check.** Before evaluation, revision submissions compare the current
canonical authored bytes in a short read-only snapshot. A match produces
an internal proof carrying the entity ID and expected `resource_version`, without loading
the dependency closure, deriving a vector, validating, or materializing artifacts. Its
commit still claims `entity_write_order` as the first SQL statement, then checks that the
same entity exists, is live, and has the recorded version before CAS-terminalizing the item.
Every authored-content write advances that version; dependency refreshes do not change the
bytes, so their fingerprints need no guard here. A concurrent edit fails the precondition;
a concurrent deletion reports `entity_deleted`. The proof cannot be constructed for a
creation. A probe miss releases its snapshot and runs the ordinary evaluation/commit path;
this adds one small read-only transaction to genuine revisions.

This fixes the case where an identical submission was refused during evaluation because
its dependents exceeded `activation_write_set`. No-op submissions also bypass resolution
and materialization budgets, since they perform neither operation. Real edits retain all
those bounds. Tests cover bounded no-ops, a changed conforming type for an unchanged instance,
and concurrent edits, deletion, and dependency refresh between the probe and commit.

**`limits.activation_write_set` is asked twice for evaluated edits**, because the vector's reverse-impact
read is the refresh's read. An over-bound candidate is refused at *evaluation* under the
same `activation_write_set_exceeded` reason — earlier, cheaper, and invisible to a client;
T14's refusal stays as the backstop for a set that grew in between.

**Acceptance criteria:**
- [x] Vector carries `resource_version` and, where effective content was consumed,
  `resolution_fingerprint` — the latter for live Type Schema dependents, whose effective
  artifacts the refresh consumes to decide whether to rewrite them. `None` for a
  dependency (only its authored document is read, and that moves only with
  `resource_version`), for an Instance dependent (`instance` carries no artifacts) and for
  a tombstone — the two the refresh skips
- [x] A new, removed or moved dependency/dependent rolls the transaction back —
  `VectorDrift::{Appeared, Vanished, Moved, Refreshed}`, travelling as
  `WorkerError::RevalidationRequired`, which is what rolls it back. `Refreshed` is the
  fourth shape and not a redundant one: a refreshed dependent moves no version
- [x] Every artifact write carries a compare-and-swap on `(revision_no,
  resolution_fingerprint)` — `CurrentSchemaCas` — and a miss is
  `VectorDrift::CurrentProjectionMoved`. The two paths differ in what the token is: for the
  **refresh** it is the state its artifacts were computed against, carried out of the read
  that selected the document (`CurrentDocument::projection`); for the **candidate's own
  revision** the artifacts predate any transaction, so its token is read inside the commit and
  is a post-guard sentinel — guard establishes the evaluation still holds, sentinel establishes
  nothing wrote the row since. It is needed because the entity compare-and-swap does not cover
  that row.
  **Found in review, not by a test**: the row lock orders two writes and recomputes neither,
  so the unconditional write let the loser put artifacts from before the winner over it,
  paired with a `revision_no` read afterwards.
  `a_refresh_write_is_a_compare_and_swap_on_the_fingerprint_it_read` pins the predicate.
  **Two interleavings are owed on the container backends** and neither can run on `SQLite`:
  a dependent revising itself under a refresh, and a candidate refreshed by the edge it
  drops. Both were written and both passed unfixed — `SQLite` either refuses the concurrent
  writer with `database is locked` or forces a transaction retry that re-reads the token, so
  the stale-token window never opens. `PausingStores::new_at_occurrence` was added for them
  and is what the container versions need
- [x] Retries are bounded by `worker.max_revalidation_attempts`; exhaustion terminalizes
  the item as `failed` — reason `revalidation_exhausted`, message naming the last drift.
  The key moved off `inert_limit_keys` and gained a zero-refusal in `validate()`, as
  `activation_write_set` did at T14
- [x] ~~Lock order is family → entity/current rows, in canonical identifier order, everywhere.~~ **Both halves are retired, for different reasons.** The row half was mistaken: a lock over the revision vector cannot do the guard's job and costs a round trip per member inside the commit transaction (P15, SPEC §8.1 step 4.2). The family half became redundant and then harmful — see the lock paragraph above. What replaced both: the `entity_write_order` claim orders commits, and no family lock is taken on either commit path — the claim makes the whole transaction exclusive, so the family rules are already serialized. Canonical order is kept where it is observable and where locks are actually many: the currently uncalled `lock_order` helper sorts and dedups family keys for the next writer, and the vector is `(gts_id, role)`-sorted on both sides, which is what makes the comparison one merge walk and the reported drift deterministic

**Verification:**
- [x] Gear tests, all three backends — `cargo nextest run -p cf-gears-types-registry`:
  **570 passed** (564 before the rebase onto the family lock); `--features integration` on `repo_backends_test`,
  `migration_backends_test` and `revision_race_backends_test`: **6 passed**
  (`PostgreSQL` + `MySQL` containers)
- [x] Test: a dependency mutated between evaluation and commit causes exactly one rollback
  and one successful retry —
  `revalidation_test.rs::a_dependency_mutated_between_evaluation_and_commit_costs_one_\
rollback_and_one_retry`. "One rollback" is read off the versions (revision 2, not 3);
  "successful retry" off the artifacts, which inline the base's **new** property — only a
  fresh evaluation could have produced that, and a committed stale one would still inline
  the old
- [x] Test: a phantom dependent created after the initial scan is detected — and detected on
  *membership*, with no column of any recorded entry changed
- [x] Test: two pods against one database — a commit on one is visible to the other's first
  post-commit read. Two `DBProvider`s with their own pools over one database file; B's
  miss is asserted first, so the hit is a read it actually performed
- [x] Mutation-tested: with both `vector::guard` calls removed, the six guard tests fail and
  the three controls (`a_commit_whose_vector_did_not_move_stands`, the `unchanged`
  re-submission, the two-pod read) still pass — which is the shape that says the suite
  measures the guard and not the fixtures
- [x] `make fmt`, `make clippy` (`--all-targets --features integration`) — clean

**Added:** `TR/src/domain/admission/vector.rs` + `vector_tests.rs` (10 tests over the pure
comparison), `TR/tests/revalidation_test.rs` (9 tests),
`current_projections_read_every_named_entity_that_has_one` in `repo_backends_test.rs`,
`a_zero_revalidation_budget_fails_startup` in `config_test.rs`,
`PausePoint::RevisionEntityRead` in `tests/common/mod.rs`.

**Batched state checks use `current_schema_projections`.** The port returns only
`entity_id`, `revision_no`, and `resolution_fingerprint`; vector derivation and refresh
do not load the three materialized documents merely to compare state. Instance evaluation
uses the same projection to record its conforming type's revision. `current_documents`
uses this narrow SQL projection for its pointer read and selects only identity and authored
text from the revision table. `find_current_schema` retains the full
artifacts for entity reads. Transaction boundaries and CAS checks remain unchanged.
`schema_projection_test.rs` checks the executed SQL excludes unused payload columns;
the repository backend suite verifies projection values on PostgreSQL and MySQL.

**Clippy-driven extractions, worth naming because they are also better shapes.**
`process_item` passed the cognitive-complexity bound once the revalidation loop went in, so
the success mapping became the pure `committed_outcome`, and `limits` + `worker` became one
borrowed `Tuning` — which supersedes T14's `ItemConfig`, the same grouping under a name that
also carries the worker tuning. `commit_unchanged` was T15's third such extraction until the
rebase: T14 had already reached the 200-line bound on `commit_revision` and cut it the same
way, so T15 keeps the docstring that states the re-read argument and adds nothing else.

**Dependencies:** T14
**Files touched:** `TR/src/domain/admission/{vector,vector_tests}.rs` (new),
`TR/src/domain/admission/{mod,unit,worker,errors}.rs`, `TR/src/domain/gts_store.rs`,
`TR/src/domain/ports.rs`, `TR/src/domain/registry_service.rs`,
`TR/src/infra/storage/{store.rs,repo/type_schema_repo.rs}`, `TR/src/api/rest/error.rs`,
`TR/src/config.rs`, `TR/tests/*`, `docs/p0/{SPEC,plan}.md`

### - [x] T16: Observability for the admission path

**Description:** Instrument admission so production behaviour is diagnosable: structured
spans per operation and per admission unit, and counters for the outcomes and bounds that
matter.

**Acceptance criteria:**
- [x] One span per operation and one per admission unit, carrying `operation_id`, `gts_id`,
  kind and dry-run mode. `types_registry.admission.operation` opens **before** the first
  read, so it covers the whole pass — which is why its `kind` and `dry_run` are
  `field::Empty` and filled in by `record_operation_facts` once the operation row is
  read. A pass that fails before that read therefore carries *no* `kind` label rather
  than a blank one, which is its own test. `types_registry.admission.unit` restates
  `operation_id`, `kind` and `dry_run` beside `gts_id` and `operation_item_id`:
  deliberate duplication, because under a flat log format a field that lives only on the
  parent span is not on the line an operator greps
- [x] Counters: candidates by terminal status, refusals by reason, revalidation retries,
  activation-set size, worker duration. Five instruments —
  `types_registry_candidates_total{status}`, `types_registry_refusals_total{stage,reason}`,
  `types_registry_revalidations_total{drift}`, `types_registry_activation_write_set` and
  `types_registry_operation_duration_seconds`. **Every label value comes from a closed
  vocabulary** and no identifier is ever a label: the `drift` label is the *shape* of the
  drift (`appeared` / `vanished` / `moved` / `refreshed` / `current_projection_moved` — the
  last one added with the refresh compare-and-swap, and the only shape raised by a write
  rather than by the vector guard), not the `gts_id` that drifted, because that one is
  unbounded and belongs on a span
- [x] Every refusal reason in the acceptance path is countable and distinguishable —
  `AcceptanceError::reason()`, one arm per variant over an exhaustive match, so a refusal
  a later task adds cannot compile until it has a reason — demonstrated by the rebase onto
  the family-lock work, whose `force_compatibility_unavailable` and
  `minor_type_schema_revision` refusals broke the build here until both were named. Distinct from the RFC-9457
  `reason` code `api/rest/error.rs` maps onto, which is deliberately coarse (six variants
  share `VALIDATION_FAILED`): a client branches on the class, an operator needs the
  variant. T17's `Unknown` compatibility verdict is refused in the **worker**, so it
  earns an `ItemFailure` reason and is counted by the same `refusals_total` under
  `stage="admission"` with no change here
- [x] Structured fields only; no print macros (DE13xx) — nothing added uses one

**Verification:**
- [x] Gear tests (see [Commands](#commands)) — `cargo nextest run -p cf-gears-types-registry`:
  **592 passed** (570 before, +22: 12 in-source, 10 integration; 586 before the rebase onto
  the family lock, whose own six suites are the difference).
  `--features integration` on the three container suites: 6 passed
- [x] Test: the instrument contract — rendered names, label keys, label values and bucket
  layouts — asserted against a local `InMemoryMetricExporter`, not reviewed. That is what
  a dashboard depends on, and a dropped `_total` or a renamed label value is invisible in
  a code review and silently empties a panel
- [x] Test: the emission sites, end to end through the real `accept` / `run_operation`.
  `tests/observability_test.rs` installs its own global meter provider and a
  `types_registry=debug` subscriber for the binary, then drives a success, an `unchanged`
  re-submission, an admission refusal, two acceptance refusals, a revision that refreshes
  one dependent, and a real revalidation retry through `PausingStores`
- [x] Mutation-tested: with all eight emission calls and both `.instrument()` layers
  removed, **all 10** integration tests fail; restored, all 10 pass. The suite measures
  the emission and not the fixtures
- [x] `make fmt`, `make clippy` (`--workspace --all-targets --all-features`) — clean
- [x] Manual, at runtime: booted the focused example server
  (`--features static-tenants,static-authn,static-authz,otel`) with the metrics exporter
  pointed at a local OTLP/HTTP sink, then `POST /cf/types-registry/v2/entities` three
  times — an admitted schema (`202`), a revision naming version 1 of an absent identifier
  (`202`, item `failed`), and an empty batch (`400`).
  **Spans:** both appear on the real log lines, nested inside the gateway's own
  `http_request` span —
  `…:types_registry.admission.operation{operation_id=… kind="registration" dry_run=false}:types_registry.admission.unit{operation_id=… gts_id="gts.cf.core.t16.probe.v1~" kind="registration" dry_run=false operation_item_id=1}: … candidate admitted`.
  **Counters:** the pushed OTLP payloads carry
  `types_registry_candidates_total`, `types_registry_refusals_total` and
  `types_registry_operation_duration_seconds`, with the label pairs
  `status=succeeded`, `status=failed`, `stage=acceptance`, `stage=admission`,
  `reason=empty_batch` and `reason=precondition_failed` all present as
  protobuf key/value pairs. Metrics reach a collector only by OTLP push — this gear
  declares no exporter and no `/metrics` endpoint — so the sink is what makes the
  check observable at all
- [x] **One pre-existing boot failure found and set aside, not caused by T16.** With
  `config/quickstart.yaml` unchanged, the focused server dies at types-registry post-init:
  the seeded `…am.tenant_type.v1~cf.core.am.platform.v1~` derives from
  account-management's base schema, which this feature set does not link
  (`Base schema 'gts.cf.core.am.tenant_type.v1~' not found for chain validation`).
  The manual check therefore ran against a copy of the config with `entities: []`.
  Attributed rather than assumed: the failing step is `switch_to_ready`'s chain
  validation on the **legacy in-memory** path, which T16 does not touch — no line of this
  task is on that path — and the missing base schema is a feature-selection fact about the
  focused server, not a registry defect. Not run against `main`'s tree, so "pre-existing"
  here means "independent of this task", which is what the call graph shows

**Two design decisions worth stating, because both are trade-offs:**

**The instruments are a domain port; only the spans are process-global.** *(Record corrected — P16. This entry and the commit message both argued the opposite, for a process-global instrument set reached the way `tracing` is. The code that shipped does not do that, and the port is the shape to extend.)* `domain::ports::metrics::AdmissionMetrics` is the trait, `infra::metrics` the OpenTelemetry adapter, and `TypesRegistryGear::init` injects the `Arc<dyn AdmissionMetrics>` into the service, which carries it down `run_operation` → `process_item` → `commit_evaluated` → the commit transaction's `'static` closure → `commit_creation` / `commit_revision` → the reverse-impact refresh. The `'static` closure is why the parameter is an `Arc` and not a reference: each retry attempt clones the handle. A caller with no meter passes `NoopMetrics`, which is the pre-T16 behaviour exactly, and that is what the several dozen existing worker test call sites pass. The deciding argument is the layer boundary: an OpenTelemetry type reached from `domain` through a crate-root module hides from `de0301_no_infra_in_domain`, and the emission sites are all on the admission call path. `observability.rs` keeps the two span constructors as free functions, because a `tracing` span *is* a global sink the emitting code neither carries nor injects, and its module header states the split. The instrument names carry a configurable prefix (`MetricsConfig`, default `types_registry`) and the rendered names are pinned by `infra::metrics_tests`.

**The activation-write-set histogram counts revisions, not admissions.** A creation
observes **nothing** rather than zero, because nothing can depend on an identifier the
registry did not hold a moment ago and `commit_creation` runs no reverse-impact refresh at
all. A zero *is* recorded for a revision whose dependents all recomputed to identical
bytes. Both halves are tests, and the creation one carries a control assertion so its zero
reads as scope rather than as silence.

**Two smaller findings, both from the tests:**
- `tests/revalidation_test.rs` needed `#![recursion_limit = "256"]`. Its spawned pass nests
  the whole admission future, and one `tracing::Instrument` layer per level put it over the
  default 128 — the same reason `lib.rs` already carries the attribute
- The integration tests **all** hold one serial lock, including the two span tests. Letting
  the span tests run alongside made `a_creation_observes_no_activation_write_set` see three
  successes instead of one, intermittently: their admissions increment the very counters the
  others measure a delta of. `Temporality::Delta` is what makes the per-test reset
  meaningful — under the default cumulative temporality a flush re-exports every count
  since process start and `reset()` clears the batches without clearing the sums

**Dependencies:** T8 (may run parallel with T14, T15)
**Files touched:**
- `TR/src/observability.rs` — NEW, the two span constructors and the `kind` label
- `TR/src/domain/ports/metrics.rs` — NEW, the `AdmissionMetrics` port and the label vocabularies
- `TR/src/infra/metrics.rs`, `TR/src/infra/metrics_tests.rs` — NEW, the OpenTelemetry adapter and
  the rendered-name contract tests
- `TR/src/observability_tests.rs` — NEW, 12 in-source contract tests
- `TR/tests/observability_test.rs` — NEW, 10 emission tests
- `TR/src/domain/admission/worker.rs` — the two spans, the duration histogram, the
  candidate counters and the revalidation-retry counter; `run_operation` split into a
  wrapper and `run_operation_inner` so the span covers the first read
- `TR/src/domain/admission/acceptance.rs` — `AcceptanceError::reason()`; `accept` split
  into a wrapper and `accept_inner` so every refusal of the path passes one counting point
- `TR/src/domain/admission/unit.rs` — the activation-write-set observation
- `TR/src/gear.rs` — `bind_instruments()` at a known point after `ToolKit` installs the
  provider
- `TR/src/lib.rs`, `TR/Cargo.toml`, `TR/tests/revalidation_test.rs`
**Scope:** S

---

### Checkpoint 3
- [x] Dependent refresh is atomic with the new revision; identical recomputation is a no-op —
  `refresh_test.rs::revising_a_base_refreshes_every_dependent_schema` and
  `a_dependent_whose_artifacts_are_identical_is_not_rewritten`
- [x] Activation bound refuses rather than partially commits —
  `refresh_test.rs::an_over_bound_write_set_commits_nothing`, with the earlier evaluation-side
  refusal from T15 in front of it
- [x] Multi-pod read-after-commit holds —
  `revalidation_test.rs::a_commit_on_one_pod_is_visible_to_the_others_first_read`, two
  `DBProvider`s with their own pools over one database file
- [x] Admission emits spans and metrics — T16, verified at runtime as well as in tests
- [x] `make dylint` — full workspace, once for the phase (P13). Exit 0. Two DE1201 warnings stand,
  both on crates this branch does not touch (`cf-gears-simple-user-settings`,
  `cf-gears-file-storage`): `git log main..HEAD` over their directories is empty, so they are
  pre-existing and not this phase's to clear
- [x] Gear tests at the checkpoint: **617 passed** on SQLite; **6 passed** on the PostgreSQL and
  MySQL container suites, `revision_race_backends_test` included
- [x] **Backend lock exclusion:** `a_second_commit_waits_for_the_first` observes
  `types_registry__coordination_state` waits through Postgres `pg_blocking_pids` /
  `pg_stat_activity` and MySQL `data_lock_waits` / `data_locks`. An isolated container
  identifies the writer; `ClaimHooks` verifies pending-before-release and progress
  afterwards. Both backends detect wrong-table and premature-return mutations.
- [x] **`make test-integration LANE=types-registry-db` did not run `revision_race_backends_test`, and now does.**
  The suite that proves the `entity_write_order` claim on a backend with real row locking was
  reachable only by hand, so `make ci` never ran it — the container test the last commit added to
  close a P0 blocker was outside the gate meant to protect it. One line in the `Makefile` target
- [x] Human review — four items below; item 3's design decision is resolved

**Handoff review (commit `319eb16a5`), item by item.**

1. **The migration and its upgrade test — sound.** `Migrator::up(&db, Some(1))` applies exactly
   one pending migration, so on a fresh database it stops where a deployment that only ran the
   initial migration stops; the test then asserts the table is *absent* before applying the rest,
   which is what makes it an upgrade test rather than a fresh-install one. Nothing else assumes
   the table exists at initial-migration time: `m20260817_000001_initial_tests`'s `P0_TABLES` and
   constraint counts exclude it, and `claim_entity_write_order` fails closed with a message naming
   the migration when the row is missing (`claiming_a_missing_entity_write_order_row_fails_closed`).
   **One gap worth naming:** `the_coordination_state_migration_absorbs_a_table_that_already_exists`
   pins that a pre-created table is left alone — which means such a table keeps whatever shape it
   was created with and never gains `ck_tr_coordination_state_seq`. Correct for the seed, silently
   weaker for the constraint. No path in this repository pre-creates it, so this is a note, not a
   defect.
2. **The claim really is unpreceded, in the code and in the normative text.** `commit_creation`
   (`unit.rs:375`) and `commit_revision` (`unit.rs:745`) both open with
   `claim_entity_write_order`, and the transaction closure in `worker.rs` calls one or the other
   as its only statement — there is no read between `transaction_with_retry` and the claim on
   either branch. SPEC §8.1 step 4.1 says *"nothing may precede it, reads included"* and gives the
   reason; the deletion protocol (§8.1, before Dry Run) repeats it as *"not optional and not
   merely early"*. Both read as instructions T20 and the ADR-0013 purge can follow literally.
   The mechanism is right too: an `UPDATE … SET state_seq = state_seq + 1` holds an exclusive row
   lock to commit, and `#[secure(unrestricted)]` matches every other P0 table.
3. **Store decorator duplication removed.** `TestStores<H>` forwards all seven port
   traits once. `PauseHooks`, `ClaimHooks`, and `CasMissHooks` customize `StoreHooks`.
   For T19/T20, add a `PausePoint` for timing or a hook for inspection/overrides.
4. **Dependency ordering is explicit.** Missing input dependencies fail immediately.
   Dependants submitted together follow batch ordering; separate submissions must await the
   prerequisite operation. Redelivery recovers temporary system failures, not absent inputs.

---

## Phase 4 — Compatibility

### - [x] T17: Compatibility against one baseline

**Description:** Baseline selection — the entity's current revision for a major-only
candidate, or the `ACTIVE`/`DELETED` definition of `vM.(n-1)~` for a minor-bearing one —
compared through `GtsStore::compare_documents`, which resolves both sides. `Unknown` is
rejected with its own reason, never collapsed into `Incompatible`.

**Acceptance criteria:**
- [x] `compare_documents` is the only comparison entry point; `is_minor_compatible` is not used
- [x] `CompatibilityVerdict::Unknown` fails the candidate with a reason distinct from `Incompatible` (`principle-fail-closed`)
- [x] Every admitted revision records `gts_spec_version`, `gts_impl_version` and `compat_forced`
- [x] `skip_compatibility_check` waives one eligible cross-minor check when the deployment permits it. `ForceCompatibilityUnavailable` is removed; revision `compat_forced` records the effective waiver
- [x] Major-0 candidates get no baseline and no verdict

**Observability (P16 — this task instruments what it adds):**
- [x] `types_registry_compat_verdicts_total{verdict,forced}` counts
      `compatible | incompatible | unknown`; refusals also increment `refusals_total`
- [x] `forced="true"` identifies waived cross-minor verdicts
- [x] Unit spans record `baseline_gts_id`, `baseline_revision`, verdict,
      `gts_spec_version`, and `gts_impl_version`. Identifiers never become metric labels
- [x] **The admission reason vocabulary has one home, compile-enforced** (P16 rule 3):
      `ItemFailure::new` takes `AdmissionFailureReason` from `domain::admission::reasons`.
      Stored codes are unchanged; known codes restore typed variants and their metric labels,
      while `Unknown(String)` preserves unfamiliar codes and maps them to `other` in metrics
- [x] Add this task's compatibility refusal variants to `AdmissionFailureReason`

**Verification:**
- [~] Gear tests, all three backends (see [Commands](#commands)) — 739/739 SQLite tests pass.
      `compat_backends_test` now runs the T17/T18 matrix on PostgreSQL and MySQL and is selected
      by `make test-integration LANE=types-registry-db`; its Docker execution remains to be recorded
- [x] Compatibility matrix: optional property added at a `Closed` level (compatible), at `Open` (incompatible), at `Partial` (`Unknown`)
- [x] Test: provenance columns match `GTS_SPECIFICATION_VERSION` and the crate version
- [x] Disabled `skip_compatibility_check` is refused (`force_is_refused_while_the_deployment_disallows_it`).
      Before T20, Dry Run is synchronously rejected before the force gate; the ordering is asserted
      by `a_forced_dry_run_is_refused_for_being_a_dry_run_before_force_is_considered`
- [x] Test: the verdict instrument's rendered name, label keys and both label vocabularies
      against an `InMemoryMetricExporter` — T16's bar, and the only thing that catches a dropped
      `_total` or a renamed label value
- [x] Test: emission end to end through `run_operation` — compatible, incompatible, `Unknown` and
      forced each land under their own label pair — plus a mutation check that removing the
      emission calls fails those tests
- [x] Test: every `Reason` const is reachable and the vocabulary test enumerates the module, so
      the set a dashboard depends on is asserted rather than greppable

**Dependencies:** Checkpoint 3
**Files likely touched:** `TR/src/domain/compat.rs` (baseline selection; T18's derivation chain joins it and the pair takes `TR/src/domain/compat/` — trigger table above), `TR/src/domain/admission/unit.rs`, `TR/src/domain/error.rs`, `TR/src/domain/admission/reasons.rs` (NEW — the vocabulary), `TR/src/domain/ports/metrics.rs`, `TR/src/infra/metrics.rs`, `TR/src/observability.rs`, `TR/tests/compat_test.rs`

**Storage:** `m20260908_000003_operation_item_compat_forced` persists the per-candidate
waiver request for the worker; the fingerprint cannot recover it. The entity, port
rows, and `database.sql` include the column. `compat_forced` avoids MySQL's reserved
`FORCE`; `no_backend_names_the_column_with_a_reserved_word` guards the name.

**Scope:** M

---

### - [x] T18: Derivation chain and major-0 quarantine

**Description:** Identifier-derived chain validation against every managed base, the
Draft-07 dialect pin across a major, and the ADR-0015 quarantine: a stable candidate may not
derive from or `$ref` a major-0 identifier, and a major-0 schema may not carry a registered
Instance. An entity reference (`x-gts-type-ref` / `x-gts-instance-ref`) is an instance-value constraint and is outside the quarantine.

**No preflight scan.** ADR-0015 and DESIGN were simplified to drop it (O4): the rule's base case
comes from the release boundary — T2's migration creates the storage in the same release this task
introduces the check — so there is no pre-existing edge to scan for. The obligation that survives
is negative: do not enable the rule against a database populated by a build that had the storage
but not the check. A dev database can be exactly that between T10 and T18; delete it rather than
reasoning about it.

**Acceptance criteria:**
- [x] Reuse `dependency::extract_edges`, which calls `chain_ids()`, for chain bases and quarantine. An entity reference produces no edge
- [x] A stable candidate whose immediate base or `$ref` targets include a major-0 identifier is refused. The base comes from `chain_ids()` and `$ref` targets come from `dependency::extract_edges`; no target document is needed to read its major
- [x] Refuse Instances conforming to major 0 via the `instance_of` edge from `get_type_id()`
- [x] Pin the dialect across intra-entity revisions and new minors, using the preceding minor's current definition

**Observability (P16):**
- [x] Each quarantine and dialect refusal carries **its own** `Reason` const from T17's
      vocabulary — `stable_derives_from_major_zero`, `stable_refs_major_zero`,
      `instance_of_major_zero`, `dialect_changed` — never collapsed into `invalid_schema`. An
      ADR-0015 refusal and a malformed document are different operator actions, and a shared
      reason makes them one number

**Verification:**
- [~] Gear tests, all three backends (see [Commands](#commands)) — 739/739 SQLite tests pass.
      The target now selects 10 tests across four backend binaries, including the new T17/T18
      PostgreSQL/MySQL scenarios; their Docker execution remains to be recorded
- [x] Tests: each quarantine path — a stable candidate deriving from a v0 base and one `$ref`-ing a v0 target; plus stable candidates whose entity reference (`x-gts-type-ref` / `x-gts-instance-ref`) names a v0 entity exactly or through a pattern, which must be admitted because the keyword is outside quarantine. Refusal tests register the v0 target first and assert that no entity row was written
- [x] Test: dialect change across revisions is refused — both edges, the intra-entity one and the cross-minor one
- [x] Test: the four refusals appear in `refusals_total{stage="admission"}` under those exact
      label values — asserted as label values, not as counts:
      `label_values_of("types_registry_refusals_total", "reason")` compared as a set

**Implementation:** Quarantine runs in the worker over the dependency graph's
extracted edges, before storage reads. The dialect pin runs before comparison
because it needs the baseline. See SPEC §8.1 for placement and refusal semantics.
Removing the pin makes both dialect tests fail with `compatibility_undecidable`.

**Closed gap:** [gts-rust#120](https://github.com/GlobalTypeSystem/gts-rust/issues/120).
`compare_documents` used to compare `$schema` verbatim and reject equivalent
Draft-07 spellings (`…/schema#` vs `…/schema`) as `Unknown`, although the pin
accepted them. Fixed upstream by gts-rust#121 and adopted here with gts 0.12.1, so
admission now succeeds instead of refusing with `compatibility_undecidable`.
`a_respelled_dialect_is_accepted_by_the_pin_and_the_library` covers the new behavior.
Normalizing at acceptance was rejected as the alternative: it would rewrite retained
content and its request fingerprint.

**Added:** `compat/derivation.rs` and tests; four admission reasons;
`DependencyKind::quarantine_verb`; dialect fixtures, quarantine integration tests,
and refusal-label assertions. `baseline` moved into `compat/`; acceptance shares
its Draft-07 spelling set with `derivation`. `compat_backends_test` executes the
compatibility matrix, force provenance, quarantine paths, dialect pin, and revision
provenance on PostgreSQL and MySQL. The operation-item migration now constrains
MySQL's integer boolean, and the SQLite insert chunk accounts for all 15 columns.

**Dependencies:** T17
**Files likely touched:** `TR/src/domain/derivation.rs` — **second file for the concept, so take `TR/src/domain/compat/`**: `baseline.rs` from T17 plus `derivation.rs` here. Also `TR/src/domain/admission/acceptance.rs`, `TR/tests/quarantine_test.rs`
**Scope:** M

---

### Checkpoint 4
- [x] Compatibility matrix passes including the `Unknown` tier
- [x] Provenance persisted on every revision
- [x] Quarantine and dialect rules hold, including admission of stable schemas whose entity reference names a major-0 entity (no preflight — O4)
- [x] Every verdict is counted, with `Unknown` and a forced waiver each distinguishable in the
      metrics and not only in a refusal reason (T17, P16)
- [x] Quarantine and dialect refusals each carry their own counted reason, none collapsed into
      `invalid_schema` (T18, P16) — four label values, asserted as a set
- [x] Admission reasons live in one compile-enforced vocabulary: a new refusal cannot compile
      without naming one (T17, P16)
- [~] `make dylint` — the local run is blocked before project linting by stable/nightly artifacts
      being mixed in Dylint's target directory (`E0514`); the gear's all-target/all-feature Clippy
      run with `--no-deps -D warnings` is clean
- [x] Human review

---

## Phase 5 — Batching, deletion, dry run, and dispatch

### - [x] T19: Dependency-aware partial admission

**Description:** Order candidates by `$ref`, derivation, conformance and implicit minor
predecessor edges. Detect cycles over `$ref` and derivation, refusing members with
`invalid_schema`. Process one candidate per unit and return one outcome each.
Ordering is pure; admitted state stays acyclic without atomic groups or condensation.

**Acceptance criteria:**

- [x] Independent passing branches commit despite failures elsewhere
- [x] In-batch references use candidate state, not older committed revisions (see notes below)
- [x] Failed dependency/predecessor yields `blocked_by_dependency`/`blocked_by_predecessor`
- [x] In-batch `$ref` cycles fail `invalid_schema`
- [x] Mixed `$ref`/derivation cycles fail `invalid_schema`
- [x] Self-referential GTS `$ref` fails `invalid_schema`. All cycle members fail without
  entity, revision or edge writes; existing revisions remain unchanged
- [x] Implicit predecessor edges are not stored:
  `a_minor_pair_is_ordered_by_an_edge_that_is_never_stored`
- [x] Pure `graph::order_batch(&[BatchCandidate]) -> BatchOrder`; 17 database-free tests

**Observability (P16):**

- [x] Both blocking reasons are `Reason` consts. Each blocked candidate increments
  `candidates_total{status="failed"}` and `refusals_total{stage="admission",reason}`

**Verification:**
- [x] Gear tests, all three backends (see [Commands](#commands)) — recorded at completion:
      SQLite 781/781; `make test-integration LANE=types-registry-db` 24/24 on PostgreSQL and MySQL
- [x] Tests: partial commit, blocked dependent, blocked predecessor, refused in-batch `$ref` cycle
- [x] Test: batch over `limits.batch_candidates` refused synchronously — covered by T7's
      `a_batch_over_the_limit_is_refused_with_both_numbers`
- [x] Test: each blocked candidate emits `candidates_total{status="failed"}` and the correct
      refusal reason — `a_blocked_batch_counts_one_failed_candidate_per_blocked_reason`

**Implementation notes:**
- Committed batches realize the overlay through dependency-first commits; a failed candidate
  blocks its dependents. The regression test
  `an_in_batch_reference_never_resolves_against_the_committed_revision` uses a refused base
  revision, so dependent refresh cannot mask incorrect ordering.
- An ordering loop involving a predecessor edge is refused as `invalid_schema`
  (`CycleKind::Unorderable`). When both blocking reasons apply, `blocked_by_predecessor` wins.
  Execution follows dependency order; outcomes retain submission order, one per candidate.
- Dry-run batches use one coherent snapshot plus prior successful candidates' virtual changes.
  The T20 follow-up below replaces the original per-candidate rollback limitation.

**Dependencies:** Checkpoint 4
**Main files:**
- `TR/src/domain/admission/graph.rs`, `graph_tests.rs`, `worker.rs`, `reasons.rs`
- `TR/tests/partial_admission_test.rs`, `partial_admission_backends_test.rs`
- `TR/tests/observability_test.rs`
**Scope:** M

---

### - [x] T20: Deletion and Dry Run

**Inherited from T15:** committed deletion claims `entity_write_order` as its transaction's first statement, before checking dependents. A revision-vector guard alone cannot serialize a new dependency edge against deletion. See P15, DESIGN §3.7 and SPEC §8.1 step 4.2.

**Description:** The short deletion protocol — positive `expected_resource_version`, family
and entity locks, recheck `ACTIVE` with no direct registered dependents, lifecycle to
`DELETED`, version increment, outcome — and Dry Run as a mode of both registration and
deletion, predicting the whole batch against one coherent snapshot without entity-state writes.

**Acceptance criteria:**

- [x] Committed deletion claims `entity_write_order` as its first statement, covered by
  claim-order tests matching creation/revision/`unchanged`
- [x] Live direct registered dependants block deletion; report count only
- [x] Transitive-only dependants do not block
- [x] An entity reference (`x-gts-type-ref` / `x-gts-instance-ref`) creates no edge and does not block
- [x] Paired service tests vary only `$ref`/entity reference on a string property: the former
  blocks deletion; the latter permits it and remains readable. Tenant availability is deferred
- [x] Tombstones remain exact-readable and are excluded from lists
- [x] Dry run issues no entity writes/claims; its mode enters the fingerprint and outcomes persist
- [x] Predicted `succeeded` omits `resource_version`; `unchanged` reports the existing one

**Observability — label sweep (P16 rule 2):**

- [x] `dry_run` labels candidate, refusal and compatibility-verdict counters
- [x] Dry runs do not observe the activation-write-set histogram; tested with rationale at the call
- [x] `kind` distinguishes deletion/registration in candidate and refusal counters;
  spans already carry both labels
- [x] Both port labels are required, without defaults
- [x] Deletion uses `has_registered_dependents`, `not_active` and `precondition_failed` reasons
- [x] `blocked_dependents` is a span count, never a metric label or identity list.
  Tests assert count presence and identity absence; dropping `record` fails the mutation check

**Verification:**
- [x] Gear tests, all three backends (see [Commands](#commands))
- [x] Tests: blocked deletion, transitive non-blocking, tombstone readability, dry run for both kinds
- [x] Test: reusing one key for dry run then commit is a fingerprint mismatch, not a replay
- [x] Test: instrument contract — the two new label keys and their vocabularies against an
      `InMemoryMetricExporter` (T16's bar)
- [x] Test: emission — a committed deletion, a refused deletion, a dry-run registration and a
      dry-run deletion each land under the right label pair, asserted as a per-pass delta rather
      than a total
- [x] Test: no counter from a dry-run pass appears under `dry_run="false"`
- [x] Mutation check: dropping either label, or the deletion emission, fails the suite

**Implementation notes:**
- Batch deletion orders dependents before their targets using stored edges within the batch
  (`DependencyRepo::edges_within`, `graph::order_deletion_batch`). Each deletion rechecks live
  dependents; failed deletions lead to `has_registered_dependents` on affected targets.
  Unorderable stored rows are warned about and still processed for individual outcomes.
- Dry run reuses admission checks over a snapshot and virtual candidate layers. Refused layers
  are discarded; outcomes and operation completion publish atomically after the snapshot closes.
  No entity-state write or write-order claim reaches storage.
- Replay returns the same outcome fields in both modes, including the derived Registry Reference.
- Dry runs do not observe the activation-write-set histogram. The compatibility verdict
  counter carries `dry_run` only; candidate/refusal counters also carry `kind`.
- POST registration already exposes dry run. REST deletion remains T20a's responsibility.

**Recorded verification:** SQLite 837/837; PostgreSQL/MySQL 27/27 when run binary by binary.
The combined backend selection hit Docker `PortNotExposed`; it was not green as one run.
Workspace testing had 119 OAGW failures, also reproduced on clean HEAD in that environment.
Mutation checks, formatting and clippy passed. These are implementation-time results.

**Coverage gap:** the `more than {bound}` refusal-message branch (>512 live direct dependents)
is untested; the deletion refusal itself is covered.

**Dependencies:** T19
**Main files:**
- `TR/src/domain/admission/deletion.rs`, `worker.rs`, `graph.rs`, `acceptance.rs`
- `TR/src/domain/admission/dry_run/` (`mod.rs`, `publish.rs`, `view/`)
- `TR/src/domain/ports/metrics.rs`, `TR/src/infra/metrics.rs`, `TR/src/observability.rs`
- `TR/src/infra/storage/repo/dependency_repo.rs`, `operation_repo.rs`
- `TR/tests/deletion_test.rs`, `dry_run_test.rs`, `deletion_backends_test.rs`
- `TR/tests/dry_run_batch_test.rs`, `dry_run_parity_test.rs`, `dry_run_batch_backends_test.rs`
- `TR/tests/observability_test.rs`, `api_rest_test.rs`, `common/test_stores.rs`
**Scope:** M

---

### - [x] T20a: REST deletion and dry run

**Description:** Expose single/batch deletion on `/v2/` with dry run on all mutations.
Read completion belongs to T22a (P17).

**Acceptance criteria:**

- [x] Batch `items` use `key` parsed by `EntityKey::parse`; outcomes use `gts_id` in request
  order, including UUID submissions
- [x] `:batchDelete` requires positive `expected_resource_version` per item; absence is `400`
- [x] `DELETE /entities/{entity_key}` uses the one-item batch domain path, GET's key resolution
  and `Idempotency-Key`; no handler-local deletion/precondition model
- [x] Single deletion requires a positive `expected_resource_version` query parameter and
  refuses `If-Match`. Missing, non-numeric or zero yields `400`; mismatch yields `202`
  then item `precondition_failed`, never `412` (DESIGN §3.3)
- [x] Single and one-item batch deletion return identical version-mismatch outcomes
- [x] `dry_run=false` by default; body field for registration/batch deletion, query for
  single deletion. Preserve registration's router test and cover both deletion routes
- [x] All mutations require `Idempotency-Key`: acceptance returns `202`, `Location` and
  `Retry-After`; terminal replay returns `200` and `Idempotency-Replayed: true`.
  Dry runs persist outcomes without entity/version changes; dry-run/commit key reuse conflicts
- [x] OpenAPI includes deletion routes, RFC-9457 errors, preconditions, idempotency and dry run.
  Quickstart covers registration, deletion, dry run and submit-then-poll on internal routes
- [x] All mutations keep `exposed = false` until platform identity/PDP checks precede dispatch
- [x] `QUICKSTART.md` meets the gear-layout guide: description, features, `/docs` link,
  one or two working `curl` examples
- [x] OpenAPI/quickstart identify the global platform-plane API and C8's internal-only
  mutations pending `X-ToolKit-Internal-Token`/`PlatformIdentity` and a separate listener;
  no usable gateway mutation example
- [x] Handlers only map the domain service (SPEC §8.4)
- [x] All routes use `routes::V2` for T38's promotion
- [x] Deletion routes reuse T20's `kind="deletion"` metrics
- [x] No e2e edits; `make e2e-local` stays green. Preserve v1 and its in-memory store (P12/P17)
- [x] Both breaking changelog entries belong to T38's promotion

**Verification:**
- [x] Gear tests (see [Commands](#commands)), including `TR/tests/api_rest_test.rs` through
      the real router: single/batch deletion parity for GTS Identifier and UUID, malformed
      preconditions, version mismatch and idempotency replay/conflict
- [x] Router tests: registration and both deletion spellings with `dry_run=true` reach a
      terminal operation outcome while entity state, revisions and resource versions remain
      unchanged; a committed control request demonstrates the corresponding mutation
- [x] `make e2e-local` — unchanged and still green, no e2e file edited (P12)
- [x] `make lychee`
- [x] Manual: `/cf/docs` renders the mutation contracts; `curl` against `/v2/` for
      register → poll → exact read → dry-run delete → exact read → delete → poll → tombstone

**Implementation notes:**
- Both routes map to `RegistryService::delete(DeleteRequest)` and submit `kind = Deletion`.
- UUID keys resolve in one snapshot before acceptance, which reads no entity state.
  Mappings are immutable; admission rechecks lifecycle and versions under locks.
  Identifier-only batches need no lookup.
- Unknown UUIDs return `404` because no identifier can be recovered for an outcome.
  Absent identifiers reach admission and fail with `precondition_failed`.
- Optional DTO versions let acceptance return `400 deletion_requires_version` on both
  routes; OpenAPI still declares them required.
- Shared idempotency/receipt helpers keep mutation responses consistent. `operation_location`
  replaces the last `/entities` segment and suffix, preserving mount prefixes.

**Route overlap checked:** `resource-group` owns `/v1/types*`; these `/v2/entities*`
routes have no duplicate path/method pair in the served document.

**Recorded verification:** SQLite 894/894 (44 router tests); PostgreSQL/MySQL 36/36 in one
`make test-integration LANE=types-registry-db` run. Formatting, gear clippy and link checks passed.
Manual `/cf/types-registry/v2/*` checks covered register/poll/read, dry-run and committed
deletion, tombstones, `If-Match`, missing versions, unknown UUIDs and replay.
`make e2e-local`: 324 passed, 19 skipped; no e2e edits.

**Environment:** Focused quickstart needs `account-management` for v1 seeding; full
`make run` failed on missing ONNX Runtime in `file-parser`. Manual checks used
`--features account-management,static-authn,static-authz,single-tenant,static-idp`.

**Corrected at Checkpoint 5:** Homebrew Rust 1.98 shadowed pinned 1.97.0 and caused a
spurious `clippy::unused_async_trait_impl` failure in `libs/toolkit-security`.
With `PATH="$HOME/.cargo/bin:$PATH"`, whole-workspace `make clippy`
(`--all-targets --all-features` and `cargo hack --each-feature`) passed.

**Dependencies:** T20 (deletion and dry run), T9a (interim v2 routes)
**Files likely touched:**
- `TR/src/api/rest/routes.rs`
- `TR/src/api/rest/handlers.rs`
- `TR/src/api/rest/dto.rs`
- `TR/src/api/rest/error.rs`
- `TR/src/domain/registry_service.rs`
- `TR/tests/api_rest_test.rs`
- `gears/system/types-registry/QUICKSTART.md`
**Scope:** M

### - [x] T21: Outbox dispatch wiring

**Description:** Wire `toolkit-db`'s leased outbox (`types_registry__outbox`) through a
`LeasedMessageHandler` mapping worker results to `Ok`/`Retry`/`Reject`. Payload: operation UUID only.

**Acceptance criteria:**
- [x] Every database-backed submission goes through the outbox; acceptance and enqueue
      share a transaction, so no accepted operation lacks a driver (P3). Startup seeding
      still writes the in-memory service; T31 moves it onto this path
- [x] Signal the exact partition after the acceptance commit; transaction-time
      signals can arrive before rows are visible.
- [x] Handler contains no admission logic — it resolves the operation UUID and calls the worker
- [x] Delivery is at-least-once and commits are idempotent; duplicate delivery is a no-op
- [x] Retry failures that may clear; a permanent failure or an exhausted budget records
      `system_failure` and ACKs. `Reject` is only for an envelope naming no operation
- [x] Candidate content never enters an outbox or dead-letter payload
- [x] Add `stateful`, deferred by T2: `[system, db, rest, stateful]` (SPEC §5)
- [x] Start the worker at the end of `init()`, before stateful `start` (P3); retain `OutboxHandle` and call `stop()` after `ctx.cancellation_token()` fires (see lifecycle deviation)
- [x] Started before anything can submit, so an acceptance always has somewhere to enqueue. T31's seed batch takes the same path and gates client publication on its item outcomes
- [x] An operation submitted from any consumer's `init()` is admitted without that consumer waiting for the `start` phase

**Verification:**
- [x] Gear tests, all three backends (see [Commands](#commands))
- [x] Real-router registration, batch/single deletion × committed/dry-run modes reach
      terminal outcomes via outbox. Dry runs persist operations/outcomes while preserving
      entity state, revisions and resource versions
- [x] Test: duplicate delivery of one operation UUID changes nothing
- [x] Test: an operation submitted immediately after `init()` returns reaches `completed` without the `start` phase running
- [ ] Prove shutdown does not leak tasks. Reopened: the host's 35s hard stop can abort
      `serve` while spawned outbox work continues. State is recoverable, task lifetime is not bounded
- [x] Manual: submit over REST against `make example` and observe the operation reach `completed` without direct worker invocation

**Testing exception (SPEC §§13–14):** Only real delivery uses
`common::await_delivery` with bounded backoff and a 2s deadline. Domain tests call directly.

**Implementation notes:**
- `RegistryService::admit` is the one admission driver. Missing input
  dependencies are terminal item refusals; retry classification covers only system failures.
- `batch_size(1)` gives each message its own bounded attempt budget. After repeated lease
  timeouts, delivery `N + 1` reads stored status without rerunning admission.
- Admission reserves lease time for one guarded bulk system-failure write. Client and
  dead-letter diagnostics contain stable codes and operation IDs, never raw errors.
- Eight UUID-derived partitions run concurrently, while entity commits remain serialized.
  There is no recovery scan: acceptance and enqueue share a transaction, so every committed
  operation carries a message the outbox lease redelivers.
- `OutboxDispatch` uses a weak reference to avoid an ownership cycle; runtime and tests
  share the same pipeline settings.

**Lifecycle deviation:** `serve` drains the retained handle after runtime cancellation, and
worker startup remains in `init()`, before seeding.

**Receipt behavior:** Production submissions return `pending`; terminal replays return
`200` only after outbox admission completes.

**Recorded verification:** SQLite 903/903; PostgreSQL/MySQL 39/39 in three runs;
E2E 324 passed, 19 skipped; formatting and clippy passed. Live REST mutations completed,
and `SIGTERM` drained the outbox.

**Observed flakiness:** `migration_backends_test` failed once before three passing runs,
likely from container startup contention.

**Dependencies:** T20 (worker supports both mutation kinds and dry run); T20a precedes this
task so the REST-to-outbox flow can be verified before Checkpoint 5
**Files likely touched:** `TR/src/infra/outbox.rs`, `TR/src/gear.rs`, `TR/Cargo.toml`,
`TR/src/domain/registry_service.rs`, `TR/src/domain/admission/errors.rs`,
`TR/tests/outbox_test.rs`, `TR/tests/outbox_backends_test.rs`, `TR/tests/api_rest_test.rs`,
`TR/tests/common/mod.rs`, `docs/p0/SPEC.md` (§§8.1, 13, 14)
**Scope:** M
---

### Checkpoint 5
- [ ] Partial admission, the refused `$ref` cycle, deletion safety and Dry Run all behave
- [ ] No series blends a dry run with a commit, or a deletion with a registration (T20, P16)
- [ ] Blocked candidates are counted per reason (T19, P16)
- [ ] Registration and both deletion routes support dry run on `/v2/`, with OpenAPI and
      mutation quickstart examples in place (T20a, P17)
- [ ] All three mutation routes, with and without dry run, complete through the outbox:
      submit → poll → terminal outcome, no direct worker invocation (T21). Dry-run operation
      and outcome records persist while entity state and resource versions remain unchanged
- [ ] `make e2e-local` still green with no e2e file edited — P12's invariant holds through this
      phase (T20a, T21, P17)
- [ ] Gear tests (see [Commands](#commands))
- [ ] `make dylint` — full workspace, once for the phase (P13)
- [ ] Human review

---

## Phase 6 — Read API and conditional reads

### T22: Deferred to P1 — inventory ownership metadata and per-gear push

**Moved, not completed.** The task is now [#4827](https://github.com/constructorfabric/gears-rust/issues/4827)
under [P1 #4628](https://github.com/constructorfabric/gears-rust/issues/4628)
and plan P18, which supersedes P4's P0 scope. The original T22 metadata/macros/filtering work
and per-gear startup inventory integration now ship with the platform-plane client and authN.
C3 remains open in P0. Task IDs are retained; this transfer note is not an open P0 task.

T23 retains reconciliation of explicitly supplied documents. T24 retains process-wide
inventory collection while moving admission to the database; T25/T26 migrate existing calls
without adding inventory registration to every declaring gear.

---

### - [x] T22a: REST batchGet and discovery

**Description:** Add `POST /entities:batchGet` and bounded, content-free `GET /entities` on `/v2/`; complete seven-route OpenAPI and quickstart coverage (P17). First in Phase 6 after Checkpoint 5; REST/SDK share SPEC §10.1/§10.2. Inventory attribution metadata remains P1 (P18, #4827).

**Acceptance criteria:**

- [x] `batchGet` returns an explicit result per key, including absence; duplicate keys collapse
- [x] All batch bodies use `items`; each item's `key` uses the path's `EntityKey::parse`
  (DESIGN §3.3)
- [x] Impossible identifiers return identical errors through batch and exact reads
- [x] `batchGet` echoes requested keys
- [x] Reject a batch `If-None-Match` header; validators belong in per-item `if_none_match`
- [x] Discovery excludes tombstones and sorts by canonical identifier
- [x] Return one bounded page (D12): default `limits.page_size_default` (50), reject above
  `page_size_max` (100). Use `toolkit-odata` cursors and reject unknown versions
- [x] Pages contain identity/metadata only: no `content`, `resolved_schema`, `effective_traits`,
  `effective_traits_schema` or validator (§8.5)
- [x] Exact/batch reads retain full representations and D3 artifacts
- [x] Reject `$select` with an RFC-9457 problem naming the parameter (§10.2)
- [x] Exercise sparse and exact pattern pages through the route
- [x] Use T4's DB reads; decide the pattern in SQL over stored segments (SPEC D14)
- [x] OpenAPI includes RFC-9457 errors for all seven routes: exact/list/batch/operation reads
  and registration/batch deletion/single deletion
- [x] All mutations keep `exposed = false` until platform identity/PDP checks precede dispatch
- [x] Extend quickstart with batch reads, discovery, cursor traversal and full-document hydration
- [x] OpenAPI/quickstart identify the global platform-plane API and C8's internal-only
  mutations pending `X-ToolKit-Internal-Token`/`PlatformIdentity` and a separate listener;
  no usable gateway mutation example
- [x] Handlers only map the domain service (SPEC §8.4)
- [x] All routes use `routes::V2` for T38's promotion
- [x] DTOs follow SPEC §10.1/§10.2: `items`, `key`, `ListEntitiesResponse`; T29 follows the same contract
- [x] No e2e edits; `make e2e-local` stays green. Preserve v1 and its in-memory store (P12/P17)
- [x] Both breaking changelog entries belong to T38's promotion

**Verification:**
- [x] Gear tests (see [Commands](#commands)), including `TR/tests/api_rest_test.rs` driven
      through the real router: per-key batch outcomes, exact/batch key-classification parity,
      pagination, sparse and exact patterns, `$select` and cursor-version refusals
- [x] `make e2e-local` — unchanged and still green, no e2e file edited (P12)
- [ ] `make lychee` — **not run.** The target stops on `ensure-submodules` in this worktree
      (`docs/web-docs` and friends are uninitialized), and its path list is
      `docs examples guidelines gears/system/event-broker/docs`, which never covered this
      gear's `QUICKSTART.md` anyway. `lychee` run directly over the two files this task
      touched is clean: 22 OK, 0 errors
- [x] Manual: `/cf/docs` renders all seven operations; `curl` against `/v2/` for
      register → poll → page → batchGet → delete → poll → page, including a `limit` above
      `page_size_max` and a `$select` refusal

**Dependencies:** T4 (database reads), T9a (v2 routes and exact reads), T20a (mutation routes
and quickstart, for the seven-route completeness check)
**Files likely touched:**
- `TR/src/api/rest/routes.rs`
- `TR/src/api/rest/handlers.rs`
- `TR/src/api/rest/dto.rs`
- `TR/tests/api_rest_test.rs`
- `gears/system/types-registry/QUICKSTART.md`
**Scope:** M

**Implementation notes:**
- **Discovery became a port.** `EntityStore::list_page` and `TypeSchemaStore::current_schemas`
  are new; `PageRequest` / `EntityPage` moved from `infra::storage::repo::entity_repo` into
  `domain::ports` (re-exported from `repo/mod.rs`, so T4's tests are unchanged) because a
  domain method now names them. `AdmissionView` refuses both: an overlay holds candidates
  with no position in the stored keyset, and admission reaches entities by key, by id or
  through the dependency relation, never by page.
- **`current_schemas` keeps a batch read constant in round trips.** `find_current_schema` is
  single-entity, so a 100-key batch would have cost 100 extra queries for the very artifacts
  D3 materialized to avoid work. The batch read is two identity reads plus three
  current-state reads under one snapshot, whatever the batch size.
- **The exact read is now one key's `batch_get`**, as `delete_entity` is one target's
  `delete`. That is what makes "impossible identifiers return identical errors through batch
  and exact reads" structural rather than asserted: the key is classified once, an absence is
  an absence on both, and a corrupt current-state row is `CorruptDocument` on both. An
  impossible identifier is therefore an *absence* — no read surface validates the key and
  refuses early while the other looks it up.
- **The cursor is transport, not policy** (`TR/src/api/rest/cursor.rs`). The domain's position
  is a stored `gts_id`; the base64url envelope is how one page hands that to the next over
  HTTP. `toolkit-odata`'s `CursorV1` supplies the property the contract needs — an unknown
  version is refused rather than read — and the pattern is bound in through `f` so replaying a
  cursor under a different pattern is `FILTER_MISMATCH` rather than two spliced traversals.
  `validate_cursor_against` compares filters only when both sides carry one, so the
  unfiltered/filtered pair is checked explicitly; a unit test pins that.
- **`limit` and the page ceiling live in the domain**, which is why `DiscoveryPage` carries the
  size it was read at: a handler must not read `limits.page_size_default` to fill `page_info`,
  or REST and a future gRPC adapter could report different defaults for the same read.
- **The batch ceiling is 100, not DESIGN §3.3's 500** — SPEC §9 ceiling C10, declared rather than taken silently. DESIGN's higher number bought a reconciliation the headroom to read every identifier it might write before selecting its ≤100 candidates; P0 gives that up because a `found` result is a full representation and §3.2 bounds a resolved document at 1 MB, so the key count is the only bound on one response. The upgrade path is T29's helper paging its reads plus a bound on response bytes rather than on keys. A constant rather than a config key because §10.3's configuration is fixed for P0. It equals `limits.batch_candidates` today and is still not the same bound: raising the write ceiling must not silently widen read fan-out. Two tests hold it — one pins the literal `100` so the value cannot move unnoticed, one drives the boundary off the constant so exactly-at-ceiling is served and one past it is refused.
- **`if_none_match` is declared and not consulted.** No read emits a validator until T22d, so nothing a caller could hold can be compared against; the field exists now so the wire shape does not change under the callers migrating in T32/T38. `EntityLookupStatusDto` is `found` / `not_found` only — declaring `unchanged` before T22d emits one would publish a vocabulary value this gear never produces.
- **Discovery filters by `pattern`, `limit` and `cursor` only.** DESIGN's `depth`, `origin`, `availability`, `scope` and `tenant_id` are each out of P0 (SPEC §2). `kind` is *not* named by this task's criteria and would change `EntityRepo::list_page`'s T4 signature, so it stays out; v1's `kind` / `vendor` / `package` / `namespace` / `segment_scope` filters have no v2 equivalent, which T38 sees when it migrates the suites onto the promoted paths.
- **`$select` is refused on the discovery route only**, where §10.2 legislates it as a query
  parameter. `:batchGet` has no `$select` field to refuse: its one fixed field set is the full
  representation, a superset of anything a projection could name.
- **`PageInfoDto` is `{next_cursor, limit}`**, not `toolkit-odata`'s `PageInfo`: discovery pages forward only, so a `prev_cursor` that is always `null` would publish a direction this route does not travel. The envelope still names its array `items`, per SPEC §10.1's `ListEntitiesResponse`.
- **Standing bar.** `make fmt` green; gear tests green on all three backends (943 SQLite,
  39 container-backed); `RUSTFLAGS="-D warnings" cargo check --workspace --all-targets
  --all-features` clean, so no other gear regressed. `make clippy` is **red at HEAD for an
  unrelated reason** — a newer `clippy::unused_async_trait_impl` fires in
  `libs/toolkit-security` and `libs/toolkit-db`, neither of which this task touches. Clippy
  over this gear with only that lint allowed is clean.
- **Runtime evidence.** `make quickstart` (types-registry alone) and `make run` (all gears)
  both fail to boot in this worktree for pre-existing reasons — v1 ready-mode seeding wants
  account-management's base schemas in the first, and `file-parser` wants an ONNX Runtime that
  is not installed in the second. The manual pass ran the example server with
  `--no-default-features --features account-management,static-authn,static-authz,static-tenants,static-license`
  and exercised the whole flow through api-gateway's `/cf` prefix, cursor traversal included.

---

### - [x] T22b: Field projection on all three read routes

**Description:** Implement DESIGN §3.3's `$select` on exact read, `:batchGet` and discovery before T29 publishes the SDK models and T22d computes validators (historical decision P19). An absent `$select` returns P0's document-free metadata set; callers explicitly request `content`, `resolved_schema`, `effective_traits`, `effective_traits_schema` or the Type Schema `provenance` group. The default includes managed `origin`, as fixed in SPEC §10.2. P0 has no availability or tenant fields, so those unavailable DESIGN fields are not advertised or synthesized. Keep T22a's 100-key batch ceiling and bounded discovery page.

**Acceptance criteria:**
- [x] Update SPEC §§2, 8.3, 8.5, 9, 10.1, 10.2 and 16 before coding: fix the exact P0
  field allowlist and default, mandatory
  `kind` and `lifecycle_status` and result-envelope metadata, and the intentional P0 omissions from
  DESIGN. Remove the `$select` refusal and fixed-projection statements superseded by P19;
  retain C7 only for absent tenant/visibility dimensions and revise C10's full-response
  rationale without silently raising the 100-key ceiling. Done in the P19 contract revision;
  implementation and verification criteria below remain open
- [x] Define one transport-neutral normalized field set for all three routes and T29/T22d: field names follow ToolKit OData's case-insensitive, whitespace-trimming rules; order does not change identity, and absent `$select` equals an explicit default selection. Reject duplicates, empty, unknown, unavailable, malformed or excessive selections with an RFC-9457 field violation naming `$select`, honoring ToolKit's parser limits. Check raw empty comma segments too: ToolKit's parser currently drops them. A document field selects the whole JSON document, not a nested path within it; inapplicable Type Schema fields are absent on Instances
- [x] `provenance` is a group outside the default, selected only whole (its member is not a selectable name; a path into it is a nested selection), on generic `Entity` and `TypeSchema` (`Option<Provenance>`), never on `Instance`. Its one member, `compatibility_check_skipped`, is a required boolean: `true` means an applicable cross-minor check was deliberately waived with `skip_compatibility_check`; `false` means no waiver, not that a check ran. A selected `false` is present inside the group; an Instance omits the group on every read, selected or not, and SDK `Instance::try_from` refuses a snapshot carrying it. Reads carry the stored `type_schema_revision.compat_forced`; a selected Type Schema value that does not come back is corruption, not `false`. The revision's `gts_spec_version`, `gts_impl_version` and `compat_forced` stay internal audit and are not selectable
- [x] Exact and batch reads accept the same selection and produce the same projected
  entity for one key. Batch `"$select"` is a top-level body field applied to every key;
  per-item `if_none_match` remains declared for T22d. `key`, lookup status and future `etag`
  stay outside projection; `kind` and `lifecycle_status` are mandatory even when omitted
  from the selected set, so a projected entity says which documents apply and a projected
  tombstone is distinguishable from `not_found`
- [x] Discovery accepts `$select` in the query and returns one bounded page in the same
  canonical order. Its cursor binds the normalized selection as well as `pattern` and
  position: resuming under another selection is `400`; absent and explicit-default
  selections are interchangeable. A page still carries no validator; the default page
  remains content-free
- [x] Selection controls **which stored documents are fetched and parsed**, not merely
  which fields are removed after full `EntityDto` serialization. Read selected columns in
  bounded batches under the same snapshot as identity/current state; no per-entity query,
  no full artifact hydration for a metadata-only read. Keep all reads in SecureORM and
  compare the same behavior on SQLite, PostgreSQL and MySQL
- [x] REST DTOs distinguish an unselected field from JSON `null`, preserve the flat document fields of DESIGN and declare optional fields accurately in OpenAPI. Specify the matching transport-neutral SDK request/result shape for T29: its reconciliation explicitly selects `content`, and its list helpers select the fields they hydrate. T22d digests this exact normalized set, never the raw query text
- [x] Unknown query parameters, including unsupported OData options and v1-only filters, are refused rather than silently ignored. Use ToolKit's OData `$select` registration and extraction for GET routes, with a gear-level allowlist for accepted options and fields; batch reads reject query-string `$select`. Keep v1's in-memory routes unchanged until T38

**Implementation order:**
1. Contract and normalized field-set parser, with pure tests and SPEC/OpenAPI examples.
2. Exact and batch read projection through domain, repositories, DTOs and router tests.
3. Discovery projection and cursor binding, then end-to-end tests and quickstart examples.

**Verification:**
- [x] `make fmt`, gear tests on SQLite (1033) and both container backends (42, including
  `projected_read_backends_test`), `git diff --check`. `make clippy` keeps T22a's unrelated
  baseline failure (`clippy::unused_async_trait_impl` in `libs/toolkit-security`); clippy
  over this gear with only that lint allowed is clean
- [x] Router tests: default metadata-only response; each document alone; mixed batch;
  managed `origin` shape, selected `provenance` (`false` kept inside a Type Schema's
  group, the group absent on an Instance, on all three reads, `ETag` scoped to it), Instance
  inapplicable fields; tombstone with `$select=content`; absent key; invalid, unknown and
  unsupported selections, including a `provenance` member alone or by path; exact/batch
  parity; generated OpenAPI lists exactly the current `EntityDto` and `ProvenanceDto`
  properties
- [x] A forced revision reads back `provenance.compatibility_check_skipped=true`
  (`compat_test`, typed SDK); e2e TR-READ-005/102, TR-REG-606/902/903 assert it over HTTP
- [x] Router tests: discovery with metadata and document selection, multiple pages,
  cursor refusal after changing selection or pattern, and equivalent default spelling;
  malformed/unsupported OData options are refused
- [x] Instrumented repository test: metadata-only exact/batch/list reads do not fetch or
  parse authored/effective documents; selected documents are fetched in bounded batches
  inside one snapshot, including on PostgreSQL and MySQL
- [x] Manual `/cf/docs` and `curl` check of all three routes, including a selective
  `batchGet` and continuation under the same `$select`; update `QUICKSTART.md` to show
  explicit hydration in batches of at most 100 keys

**Implementation notes:**
- **One `FieldSelection` bitset** (`TR/src/domain/selection.rs`) is the normalized identity:
  order, case and duplicates cannot survive construction, `kind` and `lifecycle_status`
  are always members, and `canonical()` is the sorted spelling the cursor binds and T22d will digest.
  The REST parser runs ToolKit's `parse_select` for its limits, then refuses the empty
  comma segments ToolKit drops. Every refusal is `400` naming `$select` with reason
  `INVALID_SELECT`; *unavailable* is `availability` / `owned_by_context_tenant`.
- **Projected reads are new ports beside the admission ones.** `read_current_schemas` /
  `read_current_values` select only the named columns; `current_schemas`,
  `current_documents` and `current_values` are unchanged for admission and dry run.
  The revision row's identity is always read, so a missing current-state row is corruption
  under every selection. An unselected column is absent from the result set and `SeaORM` reads it into
  `Option` as `None`; the domain refuses a *selected* column that comes back `None`.
  `projected_read_backends_test` records the SQL on all three backends: metadata-only
  exact, batch and discovery reads never name a document column, selected ones name only
  theirs, all in one snapshot transaction, at most six statements for a batch.
- **Wire shape.** `EntityDto` omits unselected fields; only `kind` and `lifecycle_status`
  were required in OpenAPI (T22c's amendment adds `gts_id`/`gts_uuid`), metadata is non-nullable, documents are any-JSON (so a selected `null`
  stays). `origin` is `{"type":"managed",resource_version,created_at,updated_at}`;
  `provenance` is `{"compatibility_check_skipped":false}` with a required non-null
  boolean, omitted when unselected or inapplicable;
  `owning_gear` stays internal attribution that no read returns, and exposing it is P1
  work (SPEC §10.2). Instance artifacts are absent even when selected.
- **Strict query parameters.** A guard extractor refuses undeclared keys
  (`UNSUPPORTED_QUERY_PARAM`, one violation per key) and repeated keys before ToolKit's
  `OData` extraction. Exact read accepts `$select`; discovery `pattern`, `limit`/`$top`,
  `cursor`/`$skiptoken`, `$select`; `:batchGet` no query parameter at all, its body now
  `deny_unknown_fields` (a misspelled `select` is `422`, like the other v2 bodies).
  `limit=0` is refused under the spelling used before ToolKit reports it as `$top`.
- **Cursor binding.** The filter hash covers the pattern *and* `$select=canonical`, so it
  is never empty and `validate_cursor_against` always compares. A T22a token (no selection
  binding) is refused; absent and explicit-default selections resume one traversal
  (asserted by comparing whole responses).
- **Seeded discovery rows** in `api_rest_test.rs` now get a revision and current state,
  because every page checks the current revision behind each row.
- **`content_hash` removed.** The managed digest is no longer selectable, returned or
  stored: `m20260924_000004_drop_revision_content_hash` drops it from both revision
  tables, `$select=content_hash` is an unknown-field `400`, and `unchanged` compares
  canonical authored bytes only. `resource_version` and `resolution_fingerprint` are
  unchanged. The PRD, DESIGN and ADRs 0002/0004/0005/0006/0007/0011 drop the digest from
  the external plugin contract too: a source returns only the opaque `external_revision`,
  which changes with any source-owned response field (canonical content, effective
  artifacts, lifecycle, ownership scope, tenant enablement) but not with platform-owned
  availability or visibility, and conditional reads are delegated to the plugin.
- **Runtime evidence.** Example server as in T22a; `/cf/openapi.json` shows `$select` on
  both GET routes and the body field on `:batchGet`; `curl` exercised the default and
  selective exact read, a selective `batchGet`, continuation under a respelled `$select`,
  `400` after changing selection or pattern, `$top`/`$skiptoken`, and the corrected
  QUICKSTART loop (216 ids, hydrated in 100-key batches). The T22a loop sent an empty
  `cursor=` on its first page, which is a `400`; fixed here.

**Dependencies:** T22a. Must complete before T29 fixes the SDK request/result models and before T22d fixes validator inputs; no dependency on P1 inventory attribution metadata. **Files likely touched:** `docs/p0/SPEC.md`, `TR/src/domain/registry_service.rs`, `TR/src/domain/ports/mod.rs`, `TR/src/infra/storage/repo/`, `TR/src/api/rest/{dto,handlers,routes,cursor}.rs`, `TR/tests/api_rest_test.rs`, `TR/tests/repo_backends_test.rs`, `QUICKSTART.md`. **Scope:** L across three read surfaces; land the three implementation slices above separately, each with its focused tests and a working gear.

---

### - [x] T22c: Discovery filters by GTS chain depth and entity kind

**Description:** Add DESIGN §3.3's `depth` and `kind` filters to `GET /entities` on the interim v2 route before T29 publishes `ListEntitiesRequest` (historical decision P20, SPEC D14/§10.2). These filters intersect with `pattern` and active-only discovery, before pagination and T22b's `$select`; they do not change exact read or `batchGet`. Every discovery filter is exact SQL before `LIMIT limit + 1` (SPEC D14). T22a's completed pattern-only filter record remains historical.

**Acceptance criteria:**
- [x] REST accepts optional `depth=1..255` and `kind=type_schema|instance` with typed
  OpenAPI parameters. `depth` is an inclusive maximum `GtsId::segments().len()`;
  one-segment roots have depth 1, and derived schemas and Instance tails add one
  segment each. The SDK uses `EntityFilter::max_chain_depth: Option<u8>` and
  `kind: Option<EntityKind>`. No `pattern` is required for either filter
- [x] Filter by stored `entity.kind` and `entity.chain_depth` in SecureORM/SQL, and match
  `pattern` exactly in SQL: `gts-rust` parses it, and the repository compiles the parsed
  segments into joins on `entity_gts_segment`. Intersect all filters before counting a
  page item or hydrating selected documents. No Rust post-filter; do not hand-count `~`,
  walk dependency edges, materialize the full result set or add per-entity queries
- [x] Migration `m20260925_000005_entity_gts_segment` adds `entity.chain_depth`
  (`GtsId::segments().len()`), `entity_gts_segment` (0-based `segment_no`, binary
  `segment_name`, `major`, nullable `minor`, `is_type`; FK cascade; lookup index) and
  the entity indexes `idx_tr_entity_depth`, `idx_tr_entity_kind_lifecycle` and
  `idx_tr_entity_lifecycle` on all
  three backends, without backfill: `up` refuses a non-empty `entity`. Admission writes
  the segments in its transaction and refuses a UUID-tail identifier
- [x] Extend discovery's versioned cursor identity with canonical optional `depth`
  and `kind`, alongside `pattern` and T22b's normalized selection. A continuation
  changing any filter returns `400`; absence is distinct from an explicit value.
  No release preceded T22c, so by decision the wire version stays `CursorV1`'s `1`: a
  token without the new dimensions resumes only under the same absent `depth`/`kind`
  and the same `pattern`/`$select`, and is refused as soon as either filter is named;
  it is never read as an unfiltered traversal of a filtered query. Keep ordering by
  canonical `gts_id`. The cursor is the last returned row and appears only when another
  match exists, so a page with a cursor is full; no matching row is skipped or duplicated
- [x] Reject zero, negative, fractional, non-numeric and overflow `depth`, unknown `kind`, duplicate/unknown parameters and legacy v1 `is_schema` with RFC-9457 field violations. `kind` is an enum rather than a free string; neither generic `$filter` nor v1 `vendor`/`package`/`namespace`/`segment_scope` is accepted. Leave v1's in-memory route untouched until T38

**Implementation order:**
1. Add typed `kind` filtering through REST, domain and SQL with router/backend tests.
2. Add GTS segment `depth`, cursor binding and sparse multi-page traversal tests;
   update OpenAPI and quickstart.
3. Materialize `chain_depth` and segments (migration 000005), compile `pattern` to SQL,
   and pin it with the differential corpus. Each slice leaves the gear building and its
   focused tests green.

**Verification:**
- [x] `make fmt`, gear tests on SQLite (1049) and both container backends (45, including
  `discovery_filter_backends_test`), `git diff --check`. `make clippy` keeps T22a's
  unrelated baseline (`clippy::unused_async_trait_impl` in `libs/toolkit-security`); this
  gear is clean with only that lint allowed. `make lychee` stops on uninitialized
  submodules in this worktree, as recorded under T22a; `QUICKSTART.md` adds no links
- [x] Router tests: `depth=1` versus `depth=2` on roots, derived schemas and
  Instances; each `kind`; all combinations with `pattern`, `$select`, absent
  filters and tombstones; typed OpenAPI and RFC-9457 invalid-input responses
- [x] Repository/backend tests: SQL kind and depth predicates, sparse filters returning
  full pages, exactly-`limit` and empty results without a cursor, and
  mixed-depth/mixed-kind traversal without omission or duplication on all three backends
- [x] Differential test (`discovery_pattern_backends_test`): every generated pattern —
  wildcard cuts, bare `~*`, early-segment minors, instance tails, UUID-tail patterns —
  composed with `depth`, `kind` and `lifecycle`, returns exactly what
  `GtsId::matches_pattern` accepts on SQLite, PostgreSQL and MySQL
  (`make test-integration LANE=types-registry-db` 48/48; gear tests 1088/1088)
- [x] `EXPLAIN` of each page's recorded statement over 18k rows, after `ANALYZE`, on all
  three backends: a
  broad pattern reads `gts_id` order with an early `LIMIT`; a sparse minor drives from
  `idx_tr_entity_gts_segment_lookup`; `depth=1` uses `idx_tr_entity_depth`; `kind` uses
  `idx_tr_entity_kind_lifecycle`; tombstone-only uses `idx_tr_entity_lifecycle`; no page
  sorts more than its matches. MySQL: broad 0.80 ms, sparse kind 0.52 ms, tombstones
  0.35 ms, `depth=1` under a broad pattern 2.10 ms (~3k rows read). A lifecycle-first
  kind index was rejected: MySQL used it for every page and sorted (broad 23.5 ms)
- [x] Cursor tests: changing each of `pattern`, `depth`, `kind` or `$select` returns
  `400`, unchanged filters resume, and an old cursor version is refused
- [x] Manual `/cf/docs` and `curl` traversal with `pattern`, `depth`, `kind`,
  `$select` and a second page; `QUICKSTART.md` shows the inclusive depth rule

**Implementation notes:**
- **`EntityRepo::list_page` takes one `ListFilter`** and runs one statement through
  `SecureSelect::project_all`: `kind`, `lifecycle` and `depth` on entity columns (`depth=1`
  as equality, for `idx_tr_entity_depth`), and one inner join per constrained pattern
  segment. `repo/segment_filter.rs` mirrors `matches_views`: a concrete segment pins name,
  major and type marker, and minor only when given; a wildcard pins its given name prefix
  (a byte range, not `LIKE`) and major; a bare `*` adds nothing; a UUID tail cannot match.
  The first segment also bounds a `gts_id` range, an access path only. `LIMIT limit + 1`
  decides the cursor.
- **Depth range is the domain's.** REST accepts plain decimal digits only (`u8::from_str`
  would take `+5`) and refuses overflow; `0` parses and `discover` refuses it as
  `DepthOutOfRange`, so a future gRPC adapter gets the same rule. Every refusal names
  `depth`.
- **The cursor stays `toolkit-odata`'s `CursorV1`**, and `GET /entities` keeps ToolKit's
  full `extract_odata_query`. `depth`/`kind` are extra terms `And`-ed onto T22b's exact
  pattern/`$select` expression in the filter hash, added only when present: absent and
  every explicit value differ, and a T22b token resumes under the same absent filters
  (unit test built from T22b's formula). The base expression's operand order is part of
  that compatibility.
- **OpenAPI.** `depth` is `integer` with `minimum: 1`; `ParamSpec` has no `maximum` or
  `enum`, so 255 and the `kind` vocabulary are in the descriptions. Closing that needs a
  ToolKit `ParamSpec` change outside this gear.
- **Runtime evidence.** Example server as in T22a: root, derived schema and Instance under
  one pattern answered `depth=1` / `depth=2` / `kind` combinations as expected; a
  `depth=2&limit=1&$select` walk resumed under a respelled `$select` and was refused after
  changing `depth` or adding `kind`; the issued token decodes as `CursorV1` `"v": 1` and
  resumes through `$skiptoken` under a respelled `$select` (ToolKit's extractor), while a
  2100-character `$select` is refused by ToolKit; malformed `depth`,
  unknown `kind` and `is_schema` were `400`.

**Amendment (2026-09-23): lifecycle filter and mandatory identity.**
- [x] Discovery accepts `lifecycle_status=active|deleted|all` (default `active`); unknown,
  empty or repeated values are `400` naming it. It is an SQL predicate in
  `ListFilter::lifecycle`, applied with the other filters before the page limit. Exact
  read and `batchGet` are unchanged
- [x] The cursor adds a `lifecycle_status` term only for `deleted`/`all`, so absent and
  explicit `active` share one binding and changing the value on resume is `400`
- [x] `gts_id` and `gts_uuid` join `kind` and `lifecycle_status` as members of every
  `FieldSelection` and required, non-nullable `EntityDto` fields. Canonical selections other than the
  default gained `gts_id,gts_uuid`, so a pre-amendment cursor under such a `$select` is
  refused rather than resumed
- [x] Tests: generated OpenAPI (`OpenApiRegistryImpl`) for the `EntityDto` required set,
  its use by all three reads, and `lifecycle_status` as an optional string parameter
  (`ParamSpec` has no `enum`/`default`; vocabulary is in the description); REST lifecycle
  traversal, malformed values and cursor binding; repository tombstones among many active
  rows on all three backends

**Dependencies:** T22b (projection and cursor contract); T22a (bounded discovery). Must complete before T29 fixes the SDK `ListEntitiesRequest` shape. No dependency on P1 inventory attribution metadata or on tenancy/federation. **Files likely touched:** `TR/src/domain/registry_service.rs`, `TR/src/infra/storage/repo/{entity_repo,segment_filter}.rs`, `TR/src/infra/storage/entity/{entity,entity_gts_segment}.rs`, `TR/src/infra/storage/migrations/m20260925_000005_entity_gts_segment.rs`, `TR/src/api/rest/{dto,handlers,routes,cursor}.rs`, `TR/tests/{api_rest_test,repo_backends_test,discovery_pattern_backends_test}.rs`, `docs/database.sql`, `QUICKSTART.md`. **Scope:** L across the discovery REST/domain/repository path; split into the two working implementation slices above.

---

### - [x] T22d: Freshness validators and conditional reads

**Description:** The per-request validator of DESIGN §3.3 and the conditional reads it enables (SPEC §8.5, P9/P19). For a P0 managed platform-plane read the validator is a versioned digest over `entity.resource_version`, `type_schema.resolution_fingerprint` (Type Schemas only) and T22b's normalized selected-field set — every other input in DESIGN's table is `tenant plane only`, availability-conditional or external, so none of them applies here. Ships the `ETag` / `If-None-Match` → `304` path on exact reads and per-key validators on `batchGet`.

**Placement.** Last in Phase 6, after T22c and before T29 (P21), so Checkpoint 6 closes the complete P0 REST contract on `/v2/`. The task is server-only: the domain service computes the validator and the REST adapter carries it. T29 then exposes that same value in the SDK models — P9's requirement that validators precede remote clients and caches is unaffected. T22a already accepts and length-checks each item's `if_none_match`; this task is where it is first compared.

**Acceptance criteria:**
- [x] Validator is **computed per request, never stored** (`cpt-cf-types-registry-principle-derive-not-store`) — no column, no cache entry holds one as authority
- [x] Inputs are exactly `resource_version` + `resolution_fingerprint` (Type Schemas; Instances have no derived form) + T22b's normalized field set. A `TODO` names the P1 additions: subject visibility-chain version, Context Tenant availability-chain version, routing generation
- [x] Wire form per DESIGN: base64url of a **versioned** JSON object, identical bytes in `ETag` and in batch bodies; 128-bit digest for the managed case. The version field is what lets P1 add inputs without honouring a P0 token
- [x] Comparison decodes fields — never compares encoded strings, so serialization differences cannot read as a change
- [x] Projection is digested as the **normalized field set**, not the query string; absent `$select` equals the explicit T22b default set (RFC 9110 §8.8.3). A narrow token never produces false `unchanged` for a wider representation, including two selections of the same key in P0
- [x] Exact read: response carries `ETag`; a matching `If-None-Match` returns a bodyless `304` **that still carries the `ETag`**, declared through `no_content_response(StatusCode::NOT_MODIFIED, ..)`
- [x] `batchGet`: validators travel **beside individual keys**, in each item's `if_none_match`, because one header cannot represent them; a result may be `unchanged`, and the response stays `200` even when all are. The two body fields are the two header names lowercased — request `if_none_match`, response `etag` — so the batch surface reads like the single one
- [x] An `unchanged` result **carries its `etag`**, and the exact read's `304` **carries its `ETag`** — RFC 9110 §15.4.5 has a `304` send the validator a `200` would have. Every result but `not_found` therefore has one, so a refresh loop has no special case
- [x] Test: `unchanged` and `304` both return the same validator the caller sent, byte for byte
- [x] **Discovery pages carry no validator** and are never conditional (DESIGN: validators are for exact reads, *"never discovery pages"*) — `GET /entities` is unaffected
- [x] A deleted entity still has a validator, and deletion moves it (deletion increments `resource_version`)
- [x] Handlers stay mapping-only: the validator is computed in the domain service, so a future gRPC adapter gets it without new domain methods (SPEC §8.4)
- [x] `types-registry-sdk` is untouched: the validator is a domain value, and T29's local client maps it into the SDK models rather than recomputing it (P21)
- [x] `ponytail:`-style comment where the digest is built records ceiling C7's absent tenant/visibility dimensions and names the version field as the upgrade path

**Verification:**
- [x] Gear tests, all three backends (see [Commands](#commands))
- [x] Test: unchanged entity yields a byte-identical validator across two reads; a revision changes it
- [x] Test: a dependent whose `resolved_schema` was refreshed gets a new validator **even when its own `resource_version` did not move** — this is why `resolution_fingerprint` is an input, and it is the case a `resource_version`-only digest gets wrong
- [x] Test: `If-None-Match` with the current validator returns `304` with no body; with a stale one returns `200` and the document
- [x] Test: `batchGet` with a mix of current and stale validators returns `200`, `unchanged` for the current ones, full snapshots for the rest
- [x] Test: an Instance validator omits `resolution_fingerprint` and still changes on revision
- [x] Test: decoding rejects a validator whose version field is unknown rather than treating it as a match
- [x] Test: one key under two different selected-field sets has two different validators;
  field order, case, an explicit default set and explicitly naming mandatory
  `kind` or `lifecycle_status` do not change the normalized validator when the effective fields match
- [x] Test: deletion changes the validator
- [x] `make e2e-local` remains green with no e2e file edited — only `/v2/` routes change

**Dependencies:** T22a (`batchGet` and its per-item `if_none_match`), T22b (normalized projection and the three read routes, Phase 6 per P19). Not T29: the SDK carries the validator this task computes, not the reverse (P21) **Files likely touched:**
- `TR/src/domain/validator.rs`
- `TR/src/domain/registry_service.rs` (the database read path; `service.rs` is the legacy in-memory service)
- `TR/src/api/rest/handlers.rs`, `TR/src/api/rest/routes.rs`, `TR/src/api/rest/dto.rs`,
  `TR/src/api/rest/etag.rs`
- `TR/tests/validator_test.rs`, `TR/tests/api_rest_test.rs`, `TR/tests/projected_read_backends_test.rs`
**Scope:** M

---

### Checkpoint 6
- [x] The REST surface is complete on `/v2/`: all seven routes in OpenAPI; `batchGet` returns
      explicit per-key results; all three read routes apply `$select`; the default is
      document-free and discovery filters by `pattern`, inclusive `depth` and `kind`;
      its cursor binds those filters and the normalized field set while traversing the
      matching stable set exactly once; `QUICKSTART.md` covers reads and mutations
      (T20a, T22a, T22b, T22c, P17/P19/P20)
- [x] Conditional reads work on `/v2/`: an exact read carries a per-request validator and
      honours `If-None-Match` with a `304` that carries its `ETag`; `batchGet` reports
      `unchanged` per key with its `etag`; discovery carries none (T22d, P21)
- [x] P0 REST is complete; the remaining server work is the atomic cutover and path
      promotion in T38, Phase 7
- [x] `make e2e-local` remains green with no e2e file edited; gear tests and `make lychee` pass
- [x] Nothing is cut over yet — consumers still on the old path, and the new SDK trait is not
      written yet (T28 opens Phase 7 with the publisher signature, followed by T29's contract); inventory attribution metadata remains P1 (P18, #4827)
- [x] `make dylint` — full workspace, once for the phase (P13)
- [ ] Human review

---

## Phase 7 — Both clients, real local AM, and every gear on the persistent registry

Plans P26–P31. T28, T29 and T30 are complete; the next task is **T31**. T33 is implemented on branch `toolkit-platform-route-auth` and is checked off only once merged into `main`. The remaining sequence is **T31 → T32 → T33 → T34 → T35 → T35a → T35b → T36 → T37 → T38**. T35a adds the resolved-payload validation wrapper and T35b aligns mock Type Schema admission before consumer cutover; existing task numbers stay unchanged.

- **Traits and local clients (T29, T30).** Both contracts, the platform and tenant extension helpers and both local clients land together, in one pull request.
- **Embedded local handoff (T31).** All linked inventory and `cfg.entities` are admitted into the database before the new APIs are published in ClientHub. A new gear can reconcile in `init()` without waiting for REST, T37 or the fleet migration.
- **Real local application (T32).** AM, both IdP writers and the tenant-resolver family use the new catalogue together. Existing tenant/metadata/IdP REST e2e prove real behavior; the rest of the fleet stays legacy until T38.
- **SDK contract handoff (T34–T37, Checkpoint 7A).** REST/TCP contracts, resolving clients, cache and generic lifecycle are verified, alongside T32's real local AM. Full process/directory/cold-start proof belongs to real remote AM in T41.
- **Persistent registry for every gear (T38, Checkpoint 7).** One atomic, shim-free cutover moves every remaining consumer onto the already-seeded database and new SDK; P0 then ends on one REST version.

**e2e windows.** T34 and T35 change routes and their e2e callers in the same commit, so `make e2e-local` stays green through Checkpoint 7A and the T38 cutover: legacy deletion, tenant path promotion and callers merge together (P30). No intermediate red window is accepted.

### - [x] T28: Toolkit — publisher signature and publication status

**Description:** Publication data, SemVer and supervision needed by T29 (D16/D18/D21).

**After review:** types moved from `toolkit-gts` to SDK `publication.rs`; supervision moved from toolkit to SDK `supervised.rs`. `spawn_supervised` is crate-private; `Supervised`, `SupervisedStatus` and `TaskExit` remain public for returned handles. The unused toolkit publisher signature, test and re-exports were removed; the SDK imports `tokio_util` directly. T37 adds only generic readiness/supervision to toolkit. `PublisherContext`, `PublisherVersion` and `PublisherVersionError` later moved to `models.rs`: they are fields of the write requests, not publication state. The publication status, `supervised.rs` and their tests were then **removed** as unused before their first caller; T37 writes them under its own contract (the T28 code is in `c5f491cc8`, `TR-SDK/src/publication/`). The checked criteria below retain the initial implementation record.

**Acceptance criteria:**
- [x] Initial `toolkit::GtsPublisher` signature: `fn(Arc<ClientHub>, PublisherContext, Vec<GtsDeclaration>, CancellationToken) -> Supervised<PublicationStatus>`, returning immediately. Declarations are `(String, serde_json::Value)` plain data; no `toolkit-gts` type crosses collector boundaries. Signature removed after review above
- [x] Initial `toolkit-gts` publication data (now SDK): per-identifier pending/admitted/rejected/superseded. Readiness requires all admitted or superseded-live; empty sets satisfy it. Terminal means no pending identifiers. Reasons distinguish waiting, unreachable registry, blocked dependency, registry refusal and stopped publisher; superseded-deleted does not satisfy readiness
- [x] `PublisherVersion` wraps `semver::Version`; Eq/Ord/Hash use precedence and ignore build metadata, while Display preserves it. `FromStr` checks `MAX_LEN = 128` before parsing; `TryFrom<&str>` delegates. Invalid/oversized inputs fail. T44 adds only domain stamp/name validation; workspace `semver = "1.0"` was already transitive
- [x] Supervision owns one join handle, catches construction/poll/cancellation-drop panics, and settles unfinished status via `SupervisedStatus::stopped(exit)`. Pending identifiers become `PublisherStopped`; settled outcomes remain and terminal status ignores reports. Workers receive child cancellation tokens. `join()` waits; dropping a handle detaches without cancellation so status still settles. Initially toolkit, now SDK (review above)
- [x] `version` is a validated SemVer value; the type has no constructor that reads the calling crate's own `CARGO_PKG_VERSION`, so the version can only come from the publishing crate (T39)

**Verification:**
- [x] Initial tests: `toolkit-gts/tests/publication_test.rs` (15) covers SemVer precedence/hash/128-byte bound and per-identifier readiness; `toolkit/tests/supervised_test.rs` (11) covers return/error/panic/cancellation/drop, terminal reports and child-token isolation. Disabling settlement fails 5 tests. `cargo test -p cf-gears-toolkit -p cf-gears-toolkit-gts --all-features` passed; tests later moved to SDK
- [x] Initial `toolkit/tests/gts_publisher_test.rs` compiled using only toolkit re-exports; independent review confirmed no other extern crate was needed. Removed with the unused signature after review
- [x] `cargo check --workspace --all-targets` green — the new root re-exports collide with no glob import; `make fmt`, `make clippy`

**Dependencies:** Checkpoint 6 **Files touched:** `types-registry-sdk/src/{publication,publication_tests}.rs` (after the revision above; originally `libs/toolkit-gts/src/{lib,publication}.rs` and `libs/toolkit-gts/tests/publication_test.rs`), `types-registry-sdk/src/{supervised,supervised_tests}.rs` (after the revision above; `libs/toolkit` is untouched), `Cargo.toml` / `Cargo.lock` (`semver`) **Scope:** S

---

### - [x] T29: `PlatformTypesRegistryApi` contract, models, local client, reconciliation and publication

**Description:** `PlatformTypesRegistryApi` plus its models per SPEC §10.1 and D15, served in process by the local client. It carries two SDK helpers so that callers need not hand-roll batching, idempotency or retry:
- the reconciliation workflow of DESIGN §3.3 over explicitly supplied desired documents;
- `publish_gts`, which runs that reconciliation for a gear in the background and reports a status handle (D16).

The REST clients are T34 (platform) and T35 (tenant); the per-crate collectors that feed `publish_gts` are T39. The old trait is **not** kept — T38 moves every consumer and deletes it.

**Placement.** T28 supplies the data/supervision types; T29 precedes T31's local handoff and T38’s remaining consumer cutover. Local reconciliation stays synchronous in `init` until T42. The complete read/validator contract and required publisher model land now to avoid another consumer API migration; adapters send publisher only from T44.

**After review:** `reconcile_entities_and_await` replaces the public `register_entities_and_await`: startup declarations need read/compare/update, since creating an existing identifier yields `already_exists` even for equal content. The crate-private `await_registration` retains submit/poll deadline and cancellation tests. Callers needing their own keys/preconditions compose `register_entities` and `get_operation`.

**After review, unused surface removed** (the removed code is in `c5f491cc8`):
- `publish_gts` / `publish_gts_with` / `PublishOptions`, the publication status and the supervised task had no caller; T37 writes them with its hook and readiness contract.
- `reconcile` moved to `TR-SDK/src/reconcile.rs` and became crate-private; the public entry is `reconcile_entities_and_await`. `Outcome` and `PendingCause` are `ReconcileOutcome` and `ReconcilePendingCause`; the `GtsDeclaration` alias is gone (`&[(String, JsonDocument)]`).
- `ReconcileOutcome::Superseded` and `Liveness` (the post-outcome liveness re-read) are gone: the registry emits `superseded` only from T44, so before then it could not occur. A `superseded` item is an ordinary `Rejected` until T45 restores the variant.
- `PlatformTypesRegistryApiExt::delete_entity` had no caller: a deletion is one `delete_entities` call. The fake lost its `deletions()`, `hang_read`, `panic_on_submit` and `x-fake-superseded*` hooks with their last users.

The checked criteria below retain the initial implementation record.

**Acceptance criteria:**
- [x] `PlatformTypesRegistryApi` is declared with `#[toolkit::contract(gear = "types-registry", version = "v1")]` and compiles: the `Api` suffix, `Result<_, CanonicalError>` on every method, `#[idempotency(..)]` per SPEC §10.1
- [x] Every method takes `&PlatformSecurityContext` first. Callers pass `PlatformSecurityContext::outbound_marker()`. The local client passes it through without validating credentials, and records no principal from it (C2)
- [x] The trait has **no default methods**. `delete_entity`, `register_entities_and_await` and the convenience reads live in `PlatformTypesRegistryApiExt`, blanket-implemented for `T: PlatformTypesRegistryApi + ?Sized`
- [x] Object-safe: `hub.get::<dyn PlatformTypesRegistryApi>()` compiles, and the extension methods are callable on it
- [x] Models are field-for-field the SPEC §10.1 shapes, with out-of-scope fields absent rather than renamed. The mutation and operation types are DESIGN's (`RegistrationOperation`, `DeletionOperation`, `Operation`)
- [x] **No serde, no utoipa, no HTTP types on the semantic models.** Wire DTOs belong to T34, behind `rest-client`
- [x] Flat, wire-expressible models: no `Arc` parent graphs or trait objects (P5)
- [x] **Convenience reads as extension methods** over the two read primitives, so consumers keep familiar call shapes:
  - `get_type_schema`, `get_instance`, `batch_get_type_schemas`, `batch_get_instances`;
  - the `_by_uuid` variants;
  - `list_type_schemas`, `list_instances`.

  Kind narrowing costs no round trip: the kind is the trailing `~` of the identifier, so a kind-mismatched argument fails locally
- [x] `Entity` exposes the materialized documents as **plain fields** (`content`, `resolved_schema`, `effective_traits`, `effective_traits_schema`) plus a `segments` accessor, so the ~40 call sites using the old models' computed methods become field reads rather than rewrites
- [x] `origin` is the DESIGN managed variant in P0. It carries the read `resource_version` and the timestamps used by reconciliation. There is no content digest (SPEC §10.2), and selectable documents are flat; `provenance` is the one group, carrying the Type Schema waiver as `compatibility_check_skipped`
- [x] Documents are individually selectable, with explicit omission and T22b normalized selection; selecting traits need not transfer the 1 MB-bounded resolved schema
- [x] **No `effective_*` recomputation exists in the SDK.** The old `GtsTypeSchema::effective_schema` / `effective_properties` / `effective_required` / `effective_traits` / `effective_traits_schema` are not reproduced (SPEC §10.1)
- [x] `ListEntitiesRequest` carries `limit` and `cursor`, and `ListEntitiesResponse` carries the next cursor (D12)
- [x] `ListEntitiesRequest::filter` carries `pattern`, `max_chain_depth`, `kind` and `lifecycle` as typed fields. `list_type_schemas` and `list_instances` request their `kind` server-side, preserving caller-supplied pattern/depth and cursor
- [x] `list_instances` / `list_type_schemas` **explicitly select the documents their callers read**, on the discovery page or through a following `batchGet`. The doc comment states the trade: complete with respect to the traversal, not to an instant
- [x] **The validator field is in the models from this task**, carrying the value T22d's domain service computes; the local client maps it and never recomputes one. `BatchGetItem::if_none_match` and an `unchanged` result variant are part of the same shape (SPEC §8.5, P9)
- [x] Mutations submit then read `get_operation`; never synthesize from receipts, including terminal replay. Read-back errors name `operation_id` for same-key replay (D19). REST handles `Retry-After` internally; semantic models omit it and `replayed`
- [x] `register_entities_and_await` holds one monotonic deadline across submit and polling, honours cancellation, and names the `operation_id` on timeout; a timeout never cancels the accepted write
- [x] Required request-level `PublisherContext { name, version }` on registration/deletion. `CandidateStatus` is unchanged; `superseded` carries stored/offered versions and `publisher_mismatch` stored/offered publishers. Snapshots omit publisher metadata
- [x] **Reconciliation helper** implements DESIGN §3.3's five steps:
  - batch-read the desired identifiers;
  - omit content equal to current;
  - set `expected_resource_version` from the read for differing ones, and leave it unset for missing ones;
  - return `UpToDate` with no POST when nothing remains;
  - otherwise submit and poll to terminality.
- [x] The helper accepts an explicit desired-document set, requests `content` for its comparisons, and batches within `limits.batch_candidates`. It collects no inventory itself and deletes nothing absent from its input
- [x] **Idempotency key scope:**
  - one key spans retries of an **identical** submission and its polling;
  - a new cycle — a re-read that changes candidates or preconditions — takes a new key (ADR-0012:168).
- [x] `already_exists` / `precondition_failed` caused by a concurrent publisher of the same content lead to a re-read, not a failure
- [x] **`publish_gts`** has the publisher shape T28 described and T37 defines as `toolkit::GtsPublisher`: it takes the `ClientHub` (resolving `dyn PlatformTypesRegistryApi` from it), the caller's `PublisherContext` (never built inside the SDK), plain-data declarations and a cancellation token, returns `toolkit-gts`'s publication status, and runs reconciliation in a background task supervised by the SDK's `spawn_supervised` (T28, revised):
  - bounded retries per cycle for a dependency not yet registered, with backoff continuing across cycles;
  - a pace set by `Retry-After`.
- [x] It returns a status handle — *pending*, *admitted*, *rejected* or *superseded*, with per-identifier reasons. A permanently rejected or superseded candidate stops retrying, and failure names the gear and identifier. A task that panics or exits is observed and never leaves the status *pending*
- [x] Reconciliation keeps the `UpToDate`/no-`POST` contract until T45 makes it always submit, once the server confirms a higher version (SPEC D18). Every caller supplies a `PublisherContext`, because the models require it, but until T44 neither adapter sends it to the server
- [x] `publish_gts` and every remote use are never called from `init()`: remote clients are wired after every `init` (SPEC §8.4), so publication belongs to the post-wiring hook (T37). The doc comments say so. The one transitional exception is T31–T42: synchronous reconciliation through the **local** client inside `init`, which T41/T42 remove

**Verification:**
- [x] `cargo test -p cf-gears-types-registry-sdk`
- [x] Test: a mock consumer round-trips submit → poll → read through the new trait
- [x] Gear test (`cargo nextest run -p cf-gears-types-registry`): through the real local client over `RegistryService`, a read carries T22d's validator byte for byte, and a `BatchGetEntitiesRequest` naming the current validator returns `unchanged`
- [x] Test: reconciliation returns `UpToDate` without submitting when everything already matches
- [x] Test: only supplied documents are reconciled; unrelated entities are neither submitted nor deleted
- [x] Test: publication converges when a base type is admitted only on a later cycle, and each cycle uses a new idempotency key
- [x] Test: two publishers of identical content both reach `admitted`
- [x] Test: a permanently invalid declaration reports `rejected` with its reason and stops retrying
- [x] Test: an operation nothing will drain fails on its deadline with a diagnosable error, not a hang
- [x] Test: a terminal replay returns the operation read back, with its real items; a read failing after an accepted submit surfaces the `operation_id`, and the retry replays (SPEC §13)
- [x] Test: a publication task that panics reports a terminal failure, not *pending*

**Outcome (five commits, `a29d9de1d` → S5).** Review findings incorporated.

- Request/response envelopes adopt AIP names in DESIGN/SPEC before T38 consumer migration; resource, item and method names stay unchanged.
- Additive sample deviations: `RegisterItem::gts_id` permits Instances without document IDs and checks schema `$id`; `register_entities_and_await` adds deadline/token cancellation. Expiry/cancellation wins before polling; timeout names a known operation, while cancellation relies on the caller’s key.
- `item_failure` encodes `FailedPrecondition`: violation 0 is reason/key/message; remaining violations are context.<name>/value. Unknown reasons/context round-trip. Default list helpers select content and schema materializations; explicit Select is preserved.
- `domain::local_client::LocalClient` shares the domain cursor and the error ladder with REST, and hands out the domain validator token unquoted (SPEC §8.5). Accepted-submit read-back failures are Aborted with operation ID; an injected outbox/database test proves same-key recovery. ClientHub registration was deferred; P28 assigns production local ClientHub registration to T31.
- Reconciliation settles equal content immediately. Only explicit NotFound permits creation; missing/incomplete/Unchanged replies are protocol faults. Batches get fresh keys; identical retries reuse them. A permanent submission refusal stops the call and returns a structured canonical report; hinted `ResourceExhausted` resends the same request after the hint. The canonical category and context decide for local and remote clients alike. Require one outcome per candidate and canonical caller spelling. Publisher mismatch rejects; supersession reads liveness after the outcome.
- Publication backs off across cycles with a 50 ms floor and clamped bounds. Unverified superseded liveness stays pending for bounded re-reads, never resubmission. Transport handles Retry-After. T37 checks its publisher signature.
- `FakePlatformRegistry` (`test-util`) supplies deterministic admission/fault hooks.
- Verification: 123 SDK tests, 14 real local-client/outbox tests, full gear suite 1199 green; scoped clippy and fmt clean.

**Dependencies:** T4 (reads), T21 (async dispatch), T22b (projection contract), T22c (discovery filter contract), T22d (validators), T28 (publisher signature, publication status, `PublisherVersion`, supervision helper) **Files likely touched:**
- `TR-SDK/src/contract.rs`
- `TR-SDK/src/models.rs`
- `TR-SDK/src/publication/reconcile.rs`
- `TR-SDK/src/lib.rs`
- `TR/src/domain/local_client.rs`
**Scope:** M

---

### - [x] T30: `TypesRegistryApi` tenant contract, its extension helpers and local client

**Description:** Add read-only `TypesRegistryApi` (`SecurityContext` first), its `TypesRegistryApiExt` helpers and a local adapter over the shared domain service. T29+T30 deliver all P0 traits/local clients in one PR. P27 brought the tenant contract/local adapter ahead of REST and added the tenant helpers. REST remains T34/T35; resolving wrappers and `#[provides]` belong to T35.

Commits inside the task: (1) tenant contract and extension helpers; (2) tenant local client.

**Acceptance criteria:**
- [x] `TypesRegistryApi` is declared with `#[toolkit::contract(gear = "types-registry", version = "v1")]` and compiles: `&SecurityContext` first on every method, `Result<_, CanonicalError>`, `#[idempotency(SafeRead)]`, no default methods. Object-safe: `hub.get::<dyn TypesRegistryApi>()` compiles and the extension methods are callable on it
- [x] The tenant contract shares the platform's semantic models, projection and validators; it has no mutation and no operation access
- [x] `TypesRegistryApiExt`, blanket-implemented for `T: TypesRegistryApi + ?Sized`, offers the read conveniences of `PlatformTypesRegistryApiExt` — `get_type_schema`, `get_instance`, their plural and `_by_uuid` variants, `list_type_schemas`, `list_instances` — with the same local kind narrowing and explicit document selection. One implementation of the helper logic serves both extension traits; no copy, and no mutation helper
- [x] Its local client reuses the platform lookups, encodings and errors — no second domain implementation — and returns what the platform local client returns for the same request, `unchanged` and cursors included
- [x] The local client applies no tenant scope and records no principal from the `SecurityContext` in P0; the C2/C6 source comments say so. It is verified in fixture hosts only — the existing embedded consumers stay on the legacy client until T38

**Verification:**
- [x] `cargo test -p cf-gears-types-registry-sdk`: extension helpers over a fake tenant API — kind mismatch refused locally without a call, explicit selection preserved, list helpers select the documents their callers read
- [x] `cargo nextest run -p cf-gears-types-registry --test tenant_local_client_test` (new target): exact read, `batchGet` `found` then `unchanged` under the same `$select`, discovery cursor, and parity with the platform local client
- [x] Gear tests; `make fmt`, `make clippy`

**Outcome (two commits).** Verified with scoped `cargo fmt --check` and `cargo clippy -D warnings` for both crates.

- `TypesRegistryApi` has two methods, `batch_get_entities` and `list_entities`: the tenant REST plane's three routes map onto them as on the platform contract, the exact read being an extension helper over the batch read.
- `TypesRegistryApiExt` and `PlatformTypesRegistryApiExt` bind their contract and context into one private `EntityReads`; the helpers exist once. `FakePlatformRegistry` serves both contracts from one store through shared inherent reads.
- `domain::local_client::LocalClient` (renamed from `PlatformLocalClient`) implements both traits over two shared inherent reads, so parity is structural; `tenant_local_client_test` compares every tenant answer, validators, cursors and refusals included, with the platform's. The gear registers neither client in its ClientHub; fixture hosts construct it.

**Follow-up (P28):** that outcome describes T30's implementation boundary. T31 publishes both local APIs in the ordinary database-bound gear after seeding; REST resolving wrappers and descriptors belong to T35's closing stage (P30).

**Dependencies:** T29 **Files likely touched:** `TR-SDK/src/{tenant_contract,ext,lib}.rs` and their tests, `TR/src/domain/local_client.rs`, `TR/tests/tenant_local_client_test.rs` **Scope:** M — two commits as listed

---

### - [ ] T31: Database seeding and local ClientHub handoff — a new gear publishes in `init()`

**Description:** Make T29/T30's APIs usable by a new in-process gear in the ordinary embedded host. Start the real admission worker, seed all linked inventory and `cfg.entities` into the database, await successful admission, then publish both local contracts in ClientHub. Prove a consumer resolves the platform API and registers/reads its own entities from `init()` through the actual host lifecycle. This takes seeding/local publication from T38; T38 retains the remaining fleet migration and removal of the legacy path. Plan P28 and SPEC D6/D11/§8.4 record the temporary coexistence and local-init exception.

**Host prerequisite:** a database is bound to types-registry (already true in quickstart/e2e configurations). No new API is registered in a no-db host; resolving it fails explicitly, with no legacy fallback. Legacy-only no-db hosts still boot. No REST client, resolving wrapper, T36 cache, T37 hook or publisher-version guard is required for this local handoff.

**Transition rule (until T38):** keep the old client/store/routes for existing consumers and seed them as before; seed the same initial declarations into the database for the new APIs. There is no continuous mirroring, shim or dual-write of subsequent mutations. A new consumer uses only the new API for its entire registry workflow, and no legacy writer owns its mutable identifiers. Shared static inventory declarations may be in both stores. A document depending on an entity registered only by a later legacy call cannot use this path until its dependency is made part of database seeding or published earlier through the new API. T38 deletes the old catalogue and this exception; Full remote application verification belongs to T41 (P31).

Implement sequentially in four S/M slices; do not land a client before its seed barrier works. Every slice clears the standing bar.

**Slice 1 — seed audit and deployment limits (S, three primary files):**
- [ ] Audit derivation/`$ref`/Instance-of dependencies of the actual linked inventory and `cfg.entities`, including registry control-plane and `toolkit-gts` base types. Record and resolve dependencies previously supplied only by a later legacy `init` call before enabling immediate seed validation; do not defer unresolved inline seeds to `post_init` or wait for a later registrant. Keep the fleet-wide lifecycle audit in T38. An entity reference (`x-gts-type-ref` / `x-gts-instance-ref`) is syntax/pattern validation, not target existence (P14): e2e's customer `allowed_parent_types` does not require the later AM root type, so do not duplicate AM publication or pull T40/T41 forward for that value
- [ ] Measure the actual combined inventory/configured count for the quickstart, example and e2e feature sets; record counts and feature sets in the audit. Set explicit deployment `limits.batch_candidates` values that fit these sets and check their other admission budgets. The current default is 100 and a source-level macro count is not a linked-population measurement; preserve a deliberately smaller limit in the refusal test rather than weakening enforcement or assuming the default fits
- [ ] Validate the audit against cold-start seed fixtures before publishing a client. If a genuine dependency requires declaring-crate changes, record and implement those as prerequisite commits of at most five files; do not silently broaden a slice or downgrade the full-seed/green-e2e criteria

**Verification — slice 1:**
- [ ] Record reproducible collection/count commands for each real feature set and the chosen deployment budgets. In slice 2, admit the actual configured customer schema with its linked base but without the later AM root schema; assert that a genuine missing derivation/`$ref` target, unlike an entity reference, is refused

**Files likely touched — slice 1:** `config/quickstart.yaml`, `config/e2e-local.yaml`, the seed audit in this file. Configuration paths for a distinct example feature set are included only if that composition uses another configuration

**Slice 2 — database seeding (M, five primary files):**
- [ ] Start the outbox before submitting; seed the complete combined Type Schema/Instance inventory + configured set through `RegistryService` and real outbox admission in one graph-ordered submission. Implement the seed orchestration in `domain/seeding.rs` with domain reads → compare → CAS → submit → await. The seed barrier admits the complete dependency graph in one operation; SDK reconciliation publishes independently batched consumer declarations. Check the whole seed set against configured limits before any submission; page preparatory reads if necessary. Reuse domain admission/validation
- [ ] Read/compare/CAS supports restart and permitted configuration changes; equal documents allocate no new revisions, omitted documents are never deleted, and ordinary compatibility/lifecycle rules still apply. Two same-version registry instances seeding the same empty or populated database concurrently converge: re-read `already_exists`/`precondition_failed` conflicts under the bounded seed deadline, reuse an idempotency key only for an identical submission and take a new key after candidates/preconditions change. Mixed-version ordering remains T44/T45
- [ ] Await every seed outcome under one monotonic deadline starting before preparatory reads: the whole seed budget is `cfg.worker.operation_timeout` (default five minutes), not multiplied by delivery attempts or reset after CAS retries. Use the runtime cancellation token; a short configured budget exercises expiry. Only successful/equal seeds permit client publication; rejection, pending/missing dependency, limit refusal, deadline and cancellation fail boot with identifiers/reasons. The registry waits only for its own seeds, never a future consumer. Publisher stamps remain Phase 9; do not add an attribution/ordering mechanism here

**Verification — slice 2 / intermediate review:**
- [ ] `cargo nextest run -p cf-gears-types-registry --test seeding_test`; `make test-integration LANE=types-registry-db` — seed/restart/refusal and concurrent same-version seeding behaviour on SQLite, PostgreSQL and MySQL, using existing shared delivery helpers. Put container cases in `seeding_backends_test.rs` behind the existing `integration` feature so the make target actually selects them
- [ ] At least two linked crates, a cross-crate dependency, real `toolkit-gts` base declarations, registry declarations and a configured dependent entity are readable through database-backed reads; a second boot preserves revisions/artifacts, and a permitted configured-content change updates through CAS
- [ ] Invalid/unresolved seeds and an oversized combined set fail before either new client is published; a seed failure/cancellation leaves no running worker owned by the failed startup. The configured customer schema succeeds without its entity-reference target being stored, and a real missing parent/`$ref` fails; concurrent seeders both succeed with one admitted version of each equal document. Review the audit, budgets and seed barrier before slice 3

**Files likely touched — slice 2:** `TR/src/gear.rs`, `TR/src/domain/mod.rs`, `TR/src/domain/seeding.rs` (new), `TR/tests/seeding_test.rs` (new), `TR/tests/seeding_backends_test.rs` (new). Reuse existing DB/outbox fixtures

**Slice 3 — ClientHub and consumer `init()` (M, two primary files):**
- [ ] After the seed barrier, construct `domain::local_client::LocalClient` over the existing `RegistryService` and register `dyn PlatformTypesRegistryApi` and `dyn TypesRegistryApi` over that same service. New reads/mutations never fall back to `TypesRegistryService`, recompute artifacts or read the legacy cache; no-db/failed-seed hosts expose neither new API
- [ ] A fixture gear declares `deps = [types_registry]`, resolves the platform API directly from ClientHub in `init()` and calls `reconcile_entities_and_await` for explicit owned documents with `PlatformSecurityContext::outbound_marker()`, its own name/`CARGO_PKG_VERSION`, an explicit deadline and `ctx.cancellation_token()`. `UpToDate` or all-`Admitted` is success; every `Rejected`/`Pending` or call error fails that consumer's boot with identifier/reason. Do not treat acceptance of an operation or `Ok(Reconciled(..))` alone as success
- [ ] Exercise the real host's DB migration → registry init/seed → consumer init path before `post_init`/`start`. Build an explicit `toolkit::registry::RegistryBuilder` containing only the production registry and fixture consumer; register the registry's core/DB/system capabilities and its real `WithLifecycle` runnable, but omit REST registration in this local fixture (otherwise a REST host is required). Use `HostRuntime::new`, `DbOptions::Manager` and public `run_gear_phases()` as in toolkit runner tests. Keep the fixture in this integration-test binary, avoid `discover_and_build()`/global gear discovery, observe consumer-init results through test synchronization, then cancel and join the host. The consumer creates/reads an entity depending on a seeded base/configured type, updates on restart and succeeds unchanged on equal restart. Assert tenant API resolution and read parity from the same hub; no runtime consumer imports registry internals or needs a new test-only toolkit lifecycle API

**Verification — slice 3 / intermediate review:**
- [ ] `cargo nextest run -p cf-gears-types-registry --test local_client_boot_test` — real ClientHub/lifecycle tests for first boot, restart, update, absent database, invalid seed, missing consumer dependency, rejected declaration, deadline and cancellation; use existing shared delivery helpers rather than test-local polling/sleeps
- [ ] The missing-dependency case terminates within the configured reconciliation budget and fails boot; it never waits for a later gear's `init`. Boot does not depend on `serve()` starting the already-running outbox
- [ ] Existing legacy consumers/legacy REST remain green; the new path writes only the database and is not silently visible to legacy reads. Review the seed barrier and lifecycle proof before the documentation handoff

**Files likely touched — slice 3:** `TR/src/gear.rs`, `TR/tests/local_client_boot_test.rs` (new, fixture gear declared in this target). Reuse `TR/tests/common/` only where shared setup is necessary

**Slice 4 — local integration handoff (M, three or four primary files):**
- [ ] `types-registry-sdk/README.md` leads with the new API: `PlatformTypesRegistryApi` for a gear's publication/reads and read-only `TypesRegistryApi` for tenant requests, current semantic models and canonical errors. Show complete usable examples of obtaining the client from ClientHub, reconciling entities and waiting for outcomes, then reading them; the local gear example includes dependency declaration, platform context, publisher, cancellation/deadline and exhaustive outcome handling. Move **all** legacy-client/API/model/error descriptions and examples into a separate `Legacy API` section, clearly marked for removal at T38; no main example uses the old API. QUICKSTART carries the local flow and explains the seed sources, immediate validation, no-db refusal, uncached pre-T36 behaviour and temporary independent catalogues
- [ ] Replace the SDK's duplicate crate-level `//!` introduction/legacy usage with `#![doc = include_str!("../README.md")]` in `src/lib.rs`, keeping module/API docs intact, and compile the main usage examples as `no_run` doctests (not `ignore`/text). Include the explicit dependency declaration and no-db resolution error; add missing example-only imports as workspace dev-dependencies in `TR-SDK/Cargo.toml` following dependency guidelines. Correct stale old symbols/examples when moving them into `Legacy API`. Document that `deps` orders `init` only; remote wiring is later; local tenant contexts remain unvalidated/unscoped under C2/C6. The local-init exception ends in T41/T42; publisher metadata is required in the SDK but forwarded/enforced only in T44/T45, without an early mixed-version guarantee
- [ ] `make quickstart` and the example composition boot with all linked inventory/configured entities readable through the new database API; restart is idempotent. Audit the current v2 e2e registration/discovery suites against the now-prepopulated database: scope test entities/counts to their own namespaces and do not create an already-seeded identifier expecting fresh creation. Any necessary e2e adjustments land in separate commits of at most five files, preserve admission assertions and keep `make e2e-local` green on its current routes, with no earlier legacy-v1 red window

**Verification — handoff:**
- [ ] `cargo test -p cf-gears-types-registry-sdk`; `cargo test -p cf-gears-types-registry-sdk --doc`; `cargo nextest run -p cf-gears-types-registry`; `make test-integration LANE=types-registry-db`; `cargo check -p cf-gears-types-registry --all-targets`; `make fmt`; `make clippy`
- [ ] Run the documented local consumer example against the ordinary registry gear and repeat its startup; the existing manually assembled `platform_local_client_test` alone does not satisfy this handoff. Review every SDK README section: old `TypesRegistryClient` and its legacy models/examples appear only under `Legacy API`, and the main examples use the signatures/models actually exported by the new SDK

**Files likely touched — slice 4:** `TR-SDK/README.md`, `TR-SDK/src/lib.rs`, `gears/system/types-registry/QUICKSTART.md`, plus `TR-SDK/Cargo.toml` only if compiled examples need additional dev-dependencies; e2e namespace corrections under `testing/e2e/suites/types_registry/` are separate S/M commits if the audit identifies them

**Dependencies:** T29, T30, T21 (outbox), T22b–T22d (reads/validators); no dependency on T33–T37. Execution continues with T32 after T31; Checkpoints 7A/7 and their full gates are unchanged **Scope:** four sequential S/M slices above, reviewed as one local registration feature, with small prerequisite/e2e corrections explicitly recorded by their audits. T38 must recheck these guarantees after deleting legacy, rather than reimplement seeding

---

### - [ ] T32: Full Account Management local migration — one registry client and real REST proof

**Description:** Translate the entire Account Management registry workflow to `PlatformTypesRegistryApi` immediately after T31, and prove it through existing product HTTP requests. AM resolves one platform client and shares it with all services: no legacy client, hidden compatibility adapter, fallback or dual write. Move the connected writer/reader group atomically so optional configurations do not lose their plugin. This is the first real local application gate; licensing and a metadata-only partial migration are outside this task (historical decision P29).

**Group — seven gears:** `account-management`, `static-idp-plugin`, `keycloak-idp-plugin`, `tenant-resolver`, `static-tr-plugin`, `single-tenant-tr-plugin`, `rg-tr-plugin`. AuthZ and RG business implementations remain on their current SDKs; their public clients do not change. Keep `deps = [types_registry]` for synchronous local registration and initialization. T41, not this task, add post-wiring/remote behavior and publication readiness.

**Delivery:** six sequential stages, each split into S/M commits of around five files when needed; one atomic merge of the complete group and its e2e helpers. Stage boundaries organize implementation/review and are not deployable mixed catalogue configurations. `AM/` below means `gears/system/account-management/`.

**Stage 1 — prerequisites and database bindings:**
- [ ] Audit every AM registry reader/writer and external consumer of its identifiers; classify SDK inventory, configured root, IdP instances, optional AM TR instance and runtime metadata schemas. Record why each writer and its reader move together, and check RG type-code/metadata dependencies. Audit registration policy for configurable GTS IDs, especially a non-`cf` root suffix: add only explicit intended region/vendor allowances to the affected deployment and test a closed-region refusal with clear diagnostics. Distinguish the GTS ID's last-segment vendor from `PluginV1.vendor` selection metadata; current fixed `cf` plugin IDs do not need an allowance for a metadata vendor string. RG user-group bootstrap uses its own DB and `metadata_schema = None`; preserve its seal. Resolve any real external dependency before merge, never by mirror/fallback
- [ ] Inventory effective feature/config compositions where TR/IdP writers run and add a Types Registry database binding wherever missing. Check `config/{static-tenants,mini-chat,quickstart-windows,github-mirror-dev,e2e-launcher}.yaml`, usage-collector configs and mini-chat/resource-group suite overlays; distinguish overlay inheritance from complete configurations. Include `GEAR_SERVER_BASE_FEATURES` launchers. Keep supported boots working; no new API means an explicit failure, not legacy fallback
- [ ] Audit immediate-admission order and preserve T31's seed barrier: AM SDK/base types and configured customer types are already in the database; every dynamic dependency must be admitted before its first consumer. Retain the current reduced RG test profile and track the pre-existing #4568 bootstrap failure separately, without adding its repair to this task

**Stage 2 — complete tenant-resolver instance family:**
- [ ] Move core discovery to `PlatformTypesRegistryApiExt::list_instances` with explicit content and complete page traversal, canonical errors and unchanged vendor/priority selection. A failure/limit must propagate, never select from a truncated page
- [ ] Move all three standalone TR plugins to bounded/cancellable reconciliation with their own publisher context. Publish scoped clients only after `UpToDate` or all-`Admitted`; refuse `Rejected`/`Pending` with useful diagnostics. No legacy instance writer remains for this contract
- [ ] Prove static/single-tenant behavior and the RG plugin's publication independently of RG's known bootstrap issue; include a winner on a later discovery page, missing instance, restart and changed plugin metadata. AM's optional TR instance joins this same new catalogue in stage 4

**Stage 3 — all AM schema readers and models:**
- [ ] Migrate tenant/user/service-account payload checks, parent/child type enforcement, metadata schema registry, forward/reverse UUID hydration and optional TR plugin queries to the single new platform client. Use new typed IDs/snapshots; no production `TypesRegistryClient`, legacy models or parent-graph helpers remain in AM at completion
- [ ] Select authored content, materialized `resolved_schema` and `effective_traits` where needed; validate against those fields, not reconstructed parent chains. Handle `Result<HashMap<_, Option<_>>, CanonicalError>` correctly: unknown schema is absence, failed lookup is an error. Preserve tenant validation, metadata 404/validation mapping, dependency-unavailable behavior and UUID round-trip checks; incomplete projections are not success
- [ ] Port adjacent tests/fakes to the new contract and materialized values without weakening behavior assertions. Use `types_registry_sdk::testing_platform::MockTypesRegistry` (`test-util`) for shared test doubles; T38 later moves that import to `types_registry_sdk::testing::MockTypesRegistry` when legacy is deleted. No private schema memo/cache is added; T36 supplies the SDK cache. Review metadata, type enforcement and UUID scenarios against real database snapshots, not only fakes

**Stage 4 — root registration, bootstrap and AM TR plugin:**
- [ ] Preserve the existing init-time stored-root binding check **before any root reconciliation/write**. A configured root UUID mismatch fails under both strict policies without creating an orphan root schema or allocating a registry revision; assert that against the real database. Then replace root registration with local `reconcile_entities_and_await` using AM's publisher, deadline and runtime cancellation; require admission before bootstrap reads. Equal restart is idempotent; permitted content changes use CAS and normal compatibility rules
- [ ] Move bootstrap preflight and the optional AM TR plugin's instance registration and schema/UUID reads to that same new client. When enabled, the instance is visible to stage 2's migrated resolver and serves actual tenant queries; when disabled, neither instance nor scoped client is published. Test both states and vendor/priority selection without changing the opt-in default
- [ ] Preserve current local lifecycle and `bootstrap.strict` semantics, IdP prerequisites and RG's seal. No registry call waits on a later `init()` registrant, and no pending/accepted operation is treated as ready. Root registration failures remain diagnosable; deferred supervised readiness semantics belong to T41

**Stage 5 — complete IdP instance family:**
- [ ] Move `LazyIdpProvider` discovery and both static/Keycloak writers together to the new platform API; use the complete list helper and each plugin's own publisher context. Retain required-vs-optional IdP semantics and only expose a scoped backend after admission
- [ ] Preserve Keycloak connection/credential and backend behavior; exercise its registration and SDK/error mapping with existing test support. The local product handoff uses static IdP; do not claim an external Keycloak deployment was verified by a static test
- [ ] Prove IdP selection, absent/delayed/rejected registrations and idempotent restart/configuration updates through the new client; no second legacy discovery or `AlreadyExists` shortcut remains

**Stage 6 — existing REST e2e, restart and handoff:**
- [ ] Move AM e2e schema writers, especially `testing/e2e/suites/account_management/conftest.py::create_metadata_schema`, to the current database REST registration contract with idempotency/preconditions and operation polling. Use the existing shared operation helper where available. Allow the suite's `x` vendor only in its metadata-test namespace; do not open unrelated regions or weaken policy. T32 uses the current `/v2/` route; T34 updates these helper paths/credentials with the platform route move, in the same commit, and T38 updates any interim tenant reads
- [ ] Run existing AM REST scenarios through real gateway, databases, registry, authz and static IdP: tenant CRUD, unknown/incompatible tenant type refusal, metadata PUT/GET/inheritance/list UUID hydration and malformed-value rejection, plus user provision/deprovision. Assert a schema registered only through the database API is actually consumed, and the configured root/customer types are readable from the same database. Boot or registered type presence alone does not satisfy this gate
- [ ] Add real host/database restart and configuration-drift coverage: equal root/instances preserve revisions; allowed content updates converge; root-binding mismatch fails before a registry write and incompatible edits are refused. Include a custom root's explicitly allowed policy and its closed-policy refusal. Verify optional AM TR plugin discovery and affected host/config boots. Grep the seven migrated crates (production and tests) for old client/models/helpers and remove remaining references; update their usage docs to the new local API and boundaries

**Verification / review checkpoints:**
- [ ] After stages 1–2: recorded config/ID dependency audit; affected static/single-tenant boots; registry and TR-family tests; explicit database refusal cases and no cross-catalogue fallback
- [ ] After stages 3–5: `cargo test -p cf-gears-account-management -p cf-gears-account-management-sdk -p cf-gears-static-idp-plugin -p cf-gears-keycloak-idp-plugin -p cf-gears-tenant-resolver -p cf-gears-static-tr-plugin -p cf-gears-single-tenant-tr-plugin -p cf-gears-rg-tr-plugin`; scoped builds/lints and shared fixture tests pass. Local API/type projection/canonical-error and bootstrap policies are reviewed before the final application gate
- [ ] Final: `make e2e-local SUITE=account_management`, full `make e2e-local`, `make quickstart`, affected mini-chat/static-tenants/usage-collector/config boots, AM's existing SQLite/backend coverage, `cargo test --workspace`, `make fmt`, `make clippy`. Reproduce the schema-registration → AM HTTP consume path and host restart; no external Keycloak or known-broken RG-chain success is inferred from these commands

**Dependencies:** T31, T29/T30; no dependency on T33–T37. Sequence is T31 → T32 → T33. All existing Checkpoint 7A/7/8/9 gates remain **Files likely touched:** `AM/account-management/src/{gear,domain,infra/types_registry,infra/idp,tr_plugin}/` and their tests; both `AM/plugins/{static-idp-plugin,keycloak-idp-plugin}/` registration paths; tenant-resolver core and its three plugin crates/tests; affected manifests/configs; AM e2e helpers/scenarios and usage docs. Allocate concrete files to each small implementation commit using the stage audit; this is an explicitly large delivery, not a one-session task **Scope:** L umbrella, six ordered stages above split into S/M commits; the complete AM/IdP/TR group merges atomically. T38 verifies this API migration rather than repeating it; T41/T42 still own lifecycle/remote changes

---

### - [ ] T33: Toolkit — a platform-authenticated auth axis; one plane per route

**Status:** implemented on branch `toolkit-platform-route-auth` (`af5517c38`) as its own toolkit pull request. The criteria below are unchecked until it merges into `main`; the evidence recorded beside them is the branch's and is re-run at merge.

**Description:** `OperationBuilder` has tenant (`.authenticated()`) and anonymous axes only. toolkit's own codegen registers platform-plane routes `.anonymous()` *"until `OperationBuilder` grows a dedicated platform axis"* (`toolkit-contract-macros/src/rest_contract.rs:1579`). T34 needs that axis for the registry's platform routes (SPEC D17, D20).

**History.** P24 extended platform enforcement to every host; P25 removed the either-plane axis before use. The final API has distinct platform and tenant routes.

**Acceptance criteria:**
- [ ] `.platform_authenticated()` on `OperationBuilder`, recorded in `OperationSpec` as `auth_plane: AuthPlane { Tenant, Platform }` beside `authenticated`, which it sets to `true`. `.authenticated()` and `.anonymous()` keep `AuthPlane::Tenant`. No route accepts either plane (P25)
- [ ] `compose_oop_router` derives exact method/template `RouteAuthPolicy` from specs; a router layer outside both planes inserts RouteAuth/AnonymousRoute before auth. MethodRouter layers are too late. Undeclared HEAD inherits GET. The inner gate requires a validated non-marker platform context and validates all presented credentials, including a non-anonymous tenant context for a bearer. Missing authenticators and inconsistent specs fail closed
- [ ] Gateway resolves Platform policy under `prefix_path`, validates any bearer and requires its inbound internal authenticator through the shared gate in both auth modes. Exact templates, no synthetic tenant context, no tenant scope checks; identity-keyed throttling is refused at startup. OpenAPI uses `internalToken`; discovery excludes operations requiring it in every alternative. Gateway HEAD inherits GET unless explicitly declared, fixing anonymous fallback when default auth is disabled
- [ ] Handlers read `Extension<PlatformSecurityContext>`; behind the gate it is always present, and a refusal is a canonical `401`, never `500`
- [ ] Generated REST methods with owned/borrowed `PlatformSecurityContext` use `.platform_authenticated().no_license_required()` and reject `#[anonymous]` at compile time. `authz-resolver::evaluate` now requires a validated internal token on every host

**Verification:**
- [ ] Toolkit tests on both middleware stacks, including mixed credentials:
      - `toolkit-http-middleware` `auth_tests.rs`: platform-route matrix (neither, bearer alone, token alone, both, a forged bearer or token beside a valid one, a non-bearer scheme), each refusal's reason, `HEAD` via `GET`, each plane unconfigured, an anonymous tenant context beside a valid token, the outbound marker injected behind a context-free handler
      - `toolkit` `oop_serve_tests.rs`: the matrix through the real assembly — `OperationBuilder` specs → `route_auth_policy` → `layer_gear_router`, every plane combination; anonymous and authenticated routes keep their behaviour; an inconsistent spec fails closed
      - `toolkit-contract` `tests/rest_server_platform_plane.rs`: generated routes for owned and borrowed contexts declare `internalToken`; behind the real stack a validated token reaches the service, and a missing or forged one is refused before dispatch (`401`); turning the codegen back to `.anonymous()` fails all three. trybuild `fail/rest_anonymous_platform_method.rs`
      - `api-gateway` `tests/auth_middleware.rs`, embedded gateway with `require_auth_by_default: false`: tenant `GET`/`HEAD` on an internal token alone or nothing is a gateway `401` (pinned by its `WWW-Authenticate` challenge); platform `POST`, `GET` and inherited `HEAD` across valid/forged token × valid/forged/absent bearer; refused without a gateway internal authenticator; under `prefix_path: /cf`; with `auth_disabled: true`; against a matching tenant scope rule (skipped for platform, still `403` on the tenant route); a presented token is validated on tenant routes when the authenticator is configured and ignored when not. Profile 3: through `ToolKitGatewayProvider` the tenant route arrives bearer-required, `HEAD` included, and a platform path is not published
      - Unit: `GatewayRoutePolicy` (platform wins, exact template, `HEAD` inheritance with explicit-`HEAD` precedence), identity-keyed rate and in-flight zones refused on platform operations, OpenAPI security per axis, discovery's AND/OR/inherited `security` cases
      - Mutations removing the gate, the marker filter, the policy layer, the gateway gate or `HEAD` fallback, re-adding the synthetic tenant context, dropping the scope skip, or keying the policy on the prefixed path each fail tests
- [ ] `make fmt`, `make clippy`; `cf-gears-toolkit-http-middleware` (44), `cf-gears-toolkit` (651, all features), `cf-gears-toolkit-contract`, `cf-gears-toolkit-contract-macros-tests`, `cf-gears-api-gateway` + `cf-gears-toolkit-gateway` (307), `cf-gears-authz-resolver{,-sdk}` (98) green

**Follow-ups, not T33:**
- Method-aware proxy routing: `ProxyRegistry` routes by path, so any method on a published path is forwarded — including an `exposed = false` method sharing the path. Deny unpublished methods, with `GET` → `HEAD`.
- api-gateway's `extract_bearer_token` reads only the first `Authorization` header, so a second, unvalidated one is ignored on every authenticated route (nothing in the gateway consumes it, and an `oop_serve` listener refuses duplicates). Reject duplicate `Authorization` headers there and add parser regression tests.
- `oop_serve` installs the tenant plane only when a bearer authenticator is configured, so without one an `.authenticated()` route reaches its handler with no context — `500` from an extractor, or a context-free handler served. Pre-existing for every OoP gear; T34 must give the registry's tenant routes a canonical `401` there.

**Dependencies:** T29 **Files touched:** `libs/toolkit/src/api/{operation_builder,openapi_registry,mod}.rs`, `libs/toolkit/src/runtime/{oop_serve,host_runtime}.rs`, `libs/toolkit-http-middleware/src/{auth,lib}.rs`, `libs/toolkit-contract-macros/src/{rest_contract,rest_contract_parse}.rs`, `libs/toolkit-gateway/src/toolkit_provider.rs`, `gears/system/api-gateway/src/{gear,middleware/*}.rs`, tests and `OperationSpec` literals beside them, `docs/toolkit_unified_system/06_authn_authz_secure_orm.md`, `libs/toolkit-http-middleware/README.md` **Scope:** M

---

### REST contract — binding on T30, T34, T35

A normative reference, not a task: its items carry no checkboxes. Each names its owning task in brackets, and that task's acceptance criteria track it. T30 supplies the tenant local client, T34 the platform half, T35 the tenant half, and T35 closes the contract (P27). The semantic models stay transport-free. `rest-client` gates the wire DTOs and both resolving clients; `rest-server` supplies the descriptors `provides` requires. One client instance carries no request-specific publisher or tenant context. Local adapter registration is delivered in T31 and used by the full AM/IdP/TR group in T32; other existing consumers stay legacy until T38. Clients are hand-written on toolkit-contract's runtime helpers (`build_request_url`, `attach_internal_token`, `map_http_error`, `retry_with_backoff`) and wrapped in `DirectoryResolvingClient`. Do not use `#[rest_contract]` for them: its custom-header and success-metadata limitations are why this transport is hand-written.

**Client:**
- **[`rest-client` T34; `rest-server` T35]** `TR-SDK` gains `rest-client` (and, for `#[provides]`, `rest-server`) features. The client, its wire DTOs and the resolving wrapper compile only under `rest-client`
- **[T34; tenant DTOs T35]** Wire DTOs derive serde only, convert to and from the §10.1 models, and are not referenced by the models
- **[T34]** The platform client implements `PlatformTypesRegistryApi` over the `/types-registry/platform/v1/` wire exactly as the handlers serve it:
  - submission sends `Idempotency-Key` as a header;
  - it reads `Location` / `Retry-After` and tells a replay's `200` from `202`, then reads the operation through `get_operation` (SPEC D19);
  - it maps the exact read's `ETag` / `304`;
  - `batchGet` is `POST …/entities:batchGet`;
  - discovery sends `pattern`, `depth`, `kind`, `lifecycle_status`, `limit`, `cursor` and `$select`.
- **[T34]** Every platform method attaches the process's `X-ToolKit-Internal-Token` through `attach_internal_token`, resolved on every attempt, and never `Authorization`. The `PlatformSecurityContext` argument is never serialized
- **[local client T30; REST client T35]** `TypesRegistryApi` — exact read, `batchGet` and discovery, with the platform models and the same projection, validators and `304` / `unchanged` semantics — gets a local client over the domain service and a REST client that sends the caller's bearer from its `SecurityContext` and never the internal token. Its base path is `/types-registry/v2/` until T38, held as an adapter-internal constant
- **[T34; the tenant exact read T35]** Submission and the exact read do not use `runtime::client::send_unary`, which discards success headers and status and treats `304` as an error. They read status and headers themselves and reuse `runtime::http::map_http_error` for Problems
- **[T34, T35]** Per-method client spans and RED metrics match what a generated client emits. Retry is enabled only for safe reads and for submissions carrying a key, under bounded deadlines and cancellation
- **[T34, T35]** Problem responses map to `CanonicalError` without loss: category, `resource_type` / `resource_name`, field violations
- **[T35]** types-registry declares `#[provides(transports = [local, rest], rest_client = …)]` for both APIs — preserve T31/T32's seeded local path, prove remote resolution in fixture hosts, and expose each resolving client where `#[consumes(resolving_client = …)]` expects it. T38 cuts over only the remaining legacy consumers

**Server:**
- **[T34, once T33 is in `main`]** The seven platform routes move to `/types-registry/platform/v1/` with `.platform_authenticated()`; no `/v2/` platform route remains. On `oop_serve` and through api-gateway they serve a valid internal token and refuse a bearer alone; through the gateway the token is validated by its inbound authenticator, and with none configured the routes are refused
- **[T35]** The tenant routes — `GET /entities/{entity_key}`, `POST /entities:batchGet`, `GET /entities` on `/types-registry/v2/` — use `.authenticated()` and share their handlers' domain calls with the platform twins; they serve a valid bearer and refuse an internal token alone on both hosts. `get_operation` has no tenant route
- **[platform T34; tenant T35]** Every refusal is a canonical `401` Problem, never `500`: with no credential, and with an invalid credential beside a valid one wherever that stack validates it — the registry's listener validates both, api-gateway an internal token only when its authenticator is configured
- **[T35]** The standalone registry binary installs `AuthNResolverBearerAuthenticator` over its linked local `AuthNResolverClient`, following P30, and T35 records that production topology in SPEC §8.4. **Without this authenticator, a tenant route answers a canonical `401`** — not `500` from the handler's extractor and not a served request: `oop_serve` installs no tenant plane then, so the registry (or toolkit) adds a gate for it
- **[T34]** Mutation routes keep `exposed = false`; tenant routes may be exposed. C8's source comment states what `exposed = false` does not bound in Profile 1
- **[T34, T35]** Platform handlers read `Extension<PlatformSecurityContext>`, tenant handlers `Extension<SecurityContext>` — one plane per handler
- **[T34]** No principal is recorded: `P0_PRINCIPAL_ID` stays nil, and C2's source comment names T34 as where the identity became available but unrecorded
- **[T34]** SPEC C6/C8 source comments are updated to the D17 wording

**Verification shared by T34, T35:**
- **[T34]** `TR/tests/rest_client_contract_test.rs`: the real platform client against the real platform routes over TCP, with the platform middleware installed — submit → poll → exact read → `batchGet` (`found`, then `unchanged` under the same `$select`) → discovery; replay with the same key → `replayed`; a different request under the same key → `AlreadyExists`; `404` / `400` mapping
- **[T35]** The tenant client against the real tenant routes: exact read with `ETag`/`304`, `batchGet` `unchanged`, discovery cursor, the forwarded bearer re-validated by the listener
- **[platform rows T34; tenant rows T35; the full matrix on both hosts T35]** Auth matrix through the **real** api-gateway middleware and through `oop_serve`'s middleware stack:
  - no credential → `401` on both route sets;
  - platform: a valid token → served (at the gateway when its internal authenticator is configured, refused otherwise); a valid bearer alone → `401`; a forged token, or a forged bearer beside a valid token → `401`;
  - tenant: a valid bearer → served; a valid token alone → `401`; an invalid bearer → `401`; a forged token beside a valid bearer → rejected on `oop_serve`, and at the gateway when its internal authenticator is configured (otherwise the header is ignored).
- **[T35]** Bootstrap test: the registry started out of process **without** a tenant authenticator answers a canonical `401` on tenant routes and still serves platform routes; with one, it serves a valid bearer
- **[T34, T35]** `make e2e-local` green after each task. **Platform calls need a platform token:** `config/e2e-local.yaml` (and the launcher's base config) configures the gateway's `internal_auth: shared_secret`, the root `conftest.py` gains a `platform_headers` fixture beside `auth_headers` reading the secret from the environment, and the `types_registry` suite's v2 calls split by plane — submissions, deletions and operation polling move to `/types-registry/platform/v1/` with `platform_headers`; the entity reads stay on `/types-registry/v2/` with their bearer. T32's AM schema helper is already database-backed and moves with T34's platform paths/credentials. Only still-legacy `oagw/helpers.py` and `types_registry/legacy/` wait for the atomic T38 cutover
- **[platform examples T34; tenant flows and the final pass T35]** `QUICKSTART.md`: the platform examples use `/types-registry/platform/v1/` and send `X-ToolKit-Internal-Token`, with the `internal_auth` config they need; the tenant read examples use a bearer
- **[T34]** The spike test and the spike `Cargo.toml` changes are removed or folded into the contract test
- **[T34, T35; closed by T35]** `make dylint` clean for the SDK crate

---

### - [ ] T34: Platform API over REST

**Description:** Implement the remote platform API and atomically move its seven routes, credentials, fixtures and e2e callers to the final platform-authenticated paths (`TR/src/api/rest/routes.rs:45`). Keep the three authenticated entity reads on `/types-registry/v2/` until T35 supplies their tenant client.

Commits inside the task: (1) private wire DTOs and codecs; (2) the platform client's reads; (3) its submission, read-back and deletion; (4) the route move with its e2e callers and QUICKSTART examples, as one commit.

**Acceptance criteria:**
- [ ] Codecs round-trip projected snapshots, validators and operations without serde or HTTP on the semantic models; a received entity-tag becomes `Validator` without its RFC 9110 quotes and gets them back when sent, so a validator is the same token from either transport (SPEC §8.5); unknown failure reasons and context are preserved; omission and `null` stay distinct; a malformed operation payload is refused, not defaulted
- [ ] Every `PlatformTypesRegistryApi` method works over real TCP and meets the platform rows of the REST contract — `Idempotency-Key`, `Location`, `Retry-After`, `200`/`202`, read-back through `get_operation`, `ETag`/`304`, per-key `unchanged`, projection, bounded pagination and cursor/filter binding — with a freshly resolved internal token on every attempt and never a bearer
- [ ] Submit → poll → read returns real item outcomes, never a synthesized receipt; a lost read-back names the accepted operation so the caller can replay the same key (D19); only keyed submissions and safe reads retry
- [ ] The platform route set serves only `/types-registry/platform/v1/` with `.platform_authenticated()`; no `/v2/` mutation or `get_operation` route remains; the three `/v2/` reads keep their bearer. C2, C6 and C8 source comments match the REST contract
- [ ] The async-surface e2e callers and configuration move in the same commit as the routes (REST contract, e2e item), including T32's AM metadata/schema-registration helper: switch its writes and operation polling to platform paths/internal credentials, never a tenant bearer. Keep AM's real application suite green; only still-legacy v1 writers are untouched
- [ ] Platform routes pass `CallerContext::Platform` from the validated internal token, and `RegistryService::{submit, delete, operation}` narrow to it: a `CallerContext::Tenant` reaching them is refused (D17), so a tenant bearer cannot mutate or read an operation by any path

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry-sdk --features rest-client`; `cargo check -p cf-gears-types-registry-sdk --all-targets --all-features`
- [ ] `cargo nextest run -p cf-gears-types-registry --test rest_client_contract_test` (new target) — the platform cases of the shared verification
- [ ] `cargo nextest run -p cf-gears-types-registry --test platform_local_client_test` — local and TCP clients agree on outcomes, conditional reads, replay and errors
- [ ] The platform rows of the auth matrix through the real api-gateway and `oop_serve` stacks; `api_rest_test.rs` updated in the route commit
- [ ] `make e2e-local` green; gear tests

**Dependencies:** T30, T33. Commits 1–3 (DTOs and the client) need only T29 and the `attach_internal_token` helper already in `main`; commit 4 (the route move to `.platform_authenticated()`) and the TCP contract test need T33 merged into `main` **Files likely touched:** `TR-SDK/Cargo.toml`, `TR-SDK/src/{lib,rest_client/mod,rest_client/dto,rest_client/platform}.rs` and their tests, `TR/src/api/rest/{paths,routes,handlers}.rs`, `TR/tests/{rest_client_contract_test,api_rest_test}.rs`, `config/e2e-local.yaml`, launcher base config, `testing/e2e/conftest.py`, `testing/e2e/suites/types_registry/{helpers,test_*}.py` (async surface), T32's AM e2e schema helper, `QUICKSTART.md` **Scope:** L — four commits as listed; the route move and its e2e callers are one commit

---

### - [ ] T35: Tenant REST and both resolving clients — complete SDK transport handoff

**Description:** Serve T30's `TypesRegistryApi` to a gear in another process through a bearer-forwarding REST client on the `/v2/` reads, and give the standalone registry a tenant authenticator and a canonical-`401` gate. The contract, its extension helpers and its local client moved to T30; resolving wrappers, `#[provides]` and the contract's closure moved to T35 (P27).

Commits inside the task: (1) tenant routes on shared domain calls and the tenant REST client; (2) the authenticator bootstrap/gate and SPEC §8.4.

**Acceptance criteria:**
- [ ] Real TCP tenant reads match the T30 local client: `ETag`/`304`, `unchanged` and cursor semantics, the validator as the unquoted token (SPEC §8.5); each call forwards its own `SecurityContext` bearer and never the internal token; tenant handlers read `Extension<SecurityContext>` only
- [ ] The tenant routes share their handlers' domain calls with the platform twins; `get_operation` has no tenant route
- [ ] The standalone registry installs `AuthNResolverBearerAuthenticator` over a linked production authn-resolver/plugin and records that topology in SPEC §8.4 (P30). `AuthNResolverClient` has no current remote contract/adapter; do not add an unrelated RPC migration. Without an authenticator a tenant route answers a canonical `401` before dispatch while platform routes are served
- [ ] Provide the production registry host entry in a separate composition crate (`apps/cf-types-registry-host`) using existing OoP bootstrap/CLI, its own database and linked production AuthN. Do not add authn-resolver as a dependency of the registry gear crate: authn already depends on it, which would create a Cargo cycle. Use the standalone listener without pulling AM/RG/TR declarations through the full example/gateway composition. This deployable registry entry is reused by T41/T43
- [ ] Every REST-contract item owned by T35 is met; the interim tenant path is adapter-internal, so callers see the semantic API only

**Verification:**
- [ ] The tenant cases of `rest_client_contract_test`
- [ ] The tenant rows of the auth matrix and the bootstrap test of the shared verification, on both hosts
- [ ] `cargo test -p cf-gears-types-registry-sdk --features rest-client`; `cargo check -p cf-gears-types-registry-sdk --all-targets --all-features`
- [ ] `make e2e-local` green; gear tests

**Closing stage — resolving clients, provides and both-host auth matrix:**

**Description:** Expose `DirectoryResolvingClient` wrappers and `#[provides]` descriptors for both APIs, run the full auth matrix on both hosts, and close the REST contract with its documentation. After this task another developer can consume either API in either transport. Formerly tracked separately by P27; P30 now makes this the closing stage of the same T35 feature.

Commits inside the task: (1) resolving wrappers, `rest-server` and `provides` with their fixture hosts; (2) the full auth matrix on both hosts; (3) QUICKSTART tenant flows and the contract's closing pass.

**Acceptance criteria:**
- [ ] Both resolving wrappers sit where `#[consumes(resolving_client = …)]` expects them; local wins in a local host, a remote host has no local fallback, an unavailable target is a canonical error, and resolution rebuilds after a target moves. Preserve T31's seed barrier and local registrations; existing legacy consumers still use their catalogue until T38
- [ ] types-registry declares `#[provides(transports = [local, rest], rest_client = …)]` for both APIs; local and remote resolution are proven in fixture hosts
- [ ] The full auth matrix of the shared verification passes for both route sets through the real api-gateway and `oop_serve` stacks
- [ ] Every item of the REST contract is met for both APIs

**Verification:**
- [ ] A `consumes`/`provides` fixture compiles for each API with `local` and `rest` features; focused resolving tests move a directory target
- [ ] The full auth matrix of the shared verification, on both hosts
- [ ] `cargo test -p cf-gears-types-registry-sdk --features rest-client`; `cargo check -p cf-gears-types-registry-sdk --all-targets --all-features`; `make dylint` for the SDK crate
- [ ] `make e2e-local` green; both authenticated QUICKSTART flows run as written

**Dependencies:** T30, T34; the platform-authenticated prerequisite is supplied by T33/T34 **Files likely touched:** TR-SDK tenant/resolving REST clients, rest-server descriptors, registry providers, production registry host entry/manifests, real-host contract/auth tests and QUICKSTART **Scope:** L feature with tenant/authenticator and resolving/closure stages, each split into S/M commits; one transport handoff

---

### - [ ] T35a: SDK payload validation over a resolved Type Schema

**Description:** Add `TypeSchema::validate_value(&serde_json::Value)` as a thin adapter to `gts::ResolvedType::validate_payload`, so consumers can use the common GTS validation rules without building a store or configuring their own `jsonschema` validator. The upstream helper is implemented locally in `gts-rust`, branch `feat/resolved-payload-validation`, commit `0b5b7b1`; merge and a compatible published release are prerequisites, not completed dependency adoption. Any GTS/spec migration required by that release must be planned and verified separately before this task, rather than hidden in the wrapper change. One implementation commit after the prerequisite is satisfied; consumer migration stays in T38.

**QE consumer note (T38):** QE already wraps `gts::PayloadValidator` in `CompiledContract` and compiles each selected contract once at catalogue bootstrap. Review the SDK wrapper against this use case: a convenience `validate_value` method that recompiles on every call is insufficient for replacing QE's request-path validator. Preserve a reusable compiled-validator path over the materialized Type Schema; keep the thin QE wrapper if the SDK offers only one-shot validation. The main migration saving is in registry resolution, as recorded under T38, rather than in `domain/catalog/model.rs`.

**Acceptance criteria:**
- [ ] The public method delegates payload validation to the upstream helper, using the returned resolved schema and the selected leaf's authored metadata for abstractness. It neither reconstructs effective artifacts nor reimplements dialect, format, regex or reference rules; it creates no registry client or local store
- [ ] Rustdoc and the SDK README state the required projection fields (including `content` and `resolved_schema`) and define the error mapping for missing fields and upstream failures without parsing diagnostic strings. Missing fields or unavailable schema resources fail explicitly; validation never silently succeeds. The wrapper does not modify the payload or insert defaults
- [ ] The contract explicitly checks reference syntax and pattern conformance only (`GtsRefValidation::None` semantics); target presence/validity, lifecycle and availability remain separate registry checks. Type-schema admission and trait/derivation validation remain upstream responsibilities; gear-specific payload envelopes remain with their consumers

**Verification:**
- [ ] SDK tests compare valid and invalid payload outcomes with the upstream helper for Draft-07, 2019-09 and 2020-12, including format assertions, GTS references, pattern references, inherited `/$id` and concrete descendants of abstract bases
- [ ] SDK tests reject abstract types, missing projection fields and unresolved external schema resources; defaults do not satisfy a missing required payload property, and inputs remain unchanged
- [ ] `cargo test -p cf-gears-types-registry-sdk --all-features`; `cargo check -p cf-gears-types-registry-sdk --all-targets --all-features`; the standing `make fmt`, `make clippy` and gear-test gates pass
- [ ] Manual: the README example reads a Type Schema with the documented projection and validates a payload without retaining a `GtsStore` or making another registry call

**Dependencies:** T29 and T30; a compatible published `gts-rust` release containing `ResolvedType::validate_payload`. T35 is the queue predecessor, not a transport dependency. **Files likely touched:** `TR-SDK/src/models.rs`, `TR-SDK/src/models_tests.rs`, `TR-SDK/README.md`; `Cargo.toml` and `Cargo.lock` only for compatible released-dependency adoption. **Scope:** M (up to five files; broader dependency/spec migration is a separate prerequisite)

---

### - [ ] T35b: Mock Type Schema admission uses GTS validation

**Description:** Make `MockTypesRegistry` validate Type Schema creations and content revisions before reporting success or storing them. Reuse `gts-rust`'s `GtsStore::validate_schema` for schema, derivation and trait checks; an invalid registration must fail at admission, rather than succeed and later fail a document read with `Internal`. Keep explicit `reject()` admission refusals, `Fault` injection for call failures such as `ServiceUnavailable`, and `seed()` as the fixture bypass. This task covers Type Schema content validation, not a second admission engine: publisher guards, database locking, schema-evolution history and complete production-admission parity remain outside its scope.

**Ordered stages — each clears the standing bar:**
- [ ] **1. Prepare fixtures (M):** replace simplified Type Schema fixtures registered through the contract with valid GTS Type Schemas in `TR-SDK/src/{ext_tests,testing_platform_tests}.rs` and `TR-SDK/src/reconcile/{mod_tests,telemetry_tests}.rs`. Update expected documents with their fixtures; preserve each test's purpose and assertions. The existing mock still passes the complete SDK suite after this stage
- [ ] **2. Validate admission (M):** add validation in `TR-SDK/src/testing_platform.rs`, regression tests in `testing_platform_tests.rs`, shared real-client/mock cases in `testing_platform/conformance.rs`, and the contract limits in `TR-SDK/README.md`. Do not disable validation or silently repair registration content to keep old fixtures passing

**Acceptance criteria:**
- [ ] Creation and content revision validate the candidate and its required schema context before changing stored content or resource versions. Invalid schemas, derivation or traits return a terminal item failure with `InvalidSchema`; rejected revisions preserve the previous entity and its readable artifacts. Existing lifecycle, precondition and unchanged-result semantics remain intact
- [ ] Valid in-batch dependencies work regardless of input order, including child before parent. An invalid or otherwise refused candidate cannot supply a committed dependency to another item; independent valid branches can still succeed. Dry run uses the same validation and outcomes without changing stored entities or versions
- [ ] Explicit `reject()` and `Fault` rules remain deterministic and preserve their documented precedence; `seed()` can still create intentionally invalid fixtures. Module docs and README state the validation scope and limits, without claiming full production-admission parity or duplicating GTS rules locally

**Verification:**
- [ ] Shared conformance cases pass against both the mock and the real local client: valid roots/derived types; malformed schema, invalid traits and invalid derivation refused at admission; failed update leaves prior content/version/artifacts unchanged; unordered and mixed-validity batches; dry-run refusal without writes
- [ ] SDK regression tests preserve injected admission refusals, unavailable-call faults, idempotent replay and the explicit seed bypass; removing candidate validation makes the new admission tests fail
- [ ] `cargo test -p cf-gears-types-registry-sdk --all-features`; `cargo nextest run -p cf-gears-types-registry`; `make test-integration LANE=types-registry-db`; `cargo check -p cf-gears-types-registry-sdk --all-targets --all-features`; the standing fmt/clippy gates pass

**Dependencies:** T29 and T30 (mock and both contracts). Uses the existing GTS schema-validation API; does not require T35a's new helper or its upstream release. T35a is the queue predecessor only. **Files likely touched:** listed separately for each stage above. **Scope:** L umbrella over the two ordered M stages above, at most four files per stage; fixture preparation precedes enabling validation so every commit stays green

---

### - [ ] T36: SDK client cache — freshness window, byte bound, `fresh` bypass

**Description:** Port the cache to the new `Entity` models as an SDK decorator over any `dyn PlatformTypesRegistryApi`, covering local and REST clients before T38's cutover (SPEC §8.3, DESIGN §3.3, P7/P22). T22d validators enable revalidation; T22b supplies projection isolation. Visibility and Context-Tenant dimensions stay fixed until P1 tenancy.

**Acceptance criteria:**
- [ ] Cache is typed on `Entity`; the `GtsTypeSchema` / `GtsInstance` implementation keeps serving the old trait until T38 deletes both
- [ ] The cache lives in the SDK as a `PlatformTypesRegistryApi` decorator. Wrap the embedded host's new platform local API delivered in T31 here; the legacy cache remains until T38. T41's real AM local/resolving clients use this decorator. Where a remote consumer's cache settings come from is decided and documented without adding a per-consumer key to the registry's config (SPEC §10.3)
- [ ] Bound is **bytes** (`store_bound`, default 64MB), LRU-evicted — not an entry count. §3.2 caps one resolved document at 1MB, so the old `capacity: 1024` permitted ~1GB
- [ ] Freshness window (`freshness_window`, default 30s per DESIGN); `0s` is meaningful and disables the window rather than being rejected
- [ ] `fresh` on a read bypasses the window for that call and revalidates unconditionally against the entry's validator
- [ ] **Expiry revalidates, it does not drop:** expired keys and their validators go out in **one** batched conditional `batchGet` — DESIGN's batch poll scheduling — and an `unchanged` result refreshes the confirmation instant while keeping the snapshot. Demand-driven, no timer
- [ ] A failed revalidation **propagates the error and never extends the window** (`principle-fail-closed`); an entry is never served while its revalidation is in flight past the window
- [ ] A terminal successful registration or deletion outcome invalidates every local entry for each returned identifier/UUID pair, under **both** key forms. A `202` acceptance invalidates nothing — the client has not observed the mutation yet
- [ ] **Late-fill guard (historical decision P23):** each key carries a generation advanced by an observed terminal outcome or by a `fresh` read returning a new validator; a read started before the advance never fills the entry when it completes, for any projection or key form
- [ ] The byte bound counts canonical payload bytes, once per snapshot whichever key form indexes it; no synchronous lock is held across a network await
- [ ] A metadata-only publisher confirmation (SPEC D18) changes no cached representation; the publisher stamp is not cached
- [ ] Entries are indexed by identifier and by UUID, so either resolution direction hits one snapshot
- [ ] Never cached, each asserted separately: `NotFound`, a failed read, a discovery page or its items, an operation resource
- [ ] The key carries T22b's normalized selected-field set, with visibility context and Context Tenant as fixed P0 markers. A narrow cached representation is never returned for a wider selection; P1 adds real tenant dimensions without reshaping the key
- [ ] `freshness_window` / `store_bound` configure the new cache; the four old keys become accepted-and-ignored in T38, when the old cache goes. A `ponytail:`-style comment records what is left of ceiling C7 (the key dimensions, not revalidation)
- [ ] The tenant API gets no cache in P0: this task specifies none and introduces no tenant-cache semantics

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry-sdk --features rest-client` (runs SDK cache tests), plus `cargo test -p cf-gears-types-registry` for adapter parity
- [ ] Test: read inside the window after a direct database change returns the cached value; the same read with `fresh` returns the new one
- [ ] Test: `0s` window never serves a cached entry
- [ ] Test: terminal outcome invalidates under identifier **and** UUID; a bare `202` invalidates nothing
- [ ] Test: byte bound evicts on one 1MB document where a thousand small entries do not
- [ ] Test: each of the four never-cached cases
- [ ] Test: the same entity under two projections occupies distinct entries, while a reordered, case-varied or explicit-default selection reuses its normalized entry
- [ ] Test: a failed read leaves no entry and does not extend an existing window
- [ ] Test: an expired entry whose content did not change is revalidated to `unchanged` and **kept**, not refetched — assert on the absence of a full-snapshot response, not only on the returned value
- [ ] Test: two expired keys produce **one** conditional `batchGet`, not two
- [ ] Test: a failed revalidation surfaces the error and leaves the window unextended
- [ ] Test: `read(old)` starts, a terminal outcome invalidates, `read(old)` completes — the entry stays empty and the next read fetches the new snapshot
- [ ] Gear test through the real local client and the REST client: register → poll → read shows no stale entry after the terminal outcome

**Dependencies:** T35, T29, T31 (embedded local API), T22d **Files likely touched:** `TR-SDK/src/cache/` (moved from `TR/src/infra/cache/`), `TR/src/domain/local_client.rs`, `TR/src/gear.rs`, `TR/src/config.rs`, `TR-SDK/tests/client_cache_test.rs` **Scope:** M

---

### - [ ] T37: Toolkit post-wiring lifecycle — hook, supervision and `Required` readiness

**Description:** Give all gears a post-wiring hook and `/readyz` contribution, including gears without REST. Existing `post_init` requires `SystemCapability`, `start` requires `RunnableCapability`, and readiness is exposed only through `RestApiCapability::healthcheck` (`libs/toolkit/src/contracts.rs:30, :172, :64`). Add generic supervision to toolkit and connect SDK publication status to readiness (D16/D21). Write `publish_gts` and its status/supervised task over reconciliation; T28/T29's unused versions are archived in `c5f491cc8`. Production gears consume this in T41 with T39's collectors.

Commits inside the task: (1) the hook; (2) generic supervision moved from the SDK; (3) readiness contributions and the publisher entry.

**Acceptance criteria — hook:**
- [ ] `Gear::post_wiring(&GearCtx)` with a no-op default, invoked once for every gear right after proxy wiring in `run_init_wiring_post_init` (`host_runtime.rs:631`), so both the embedded and OoP paths run it, before `post_init` and `start`
- [ ] The hook is for local setup and spawning only. Its doc comment forbids awaiting a remote call in it; existing gears with the default hook keep their lifecycle behaviour
- [ ] `docs/toolkit_unified_system/08_lifecycle_stateful_tasks.md` names the hook in both lifecycle paths

**Acceptance criteria — supervision and readiness:**
- [ ] **Toolkit knows no publication semantics (T28, revised):** `toolkit` gains a generic `ReadinessStatus` trait — `is_ready()` and `is_terminal()` — and the supervised-task helper with its tests (T28's SDK `supervised.rs` in `c5f491cc8` is the starting point; its `SupervisedStatus` folds into or extends the trait); the SDK adds `publish_gts` and `PublicationStatus` and implements the trait for it (`is_ready`: every identifier admitted). Toolkit never names `PublicationStatus`, `PublisherContext` or `PublisherVersion`; `toolkit-gts` stays free of publication types
- [ ] Construction, poll and drop panics and early exit settle the status; child cancellation, explicit join, detach-on-handle-drop and terminal reporting keep T28's semantics. A task spawned from the hook uses this helper, not only a `CancellationToken`, and is joined on shutdown and on failed startup
- [ ] A gear without `RestApiCapability` can contribute a readiness check that `/readyz` aggregates; a gear with it composes the same contribution into its `healthcheck()`. A contribution is registered before the first readiness probe can run and is cancelled on shutdown with the rest of the healthcheck registry
- [ ] Readiness from a publisher's status is **`Required` only** in P0: toolkit holds the gear's readiness until `is_ready()`, then latches it. What counts as ready stays in the SDK: pending and rejected are not; admitted is. The superseded states arrive with T45. `ReportOnly`, a runtime override and optional consumption are P1 (SPEC D21)
- [ ] **Publisher contract:** the publisher receives the caller's name and version as text, the declarations as `(String, serde_json::Value)`, the `ClientHub` and a cancellation token, and returns `Supervised<S>` for any `S: ReadinessStatus`; it is reached through a generic `toolkit` helper, so no fn-pointer type names a status. `publish_gts` parses the version into `PublisherVersion`; an invalid version settles every identifier as rejected and names the gear

**Verification:**
- [ ] Toolkit tests: the hook runs once per gear after wiring, in both lifecycle paths; the default hook changes nothing
- [ ] Toolkit tests with a test `ReadinessStatus`, for a REST and a non-REST gear: not ready holds `/readyz` at `503`; ready releases it and stays released after the status regresses; a contribution exists before the first probe
- [ ] Toolkit test: a panicking spawned task reports a terminal status and is joined on shutdown and on failed startup
- [ ] SDK tests for the publication matrix: pending and rejected are not ready; admitted is. Caller-version parsing tests
- [ ] `cargo test -p cf-gears-toolkit -p cf-gears-types-registry-sdk --all-features`; both crates build together, catching re-export or type-identity drift; affected runtime targets build

**Dependencies:** T28, T29 (complete). T36 is only the queue predecessor, not a toolkit-hook dependency; T41 requires both for the real remote proof **Files likely touched:** `libs/toolkit/src/{lib,contracts,supervised}.rs`, `libs/toolkit/src/runtime/host_runtime.rs`, `libs/toolkit/src/healthcheck/`, `TR-SDK/src/{publish,lib}.rs` (new `publish.rs`), focused tests, `docs/toolkit_unified_system/08_lifecycle_stateful_tasks.md` **Scope:** L — three commits as listed

---

### Checkpoint 7A — local application and SDK contract handoff
- [ ] T31's database seed/local ClientHub path and T32's complete AM/IdP/TR group work through existing real AM REST scenarios
- [ ] Both SDK APIs pass T34/T35's real TCP contract, resolving-client and auth checks; T36's local/REST cache contract and T37's generic hook/supervision/readiness tests pass
- [ ] T35a's public payload-validation wrapper matches the released GTS helper, rejects incomplete projections and documents the reference checks that require the registry
- [ ] T35b's mock Type Schema admission passes shared real-client conformance, including invalid candidates, unordered dependencies and dry runs; consumer fixtures remain valid through T38
- [ ] The handoff documents local AM and verified SDK/TCP usage; it makes no complete cross-process startup/recovery guarantee yet. That application-level proof belongs to T41/Checkpoint 8A
- [ ] `make ci`, gear tests on three backends, SDK feature builds and `make e2e-local` green; human review. The local handoff and bounded legacy/new coexistence remain as P28/P29 specify; final deployment still waits for Checkpoint 9

---

### - [ ] T38: Remaining-fleet cutover and one REST version — delete legacy, keep e2e green

**Description:** Audit the current `init` → wiring → `post_init` barrier (`host_runtime.rs:612`) before migrating the remaining consumers and deleting the legacy store. AM bootstrap assumes a complete catalogue (`account-management/src/gear.rs:678`); oagw resolves the root tenant in `post_init` (`oagw/src/gear.rs:216`) through a configured plugin instance (`static-tr-plugin/src/gear.rs:59`).

**The audit — the first commit.** Lifecycle requirements drive the cutover and Phase 8 rather than follow them (historical decision P23). It reads the existing fleet's startup.

**The cutover.** Preserve and reverify T31's outbox → database seeding → local client-publication barrier; do not implement a second seeding path. T39 preserves global collection during migration; T42 narrows the seed set. Delete `switch_to_ready`, the `temporary`/`persistent` split, `SystemCapability::post_init` and the in-memory repository; reads use the database (D2, §8.2). Keep T36's new SDK cache and delete only the old model-typed cache (P7, §8.3).

Switch every remaining legacy consumer to `PlatformTypesRegistryApi` atomically, deleting the old trait without a shim. Mechanical changes: add `#[consumes]` beside `deps`, pass `PlatformSecurityContext::outbound_marker()`, use extension helpers, read materialized fields and replace `register` with local reconciliation. Registration remains synchronous in `init` until Phase 8. Validation changes from staging until `post_init` to immediate admission, which requires the startup audit.

**QE migration note — consume resolved artifacts:** In `gears/system/quota-enforcement/quota-enforcement/src/infra/types_registry.rs`, `reference_closure`, `enqueue_chain` and `resolve` currently load parent/transitive `$ref` dependencies into a local `GtsStore` and recompute the resolved schema and traits. Replace this roughly 80-line block with reads of the new SDK's materialized `resolved_schema` and `effective_traits`, selecting `content` for the leaf's authored metadata, including abstractness. This is an expected simplification, not a net line-count target: projections, error handling and registration migration still need code. Keep QE's metric/scope membership, contract uniqueness and active-Quota compatibility checks, bounded registry calls, and process-local catalogue; the new SDK does not replace those responsibilities. T29/T30 supply the materialized reads; T35a supplies the payload-validation API review above.

**Operator-configured entities (`cfg.entities`).** Preserve `gears.types-registry.config.entities` for deployment-specific identifiers (e.g. the customer tenant type in `e2e-local.yaml`). T31 already seeds it through the outbox/database path; remove only the legacy copy here. Invalid or oversized combined sets still fail boot.

**Acceptance criteria — audit:**
- [ ] Every registry-dependent step in `init`, `post_init`, `start`, bootstrap sagas and first plugin selection across the workspace is listed by grep, with its gear, phase and prerequisite, and recorded in this file in the first commit
- [ ] Every registration whose document depends on a declaration another gear registers later in startup is listed — the cases immediate validation would refuse
- [ ] The list gives Phase 8 its migration order: a consumer that waits for a publication in its own startup moves before that publication leaves `init`, or the two move in one change. It marks Account Management's host prerequisites (T41) separately
- [ ] The toolkit documentation no longer recommends registry calls from `init` for remote-capable consumers

**Acceptance criteria — cutover:**
- [ ] QE consumes materialized artifacts without a local `GtsStore`, transitive reference fetches or effective-artifact recomputation; discovery still follows no references, so an unrelated broken contract cannot fail its bootstrap. Missing required projection fields fail explicitly
- [ ] QE compiles selected payload validators once at catalogue bootstrap and reuses them without registry calls or schema compilation on ingress. Adopt T35a's wrapper only if it preserves this property; retain `CompiledContract` otherwise
- [ ] T31's seeding/local-publication guarantees and tests still pass after legacy deletion: all linked inventory + `cfg.entities`, idempotent restart, no publisher stamp, worker before admission, all seeds settled before publication and explicit combined-set limit refusal. No per-gear filter or dependency-separating split is introduced; T42 later narrows the seed set
- [ ] The legacy v1 routes T9a restored are deleted **together with** the repository they read — `POST /v1/entities` (`types_registry.register`), `GET /v1/entities/{gts_id}` (`types_registry.get`) and the in-memory `GET /v1/entities` list. A route left pointing at a deleted repository is the failure mode; T38 then promotes v2 onto those paths
- [ ] **Every remaining legacy consumer moves onto the new SDK in this task**, mechanically; T31's new consumers and all seven T32 gears already use it. Record/recheck T32's API migration and ID/config audit rather than translating those crates again; keep their outstanding lifecycle calls on the Phase 8 audit. The remaining assignment is derived by grep and recorded
- [ ] Each consumer declares `#[consumes(contract = PlatformTypesRegistryApi, from = "types-registry", …)]` **and keeps `deps = [types_registry]`**: the init order is built from `deps` (`libs/toolkit/src/registry.rs:560`), and `#[consumes]` wiring runs only after every `init`, so a consumer that still calls the registry in `init` needs the registry initialized first and resolves the local client from the `ClientHub` directly. Phase 8 drops each gear's `deps` together with its last startup call
- [ ] Remaining legacy read sites move mechanically using T29's typed extension helpers, context arguments and materialized fields. Plural reads gain the `batch_` prefix and handle `Result<HashMap<_, Option<_>>, CanonicalError>`: absence is `None`, a failed batch is an error, never silently dropped (`settings-service/src/domain/declaration/service.rs`). T32 already migrated AM's checker and other readers; recheck their evidence without rewriting them. Typed IDs and explicit projections apply everywhere; no consumer reconstructs effective artifacts locally (D3)
- [ ] Every remaining legacy `register(...)` site becomes bounded synchronous `reconcile_entities_and_await` through the local client: read/compare, CAS for changed content, submit and wait. A bare creation is insufficient on restart/configuration drift; permitted settings-service type changes update under normal admission rules. Recheck T32's root/IdP/TR reconciliation rather than implementing it here. Handle every outcome and each gear's own publisher; no `Pending` is success and nothing sends publisher metadata before T44. `rate-provider-sdk`'s shared registration helper moves with its callers
- [ ] Where a materialized `effective_*` field differs from what the deleted client-side method returned, the **materialized value is accepted** — the difference is the old approximation being wrong (unresolved non-parent `$ref`, trait-default order), and `gts-rust` is authoritative. A failing assertion is updated to the new value, never "fixed" back
- [ ] `TypesRegistryClient`, its models (`RegisterResult`, `RegisterSummary`, `TypeSchemaQuery`, `InstanceQuery`, `GtsTypeSchema`, `GtsInstance`) and `testing::MockTypesRegistryClient` are deleted — the whole `TR-SDK/src/legacy/` directory and the legacy block in `TR-SDK/src/lib.rs`; `types-registry-sdk` exports only the new surface. `GtsTypeId` / `GtsInstanceId` stay as root re-exports of `gts`. `precondition.rs` and `TypesRegistryError::ParentNotRegistered` are checked: if only the old `register` pre-check emits them, they go too, and crate docs and comments stop naming the old trait. Delete the SDK README's `Legacy API` section introduced in T31
- [ ] With the legacy `testing` module gone, `TR-SDK/src/testing_platform.rs` (with its `testing_platform/conformance.rs` module directory) becomes `testing.rs` / `testing/conformance.rs`, so `MockTypesRegistry` (renamed from `FakePlatformRegistry`; the name follows the trait it implements, as `MockAuthZResolver` does for `AuthZResolverApi`) moves to `types_registry_sdk::testing::MockTypesRegistry`. Consumers of the deleted `testing::MockTypesRegistryClient` switch to it. It serves both contracts: seeds (`seed`, `Seed`, `seed_inventory`), Type Schema documents resolved by `gts-rust`, `Fault` rules, `reject` / `depends_on`, `strict()`, `calls` / `reads` / `registered`, and `install(&ClientHub)`. Remaining hand-written old-trait stubs move onto it (the original population was ~17 across AM, products, pricing, credstore, licensing, RG and mini-chat). T32 already migrated its group's stubs; recheck/update those imports rather than repeating that migration. `conformance::run` stays green against the fake and `LocalClient` (`types-registry/tests/platform_local_client_test.rs::the_local_client_conforms_to_the_contract`)
- [ ] The legacy `GtsTypeSchema` / `GtsInstance` give way to the SDK's `TypeSchema` / `Instance` at every site. Field mapping, applied at every site: `raw_schema` / `object` → `content`; `type_id`, `type_uuid`, `id`, `uuid` keep their names; `segments` → the identifier's segments; `traits` / `traits_schema` → `effective_traits` / `effective_traits_schema`, selected in the projection; `title` / `description` → read from `content`; `parent` and `type_schema: Arc<_>` → a `get_type_schema(type_id)` read, which can fail and is handled at the site. Each site's projection selects the documents it reads
- [ ] Every registration the audit lists as depending on a later registrant is resolved here — reordered, folded into the linked inventory, or moved to the registrant that owns the dependency
- [ ] Ready mode and the in-memory repository are gone; `ready_mode_tests.rs` deleted. The four `local_client.cache.{type_schemas,instances}.{capacity,ttl}` keys become accepted-and-ignored with a warning naming their T36 replacements, and the production host wraps its clients in the T36 cache
- [ ] `owning_gear = "types-registry"` remains a compatibility placeholder until T44 renames the column to `publisher_name` and the first publication carrying a publisher claims the row (T44); binding it to the authenticated workload stays P1 (C3). Its source comment describes incomplete attribution and that path. Keep the column and its NOT NULL constraint; no read returns the placeholder
- [ ] No entity-derived state survives `init()` — no `ArcSwap`, no entity map, no `GtsOps` field on the gear or the service. Grep-checkable; the ceilings C1/C4 struck by D2 depend on it

**Verification:**
- [ ] The audit list is reviewed against the `rg` output recorded in its commit
- [ ] QE: `cargo test -p cf-gears-quota-enforcement --lib domain::catalog::`, `cargo test -p cf-gears-quota-enforcement --lib infra::types_registry::` and `cargo check -p cf-gears-quota-enforcement --all-targets`; adapter tests cover materialized local/mixin references and missing projection fields, and a request-path test proves validation makes no registry calls or repeated schema compilations
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Re-run T31's seeding and real-host local-client boot suites after removing legacy, including restart/update, two-crate dependencies, configured entities and the over-limit/failed-seed barriers
- [ ] Boot T35's production registry host again after authn consumers migrate and legacy is deleted: linked authentication, canonical tenant refusal without a valid credential and platform-token admission still work. Do not wait for T41 to discover a regression in the standalone registry entry
- [ ] Test: a consumer's configuration-built Instance — first start, configuration change, second start — is updated through reconciliation, not refused as `already_exists`; two consumers starting concurrently with the same content both converge
- [ ] Integration test: a consumer registers during `init`, before any `post_init`, a document that depends on a declaration from the audit list, and the outcome matches the order the audit fixed
- [ ] Test: a read issued after an entity is written directly to the database (not through the service) returns it — the single-process form of SPEC §13's two-pod criterion
- [ ] `cargo test --workspace`; `grep -r TypesRegistryClient` finds nothing outside history
- [ ] `make quickstart` and `make example` — server boots with all linked inventory present in the database; the configured limits cover the real seed set
- [ ] `make e2e-local` boots with `cfg.entities` populated and readable and remains green: legacy deletion, path promotion and callers are one cutover
- [ ] Manual: restart, confirm entities and artifacts byte-identical

**Final cutover stage — tenant path promotion and all remaining e2e callers:**

**Description:** In this same cutover, after deleting legacy v1, promote the three tenant reads from `/types-registry/v2/` to `/types-registry/v1/` and migrate synchronous e2e callers (D10). Platform routes already use `/types-registry/platform/v1/`. The cutover merges with its e2e callers; no red window is accepted.

The e2e surface: the legacy v1 suite `testing/e2e/suites/types_registry/legacy/` — six test files, ~95 references to `/types-registry/v1/entities` — plus T32's already-migrated `testing/e2e/suites/account_management/conftest.py` (recheck its platform writes/polls; update interim read paths only if used), and `testing/e2e/suites/oagw/helpers.py:74`, which registers a batch of OAGW schemas *and instances* and reads them back through `list_oagw_types` (`GET /entities`), so it needs both migrations. Four async-registration scenarios already exist in [`registration.md`](../../../../../testing/e2e/suites/types_registry/scenarios/registration.md); the local launcher uses SQLite, so these runs make no PostgreSQL/MySQL-specific claim.

**Acceptance criteria — promotion:**
- [ ] No `/v2/` path remains in the crate, in OpenAPI or in `QUICKSTART.md`
- [ ] The promotion changes paths only: the tenant reads keep `.authenticated()`, and the platform routes stay on `/types-registry/platform/v1/` with `exposed = false` on mutations until C8's platform listener and authorization gate exist
- [ ] The tenant REST client moves its base path constant to `/types-registry/v1/` in the same change, and its contract test passes on the promoted paths; the platform client does not change
- [ ] `operation_id`s are unchanged — `types_registry.submit_entities`, `.get_operation`, `.get_entity` and T20a/T22a's additions keep their names; bodies, statuses and semantics are the ones the routes already had
- [ ] The three tenant reads promote together; T22d's `ETag` / `If-None-Match` and per-key `unchanged` move with them unchanged — additive, so no changelog break entry (P21)
- [ ] Old v1 handlers, DTOs and routes are **deleted**, not repointed, verified by scoped symbols rather than by type names the new SDK reuses:
  - `TR/src/api/rest/routes.rs` has no `register_v1` and registers no `types_registry.register`, `types_registry.list` or `types_registry.get` operation (the v2 IDs `types_registry.get_entity` / `.list_entities` and the rest remain);
  - the v1 handlers `register_entities`, `list_entities` and `get_entity` that read `TypesRegistryService` are gone from `TR/src/api/rest/handlers.rs`;
  - the v1-only DTOs in `TR/src/api/rest/dto.rs` — `RegisterEntitiesRequest`, `RegisterResultDto`, `RegisterEntitiesResponse`, `RegisterSummaryDto`, and `GtsEntityDto`, `GtsIdSegmentDto`, `ListEntitiesQuery`, `ListEntitiesResponse` wherever only v1 uses them — are gone. The SDK's semantic `RegisterEntitiesRequest` (`TR-SDK/src/models.rs`) and the v2 wire DTOs are unaffected;
  - no file under `TR/src/api/` references `TypesRegistryService` or the in-memory repository (T38's `TypesRegistryClient` grep covers the old trait); the generated OpenAPI lists none of the three v1 operation IDs; every surviving v1 route reads the database
- [ ] SPEC §10.2 records the final shape and closes the interim window, naming T9a as where it opened and this task as where it closed
- [ ] Changelog: the v1 `POST` break (it moves to `/types-registry/platform/v1/entities` with a platform token, body shape, `202`, submit-then-poll) and the read-shape break (`GET /entities` pagination plus document-free defaults) are **one release, two entries** (P17/P19, P25)
- [ ] `api_rest_test.rs` needs only its per-version path constant changed — if it needs more, T9a's last criterion was not met and that is the finding

**Acceptance criteria — e2e:**
- [ ] Every e2e registration, deletion and operation poll — the `types_registry` suites, `account_management`'s registration helper, `oagw/helpers.py` — uses `/types-registry/platform/v1/` with T34's `platform_headers`; entity reads use `/types-registry/v1/` with their bearer
- [ ] A shared polling helper lives in `testing/e2e/suites/types_registry/helpers.py` (the async-surface helper T34 already updated) and is reused; no test open-codes a poll loop. It has a bounded deadline and fails with the operation's per-candidate errors, never on a bare timeout
- [ ] Recheck T32/T34's AM registration helper: it already polls to terminality before returning and stays compatible with final paths; do not implement it again
- [ ] Assertions move from the POST body to the operation's per-`gts_id` outcomes
- [ ] `GET /entities` call sites move to the paged, document-free default (D12/D13): the shared helper pages through the cursor; an assertion needing `content` selects it explicitly
- [ ] Legacy `is_schema` filters migrate to T22c's `kind=type_schema|instance`, and callers needing a chain boundary use inclusive `depth`. A legacy `vendor`/`package`/`namespace`/`segment_scope` predicate is translated only when an equivalent GTS `pattern` is proved; otherwise the migration records a follow-up rather than silently widening the result set
- [ ] Tests that assert refusals still assert them **synchronously** — envelope, identifier, policy and idempotency failures stay pre-`202` (SPEC §8.1)

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry`; `make lychee`
- [ ] Manual: `/cf/docs` renders the tenant reads under `/types-registry/v1/`, the platform operations under `/types-registry/platform/v1/`, and no v2 path resolves
- [ ] `make e2e-local` — full suite green, including `account_management` and `types_registry`; `make e2e-docker`
- [ ] Manual: a rejected candidate surfaces its reason through the polled operation, not as an opaque failure

**Dependencies:** T37 (Checkpoint 7A), T31, T32, T35 and T36 **Files likely touched:** The remaining SDK consumers and legacy deletion paths listed above; TR REST handlers/DTOs/routes, tenant client path constant, API/contract tests, shared e2e polling and all remaining legacy callers, QUICKSTART and CHANGELOG **Scope:** L delivery split into S/M commits; legacy removal, tenant v2-to-v1 promotion and caller migration merge atomically. No intermediate red e2e window

---

### Checkpoint 7 — every gear on the persistent registry
- [ ] Every gear uses the new SDK against the database; `TypesRegistryClient`, ready mode, the in-memory repository and the old v1 routes are gone; reads are cached with the late-fill guard (T38, T36)
- [ ] `PlatformTypesRegistryApi` and its helpers work through the real local client, carrying T22d's validators byte for byte; both adapters return operations read through `get_operation` (T29, T34)
- [ ] The platform REST client passes its contract test on `/types-registry/platform/v1/` and the tenant client on `/types-registry/v1/`; platform routes serve a validated internal token only, tenant reads a validated bearer only (T34, T35, T38, P25)
- [ ] Linked inventory and `cfg.entities` seed through the outbox; a second start reports `unchanged`; no registration the audit listed is refused by immediate validation (T38)
- [ ] One REST version, and the e2e suites pass on the `202` contract (T38)
- [ ] `make ci` (including the real AM remote target from T41 onward), gear tests on three backends, `make e2e-local`, `make e2e-docker`, `make dylint`, `make lychee` green
- [ ] Human review — the contract shape is fixed here, and every gear is on it

---

## Phase 8 — Publication after wiring: Account Management first, then every gear out of process

Runs **T39 → T40 → T41 → T42 → T43**. The SDK does not change; startup does — publication leaves `init`, each gear gates readiness on it, and T42 ends the pull. Account Management already uses only the new local API from T32. T41 adds post-wiring publication/bootstrap readiness; T41 proves it with a remote registry and moves the selected host prerequisites, before the remaining lifecycle changes in T42. From T41 until Phase 9 ends, ceiling C11 applies.

### - [ ] T39: Per-crate GTS collectors and the gear's `gts(…)` publication attribute

**Description:** Add crate-local collectors and gear publication ownership (D16). `declare_gts_inventory!()` defines the local entry type, `inventory::collect!` and plain-data `gts_declarations()`. `#[gts_type_schema]`, `gts_instance!` and `gts_instance_raw!` submit to `crate::__gts_inventory` and, until T42, also to the global registry for transitional seeding. The facade retains `inventory` but permits a later local switch to `linkme` (P23). The gear attribute connects collectors to T37's lifecycle.

Commits inside the task: (1) the collector macro; (2) declaring crates in mechanical slices; (3) the `gts(…)` attribute; (4) independent collector compile/release-LTO checks.

**Acceptance criteria — collectors:**
- [ ] `declare_gts_inventory!()` exists; a declaring crate without it fails to compile with a message naming the macro
- [ ] The entry type is a **newtype defined in the declaring crate**, not an alias of a shared `toolkit-gts` type — an alias would share one registry and lose the gts-rust #130 isolation
- [ ] `gts_declarations()` returns std types only — no `toolkit-gts` or `gts` type crosses a crate boundary. Its output is sorted by identifier; two entries with one identifier and different content are a diagnosed conflict naming the crate, identical duplicates collapse
- [ ] `#[cfg]`-gated declarations gate their submission, not only their struct
- [ ] `crate_path` resolution still distinguishes `toolkit-gts` itself from integration targets (`toolkit-gts-macros/src/lib.rs:118`). Generic-schema restrictions are unchanged
- [ ] Every crate in the workspace that declares GTS entities calls `declare_gts_inventory!()`, including test, example and doctest crates
- [ ] Manual `inventory::submit!` of `toolkit_gts::Inventory*` is replaced by the new entry API: `settings-service-sdk` (`src/gts.rs:48`, `src/catalogue.rs:109`)
- [ ] `toolkit_gts::gts_declarations()` returns the base types (`PluginV1`, `AuthzPermissionV1`) for types-registry to seed
- [ ] The global registry is still populated (transition) and carries a comment naming T42 as its removal

**Acceptance criteria — the gear's `gts` attribute:**
- [ ] `#[toolkit::gear(…, gts(crates = [crate, some_sdk], publisher = path::to::publish))]` is accepted. The owner is the gear; the crates are only the units of collection
- [ ] The macro emits static ownership metadata (gear name, listed crates) that T42's coverage check reads
- [ ] The macro emits a post-wiring step that concatenates each listed crate's `gts_declarations()`, passes the gear's name and `env!("CARGO_PKG_VERSION")` **expanded in the gear's crate** to the named publisher, and composes the returned status into the gear's `Required` readiness contribution (T37)
- [ ] Generated and handwritten `post_wiring` compose: the generated step runs once even when the gear also implements the hook
- [ ] A gear without `gts(…)` is unchanged; a listed crate that does not call `declare_gts_inventory!()` fails to compile

**Verification:**
- [ ] `cargo test --workspace`
- [ ] A fixture binary built in release with LTO asserts per-crate declaration counts, including a crate reached only through its `gts_declarations()`
- [ ] Macro tests: renamed dependency, `cfg`-gated declaration, duplicate identifiers with equal and with different content; `gts(…)` gathers every listed crate once into one publisher call, forwards the gear crate's name and version (not an SDK or helper crate's), emits ownership metadata; a shared helper in a third crate forwards the caller's context and never builds one
- [ ] Independent macro/collector compiler fixtures prove crate boundaries and declaration isolation, including release+LTO and distinct provider/consumer declaration sets. Wire them into the existing macro-test CI venue
- [ ] `make gts-docs`, `tools/gts-analyze` unaffected

**Dependencies:** T38 (Checkpoint 7), T37; independent compiler fixtures require no remote-application harness **Files likely touched:** `libs/toolkit-gts/src/lib.rs`, `libs/toolkit-gts-macros/src/lib.rs`, `libs/toolkit-macros/` (the attribute), `lib.rs` of every declaring crate, `gears/settings-service/settings-service-sdk/src/{gts,catalogue}.rs`, the collector compiler fixtures **Scope:** L — four commits as listed; the declaring-crate line lands in mechanical slices

---

### - [ ] T40: Publication ownership, dependent configured entities after wiring, and late-safe plugin selection

**Description:** Reuse publisher assignments already fixed by T31/T32 and close only the remaining owner mappings (P23, O5); add post-wiring publication for configured entities with dependencies outside the inline seed set; and prevent incomplete plugin selections from being cached. The dependent-entity path is needed by the isolated real AM deployment in T41 and all registries after T42. `GtsPluginSelector` caches its first success until reset (`libs/toolkit/src/plugins/mod.rs:56`), so late instances could otherwise leave a worse-priority selection cached (D21).

**Acceptance criteria — ownership:**
- [ ] For the registry's control-plane types, the `toolkit-gts` base types and each kind of `cfg.entities` item, the publishing gear and the call that publishes it are decided. Registry-owned items publish as `types-registry` with the registry crate's version
- [ ] Reuse and verify T32's AM/IdP/TR instance owners, then assign the remaining configuration-built Instances from the audit to their building gear; shared helpers only forward that context. Do not repeat the seven-gear SDK migration or reopen the same owner decisions
- [ ] Retain the established assignments: AM owns its configured root and TR instance; each static/Keycloak IdP plugin owns its instance; each standalone TR plugin owns its instance. Deployment-specific child types stay registry-owned `cfg.entities`, published by types-registry through the path below; the isolated remote AM setup uses one. No owner is inferred from a GTS namespace or placeholder
- [ ] SPEC §17 O5 and D11 (as amended) record the outcome

**Acceptance criteria — dependent configured entities:**
- [ ] A `cfg.entities` item whose `$ref` or chain dependencies all lie in the registry's inline seed set stays inline, unchanged. One with a dependency outside it is published by types-registry after wiring through `publish_gts`, with the registry's context, and retried until its dependency is admitted
- [ ] The registry's client publication and readiness do not wait for those items; only the consumer that needs one waits for its status. An invalid configuration envelope still fails boot synchronously
- [ ] While the pull lasts, an embedded host's inline set contains the linked inventory, so `e2e-local.yaml`'s customer type stays inline there; the remote AM registry, which links no consumer, takes the post-wiring path. T42 narrows the inline set and reuses this path; it adds no new mechanism

**Acceptance criteria — plugin selection:**
- [ ] The selector does not cache a selection while an instance of **the same plugin contract and vendor** with a scoped client registered in the `ClientHub` is not yet visible in the registry; an unrelated missing plugin does not block it. An error or an incomplete set is not cached, and retry resolves again
- [ ] `choose_plugin_instance`'s priority semantics are unchanged

**Verification:**
- [ ] Walkthrough with the owner — cold `e2e-local.yaml` bootstrap after the pull ends, a plugin's configuration-built instance, the remote AM deployment's child tenant type
- [ ] Registry test: a configured entity depending on an unpublished type — the registry boots and is ready; a second publisher publishes the dependency; the entity is admitted; restart reports `unchanged`; an invalid envelope fails boot
- [ ] T41 real AM scenario: the registry-owned configured child type is admitted only after AM publishes its base; T40 covers the same mechanism through focused registry tests
- [ ] Toolkit test — two instances of one contract and vendor, the worse-priority one visible first: no cached selection until both are visible, then the better one; an absent instance of another vendor does not delay it

**Dependencies:** T39 **Files likely touched:** `docs/p0/{SPEC,plan,todo}.md`, `TR/src/gear.rs`, `TR/src/domain/seeding.rs`, `TR/tests/seeding_test.rs`, `libs/toolkit/src/plugins/mod.rs` and its tests, real AM deployment configuration **Scope:** M — the decision record, the registry path and the selector rule are separate commits

---

### - [ ] T41: AM after wiring and remotely — host dependencies, bootstrap and readiness

**Description:** Move AM publication, root-type/tenant-resolver-instance registration and bootstrap after wiring, with both IdP writers, and verify the embedded host using the real static IdP path. T32 already moved all AM registry workflows and both IdP writers to the new SDK. This task changes lifecycle, not client/model selection: move T32's root/TR-instance reconciliation and registry-dependent bootstrap barrier out of startup. Use real AM; static IdP's development echo behavior is not a production IdP guarantee.

**Acceptance criteria:**
- [ ] AM publishes its owned crates through `gts(…)`; its configured root type and, when enabled, its tenant-resolver plugin instance publish after wiring through `publish_gts` with AM's context; their status joins AM's `Required` readiness
- [ ] AM's `init` validates its configuration and the existing root binding and performs no registry operation or remote client lookup. Registry-dependent services receive the real new client after wiring through `#[consumes]` — no mock, legacy fallback or half-initialized public service. AM drops `deps = [types_registry]` with its last startup registry call
- [ ] **Two failure classes stay distinct.** The root binding — the stored root's `tenant_type_uuid` against the configured root type, read from AM's own database — is checked in `init` and stays lifecycle-fatal, independent of `bootstrap.strict`. A terminal refusal of the configured root type by the registry happens after wiring and follows SPEC D21: AM stays not ready with a terminal status naming the gear, identifier and reason, bootstrap does not run, and the process does not exit. A delayed or unreachable registry holds readiness and retries under the SDK policy; neither path relaxes type enforcement or treats a receipt as success
- [ ] The bootstrap saga runs as supervised, cancellable work: it waits for the root type and, under `idp.required`, the IdP instance, then creates or resumes the root. AM's readiness requires both its publication and completed bootstrap. Three outcomes stay distinct:
  - **registry prerequisite pending** (registry unreachable, root type or IdP instance not yet admitted) — the saga waits and retries; this is never a saga failure, never causes premature root creation, a failed `init` or a process-terminating timeout, and is not subject to `bootstrap.strict`;
  - **publication terminally refused** (root type or IdP instance rejected or superseded-deleted) — AM stays not ready with a terminal status per D21, the saga does not start, and `bootstrap.strict` does not turn it into an exit;
  - **saga failures after the prerequisites are met** (business, database or IdP errors) — keep today's `handle_bootstrap_failure` policy (`account-management/src/gear.rs:1401`): `RootBindingMismatch` is lifecycle-fatal regardless of policy, any other failure is fatal under `strict = true` and logged-and-skipped under `strict = false`. `bootstrap.strict` also keeps governing configuration validation as today
- [ ] Both static-idp-plugin and keycloak-idp-plugin move their T32 instance publication after wiring with each plugin crate's context and keep their real scoped backend client; readiness reflects admission, shutdown observes publication, and no registry call remains in either `init`. Changed configuration converges through reconciliation. Preserve backend behavior; external Keycloak tests use their existing supported environment rather than claiming static IdP is equivalent
- [ ] AM's lazy IdP selection uses T40's selector rule: it waits while the instance or its schema is delayed, and a rejected instance never reads as success
- [ ] Resource Group's init-only `ResourceGroupTypeBootstrap` keeps its sealed, no-REST boundary: `register_user_group_types` → RG `TypeService` uses RG's own database and schema validation without the registry (`resource-group/src/gear.rs:97-180`), so it stays in AM's `init`, proved by a test; the unscoped handle is never kept past its seal in `register_rest` or exposed remotely
- [ ] AM and both IdP plugins retain T32's API-migrated status and are now marked lifecycle-migrated in the audit, so T42 does not repeat either change. Drop `deps = [types_registry]` and the registry implementation-crate dependency together with the last init-time registry call; keep the SDK/resolving contract dependency

**Verification:**
- [ ] AM tests: root-binding drift fails `init` under `strict = true` and `false`; a refused root type leaves AM not ready without exiting under both policies; a delayed registry holds readiness, then admission and bootstrap release it, under both policies; a business, database or IdP saga failure after the prerequisites are met is fatal under `strict = true` and skipped with a warning under `strict = false`, as today; cancellation during the wait; tenant-type checks and metadata after wiring
- [ ] `cargo test -p cf-gears-account-management -p cf-gears-account-management-sdk -p cf-gears-static-idp-plugin -p cf-gears-keycloak-idp-plugin`; RG seal regression tests and T32's existing application scenarios
- [ ] Delayed `IdpPluginSpecV1` admission, restart and a configured-instance change through the real registry and outbox
- [ ] `make quickstart`; `make e2e-local SUITE=account_management` and the full `make e2e-local`

**Host and remote stage — prerequisite closure, production authentication and real AM proof:**

**Description:** Prove the migrated production AM application against a separately launched production registry host, using existing OoP tooling and real AM REST tests. Migrate its host prerequisites (authn-resolver, authz-resolver, resource-group, tenant-resolver and selected plugins) in T38's audited order, using production authentication. Each currently reads the registry during startup or plugin selection (`account-management/src/gear.rs:93`). The tenant-resolver family already uses the new local API from T32; move only its outstanding startup/lifecycle behavior here, for the selected plugins. T41 is the remote/production-auth adoption gate; real local AM REST adoption is already T32; this stage owns all process/runtime scenarios for the real application.

**Production authentication.** AM's gateway and the remote registry use `AuthNResolverBearerAuthenticator` over real authn-resolver, with its plugin published after wiring, using T35's linked production topology. Each host uses its own local AuthN client; there is no new remote AuthN transport. The registry host may link authentication declarations, but never AM/RG/TR declarations that would hide the application publishers. Its aggregate readiness waits for its authn schema/plugin admission; platform internal-token operations remain available independently. T43 rechecks that topology after the pull ends. Development auth does not satisfy this task.

**Acceptance criteria:**
- [ ] The AM host's prerequisites from the audit — authn-resolver and its plugin, authz-resolver and its plugins, resource-group, tenant-resolver and its selected plugins — are listed with their transports and each registry startup call; each publishes after wiring with its own context and readiness, in the audited order, with real `PolicyEnforcer` and authentication. No mocked AM, RG or bootstrap service and no early proxy-wiring shortcut masks an incomplete migration. Each is marked migrated for T42
- [ ] Close the startup dependency graph of those publishers, including their callers outside AM's direct dependency list. In particular, oagw's eager `post_init` root lookup must not race static-tr's asynchronous publication: move the dependent step to supervised/lazy execution before or with that publisher. A delayed plugin publication keeps readiness pending and never fails embedded boot; do not leave this broken interval for T42
- [ ] Before the owning publishers run, the isolated registry holds no AM schema, root type, configured instance or child tenant type. AM publishes, bootstraps an Active root; the child type (T40) is admitted after AM's base type; a real authenticated create → get → list of a child tenant succeeds with the correct `tenant_type`. Stored root and tenant state and the returned DTOs are asserted; a schema-invalid or type-incompatible request is refused
- [ ] Verify provider isolation per declaration owner: linked authn types/plugin are expected, AM/RG/TR declarations are absent before their own publication. The registry host stays not ready until its authentication plugin is admitted and tenant reads fail closed meanwhile; platform internal-token admission still works, so authentication publication can complete without a startup cycle
- [ ] Use real production registry and AM host entries with the existing directory/OoP launcher. Remove both `deps = [types_registry]` and the registry implementation-crate dependency from the entire AM host closure as its last init-time registry call moves after wiring, including authn/authz/RG/TR and selected plugins. The AM host's `cargo tree` contains no package named exactly `cf-gears-types-registry` (the `-sdk` package is expected); assert no local registry provider/client is registered. Do not reuse the full example-server or Payment binaries unchanged, because they link the registry and could mask remote failures
- [ ] Prove the real application across processes: both start orders; AM starts with the registry absent and holds readiness until recovery; configured child-type dependency admitted only after AM publishes its base; refused AM publication stays not ready with a reason; directory resolution/restart without local fallback; database/entity/artifact persistence and graceful shutdown. Drive the existing authenticated AM tenant/metadata/IdP REST workflows, and reuse T34/T35/T36 transport/auth/cache helpers rather than inventing another business consumer
- [ ] Local and remote-registry modes run the same production AM code and real persistence and authz services; the remote mode resolves SDK REST clients with no local fallback. Both start orders, delayed AM/IdP/prerequisite publications, registry and AM restarts and a configuration change converge, with readiness tied to publication and completed bootstrap
- [ ] Both API clients' read parity, credentials and conditional reads hold against the admitted AM entities. Root-binding drift stays fatal under both strict policies; a refused root type holds readiness; a transient registry delay does not fail boot
- [ ] Embedded AM e2e stays green; the handoff instructions name every directory, authentication and prerequisite process

**Verification:**
- [ ] `make e2e-am-remote` builds/launches the production AM composition (`apps/cf-account-management-host`) and registry host from T35 with directory/config/auth dependencies, reuses existing AM REST scenarios and runs the process cases above. Add this target to CI when T41 implements it; no test role switch is introduced
- [ ] Tests of each migrated prerequisite gear, including a delayed-prerequisite test per audit entry closed here
- [ ] Gear tests on SQLite/PostgreSQL/MySQL; `make e2e-local`; reproducible commands and the production files used are recorded

**Dependencies:** T40, T32; T38 supplies the full-fleet startup dependency audit **Files likely touched:** AM and both IdP publication/bootstrap lifecycle; selected authn/authz/RG/TR host prerequisites and dependent startup callers; the real AM OoP launcher and production host entries, real authentication configuration, AM REST scenarios and audit **Scope:** L feature: local publication/bootstrap, host dependency closure, then remote/production-auth proof. Split stages into S/M commits and preserve embedded boot after every merged change

---

### Checkpoint 8A — Account Management after wiring and remotely
- [ ] Real AM and static-idp-plugin publish declarations and configured entities after wiring, bootstrap an Active root and create and read child tenants through the new registry, in the embedded host and with the registry in another process (T41)
- [ ] Both start orders, delayed dependencies, a configuration change and restarts converge; type-invalid requests are refused; root-binding drift is fatal and a refused root type holds readiness; `Required` readiness covers AM publication and bootstrap
- [ ] The production authenticator path is exercised; RG's init-only bootstrap boundary and real authz enforcement are intact; embedded AM suites pass
- [ ] `make ci` (including the real AM remote target from T41 onward), gear tests on three backends, `make e2e-local` green; human review before the fleet migrates. Final deployment waits for Checkpoint 9

---

### - [ ] T42: Every remaining gear publishes after wiring; end of the pull

**Description:** T32 moved the complete AM/IdP/TR group onto the new API; T38 moved the remaining fleet. T41 then moved AM and selected prerequisites after wiring. This task changes the **startup behaviour** of every other declaring or registry-dependent gear (P22, P23), in T38's audited order, and its last commit ends the registry's process-wide pull.
- **Declarations.** Each gear lists the crates it owns — its own crate and its SDK crate — in `#[toolkit::gear(…, gts(crates = […], publisher = types_registry_sdk::publish_gts))]` (T39). The generated post-wiring step publishes them with the gear's own `PublisherContext` and gates `Required` readiness.
- **Explicit registrations.** The synchronous reconciliation calls introduced in T31/T32 or migrated in T38 that remain inside `init()`, and configuration-built Instances, move onto the same path. An unselected TR plugin from T32 may still need this lifecycle move; its SDK migration is never repeated
- **Other startup calls.** Every other registry call made during `init()`, reads included, moves to the post-wiring hook or to a lazy first use, because a remote client is wired only after every `init`. Every registry-dependent step in `post_init`, `start`, bootstrap sagas and first plugin selection on the audit list moves to a supervised task that waits for its own prerequisite (SPEC D21) — e.g. oagw's root-tenant resolution and upstream materialization in `post_init`.

Gears, minus those T41 already migrated: system — `authn-resolver` (+ static and oidc plugins), `authz-resolver` (+ static and tr plugins), `tenant-resolver` (+ static, single-tenant and rg plugins), `resource-group`, `usage-collector` (+ plugins), `cluster`, `credstore` (+ static plugin), `oagw`, `license-resolver` (+ static-license plugin), `event-broker`; domain — `bss/ledger`, `bss/rate-provider` (its shared `registration.rs` helper), `mini-chat` (+ static-audit and static-model-policy plugins), `llm-gateway`, `model-registry`, `settings-service`, `bss/pricing` (which registers during startup, `pricing/src/module.rs:1244`).

**Acceptance criteria — publication after wiring:**
- [ ] Every gear with a startup registry call or GTS declarations is either lifecycle-migrated here or recorded as such by T41; that evidence is verified, not redone. T32's API-migrated status alone does not imply post-wiring completion: close remaining unselected TR/IdP lifecycle calls where applicable, without redoing their SDK translation
- [ ] Each gear with GTS declarations lists exactly the crates it owns in its `gts(crates = …, publisher = …)` attribute, and its readiness waits for the generated publication. A crate owned by another gear is not listed, e.g. `ledger-sdk`'s rate-provider plugin schema stays with ledger
- [ ] Configuration-built Instances are published by calling `publish_gts` directly from the gear's `post_wiring` (e.g. `static-license-plugin/src/gear.rs:71`); their status joins the same readiness contribution; no site treats `pending` as success
- [ ] `rate-provider-sdk`'s shared `register_rate_provider_plugin` publishes after wiring, taking the calling plugin gear's `PublisherContext` as a parameter, so every rate-provider plugin migrates with it
- [ ] No registry call remains inside any `init()` — reads included; grep-checkable per gear
- [ ] No startup phase fails because the registry is unreachable or a declaration is not yet published; each audit entry is closed with a test that delays the prerequisite
- [ ] Explicit and configuration-built publications pass the gear's own `PublisherContext`; a shared helper only forwards it. Until T44 no adapter sends it to the server
- [ ] Gears move in the audited order: a consumer that waits for a publication during its own startup moves before that publication leaves `init`, or both move in one change; no step reintroduces a wait on a publication already moved after wiring
- [ ] A rejected declaration leaves the gear not ready, naming the gear, identifier and reason; it does not fail boot. Shared bootstrap failures remain registry startup failures
- [ ] Each gear drops `deps = [types_registry]` in the same change that moves its last startup registry call after wiring; from then on it reaches the registry only through `#[consumes]`
- [ ] Where a materialized `effective_*` value differs from an old expectation, the materialized value is accepted (P23, SPEC §13)

**End of the pull — the last commit of this task.** Once every declaring gear publishes for itself the process-wide pull is redundant (SPEC D11). Remove the global `toolkit-gts` registries, T39's dual submission and `all_inventory_*`, and narrow the registry's inline seed to its own control-plane types, the `toolkit-gts` base types (`toolkit_gts::gts_declarations()`) and the `cfg.entities` whose dependencies lie within that set. The dependent `cfg.entities` take T40's post-wiring path.

**Acceptance criteria — end of the pull:**
- [ ] `toolkit_gts::InventoryTypeSchema` / `InventoryInstance`, their `inventory::collect!` and `all_inventory_*` are deleted; the macros submit only to the crate-local collector
- [ ] The registry seeds inline its own types, the base types and the `cfg.entities` whose dependencies lie within that set, and nothing else. Every item **of that inline subset** must be `succeeded` or `unchanged` before its client is published (P2/P3); dependent `cfg.entities` are not awaited there
- [ ] Ownership coverage, two checks, both comparing an independently derived expected set against assignments, never against what was submitted:
  - **Workspace.** The expected set comes from `cargo metadata` plus a source scan for `declare_gts_inventory!()`, the same approach as `tools/gts-analyze`. Assignments are the gears' exported `gts(crates = …)` metadata, collected from test binaries that link each gear across its supported feature configurations. Every crate in the expected set must be listed by exactly one gear.
  - **Per binary, feature-aware.** Enumerate the gear metadata registered in the binary and check that each enabled gear's publication callback is registered. An execution test proves the callbacks run.

  Each collector exports its canonical package name (`CARGO_PKG_NAME`), so aliases and `crate` never have to be compared as spellings. Collectors in test, example and doctest crates are exempt unless a shipped binary links them. Linking a crate whose owner runs elsewhere — a consumer of another gear's SDK — is not an omission
- [ ] The C3 source comment names only the attribution placeholder and its replacement by the first claiming publication (T44), not a pull

**Verification:**
- [ ] `cargo test --workspace`
- [ ] `make quickstart` and `make example` — the server boots, `/health` is green and every gear's types are present
- [ ] Test per gear group: publication is idempotent across two starts, and reports `unchanged` alongside the registry's transitional seeding (before the last commit)
- [ ] Test: cold `e2e-local.yaml` — the registry serves admission before its configuration completes, account-management publishes its base type, then the dependent customer type is admitted; no bootstrap cycle
- [ ] The coverage checks pass for every shipped binary configuration and fail when one ownership assignment or one publication is removed; `grep -r all_inventory_` finds nothing outside history
- [ ] `make e2e-local` green

**Dependencies:** T41 (Checkpoint 8A) **Files likely touched:** `gear.rs` and the types-registry call sites of each gear above, `libs/toolkit-gts/src/lib.rs`, `libs/toolkit-gts-macros/src/lib.rs`, `TR/src/gear.rs`, `TR/src/domain/seeding.rs`, a coverage test per binary under `apps/` **Scope:** L — one commit per gear, never the whole set at once; the end of the pull is the last commit

---

### - [ ] T43: Out-of-process e2e run, HA and the deployment chart

**Description:** Prove SPEC §16 criteria 17, 18 and 20 with types-registry in its own process on the real platform, before the guard exists (P22, P23); criterion 19 needs the guard and is T45's. T39 supplies independently tested collectors; T41 already proves the real AM application's cold-start, publication, readiness and recovery scenarios.

**P30 unique scope:** add the end-of-pull production composition, real two-replica failover and chart/PDB checks. Run existing T41 remote, auth, dependency and readiness scenarios against this topology through shared helpers; do not author another copy of their suites. Their regression execution remains part of this gate.

**Acceptance criteria:**
- [ ] An e2e configuration runs types-registry in its own process, with at least one declaring gear in another, using the existing OoP host tooling. `cfg.entities`, discovery and the promoted v1 routes behave as in `make e2e-local`
- [ ] **HA (SPEC D20):** the configuration runs two registry replicas over one database; stopping one during a submit/poll leaves the client resolving the remaining replica, the retry under the same key replays rather than writes, and the operation completes
- [ ] types-registry ships a Profile 3 deployment chart, following `gears/mini-chat/deploy/helm/`, with at least two replicas and a PodDisruptionBudget (`minAvailable: 1`); a rendering check (`helm template` / `helm lint`) asserts both
- [ ] Reuse T41 flows in the final composition: remote resolution/publication/readback without code or local fallback; pending readiness until admission; one-direction cross-process dependencies in both start orders; a stopped-at-start registry followed by recovery without consumer startup failure. These remain required regression cases, not new test implementations
- [ ] Reuse T34/T35 auth helpers and T41 production-auth composition: every route refuses no credential with `401`, bearer-only mutation is refused, and the real platform token succeeds. Verify the T35-selected production tenant-authenticator topology after the pull ends, including the linked variant when selected
- [ ] Manual: kill and restart the registry process; consumers recover without restart

**Verification:** run the real out-of-process e2e configuration alongside `make e2e-local` and `make e2e-docker`; execute the two-replica failover scenario and the chart rendering checks. Record the exact OoP launch/test command with its config.

**Dependencies:** T42 (end of the pull) **Files likely touched:** an out-of-process e2e configuration and its launcher, the shared e2e helpers, `gears/system/types-registry/deploy/helm/` (new chart) **Scope:** M

---

### Checkpoint 8 — out-of-process operation
- [ ] Per-crate collectors, the `gts(…)` attribute, `post_wiring`, `Required` readiness and supervised publication are in toolkit; the plugin selector caches no incomplete selection (T37, T39, T40)
- [ ] Every declaring gear publishes its own crates after wiring and gates its readiness on them; no registry call remains in any `init()`, and no startup phase fails on an unpublished or unreachable registry (T41, T42)
- [ ] No process-global GTS inventory remains; the registry seeds inline only its own types, the base types and the `cfg.entities` whose dependencies lie within that set; dependent configured entities publish after wiring; the coverage test is green (T40, T42)
- [ ] With types-registry in its own process, two replicas and the chart, a remote gear publishes its own crates after wiring, is not ready until they are admitted, and reads them through the same trait (T43)
- [ ] `make ci` (including the real AM remote target from T41 onward), gear tests on three backends, `make e2e-local`, `make e2e-docker`, the out-of-process run, `make dylint`, `make lychee` green; ceiling C11 still applies
- [ ] Human review

---

## Phase 9 — Mixed-version rollout: the publisher-version guard

Plan P23, SPEC D18. **T44 → T45** completes mixed-version support after fleet migration, verified through the real database, worker and outbox. This phase renames `owning_gear` to `publisher_name`, adds `publisher_version`, the wire field and the guard. T45 requires publisher on every global platform mutation, with no activation switch or adoption manifest. Pre-migration rows are unclaimed until their first publication. Requests may omit publisher before T45 and skip the guard; intermediate tasks are never deployed.

### - [ ] T44: Publisher guard — durable state, wire context and commit-time ordering

**Description:** Everything the guard compares, stored and carried end to end: the domain stamp (the SemVer type and its precedence are T28's), the columns of SPEC §9 *Publication state* in one forward migration that reinterprets no stored content, the request-level `publisher` carried from acceptance through the operation record to every candidate, and the `publisher` body field of SPEC §10.2 on both adapters. The first stage carries the field; the commit-ordering stage enforces it, and T45 makes it required for every writer.

Commits inside the task: (1) stamp; (2) migration; (3) durable context and fingerprint; (4) adapters.

**Acceptance criteria:**
- [ ] A stamp value carries `publisher_name` and `PublisherVersion`; versions under different names are not comparable, and comparing them is `publisher_mismatch`, not an ordering. An unclaimed stamp — version absent, name a placeholder — compares as claimable by any publisher; the placeholder is never compared as a name. No API derives a version from the SDK's own crate; there is no override
- [ ] `entity.owning_gear` is renamed to `entity.publisher_name` on all three backends (`RENAME COLUMN`; SQLite ≥ 3.25), and `ck_tr_entity_owner` follows the rename; the `"types-registry"` placeholder stays in place on existing rows. `entity.publisher_version` is added, nullable, with no backfill: `NULL` marks an unclaimed row
- [ ] The operation's durable `publisher_name` / `publisher_version` are added, nullable for operations accepted before the migration, both or neither, platform plane only. No item status or column is added: `superseded` and `publisher_mismatch` are reasons on a `failed` item, in `error_payload`. A CHECK admits a stamp only on a global entity with a publisher name
- [ ] The ORM entity, repositories and every Rust reference move to `publisher_name`; raw SQL only inside the migration infrastructure; `docs/database.sql` matches
- [ ] Acceptance validates `publisher` (both fields, SemVer within its bound) and stores it on the operation; the worker reads it for every candidate and never derives it from its own package version or configuration. The request fingerprint includes it: the same key with a different `publisher` is a `409`. Idempotency records accepted before the migration keep their replay semantics; an outcome round-trip loses no stamp information
- [ ] The REST DTOs of `POST /entities` and `POST /entities:batchDelete` carry a top-level `publisher: { name, version }`; the SDK's REST client sends `RegisterEntitiesRequest::publisher` / `DeleteEntitiesRequest::publisher` there, and the local client passes it through — until this task both adapters dropped it. `publisher` is accepted from the local client and a validated platform-plane caller only; from a tenant bearer it is refused before an operation exists. The context belongs to the call, never to the shared client in `ClientHub`

**Verification:**
- [ ] Focused tests — ordering through the stamp, `publisher_mismatch`, an unclaimed row
- [ ] Migration tests — a populated pre-migration database keeps every row with its placeholder name and `NULL` version; `make test-integration LANE=types-registry-db` on PostgreSQL and MySQL, SQLite in the plain gear tests
- [ ] Acceptance and storage tests — same body with a different `publisher`, a lost response retried under the same key, a pending item re-read from a real database
- [ ] The real local client and the real TCP contract test — mixed credentials, an invalid SemVer, strict operation-result parsing

**Commit-ordering stage — claim/check, metadata-only confirmation and deletion:**

**Description:** SPEC §8.1 step 1a inside the existing serialized commit, without a new lock order: the stamp check and claim, metadata-only confirmation on every `unchanged` path, and an ordered deletion (SPEC D18).

Commits inside the task: (1) check and claim, including early refusals; (2) metadata-only confirmation; (3) versioned deletion.

**Acceptance criteria — check and claim:**
- [ ] After the `entity_write_order` claim and **before** the caller precondition: another `publisher_name` on a claimed row → `failed` with reason `publisher_mismatch` (stored and offered name in its context); lower → `failed` with reason `superseded` (stored and offered version in its context), nothing written; equal or higher → continue
- [ ] **Claim:** on an unclaimed row the check passes for any publisher, and a successful commit writes both `publisher_name` and `publisher_version`; a creation writes the stamp with the entity
- [ ] On success, content and stamp commit in one transaction; failed, refused, CAS-refused and dry-run items never move or claim a stamp
- [ ] A lower version with a stale `expected_resource_version` reports `superseded`, not `precondition_failed`
- [ ] **Early refusals too:** SPEC §8.1 step 3 terminalizes `precondition_failed`, `entity_deleted`, compatibility and dependency refusals (`dependency_not_found`, `dependency_deleted`, `blocked_by_dependency`) before the commit transaction. The stamp check takes priority over all of them: a candidate is checked against the stamp before any such refusal is finalized, and a mismatched or lower one is `publisher_mismatch` or `superseded` instead; the check and the recording of the refusal are serialized under the `entity_write_order` claim, because a stamp read earlier can be overtaken by a concurrent publication. Scenario: a newer release widened a schema, an older release restarts and offers the narrower one → `superseded`, not a compatibility refusal
- [ ] Two pods of one version with different content — configuration-built Instances during a rollout — are ordinary admissions; the last writer wins and the stamp is unchanged (SPEC D18, C11)
- [ ] Partial admission is per entity: A@2 committing does not make B@2 conflict with a B@1 still stored
- [ ] A request without `publisher` skips the check and writes no stamp until T45 refuses it

**Acceptance criteria — metadata-only confirmation:**
- [ ] Higher + identical content advances the stamp, and identical content on an unclaimed row claims it; `resource_version`, revision, `updated_at`, artifacts, `resolution_fingerprint` and the validator are byte-identical, and no reverse-impact refresh runs
- [ ] Both `PreparedUnit::Unchanged` and the in-transaction `unchanged` re-run step 1a under the claim
- [ ] For a minor-bearing Type Schema, confirmation is metadata-only and allowed only on exact authored equality; changed content is still refused by ADR-0004
- [ ] A lower confirmation never lowers a stamp; a dry run persists nothing

**Acceptance criteria — versioned deletion:**
- [ ] A deletion with the same `publisher_name` and a `publisher_version` ≥ the stamp passes the existing CAS, lifecycle and dependant checks and writes the tombstone and the stamp in one transaction; a lower one is `superseded` and another name `publisher_mismatch`, both before the precondition
- [ ] A deletion of an unclaimed row claims it with the tombstone; a deleted entity is not resurrected by a later publication; nothing is deleted because a manifest stops naming it; failed and dry-run deletions never move a stamp

**Verification:**
- [ ] Real SQLite transaction tests with controlled ordering — another name on a claimed row → `publisher_mismatch`; the first publication on an unclaimed row claims it, a second name then mismatches; a newer release dropped a reference and deleted its target, an older release offers the old document → `superseded`, not `dependency_deleted` and not a retry loop; delayed old operation (0.1 accepted, 0.2 commits, 0.1 commits → `superseded`), partial A/B; the concurrency cases on PostgreSQL and MySQL
- [ ] Snapshot and validator byte-equality tests; delayed metadata-only 0.1 after 0.2; a metadata-only claim of an unclaimed row; a negative minor content update through the real acceptance path
- [ ] Real API and worker deletion tests by identifier and by UUID key through `:batchDelete`, a delayed old deletion after a higher confirmation, dry run, dependant refusal
- [ ] Local and REST outcomes match, including `superseded` and `publisher_mismatch`; no adapter synthesizes a successful result from a receipt

**Dependencies:** T43 (Checkpoint 8) **Files likely touched:** Domain publication/stamp/acceptance/fingerprint/commit/unchanged/deletion paths, storage ports/entities/repos, forward migration and database.sql, local/REST adapters and guard/backend tests **Scope:** L server feature: stamp/migration, durable request context/adapters, then serialized check/claim/confirmation/deletion. Implementation stages use small reviewable commits; no intermediate deployment

---

### - [ ] T45: Publisher activation and mixed-version rollout proof

**Description:** `publish_gts`, reconciliation and every other writer submit with a publisher context against the guard, end to end; then the unversioned path closes (SPEC D18, §10.2). Every P0 mutation is global and platform-plane, so after this task no write bypasses the guard.

**Acceptance criteria:**
- [ ] Reconciliation always submits, equal documents included, so the registry sees and confirms a higher version; the `UpToDate`/no-`POST` shortcut is gone from the platform helper. Retries of an identical submission reuse one key, a new cycle takes a new one
- [ ] `superseded` and `publisher_mismatch` are terminal and diagnosable, with the stored and the offered value; a new cycle never raises its own version or reads one from the registry
- [ ] `ReconcileOutcome` regains a `Superseded` variant, separate from `Rejected`, with a liveness re-read after the outcome (live, deleted, or unverified when the read fails); `PublicationStatus` gains superseded-live (ready, with a warning and a metric carrying both versions) and superseded-deleted (not ready). T29 removed the first version as unreachable before T44; `c5f491cc8` has it
- [ ] The registry's own writes carry its own context: its control-plane types, the base types it seeds and `cfg.entities`, inline and after wiring, are published as `types-registry` with the registry crate's version (SPEC §17 O5)
- [ ] Every other writer in the workspace — e2e helpers, fixtures, real AM deployments, scripts and examples that `POST` or delete — sends a `publisher`; a grep over request bodies finds none without it
- [ ] A lost submit or `get_operation` response keeps the operation identity; deadline and cancellation hold; a panicking or exiting task is observed
- [ ] A new `POST /entities` or `POST /entities:batchDelete` without `publisher` is refused with `400` before allocating an operation; OpenAPI marks it required and the local client refuses likewise. An exact same-key replay of a historical accepted request is a read of its stored operation, not a new unversioned mutation; it cannot enqueue/write again. Adding publisher to that old same-key body changes the fingerprint and conflicts; resubmission with publisher uses a new key
- [ ] The single-key `DELETE /entities/{entity_key}`, which has no body to carry a publisher, is removed from the P0 route set; DESIGN keeps it for the tenant plane
- [ ] The commit-time "no publisher, no check" branch of T44 is deleted. For an old operation with no durable publisher, terminalize only unfinished items as `failed` with reason `publisher_required`, before any new entity write; preserve already-committed items, operation identity and stored replay/read outcomes. Add `AdmissionFailureReason::PublisherRequired` and its wire vocabulary, and map it to terminal `ReconcileOutcome::Rejected`, never `Pending`; do not infer a publisher/version. Test partial pre-migration operations, exact replay, publisher-added fingerprint conflict and fresh-key resubmission alongside row adoption
- [ ] Operator documentation names the field, the owner rule — an operator editing another publisher's entity gets `publisher_mismatch` — and that reverting content takes a newer version

**Verification:**
- [ ] SDK tests and real-adapter flows — all-equal documents confirmed, repeated startup, delayed dependency batches, `superseded` without a retry storm; the readiness matrix for superseded live and deleted
- [ ] API tests — a request without `publisher` on each route, the removed route returning `404`/`405`
- [ ] `make e2e-local` green with every request carrying a publisher

**Final rollout stage — mixed versions, pre-migration rows and real platform profiles:**

**Description:** Extend the real AM remote scenarios from T41 and final topology from T43 with the guard (SPEC D18, D22); reuse their launchers and API flows, adding mixed-version cases rather than another application or cold-start suite.

**Acceptance criteria:**
- [ ] 0.1 → 0.2 → restart 0.1, a delayed old operation and partial A/B leave the newer content and stamp; the restarted 0.1 is ready with a superseded warning, like a 0.1 pod that never restarted, while a superseded entity that 0.2 deleted holds 0.1 not ready
- [ ] Global validation uses the current registry schema while the older pod decodes with its own DTO; a read-modify-write of a newer payload by the older DTO is not reported as safe
- [ ] A database populated before T44's migration starts under the new release: its rows are claimed by their first publications, and a second publisher of a claimed identifier gets `publisher_mismatch`
- [ ] On three backends and in embedded and out-of-process e2e: publish 0.2, restart 0.1 → newer state persists and 0.1 is ready with a warning; a tenant bearer and a request without `publisher` are refused. These are T43's guard scenarios, run here

**Verification:** `make e2e-am-remote` with its directory/auth prerequisites, real TCP client, real outbox and controlled delays; the out-of-process e2e configuration of T43; `make quickstart`, workspace tests and the full checkpoint `make ci`

**Dependencies:** T44 (complete server-side guard) **Files likely touched:** SDK reconciliation/publisher, all registry and workspace writers, request validation/DTOs/routes, operator docs, shared real AM runtime scenarios from T41 and T43 real-platform e2e **Scope:** L activation feature: update writers/SDK, require publisher/remove bypasses, then pass the complete mixed-version proof before Checkpoint 9; no separate rollout application or harness

---

### Checkpoint 9 — ready for review
- [ ] The cutover holds: `cfg.entities` and the registry's own types seed into the database within configured limits; every declaring gear publishes its own crates; repeat startup is idempotent and the platform stays healthy. C3 (cooperative attribution) remains documented until P1
- [ ] All 20 success criteria of SPEC §16 met
- [ ] Out of process: with types-registry in its own process, a remote gear publishes its own crates after wiring, is not ready until they are admitted, and reads them through the same trait (T43)
- [ ] The publisher-version guard is active and `publisher` is required on every mutation: rows written before the migration were claimed by their first publication, and an older restart leaves newer content and stamps (T44/T45)
- [ ] The REST clients pass their contract tests on the final paths (T34, T35, T38)
- [ ] No process-global GTS inventory remains; the registry seeds only its own types, the base types and `cfg.entities`; the coverage test is green (T42)
- [ ] Every route refuses a caller without the credential it needs with `401`; mutations on the registry's listener serve the platform plane only (T33, T34, T35)
- [ ] No registry call remains inside any `init()`, and no startup phase fails on an unpublished or unreachable registry (T38's audit, T41/T42)
- [ ] `make ci` (including the real AM remote target from T41 onward), gear tests on three backends, `make e2e-local`, `make e2e-docker`, `make dylint`, `make lychee` green
- [ ] Every ceiling in SPEC §9 has a comment at the point it binds
- [ ] `TypesRegistryClient` is deleted and no crate references it (D6, T38)
- [ ] Conditional reads work end to end: an exact read carries a validator and honours `If-None-Match` with `304`, `batchGet` reports `unchanged` per key (T22d) — proved on `/v2/` at Checkpoint 6, re-checked here on the promoted v1 paths and through the SDK (T29)
- [ ] Discovery is bounded: no response is unbounded in items or bytes, and a cursor traverses the whole set exactly once (T22a, D12) — proved at Checkpoint 6, re-checked here on the promoted v1 paths
- [ ] The client cache is in place on the new models with its window, byte bound, `fresh` bypass, batched conditional revalidation and late-fill guard (T36) — P0 does not ship an uncached read path
- [ ] Human review

## Scope outside P0

Inventory attribution metadata and exposing ownership on the wire/reads remain P1 [#4827](https://github.com/constructorfabric/gears-rust/issues/4827), under [#4628](https://github.com/constructorfabric/gears-rust/issues/4628) (ceiling C3). Per-crate collection and publication after wiring are included in P0 (T39–T42); they are not deferred with attribution.
