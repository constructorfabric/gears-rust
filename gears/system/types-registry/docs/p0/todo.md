# Types Registry P0 — Task List

Plan: [`plan.md`](./plan.md) · Spec: [`SPEC.md`](./SPEC.md)

Standing bar for every task, on top of its own acceptance criteria: `make fmt`, `make clippy` and
gear tests green, no regression in other gears, behaviour verified at runtime, docs updated.
**The full `make ci` is a checkpoint gate, not a per-task one** — it ends in `dylint` and pulls in
four container-backed targets, which is `plan.md` P13's whole point. Naming `make ci` per task is
what made T1–T9 record it as *partial* every time.
Code organisation follows `docs/toolkit_unified_system/` — **not** `guidelines/DNA/languages/RUST.md`.

`TR/` abbreviates `gears/system/types-registry/types-registry/`, `TR-SDK/` abbreviates
`gears/system/types-registry/types-registry-sdk/`. Other paths are from the repository root.

**No new ADRs.** The no-PDP deviation is recorded in SPEC §9 (ceiling C6) and §12; the two wire
breaks in SPEC §10.2 — `POST /entities` (D10) and the paged content-free `GET /entities` (D12).

---

## Commands

Every task below that says **gear tests** means both halves:

```bash
# SQLite — the default backend; every unit and integration test runs here
cargo nextest run -p cf-gears-types-registry

# PostgreSQL + MySQL, one testcontainers container per test (Docker required)
make test-types-registry-db
```

**Per-task versus checkpoint.** Per task: `make fmt`, `make clippy`, gear tests. At the
checkpoint: the full `make ci`, which adds `dylint` (whole-workspace build — `plan.md` P13),
`deny`, `lychee`, `gts-docs` and the container targets.

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
- [x] Every difference in `#[gts_type_schema]`-generated schema documents versus 0.11.0 is enumerated and accounted for — none silent. 9 of 118 documents differ, all by one change (a doc-commented GTS-identifier field's subschema preserved under `allOf` instead of having its `description` overwritten); shown semantically inert, including `x-gts-ref`, which `XGtsRefValidator` still enforces by recursing through `allOf`

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
- [ ] `make ci` green — **partial, see T1**: everything that does not need Docker is green
- [x] `make dylint` — full workspace, once for the phase (P13). Satisfied by the workspace-wide run recorded at Checkpoint 1: Phase 0 is one task and that run included its changes
- [x] Every declared GTS identifier admits under 0.12.0
- [x] Generated-schema diff reviewed and accounted for — one class of change in 9 of 118 documents
- [ ] **Human review before any registry code is written**

---

## Phase 1 — One global entity of each kind, persisted, async, end to end

Exercised by fixtures and REST only. Consumers stay on the in-memory path until T24, so
nothing in this phase can regress another gear.

### - [x] T2: Migration for the 9 tables

**Description:** One initial SeaORM migration creating the P0 subset of `database.sql`:
`version_family`, `entity`, `type_schema_revision`, `instance_revision`, `type_schema`,
`instance`, `dependency`, `operation`, `operation_item`. Tenant columns and their CHECK
constraints are created exactly as specified and never populated with tenant scope, so P1
tenancy needs no migration. `source_claim` and `routing_config` are not created.

**Acceptance criteria:**
- [x] All 9 tables, their PKs, FKs, UNIQUE and CHECK constraints, and the 4 indexes from `database.sql` are created — `idx_tr_operation_status`, `idx_tr_entity_family`, `idx_tr_entity_visibility`, `idx_tr_dependency_to`. Conformance was **measured**, not argued: the Postgres list reproduced `database.sql`'s P0 constraint set 48 for 48 and all three dialects declared the same columns in the same order. That was a one-time measurement — the standing guard behind it was removed after Checkpoint 1
- [x] Identifier columns are `varchar(1024)` with binary collation and ASCII charset where the backend default is multi-byte — `family_key`, `entity.gts_id`, `operation_item.gts_id`: `varchar(1024) COLLATE "C"` on Postgres, `VARCHAR(1024) CHARACTER SET ascii COLLATE ascii_bin` on `MySQL`, `TEXT COLLATE BINARY` on `SQLite` (its default, stated so a later `COLLATE NOCASE` cannot creep in)
- [x] Enumerations stored as smallint with CHECKs enumerating allowed values. Two forms, both present and both tested: an explicit `IN` list for `kind`, `status`, `entity_kind` and `dependency.kind`; and the branch CHECK for `ownership_scope`, `plane` and `lifecycle_status`, where no branch matches a third value — `ck_tr_version_family_owner`, `ck_tr_entity_owner`, `ck_tr_operation_plane` and `ck_tr_entity_lifecycle` already close those domains, so adding an `IN` list would have been a constraint `database.sql` does not have
- [x] `DatabaseCapability::migrations()` returns the Migrator; outbox tables come from `outbox_migrations_with_prefix("types_registry_outbox")`, not from this migration. Both halves are tested: one test asserts the initial migration alone creates **no** outbox table, another applies the gear capability's full set and asserts the 9 managed tables and the prefixed outbox tables all exist
- [x] Raw SQL appears only here (`11_database_patterns.md` invariant) — the three statement lists plus the drop list live in `m20260817_000001_initial.rs`; no other file gained SQL

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
- `TR/Cargo.toml` — `sea-orm`, `sea-orm-migration`, `toolkit-db`, `toolkit` feature
  `preview-outbox`, `integration` feature, `testcontainers` dev-deps
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

**Three of the nine tables have no entity yet, deliberately.** `instance` and
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
- [x] Read primitives for the database read path (SPEC D2, §8.2): a keyed exact read, and a list read that prefilters in SQL on the stored columns and then applies `GtsId::matches_pattern` in Rust. **GTS identifier matching is never translated into SQL** — the prefix range is deliberately *wider* than the pattern (it drops the final segment, because minor-version flexibility would otherwise make the range exclude a real match), and `GtsId::matches_pattern` alone decides
- [x] The list read is a **keyset page**: `gts_id > :after ORDER BY gts_id LIMIT :n`, excluding deleted rows, so a page boundary cannot drift or duplicate (D12). It reports whether more remains — `has_more` may over-report, which is the safe direction — and never loads the whole match set: SQL is asked for bounded batches, each row is tested as it arrives, and the scan stops on the page limit or the scan budget
- [x] A dependency-closure read: given candidate identifiers, return them plus the transitive closure of what they consume, walking `dependency` edges (D5), `gts_id`-sorted. Candidates with no entity row are **reported** in `missing_roots` rather than failing the read, because a first admission's own candidate is exactly that case

**Verification:**
- [x] Gear tests (see [Commands](#commands)) — 161 lib + 149 integration tests, of which 18 are `repo_test.rs` and 7 are `repo_tests.rs`
- [x] Test: concurrent `version_family` creation yields exactly one row — 8 tasks against a file-backed pool; `SQLITE_BUSY` is retried rather than pretended away, and the assertion is on the end state
- [x] Test: CAS with a stale version affects zero rows and is reported as such
- [x] Test: list read with a wildcard pattern returns exactly what `GtsId::matches_pattern` accepts, including a case the SQL prefilter admits but the pattern rejects. **Two fixture identifiers had to be fixed first:** `…v1~x.a.type.v1~` does not parse (a chain segment is a full `vendor.package.namespace.type.vMAJOR`), so `matches_pattern` rejected it and the test had been passing for the wrong reason. The `v1~*` expectation was also wrong — a bare segment is an implicit derived-type envelope (GTS spec §3.6), so `…v1~` and `…v1~*` accept the same set, base included
- [x] Test: keyset paging over a set larger than one page yields every row exactly once, and a row inserted mid-traversal neither duplicates an earlier row nor hides a later one
- [x] Test: closure read over a chain returns the whole chain and nothing outside it — plus termination on a cycle (valid per ADR-0012, so the `seen` set is a correctness requirement)
- [x] `cargo test --workspace` (excluding the two macro crates, as `make test-no-macros` does) — passes, so no regression in any other gear
- [x] PostgreSQL / MySQL repository primitives — **run, and they found two defects.** `tests/repo_backends_test.rs` covers the properties `SQLite` cannot demonstrate: the unique-conflict handling, the keyset cursor's binary collation, and now the same two races **inside a transaction**, which is the only shape production uses. Both container suites pass; see the correction below. Run: `cargo test -p cf-gears-types-registry --features integration --test repo_backends_test`

**Added beyond the acceptance criteria:**
- **Boundedness is tested, not asserted.** 2100 rows inside the prefix range that the pattern rejects, with the single match sorted last: a read that materialised the range would return it on the first page, a bounded scan cannot. Mutation-checked by raising `SCAN_BUDGET`
- **The SQL batch adapts.** With a pattern, 256 rows per round trip so a sparse match set does not cost one trip per match; without one, the page's own remainder, because nothing can be rejected and reading ahead is waste. Still capped at 256 either way — the remainder is caller-supplied, and one round trip's memory must not be
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
- `TR/src/infra/storage/repo_tests.rs` — NEW, 7 in-source tests for the SQL prefilter
- `TR/src/infra/storage/mod.rs` — `pub mod repo`
- `TR/tests/repo_test.rs` — NEW, 17 `SQLite` tests against the migrated schema
- `TR/tests/repo_backends_test.rs` — NEW, 2 container-backed tests behind `integration`
- `TR/tests/common/mod.rs` — `SQLite`/DSN provider harness, `allow_all()` scope
- `TR/tests/entity_test.rs`, `TR/tests/migration_backends_test.rs` — the two T3 clippy fixes
**Scope:** M

---

### - [x] T5: Transient `gts-rust` store built from database rows

**Description:** One function that builds a `GtsStore` from a set of database rows, for use by
a single admission unit and dropped with it (SPEC D2, §8.2; `plan.md` P6). It takes the
candidates plus the transitive closure of what they consume — obtained from the `dependency`
table — reads them `gts_id`-sorted so a derived schema never loads before its base, and
returns an owned store. Nothing is cached, published or shared: **no `ArcSwap`, no snapshot,
no process-lifetime store.** Reads are served from the database, not from here. The old
in-memory repository keeps serving the old trait until T24 and is not touched.

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

**Description:** Extend `TypesRegistryConfig` with `allow_compatibility_force`, `limits.*`
(including P0's `activation_write_set`), `registration_policy` and `worker.*`, keeping the
existing keys. The `local_client.cache.*` keys stay live — the cache is kept (SPEC §8.3) — and
their reshaping into `freshness_window` / `store_bound` belongs to T30, not here.

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] Absent config and `config: {}` both yield the SPEC §10.3 defaults via `ctx.config_or_default()` — asserted as the *same value* rather than each checked against the table, plus a second test pinning every default against §10.3 value for value
- [x] `registration_policy` keys are validated at startup; an invalid GTS pattern fails startup rather than being skipped. `TypesRegistryConfig::validate()` returns the **compiled** `RegistrationPolicy` rather than `()`, so the boot path and the acceptance path consult one compilation instead of two that could disagree
- [x] `tenant_ownable` is **parsed and validated but inert** (SPEC §10.3). `RegistrationPolicy::tenant_ownable` resolves it by the same rules as the vendor set and is asserted to return the configured value — which is what makes it inert rather than dropped — while `admits` refuses any candidate asking for tenant ownership whatever the entry says. §9 makes that request shape unreachable, so it is a fail-closed assertion, not a feature
- [x] Per-parameter resolution implemented: longest literal prefix wins, an exact key beats any pattern, entries omitting a parameter are skipped, closed default otherwise. Specificity is `(is_exact, literal_prefix_len)` — the exact-beats-pattern rule is a separate tuple element rather than left to fall out of prefix lengths, because DESIGN states it separately
- [x] Global `cf` vendor is implicitly admitted; nothing else is. Keyed on the **last** segment's vendor, so a `cf`-rooted identifier with an `acme` derivation is an `acme` candidate — tested, since reading the first segment would open the whole platform namespace to derivations

**Verification:**
- [x] `cargo test -p cf-gears-types-registry` — 186 lib + 173 integration tests, of which 16 are `policy_tests.rs` (in-source, pure) and 14 are `config_test.rs`
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
  `operation_timeout`, T27 for the page-size pair, T13/T19 for `resolution_closure`) — the honest
  shape `tenant_ownable` already had;
- `CLOSURE_BOUND` says it is *its own* bound over what a store build **reads**, not SPEC §8.1
  step 4.6's write set, and its error message no longer borrows the other bound's name;
- `config::inert_limit_keys()` collects the keys a deployment moved off their default that P0 does
  not act on, and `init` names them in one `warn!`. Not a boot failure: a P1-ready configuration
  legitimately carries every one of them. The test over it is exhaustive, so binding a key breaks
  that test — which is the reminder to take it out of the list.

**Dependencies:** T2
**Files touched:**
- `TR/src/config.rs` — `allow_compatibility_force`, `Limits`, `PolicyEntry`, `WorkerSettings`, `ByteSize`, `ConfigError`, `validate()`
- `TR/src/domain/policy.rs` — NEW, `RegistrationPolicy` + `PolicyConfigError` + `PolicyRefusal`
- `TR/src/domain/policy_tests.rs` — NEW, 16 in-source tests
- `TR/src/domain/mod.rs` — declare `policy`
- `TR/src/gear.rs` — startup validation
- `TR/tests/config_test.rs` — NEW, 14 tests
**Scope:** M

---

### - [x] T7: Acceptance path and operation records

**Description:** The synchronous half of admission: envelope and batch bounds, canonical
identifier check, registration policy, identifier profile, Draft-07 dialect gate, `force`
gate, request fingerprint, `Idempotency-Key` resolution, then one transaction inserting the
operation, its items and the outbox message. Reads no entity state.

Outcome and evidence: the criteria below. The per-task report was folded into these and deleted.

**Acceptance criteria:**
- [x] Checks run in SPEC §8.1 order; policy precedes any existence lookup so a refusal cannot probe the namespace. Kept **structurally**, not by review: `validate` takes the request and the config and has no runner, provider or repository in its signature, so mid-validation existence checking would require changing that signature. Steps 1–6 and 8 are here; **step 7 (the ADR-0015 quarantine) is T18's** — it needs T13's reference extractor — and a `TODO(T18)` marks its position between steps 6 and 8 so it lands as an insertion rather than a reordering
- [x] Policy gates **every accepted candidate**, because P0 accepts only declared creations: a positive `expected_resource_version` is refused (`AcceptanceError::RevisionNotAccepted`) and deletion is refused with the envelope. SPEC §8.1's revision/deletion bypass is deliberately *not* implemented yet — gating on the caller's declared kind while nothing verified the claim let a request name a version and skip the gate outright (found at the Checkpoint 1 review). The bypass returns at T11, together with the commit-side precondition that makes the claim checkable. A refusal names the region and the parameter
- [x] Fingerprint covers canonical body, operation kind, owner, preconditions and each `force` flag — plus dry-run mode, plane, tenant and principal, as one table-driven test over all nine inputs. Every field is **length-prefixed**, so a digest cannot confuse `("ab", "c")` with `("a", "bc")`, and the digest carries a version tag so a future change to its coverage cannot read as a matching replay
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
hide a missing recheck. One item is one unit, in `item_no` order; in-batch references and SCC
ordering are T19. `run_operation` takes no config, because nothing in T8 reads a limit.

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
- [x] `POST /entities` returns `202` with operation `Location` and advisory `Retry-After`; `200` only on terminal replay. The `Location` is built from the path the request **arrived on** and is therefore followable under api-gateway's `prefix_path` — the credstore precedent, with one correction to it: `OriginalUri`, not `Uri`, because `apply_prefix` mounts every gear with `Router::nest`, which rewrites away exactly the prefix that has to survive (found at the Checkpoint 1 review). The receipt reports the operation's **real** status — with inline admission a fresh submission is already `completed`, and telling a caller to poll for something that has happened is worse than useless. It comes from `Accepted.status`, which `submit` already knows, rather than from the read-back this used to do: that read cost a second snapshot transaction over two statements and carried a `"pending"` fallback for an operation that had just been committed and so cannot be absent (found at the Checkpoint 1 review)
- [x] `Idempotency-Key` is required; absence is a synchronous refusal. **A toolkit gap:** `OperationBuilder` exposes `path_param` / `query_param` and no header equivalent, so the requirement cannot be declared as an OpenAPI parameter from this gear. The route description states it *and* states that it is undeclared; fixing the builder is toolkit work outside this gear
- [x] Errors are RFC-9457 problem details via `.standard_errors(openapi)`, never raw status tuples — one match arm per `AcceptanceError` variant, exhaustive, so a new refusal reason cannot reach the wire as an opaque `500` by omission
- [x] Routes are `/types-registry/v1/...` and `.authenticated()`; DTOs live only in `api/rest/dto.rs`
- [x] Added at the review gate: the four wire vocabularies — operation status and kind, item status, entity kind, lifecycle status — are `#[api_dto]` **enums**, not `String` fields whose admissible values lived in a docstring. A `String` publishes an unconstrained `string` in the served `OpenAPI`, so a generated client gets no vocabulary and a value this gear never emits type-checks against it; the five `*_str` helpers are gone with it (found at the Checkpoint 1 review)
- [x] `GET /entities/{entity_key}` accepts a GTS identifier or a `gts_uuid` — classified by `EntityKey::parse` in the **domain**, so a future gRPC adapter with the same field gets it for free
- [x] These routes are the **platform-plane** API for global entities: they keep the authentication they have and no handler assumes a tenant scope. `.anonymous()` is **not** used. Ceiling C8 is commented where the routes are registered. **Corrected after the PR's contract check:** the routes were initially `.exposed()`, but `exposed` gates gateway visibility, not `OpenAPI` inclusion — every registered operation lands in `docs/api/api.json` either way — so it bought a published interim surface with no consumer (the database path has none until T24) and cost a stale `api.json`. The v2 routes are now **internal-only** (no `.exposed()`): documented in the spec, invisible to the gateway. T24a restores `.exposed()` when it promotes them onto `V1`
- [x] **Handlers are mapping steps only.** `RegistryService` has three methods taking and returning domain values with no `StatusCode`, `HeaderMap` or `Json`. Two decisions that could have leaked and did not: the identifier-versus-UUID classification, and the `expected_resource_version` vocabulary — the stored `0` means must-not-exist and the wire spells that as an absent field, translated in the service so both adapters see one closed vocabulary. `Idempotency-Key` is read from the header and passed as a parameter

**Verification:**
- [x] `cargo test -p cf-gears-types-registry` — `api_rest_test.rs` via `Router::oneshot`, 10 cases, driving the **real** `register_routes` with a stub `OpenApiRegistry` rather than a hand-built router: a bare router would pass while a route was registered at the wrong path or without its auth stage. 226 lib + 195 integration tests pass
- [x] Manual: `curl` the three routes against `make example`; `/cf/docs` renders them — **done after this task, at Checkpoint 1; the blocker recorded here was the invocation and is retracted.** `oagw` (`api_egress`) is a non-optional dependency of `cf-gears-example-server` while `static-tr-plugin` sits behind the `static-tenants` feature, so a bare `cargo run --bin cf-gears-example-server -- --config config/quickstart.yaml run` builds the tenant resolver with **no plugin at all** and `oagw`'s root-tenant lookup is the first thing to notice it. `make example` passes `E2E_ARGS`, which defaults to `config/e2e-features.txt`, and `static-tenants` is in that set: with it, all 25 gears boot, `/cf/health` answers `200`, the three routes behave, and `/cf/docs` renders them. The `config/e2e-local.yaml` half stands as written — that is the old in-memory path T24 deletes. What the live run found: the `202`'s `Location` omitted api-gateway's `/cf` prefix and so could not be followed verbatim — **fixed at the review gate**, and pinned by a test that nests the real router under `/cf` and follows the receipt it hands back

**Superseded by T9a.** This task repointed the two existing v1 routes at the database path; T9a
restores v1 and moves this surface to `/types-registry/v2/`, for the reasons in `plan.md` P12. The
DTOs, handlers and tests below are the ones T9a re-registers under v2 — nothing here is discarded,
only re-addressed.

**Three interim shapes, each marked in code.** Admission runs **inline** until T21 starts the
outbox worker — not a throwaway path, because seeding does exactly this permanently (SPEC §8.1),
and the dispatch call still happens inside the acceptance transaction through `NullDispatch`, so
T21 changes one implementation rather than the transaction's shape. The scope is `allow_all`
(ceiling C6). Ceiling C8 is commented at the routes.

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
- `TR/src/domain/admission/mod.rs` — `NullDispatch`
- `TR/src/infra/storage/repo.rs` — `EntityRepo::find_by_gts_uuid`, `TypeSchemaRepo::find_current`
- `TR/src/gear.rs` — wire the database-backed service when a database is bound
- `TR/Cargo.toml` — `tower` dev-dependency
- `TR/tests/api_rest_test.rs` — NEW, 10 route tests
- `TR/tests/registration_tests.rs`, `TR/tests/query_tests.rs` — tests of the deleted handlers removed
- `config/quickstart.yaml`, `config/e2e-local.yaml` — bind a database to types-registry
**Scope:** M

---

### - [x] T9a: Restore the v1 REST contract; the async surface moves to `/types-registry/v2/`

**Description:** T9 **repointed** the two existing v1 routes instead of adding new ones, which
breaks the invariant this plan set for itself (risk table: *"The DB path has no consumer until
T24; no dual-write"*). Three consequences, all observable today:

1. `POST /v1/entities` changed its **body shape** and gained a required `Idempotency-Key`.
   Every existing caller gets `400`/`422`, so this is not the "expects `201`, gets `202`"
   break T28 was scoped for.
2. **Cross-gear registration over REST is functionally broken.**
   `testing/e2e/gears/oagw/helpers.py:83` and
   `testing/e2e/gears/account_management/conftest.py:182` register over REST and then resolve
   through `TypesRegistryClient` (`account-management/src/infra/types_registry/`,
   `usage-collector/src/domain/service.rs`). Those gears now **write to the database and read
   from process memory**. No e2e change fixes this — only T24 does.
3. One resource, two sources of truth: `GET /v1/entities` (list) answers from memory,
   `GET /v1/entities/{entity_key}` from the database.

Restore both v1 routes from `main` verbatim and register T9's surface under
`/types-registry/v2/`. Everything v1 needs is still in the crate —
`TypesRegistryService::{register_validated, get, is_ready}` (`domain/service.rs:66,108,144`),
`InMemoryGtsRepository`, `GtsEntityDto` — only two handlers and four DTOs were deleted, and they
come back unchanged. v2 is **interim by design**: T24 deletes v1 with the repository it reads,
and T24a promotes v2 onto the v1 paths, so P0 ends on one version.

**Acceptance criteria:**
- [x] `POST /types-registry/v1/entities` is contract-identical to `main`: `operation_id` `types_registry.register`, body `{"entities":[…]}`, `200`, `RegisterEntitiesResponse` with its summary, served from the in-memory service behind `is_ready()`
- [x] `GET /types-registry/v1/entities/{gts_id}` restored: `operation_id` `types_registry.get`, in-memory, unchanged DTO
- [x] The four deleted DTOs — `RegisterEntitiesRequest`, `RegisterEntitiesResponse`, `RegisterSummaryDto`, `RegisterResultDto` — are **restored from `main`**, not re-derived: a re-derived DTO is a second wire change nobody asked for. Lifted out of `git show main:` rather than retyped, so the wire shape cannot drift by a field name
- [x] `GET /types-registry/v1/entities` (list) is untouched; it never moved
- [x] The async surface is `POST /v2/entities`, `GET /v2/operations/{operation_id}`, `GET /v2/entities/{entity_key}`, keeping T9's `operation_id`s — `submit_entities`, `get_operation`, `get_entity`, distinct from v1's `register`, `list`, `get`
- [x] **No route straddles the two stores:** no v1 handler takes `RegistryService`, no v2 handler takes `TypesRegistryService`. Grep-checked — three handlers take `Extension<Arc<TypesRegistryService>>` (`register_entities`, `list_entities`, `get_entity`) and three take `Extension<Option<Arc<RegistryService>>>` (`submit_entities`, `get_operation`, `get_entity_by_key`); no handler takes both
- [x] **No dual-write and no fallback read.** Pinned by `a_v1_registration_is_absent_from_v2_and_the_reverse`: a v1 registration reads `200` on v1 and `404` on v2, a v2 admission `200` on v2 and `404` on v1. The two directions run against the *same* routes, so the `404` cannot be a missing route rather than an honest miss — the test would fail on the companion `200` first
- [x] `RegistryService` stays per-database-optional as T9 left it: with no database bound, v2 answers `503` and v1 works. Both halves are tested — `without_a_database_the_routes_report_service_unavailable` for v2, and `without_a_database_the_v1_routes_still_serve` for v1's register, get and list
- [x] Route paths come from one constant per version, so T24a's promotion is a constant change rather than a sweep. `routes::V1` / `routes::V2` are `pub` and the test imports **the same two constants** the routes are built from — not its own copies, which would drift at exactly the moment the promotion happens
- [x] The tests T9 deleted from `registration_tests.rs` and `query_tests.rs` return with the routes they cover — three and two respectively, and both files now `diff` clean against `main`
- [x] SPEC §10.2 is amended: the v1 break is **withdrawn** for the T9a–T24 window and reinstated at T24a, naming both tasks. The route table there is labelled as the post-T24a surface, so it is not read as describing today

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

**Moved into Phase 1** from Phase 2. Two reasons. Instances are what the platform actually
pushes today — P4 counts *"roughly eleven plugin gears"* already registering their well-known
Instances from their own `init()` — so they are on the critical path to T24 and are the longest
pole, not a widening. And T9's surface accepts an Instance and then fails it in the **worker**
(`StoreBuildError::UnsupportedKind` → `WorkerError::StoreBuild` → opaque `500`), which is a
retryable class for a decision that is final; building the feature closes that hole instead of
adding a refusal for it.

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
   with no edge table involved. T13 keeps what is genuinely edge-derived: `$ref` and `x-gts-ref`
   targets, and T14's reverse walk.
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
- [x] An Instance whose conforming schema is absent fails retryably, not terminally — `WorkerError::ConformingTypeAbsent`, checked **before** validation so the failure names the real cause rather than reporting a missing schema as a content fault
- [x] A minor or major 0 in the Instance identifier's last segment is refused at acceptance — **already built and tested in T7**; `an_instance_identifier_must_name_a_stable_major_without_a_minor` covers both halves plus the Type Schema contrast, so this task adds nothing
- [x] `instance` carries only the current-revision pointer — no derived artifact. The asymmetry with `type_schema` is documented on the entity so it is not "fixed" later: an Instance has no derived state, its value is authored and its schema revision is immutable and pinned by `ON DELETE RESTRICT`, so there is nothing that could change without a new revision and nothing to fingerprint
- [x] `entity_kind` is derived from the identifier, not passed in. Stronger than removing the literal: `EvaluatedOutcome` is an **enum** whose variant carries the kind-specific payload (artifacts for a schema, the revision pair for an Instance), and `entity_kind()` reads it off the variant. Two `Option` fields beside a `kind` discriminant would have made a mismatch representable; here it is a compile error
- [x] The transient store carries both kinds: both `UnsupportedKind` refusals are deleted and the dialect gate is keyed on the identifier's `~`. `type_id` is passed to `GtsEntity::new` **explicitly** from `GtsId::get_type_id()` with a `None` config — letting content name the conforming type would allow a value to claim conformance to a type it was not registered under, which is a validation bypass, not a convenience
- [x] The closure reaches a candidate's derivation chain with **no `dependency` row present** — `closure` seeds its worklist with `GtsId::chain_ids()`, and `missing_roots` is still computed over the original roots so a chain member the seed added is not reported as one. The Phase 1 test is inverted, and its old comment was half wrong: it blamed T13's missing edges, but a derivation base is a pure function of the identifier
- [x] A family holds one kind — `family_kind_conflict`, checked under the family lock and only for a family that already existed. Distinct from `already_exists` because the identifier *is* free; saying otherwise sends a caller looking for a conflicting entity that does not exist. Both orders are tested
- [x] No `$ref`/`x-gts-ref` target is expected to resolve yet — `a_ref_outside_the_chain_still_fails` pins it, so T10's chain seed cannot be mistaken for T13 having landed early
- [x] **An admitted Instance reads back with its value.** *Added after the task was marked done — see the correction below.* `GET /v2/entities/{entity_key}` returns the authored value and its `revision_no` for an Instance as it does the document for a Type Schema. The three `effective_*` artifacts stay absent, and that is the contract rather than the same omission: an Instance has no derived state, so there is nothing to materialize

**The read path was missed, and the task's criteria are why.** Every criterion above is about
admission — the write path, the transient store and the family rule — and none names the public
read. `RegistryService::entity()` was built at T9, when only Type Schemas existed, and kept asking
`TypeSchemaStore::{find_current_schema, current_documents}` for **both** kinds. An Instance has no
row in either table, so an admitted Instance answered `200` with `content: null` and
`revision_no: null` while its operation reported `succeeded` — the value was durable, correct, and
unreachable through the API. `InstanceStore::current_values` already existed and was already
correct; it had one caller, `gts_store.rs`, which is the **transient validation** store and not a
read path. `api_rest_test.rs` contained no Instance case at all, which is why 454 green tests said
nothing about it.

The read now branches on the row's kind into a `CurrentState` enum — the same shape
`EvaluatedOutcome` has on the write side, so "an Instance carrying a resolved schema" is not a
representable value rather than a mismatch a later edit could introduce. One statement, not two:
`current_values` returns the pointer's revision number with the value it points at.

**Consequence for later tasks, since it would have surfaced there as a regression.** T23's
`get_instance` and T27's `batchGet` both sit on this method; the SDK helpers that hydrate a
content-free page through `batchGet` would have returned pages of null values.

**Verification:**
- [x] Gear tests, all three backends — 454 on `SQLite`, and `make test-types-registry-db` green: 4 container tests, migration and repository primitives on both `PostgreSQL` and `MySQL`
- [x] Test: Instance value violating its schema is refused — `invalid_value`, distinct from `invalid_schema`: the schema is fine, the value is not
- [x] Test: `fk_tr_instance_revision_schema` prevents dangling schema-revision references — `instance_revision_cannot_reference_a_missing_schema_revision`. Foreign keys are enabled explicitly on that connection, because `SQLite` does not enforce them by default and the test would otherwise pass while proving nothing
- [x] Test: an Instance admits against a Type Schema committed by an **earlier** operation, with the `dependency` table asserted empty
- [x] Test: a derived Type Schema admits against a committed base, with the `dependency` table empty
- [x] Test: Type Schema then Instance under one `family_key` — second is refused; and the reverse order
- [x] Test: an admitted Instance reads back over the **real routes** with its authored value and `revision_no`, and with the three `effective_*` artifacts asserted absent — `api_rest_test.rs::an_instance_reads_back_with_its_authored_value`, which polls the operation first so a refused candidate cannot be mistaken for a value the read failed to reach. Plus the same Instance by its Registry Reference, pinning that the kind branch is chosen by the row and not by how it was found. Both are genuine RED→GREEN: they failed on `content: null` before the fix. Re-run on all three backends — 457 on `SQLite`, `make test-types-registry-db` green

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
- [x] Durable registration state survives closing and reopening the database pool byte-identically — `TR/tests/restart_persistence_test.rs`, two tests. One admits both a Type Schema and an Instance, drops the service/provider/pool, reopens the SQLite file, re-runs test migrations, and compares whole `Model` values in stable primary-key order across all eight affected tables; it then proves both entities are readable through a fresh service and that the persisted idempotency record replays without a write. The other pins the pre-T21 crash-window recovery of a committed non-terminal operation. This is deliberately not called a process-restart test: real `TypesRegistryGear::init`, startup seeding and a new process remain T30's e2e/manual obligation
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
- [x] **Review follow-up: unsupported dry-run fails synchronously, and replays are explicit.** Until T20 supplies a rollback-only evaluation transaction, `dry_run: true` is rejected during admission with a canonical `400` field violation, before an operation can be created or stranded in `running`. Successful idempotency replays now include `Idempotency-Replayed: true`; first submissions omit it. Domain and REST regression tests pin both contracts
- [ ] **Human review — everything after this widens the path rather than reshaping it.** Five open items, none of them a failing check:
  - **Another gear owns part of `/types-registry/v1/*`.** `resource-group` registers five routes — `POST|GET /types`, `GET|PUT|DELETE /types/{code}` — inside this gear's service namespace, from `gears/system/resource-group/.../api/rest/routes/types.rs`. T27 and T28 widen that namespace, so a collision waits for whichever gear registers a conflicting path first. Decide: report to the resource-group owners now, or carry it as a known hazard into T27/T28
  - ~~**`Idempotency-Key` cannot be declared in OpenAPI.**~~ **Retracted — the claim was false and is now fixed.** `ParamLocation::Header` exists and `openapi_registry.rs:200` already maps it onto utoipa's `ParameterIn::Header`; the generic `OperationBuilder::param(ParamSpec)` declares it. What misled us is that there is no `header_param` convenience beside `path_param` / `query_param`, so the capability is discoverable only by reading the enum. `POST /v2/entities` now declares the header as a required parameter, pinned by `the_idempotency_key_header_is_declared_as_a_required_parameter` and mutation-checked. The remaining toolkit gap is the missing convenience method — filed upstream as constructorfabric/gears-rust#4614
  - **T2's three lowering decisions were flagged *worth review* and never signed off:** MySQL `DATETIME(6)` rather than `TIMESTAMP(6)`; three extra boolean-domain CHECKs on SQLite and MySQL; MySQL's four indexes declared inline as `KEY`
  - **The migration changed after T2 was marked done** (the uuid binding). Nothing to migrate forward — no deployment had run it — but the "done" marker moved
  - **T10's marker moved too, and the reason generalizes.** The public read returned `content: null` for an admitted Instance; fixed, with the correction written into T10. What is worth deciding rather than just noting: T10's criteria covered admission exhaustively and never named the read, and the same asymmetry stands wherever a task extends the write path — T11, T13 and T20 each add state that `RegistryService::entity()` must then be able to return. Consider a standing criterion for those three: *whatever this task makes storable is readable through the public surface, with a test on the route*
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
and not later; moving three files at T12 is cheaper than moving seven at T29, and leaving them
flat is how the root reaches 25 files.

| Concept | Directory at | Contents then |
|---|---|---|
| `family/` | **T10 + T12** | `key.rs` (today's `family_key`), `rules.rs` (kind at T10, shape and contiguity at T12), tests |
| `dependency/` | **T13 + T14** | `extraction.rs` (the four edge kinds), `worklist.rs` (reverse impact), tests |
| `compat/` | **T17 + T18** | `baseline.rs` (selection + resolved comparison), `derivation.rs` (chain + major-0 quarantine), tests |

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

### - [ ] T11: Content revisions and compare-and-swap

**Description:** Second and later revisions of a logical entity: `expected_resource_version`
preconditions, immutable revision insert, current-state pointer move, and the `unchanged`
outcome for authored content equal to current.

**Acceptance criteria:**
- [ ] Acceptance stops refusing a positive `expected_resource_version` (`AcceptanceError::RevisionNotAccepted`, added at Checkpoint 1) **and** the SPEC §8.1 step 3 bypass is restored in the same change: the policy gate becomes conditional on `expected == 0` again only once the commit transaction enforces the precondition. Gating on the caller's *declared* kind while nothing verifies it is the bypass the refusal replaced — found at the Checkpoint 1 review
- [ ] Update requires `entity.resource_version == expected_resource_version`; mismatch is terminal `precondition_failed` with no silent rebase. It goes through the existing `EntityRepo::compare_and_swap_version` — written and unit-tested at T4 with no caller until now, so this task is where its behaviour is validated against a real precondition
- [ ] Equal authored content yields `unchanged`, creating no revision and not advancing `resource_version`
- [ ] `unchanged` is impossible for a create or a delete, enforced in code as well as by the CHECK
- [ ] Content hash is a prefilter only; effective artifacts are excluded from equality

**Verification:**
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Tests: stale version, equal content, content equal to an *older* non-current revision (must create a new revision, ADR-0005)
- [ ] Test: revision numbers are contiguous per entity
- [ ] Test: a revision in a region the policy has since **closed** is admitted (the restored bypass), while a creation there is still refused — the pair that Checkpoint 1's refusal stands in for

**Dependencies:** Checkpoint 1
**Files likely touched:** `TR/src/domain/admission/unit.rs`, `TR/src/infra/storage/repo/`, `TR/src/domain/ports.rs`, `TR/src/infra/storage/store.rs`, `TR/tests/revision_test.rs`
**Scope:** M

---

### - [ ] T12: Version-family shape and contiguity rules

**Description:** The remaining non-stored rules enforced under the family lock: minor shape must
be uniform within a major, and minors must be contiguous from `M.0`. Both are keyed lookups, not
scans.

**The kind rule landed with T10** (P13): a Type Schema `…ns.thing.v1~` and an Instance
`…ns.thing.v1` derive the same `family_key`, so the first Instance could not be admitted without
it. This task takes over the file it opened — `rules.rs` — rather than creating it.

**Acceptance criteria:**
- [ ] `vM.n~` refused while `vM~` exists; `vM~` refused while `vM.0~` exists
- [ ] `vM.n~` with `n > 0` refused unless `vM.(n-1)~` exists
- [ ] A `DELETED` predecessor still counts; the predecessor test is re-asked inside the commit transaction
- [ ] Family ownership is write-once; the entity's owner columns are a projection maintained under the lock
- [ ] The predecessor is excluded from `dependency` and from the revision vector

**Verification:**
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Table-driven test over shape and contiguity combinations
- [ ] Test: concurrent first registration under two owners yields one winner
- [ ] Test: family key derivation maps `v1~`, `v1.4~`, `v2~` to one row, and a preceding-segment minor survives verbatim

**Dependencies:** T10, T11
**Files likely touched:** `TR/src/domain/family.rs` — **this is where it becomes `TR/src/domain/family/`**: `key.rs` for today's `family_key`, `rules.rs` extended with the two rules below beside T10's kind rule, per the trigger table above. Also `TR/src/domain/admission/unit.rs`, `TR/src/infra/storage/repo/`, `TR/src/domain/ports.rs`, `TR/src/infra/storage/store.rs`, `TR/tests/family_test.rs`
**Scope:** M

---

### Checkpoint 2
- [ ] Revisions and CAS behave; family shape and contiguity hold under concurrency (the kind rule landed with T10)
- [ ] Gear tests on three backends (see [Commands](#commands))
- [ ] `make dylint` — full workspace, once for the phase (P13)
- [ ] Human review

---

## Phase 3 — Dependencies and materialization

### - [ ] T13: Dependency edge extraction and writes

**Description:** Extract the four edge kinds from authored content — `$ref`, `x-gts-ref`
target, immediate derivation base, Instance conformance — and replace the admitted entity's
outgoing rows on each admission.

**Unchanged by T10's move** (P13). T10 made the *forward* closure reach a candidate's
derivation chain from the identifier, so derived schemas and Instances no longer wait on this
task. What still needs rows here: `$ref` and `x-gts-ref` targets, which are not derivable from
any identifier, and derivation/conformance, which stay materialized because T14's **reverse**
walk has no identifier to walk backwards from — the criterion below already says so. Both endpoints are always managed entities. This is also
the same extractor the worker uses for in-batch ordering.

**Acceptance criteria:**
- [ ] `x-gts-ref` edge targets the exact identifier, or the pattern's longest valid identifier prefix; a pattern naming nothing valid (`gts.*`) and a GTS §9.6 relative pointer create no edge
- [ ] Admission replaces only the admitted entity's outgoing rows, through the existing `DependencyRepo::replace_outgoing` — written and unit-tested at T4, with no caller until this task
- [ ] Derivation and conformance are materialized even though derivable from the identifier
- [ ] Extraction uses `gts-rust`'s extractor, never a local scan
- [ ] Extraction is exposed as a pure function over authored content, callable without a database — required for unit testing without a fixture DB

**Verification:**
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Table-driven test over edge-kind fixtures, including the no-edge cases
- [ ] Test: re-admission removes an edge the new revision dropped

**Dependencies:** Checkpoint 2
**Files likely touched:** `TR/src/domain/dependency.rs` (extraction; T14 adds the worklist and the pair takes `TR/src/domain/dependency/` — trigger table above), `TR/src/infra/storage/repo/`, `TR/src/domain/ports.rs`, `TR/src/infra/storage/store.rs`, `TR/src/infra/storage/entity/dependency.rs`, `TR/tests/dependency_test.rs`
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
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Test: revising a base with N dependents refreshes exactly N `type_schema` rows
- [ ] Test: cyclic dependency graph terminates
- [ ] Test: over-bound case commits nothing

**Dependencies:** T13
**Files likely touched:** `TR/src/domain/dependency.rs` — **second file, so take `TR/src/domain/dependency/`**: `extraction.rs` from T13 plus `worklist.rs` here. Also `TR/src/infra/storage/repo/`, `TR/src/domain/ports.rs`, `TR/src/infra/storage/store.rs`, `TR/src/domain/admission/unit.rs`, `TR/tests/dependency_repo_test.rs`
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
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Test: a dependency mutated between evaluation and commit causes exactly one rollback and one successful retry
- [ ] Test: a phantom dependent created after the initial scan is detected
- [ ] Test: two pods against one database — a commit on one is visible to the other's first post-commit read

**Dependencies:** T14
**Files likely touched:** `TR/src/domain/admission/unit.rs`, `TR/src/domain/admission/vector.rs`, `TR/src/infra/storage/repo/`, `TR/src/domain/ports.rs`, `TR/src/infra/storage/store.rs`, `TR/tests/concurrency_test.rs`
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
- [ ] `make dylint` — full workspace, once for the phase (P13)
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
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Compatibility matrix: optional property added at a `Closed` level (compatible), at `Open` (incompatible), at `Partial` (`Unknown`)
- [ ] Test: provenance columns match `GTS_SPECIFICATION_VERSION` and the crate version
- [ ] Test: `force` refused when `allow_compatibility_force` is off, including on Dry Run

**Dependencies:** Checkpoint 3
**Files likely touched:** `TR/src/domain/compat.rs` (baseline selection; T18's derivation chain joins it and the pair takes `TR/src/domain/compat/` — trigger table above), `TR/src/domain/admission/unit.rs`, `TR/src/domain/error.rs`, `TR/tests/compat_test.rs`
**Scope:** M

---

### - [ ] T18: Derivation chain and major-0 quarantine

**Description:** Identifier-derived chain validation against every managed base, the
Draft-07 dialect pin across a major, and the ADR-0015 quarantine: a stable candidate may not
reference a major-0 identifier, and a major-0 schema may not carry a registered Instance.

**No preflight scan.** ADR-0015 and DESIGN were simplified to drop it (O4): the rule's base case
comes from the release boundary — T2's migration creates the storage in the same release this task
introduces the check — so there is no pre-existing edge to scan for. The obligation that survives
is negative: do not enable the rule against a database populated by a build that had the storage
but not the check. A dev database can be exactly that between T10 and T18; delete it rather than
reasoning about it.

**Acceptance criteria:**
- [ ] Chain bases are reconstructed with `chain_ids()`, not stored or re-derived locally
- [ ] A stable candidate whose immediate base, `$ref` or `x-gts-ref` targets include a major-0 identifier is refused
- [ ] A registered Instance conforming to a major-0 schema is refused, even though the marker is in a preceding segment
- [ ] Dialect is pinned at initial admission and cannot change across revisions of a major

**Verification:**
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Tests: each quarantine path — a stable candidate deriving from a v0 base, one `$ref`-ing a v0 target, one whose `x-gts-ref` names a v0 entity exactly and one where it is the pattern's longest valid prefix; plus the two documented non-cases, `gts.*` and a relative JSON pointer, which name no entity and must be admitted
- [ ] Test: dialect change across revisions is refused

**Dependencies:** T17
**Files likely touched:** `TR/src/domain/derivation.rs` — **second file for the concept, so take `TR/src/domain/compat/`**: `baseline.rs` from T17 plus `derivation.rs` here. Also `TR/src/domain/admission/acceptance.rs`, `TR/tests/quarantine_test.rs`
**Scope:** M

---

### Checkpoint 4
- [ ] Compatibility matrix passes including the `Unknown` tier
- [ ] Provenance persisted on every revision
- [ ] Quarantine and dialect rules hold, including the two `x-gts-ref` non-cases (no preflight — O4)
- [ ] `make dylint` — full workspace, once for the phase (P13)
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
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Tests: partial commit, blocked dependent, blocked predecessor, atomic cycle
- [ ] Test: batch over `limits.batch_candidates` refused synchronously

**Dependencies:** Checkpoint 4
**Files likely touched:** `TR/src/domain/admission/graph.rs`, `TR/src/domain/admission/worker.rs`, `TR/tests/partial_admission_test.rs`
**Scope:** M

---

### - [ ] T20: Deletion and Dry Run

**`EntityRepo::mark_deleted` already exists**, written and unit-tested at T4 with no caller; this is the task that gives it one.

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
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Tests: blocked deletion, transitive non-blocking, tombstone readability, dry run for both kinds
- [ ] Test: reusing one key for dry run then commit is a fingerprint mismatch, not a replay

**Dependencies:** T19
**Files likely touched:** `TR/src/domain/admission/deletion.rs`, `TR/src/domain/admission/worker.rs`, `TR/src/api/rest/routes.rs`, `TR/tests/deletion_test.rs`
**Scope:** M

---

### Checkpoint 5
- [ ] Partial admission, atomic cycles, deletion safety and Dry Run all behave
- [ ] Gear tests (see [Commands](#commands))
- [ ] `make dylint` — full workspace, once for the phase (P13)
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
- [ ] The gear gains the `stateful` capability — SPEC §5 puts it at `[system, db, rest, stateful]` and T2 deferred the fourth to this task
- [ ] **Worker is started at the end of types-registry's `init()`**, not in the stateful `start` (plan decision P3), wired to `ctx.cancellation_token()`; the `OutboxHandle` is retained and `stop()`ed on shutdown
- [ ] Started **after** inline seeding, so seed operations — which are never enqueued — cannot be leased concurrently
- [ ] An operation submitted from any consumer's `init()` is admitted without that consumer waiting for the `start` phase

**Verification:**
- [ ] Gear tests, all three backends (see [Commands](#commands))
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
- [ ] `EntitySnapshot` exposes the materialized documents as **plain fields** (`content`, `resolved_schema`, `effective_traits`, `effective_traits_schema`) plus a `segments` accessor, so the ~40 call sites using the old models' computed methods become field reads rather than rewrites
- [ ] Documents are selectable **individually**, not as an `effective` group: a caller wanting `effective_traits` must not be made to transfer the 1 MB-bounded `resolved_schema` with it (DESIGN §3.3, *Field selection*). `provenance` is the one group that survives
- [ ] **No `effective_*` recomputation exists in the SDK** — the old `GtsTypeSchema::effective_schema` / `effective_properties` / `effective_required` / `effective_traits` / `effective_traits_schema` are not reproduced. They resolved only the parent `$ref` and left non-parent references unresolved, and `effective_traits` was an admitted approximation (`TODO(#1723)`), so reproducing them would reintroduce both a wrong answer and a `constraint-gts-implementation` violation (SPEC §10.1)
- [ ] `EntityQuery` carries `limit` and `cursor`, and `EntityPage` carries the next cursor — the trait already declared `EntityPage` in SPEC §10.1, and without these it is a page in name only (D12)
- [ ] `list_instances` / `list_type_schemas` **hydrate a content-free page through `batchGet`**, so the ~87 existing call sites keep reading payloads from the result. The doc comment states the trade: complete with respect to the traversal, not to an instant, and one extra round trip per page which the client cache absorbs
- [ ] **The validator field is in the models from this task**, and `BatchGet` accepts a validator per requested key in `BatchGetItem::if_none_match`, even though T29 computes them and T30 consumes them. Adding either later would break the SDK contract after ~50 call sites have moved onto it (SPEC §8.5, `plan.md` P9). A result variant for `unchanged` is part of the same shape
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
- [ ] `make dylint` — full workspace, once for the phase (P13)
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
- [ ] The v1 REST routes T9a restored are deleted **together with** the repository they read —
      `POST /v1/entities` (`types_registry.register`), `GET /v1/entities/{gts_id}`
      (`types_registry.get`) and the in-memory `GET /v1/entities` list. A route left pointing at a
      deleted repository is the failure mode; T24a then promotes v2 onto those paths
- [ ] Ready mode and the in-memory repository are gone; `ready_mode_tests.rs` deleted. The old model-typed cache goes with the old models, and the four `local_client.cache.{type_schemas,instances}.{capacity,ttl}` keys become accepted-and-ignored with a warning naming their T30 replacements
- [ ] `owning_gear` comes from T22's inventory field, not a constant — ceiling C3 is struck from SPEC §9 in this task
- [ ] No entity-derived state survives `init()` — no `ArcSwap`, no entity map, no `GtsOps` field on the gear or the service. Grep-checkable, and the ceilings C1/C4 struck by D2 depend on it

**Verification:**
- [ ] Gear tests, all three backends (see [Commands](#commands))
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

### - [ ] T24a: Retire v1; promote v2 → v1

**Description:** T24 deletes the in-memory repository, so the v1 routes T9a restored lose the
store they read from and go with it — v1 cannot outlive T24, and repointing it at the database
would be a compatibility shim with no consumer. This task is the other half: every v2 route moves
onto the `/types-registry/v1/` paths, so P0 ends on **one** version rather than a permanent v2.

**Placement.** After T27, not before it: T27 authors the remaining routes (`:batchGet`,
`:batchDelete`, `DELETE /entities/{entity_key}`, the paged content-free `GET /entities`), and authoring them on v2 and then renaming is
one move, while renaming first and authoring second means T27 lands on paths that changed under
it. T28 then migrates e2e onto the final contract exactly once. Recommended Phase 7 order is
therefore **T24 → T27 → T24a → T28**, with T25/T26 floating (they depend on T24 alone).

**The e2e window is a consequence of this ordering, not of a defect.** `make e2e-local` goes red
at T24 — where the wire genuinely breaks and where it could not break earlier — and green again
at T28. T25, T26 and T27 sit inside it and are gated by `cargo test --workspace`, `make quickstart`
and `make example` instead. Before T24 the suite stays green, which is what T9a bought.

**Acceptance criteria:**
- [ ] No `/v2/` path remains in the crate, in OpenAPI or in `QUICKSTART.md`
- [ ] `operation_id`s are unchanged by the move — `types_registry.submit_entities`, `.get_operation`, `.get_entity` and T27's additions keep their names, so the promotion is a path change and nothing else. A client that already followed v2 sees only the prefix move
- [ ] Old v1 handlers, DTOs and routes are **deleted**, not repointed — verified by T24's own criterion that the in-memory repository is gone; `grep -r 'types_registry\.register\|RegisterEntitiesRequest'` finds nothing outside history
- [ ] Every surviving v1 route reads the database; none reads process memory
- [ ] SPEC §10.2 records the final shape and closes the interim window, naming T9a as where it opened and this task as where it closed
- [ ] Changelog: the v1 `POST` break (body shape, `202`, submit-then-poll) and the `GET /entities` shape change are **one release, two entries**, as T27 already requires
- [ ] `api_rest_test.rs` needs only its per-version path constant changed — if it needs more, T9a's last criterion was not met and that is the finding, not this task's scope

**Verification:**
- [ ] `cargo test -p cf-gears-types-registry`
- [ ] `make lychee`
- [ ] Manual: `/cf/docs` renders every operation under v1 and no v2 path resolves
- [ ] `make e2e-local` is **expected red** until T28 and green after it; the red set must be exactly the `/entities` call sites T28 owns, and any other failure is a regression this task introduced

**Dependencies:** T27
**Files likely touched:**
- `TR/src/api/rest/routes.rs`, `TR/src/api/rest/handlers.rs`, `TR/src/api/rest/dto.rs`
- `TR/tests/api_rest_test.rs`
- `gears/system/types-registry/QUICKSTART.md`, `CHANGELOG.md`
- `docs/p0/SPEC.md` §10.2
**Scope:** S

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
- [ ] `make example` — boots with every gear's types present

**Dependencies:** T25
**Files likely touched:** the domain-gear call sites above, `TR-SDK/src/api.rs` (deleted), `TR-SDK/src/models.rs`, `TR-SDK/src/testing.rs`
**Scope:** L — split per gear, same rule as T25

---
### - [ ] T27: REST completion, OpenAPI, QUICKSTART

**Description:** The remaining routes — `POST /entities:batchGet`, `POST /entities:batchDelete`,
`DELETE /entities/{entity_key}`, `GET /entities` — plus OpenAPI completeness, the changelog entry for
the `POST /entities` break, and `QUICKSTART.md`.

**Acceptance criteria:**
- [ ] `batchGet` returns one explicit result per requested key, including absence; duplicate keys collapse
- [ ] Every batch item names its entity in one `key` field, classified by the same `EntityKey::parse` the path segment uses; the three batch bodies all name their array `items` (DESIGN §3.3, *Naming a single entity in a batch*)
- [ ] Test: a syntactically impossible identifier answers **identically** through `:batchGet` and through `GET /entities/{entity_key}` — one classifier, so the two surfaces cannot disagree about one string
- [ ] `:batchGet` results echo the `key` they were asked by; `:batchDelete` answers with an operation whose items are keyed by `gts_id`, so a caller that deleted by UUID matches by preserved request order
- [ ] `:batchDelete` items carry a `key` plus a **required positive** `expected_resource_version`; absence is a `400`, not "delete if present"
- [ ] `DELETE /entities/{entity_key}` is a one-item `:batchDelete` over the same domain path — no second deletion model, no handler-local precondition logic. It resolves `{entity_key}` as the `GET` does and requires `Idempotency-Key`
- [ ] Its precondition is a required positive `expected_resource_version` **query parameter**; an `If-Match` header is refused, not ignored. Absent, non-numeric or `0` is a synchronous `400`; a mismatched version is `202` and then `precondition_failed` on the operation item, never `412` (DESIGN §3.3, `DELETE /entities/{entity_key}`)
- [ ] Test: the same mismatched version through `DELETE /entities/{entity_key}` and through a one-item `:batchDelete` yields the identical item outcome — the assertion that keeps the two spellings one model
- [ ] An `If-None-Match` **header** on `:batchGet` is refused, not ignored: validators are per item in `if_none_match`
- [ ] `GET /entities` excludes deleted entities and sorts by canonical identifier
- [ ] `GET /entities` returns **one bounded page and a cursor** (D12): `limit` defaults to `limits.page_size_default` (100) and a request above `limits.page_size_max` (1000) is **refused, not clamped**; cursors come from `toolkit-odata` and an unknown cursor version is rejected rather than reinterpreted
- [ ] The page is **content-free**: the default field set is identity and metadata; all four documents (`content`, `resolved_schema`, `effective_traits`, `effective_traits_schema`) are absent, and a page carries no validator (§8.5)
- [ ] One default set **per surface**: the page is content-free, while `GET /entities/{entity_key}` and `batchGet` return the full representation with D3's artifacts
- [ ] A request carrying **`$select` is refused** with an RFC-9457 problem naming the parameter — never answered with the default representation (§10.2). Accept-and-ignore is wrong here: the caller would get up to 1MB it did not ask for and would build on behaviour P1 changes
- [ ] Changelog records the `GET /entities` shape change beside the `POST /entities` one — two breaks, same release
- [ ] `EntityRepo::list_page` gets its first real consumer here: its scan budget (`SCAN_BUDGET`, `SCAN_BATCH`) and prefix-range logic (`prefilter_prefix`, `range_upper_bound`) are the most intricate in the layer and have only ever been unit-tested — a test must exercise the budget boundary and a prefix range **through the route**
- [ ] Both read routes go through T4's database read primitives; pattern filtering is `GtsId::matches_pattern` in Rust over prefiltered rows, never SQL that reimplements identifier matching
- [ ] All **seven** routes appear in the OpenAPI document with RFC-9457 error responses registered — the four reads (`GET /entities/{entity_key}`, `GET /entities`, `:batchGet`, `GET /operations/{operation_id}`) and the three mutations (`POST /entities`, `:batchDelete`, `DELETE /entities/{entity_key}`)
- [ ] `QUICKSTART.md` exists per `02_gear_layout_and_sdk_pattern.md` — description, features, link to `/docs`, one or two working `curl` examples
- [ ] OpenAPI and `QUICKSTART.md` describe this as the platform-plane API for global entities, and say plainly that platform identity (`X-ToolKit-Internal-Token` / `PlatformIdentity`) and a separate platform listener are not yet enforced (C8) — a reader must not conclude the plane is authenticated as such
- [ ] Changelog records the `POST /entities` break
- [ ] No handler added in this task carries logic the domain service does not already expose — the REST surface stays a mapping layer, so a later gRPC surface cannot diverge from it (SPEC §8.4)

**Verification:**
- [ ] `make e2e-local` — register → poll → read → re-register unchanged → delete, plus replay and `409`
- [ ] `make lychee`
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

**Corrected in T9a: the surface is one file wider.** `testing/e2e/gears/oagw/helpers.py:83`
registers a batch of OAGW schemas *and instances* over REST and then reads them back through
`list_oagw_types` (`GET /entities`), so it needs both migrations — submit-then-poll and the
paged list. It was missed because the earlier survey counted `/entities` references only under
`gears/types_registry/` and `account_management/`.

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

**Dependencies:** T24a
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
- [ ] Exact read: response carries `ETag`; a matching `If-None-Match` returns a bodyless `304` **that still carries the `ETag`**, declared through `no_content_response(StatusCode::NOT_MODIFIED, ..)`
- [ ] `batchGet`: validators travel **beside individual keys**, in each item's `if_none_match`, because one header cannot represent them; a result may be `unchanged`, and the response stays `200` even when all are. The two body fields are the two header names lowercased — request `if_none_match`, response `etag` — so the batch surface reads like the single one
- [ ] An `unchanged` result **carries its `etag`**, and the exact read's `304` **carries its `ETag`** — RFC 9110 §15.4.5 has a `304` send the validator a `200` would have. Every result but `not_found` therefore has one, so a refresh loop has no special case
- [ ] Test: `unchanged` and `304` both return the same validator the caller sent, byte for byte
- [ ] **Discovery pages carry no validator** and are never conditional (DESIGN: validators are for exact reads, *"never discovery pages"*) — `GET /entities` is unaffected
- [ ] A deleted entity still has a validator, and deletion moves it (deletion increments `resource_version`)
- [ ] Handlers stay mapping-only: the validator is computed in the domain service, so a future gRPC adapter gets it without new domain methods (SPEC §8.4)
- [ ] `ponytail:`-style comment where the digest is built records ceiling C7 — the fixed projection marker and absent chain versions — and names the version field as the upgrade path

**Verification:**
- [ ] Gear tests, all three backends (see [Commands](#commands))
- [ ] Test: unchanged entity yields a byte-identical validator across two reads; a revision changes it
- [ ] Test: a dependent whose `resolved_schema` was refreshed gets a new validator **even when its own `resource_version` did not move** — this is why `resolution_fingerprint` is an input, and it is the case a `resource_version`-only digest gets wrong
- [ ] Test: `If-None-Match` with the current validator returns `304` with no body; with a stale one returns `200` and the document
- [ ] Test: `batchGet` with a mix of current and stale validators returns `200`, `unchanged` for the current ones, full snapshots for the rest
- [ ] Test: an Instance validator omits `resolution_fingerprint` and still changes on revision
- [ ] Test: decoding rejects a validator whose version field is unknown rather than treating it as a match
- [ ] Test: deletion changes the validator

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
- [ ] `make ci`, gear tests on three backends, `make e2e-local`, `make e2e-docker`, `make dylint`, `make lychee` green
- [ ] Every ceiling in SPEC §9 has a comment at the point it binds
- [ ] `TypesRegistryClient` is deleted and no crate references it (D6, T26)
- [ ] Conditional reads work end to end: an exact read carries a validator and honours `If-None-Match` with `304`, `batchGet` reports `unchanged` per key (T29)
- [ ] Discovery is bounded: no response is unbounded in items or bytes, and a cursor traverses the whole set exactly once (T27, D12)
- [ ] The client cache is in place on the new models with its window, byte bound, `fresh` bypass and batched conditional revalidation (T30) — P0 does not ship an uncached read path
- [ ] Human review
