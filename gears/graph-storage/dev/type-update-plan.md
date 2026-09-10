# Updating a registered type in place — implementation plan

Status: **slices 1 and 2 implemented 2026-09-10** on `feature/graph-storage-v2`
(dry run, in-place update, trait recompute, refusals with locations, plus one
ground this plan did not have — see the addendum at the end). Slices 3 and 4
(payload-rewriting migrations, revision history) are still as written below.
Written 2026-09-10 for `feature/graph-storage-v2`.

The gear registers a type once and refuses a changed schema under a known id
(`infra/store/types.rs`, the `GraphStoreError::Conflict` branch). Every edit to
the Studio domain model is therefore a `409`, including edits that cannot
invalidate a single stored object. The prototype works around it with a model
revision in the type id (`--model-revision`, exporter side), which is fine for
playing with a model and wrong for a product: old objects stay behind on the old
revision and the type catalogue grows by 415 types per upload.

This plan implements the answer instead: **accept a backward-compatible change
under the same identifier, demand a stated migration for an incompatible one,
and refuse what cannot be decided.** The revision flag stays as it is; it costs
nothing and remains the escape hatch for a full reload.

## 1. We are not inventing a policy

The platform has already decided this, and the gear currently diverges from it.

| Source | What it says |
| --- | --- |
| types-registry ADR-0003 | `BACKWARD` strategy. A candidate is admissible when `Valid(baseline) ⊆ Valid(candidate)`, the baseline being the entity's **current revision**, never its history. An undecidable check is a rejection: the registry fails closed. `FULL` and `FORWARD` are deliberately not enforced. |
| types-registry ADR-0004 | A major-only GTS id names a **mutable** logical entity whose successive definitions are retained revisions; *"a content update that is backward compatible under ADR-0003 preserves the same GTS ID"*. A backward-incompatible change requires a new **major**. A minor-bearing id is immutable and is a reference-pinning boundary; the `force` waiver exists only across a minor boundary and is recorded. |
| types-registry ADR-0005 | Every admitted definition is an immutable retained revision of one logical Type Schema. |
| `gts_type` entity docstring | *"Per-tenant projection of the platform types-registry … The registry stays authoritative — this is a cache with a foreign identity, never a second source of truth."* |

A cache that refuses what its authority accepts is a bug in the cache. Two
consequences for this work:

- the rule to implement is fixed — one direction, one baseline, fail closed —
  and needs no new deliberation;
- the long-run shape is delegation: the gear asks types-registry and caches the
  verdict. Until the gear talks to the registry on the write path, it must
  decide locally **with the same crate and the same direction**, so the two
  answers cannot diverge.

## 2. What already exists and must not be rebuilt

**The compatibility checker.** `gts 0.12` (already a workspace dependency,
`Cargo.toml:826`) ships `src/schema_evolution.rs` — *"Type Schema Evolution
Compatibility (spec sec 4.2, OP#8)"*:

```rust
pub fn check_backward_diagnostics(old: &Value, new: &Value)
    -> (CompatibilityVerdict, Vec<CompatibilityDiagnostic>);
pub fn check_forward_diagnostics(old: &Value, new: &Value) -> (…);
pub fn check_accepted_set_inclusion(subset: &Value, superset: &Value) -> (…);
pub enum CompatibilityVerdict { Compatible, Incompatible, Unknown }
```

- `CompatibilityDiagnostic` carries the offending **schema location** plus a
  `CompatibilityFinding`, so a refusal can point at `/payload/properties/owner`
  rather than say "incompatible".
- `Unknown` is deliberately distinct from `Incompatible` — *"the caller, not
  this library, decides how that affects admission"*. Our decision is ADR-0003's:
  refuse.
- Both documents must be `$ref`-resolved first. `GtsStore::compare_documents`
  does resolve-then-compare and returns `SchemaComparison`, which also exposes
  `candidate_object_levels` and `ContentModel::is_evolvable_in_place` — the flag
  that says whether a *later* edit will be able to add an optional property at
  that level. Worth surfacing: it turns "your next edit will be a major" into a
  warning today rather than a surprise later.
- Note for the deck and the report: the evolution check is **OP#8**. OP#12 is
  schema-vs-schema *derivation* admission (`src/schema_derivation.rs`), which is
  a different relation over a different pair of schemas, and the gts docs are
  emphatic that the two must not be conflated.

**Everything the gear already does per registration.** `ontology::analyze`
resolves the chain, the family, the merged traits and the scalar kind of every
declared `index` path. An update re-runs exactly this; there is no second code
path to write.

**No validator cache to invalidate.** `ChainValidator::compile` runs per ingest
request (`domain/service.rs:276`), so a replaced schema takes effect on the next
request with no eviction logic.

**Node and edge rows do not reference the schema.** They carry the interned
`gts_node_type_id` / `gts_edge_type_id` (a 4-byte surrogate). An accepted
compatible update touches **no** node row.

**The re-embed lever.** Vectors are stamped with an epoch and skipped when text
is unchanged (D-027, `domain/embedding.rs`). Changing `vector_search` paths does
not need a bespoke mechanism — it needs the affected nodes marked stale.

**Batched writes.** `infra/store/ingest.rs` already does `update_many` in
bounded batches; a payload migration is the same shape of write.

## 3. The rule, as a table

`old` is the stored schema of the type, `new` the candidate. Verdict is
`check_backward_diagnostics(old, new)` over the resolved documents.

| Edit | Verdict | Gear behaviour |
| --- | --- | --- |
| new optional property | `Compatible` | accept in place, same id |
| widened enum | `Compatible` | accept in place |
| relaxed bound (`maximum` raised, `minLength` lowered) | `Compatible` | accept in place |
| description, title, examples, default | `Compatible` | accept in place |
| new `index` path, new `full_text_search` path | schema may be identical; traits differ | accept, recompute `effective_traits`; the path becomes filterable at once |
| changed `vector_search` paths | traits differ | accept, mark affected nodes stale for re-embedding |
| renamed property | `Incompatible` | refuse; accept only with a migration that renames the data; otherwise a new major |
| narrowed enum, retyped property | `Incompatible` | refuse or migrate |
| new required property | `Incompatible` | refuse unless the migration supplies a value for every existing row |
| removed property (closed model) | `Incompatible` | refuse or migrate (drop step) |
| anything the checker cannot decide | `Unknown` | refuse, naming the unproven location (ADR-0003: fail closed) |

Forward compatibility is computed and **reported**, never enforced — same
posture as the registry. It is useful information for a producer: it says
whether an old reader still accepts new payloads.

## 4. API surface

Three additions, in the order they should ship.

### 4.1 `POST /cf/graph-storage/v1/types/compatibility` — dry run, no writes

Body: the same `registrations` array `POST /types` takes. Response, per type:

```json
{
  "type_id": "gts.…cf.studio.sdlc.requirement.v1~",
  "state": "unchanged | compatible | incompatible | undecidable | new",
  "backward": "compatible",
  "forward": "incompatible",
  "diagnostics": [
    { "location": "/allOf/1/properties/payload/properties/urgency",
      "finding": "property_added_required",
      "message": "required property 'urgency' is absent from the baseline" }
  ],
  "traits_changed": { "index": { "added": ["/payload/urgency"], "removed": ["/payload/priority"] } },
  "rows": 10000,
  "migration_required": true,
  "evolvable_in_place": true
}
```

This is the cheapest useful slice and the one the architect's loop actually
wants: *"tell me what this edit costs before I do it"*. It is also the honest
answer to the question the deck raises — a PM can see that two of four edits are
free.

### 4.2 `POST /types` gains a mode

```json
{ "registrations": [ … ],
  "options": { "on_existing": "reject" } }
```

`reject` is the default and is today's behaviour byte for byte (a changed schema
is `409`), so no existing caller changes. `update` accepts compatible changes,
refuses the rest with the diagnostics above. Per-type outcome in the response:
`created | unchanged | updated | refused`.

### 4.3 A migration, as a closed set of steps

Not an expression language. Three steps cover every incompatible edit the domain
model produced in three days:

```json
{ "registrations": [ … ],
  "options": { "on_existing": "update" },
  "migration": {
    "type_id": "gts.…cf.studio.sdlc.requirement.v1~",
    "steps": [
      { "rename":  { "from": "/payload/priority", "to": "/payload/urgency" } },
      { "default": { "path": "/payload/owner", "value": "unassigned" } },
      { "drop":    { "path": "/payload/legacy_flag" } }
    ]
  } }
```

Each step renders one `jsonb` expression the store builds itself
(`jsonb_set`, `#-`, `||`) in the style already used by
`infra/store/projection.rs`: path tokens restricted to `[A-Za-z0-9_.-]` at parse
time so they can be literals, every value a bound parameter, no caller text in
SQL. A migration is refused if its steps do not, in fact, make the type's rows
valid — see §5.

## 5. Order of operations for one accepted update

1. Resolve the candidate through `ontology::analyze` — as today, so a malformed
   candidate is refused before anything else happens.
2. Read the stored schema; if byte-identical, `unchanged` (today's converging
   path).
3. Resolve both documents' `$ref`s and run the backward check. `Compatible` and
   no migration requested → step 6.
4. `Incompatible` / `Unknown` and no migration → refuse with diagnostics.
5. Migration present: count the type's rows; refuse above
   `type_update_max_rows` (configured, default 100 000) — that ceiling is what
   keeps a synchronous request honest. Then, in batches over a keyset scan:
   apply the steps to the payload, validate the result with a `ChainValidator`
   compiled from the **new** schema, and write. Collect up to
   `type_update_max_reported_rows` offending keys and refuse the whole operation
   if any row fails — validate-then-write, one transaction per batch, nothing
   half-migrated is left readable as valid.
6. Replace `type_schema` and `effective_traits`, bump `revision`, set
   `updated_at`.
7. Trait fallout: `index` paths added → nothing to do today beyond the recompute
   (the expression index itself needs gears-rust #4721); `vector_search` changed
   → clear `vector_epoch` on the type's nodes so the next ingest or a backfill
   re-embeds them; `full_text_search` changed → recompute the stored tsvector for
   the type's rows in the same batched pass.
8. Report: verdict, diagnostics, rows scanned, rows rewritten, milliseconds.

Concurrency is deliberately simple: the operation runs under one snapshot, rows
are locked per batch, and an ingest of the old shape that arrives after the
update fails validation. That is the point of the feature, and it must be said
in the API docs rather than smoothed over.

## 6. Where the code lands

| File | Change |
| --- | --- |
| `domain/evolution.rs` *(new)* | Wraps `gts::schema_evolution`. Owns `Verdict`, the classification of §3, and rendering a `CompatibilityDiagnostic` into the gear's `ItemError` shape. No storage in scope, so it is unit-testable as a pure function. |
| `domain/ontology.rs` | Expose the resolved-chain document for comparison (the walk already exists for validation) and a `traits_diff` helper over two `TypeDescriptor`s. |
| `domain/migration.rs` *(new)* | Parse and validate the step grammar; apply steps to one payload in memory (the same code the store's batch pass uses, so the validation and the write cannot disagree). |
| `domain/service.rs` | `register_types` takes the mode; new `type_compatibility` (dry run) and `migrate_type`; orchestrates §5. |
| `infra/store/types.rs` | Split the `Conflict` branch: `update_in_tx` for an accepted change, conflict kept for `reject` mode and refusals; add `count_by_type`. |
| `infra/store/migrate.rs` *(new)* | The batched keyset rewrite, the `jsonb` expression builder, the offending-key collector, tsvector recompute, vector staleness. |
| `infra/storage/entity/gts_type.rs`, `migrations/m0006_type_revision.rs` *(new)* | `revision int not null default 1`, `updated_at timestamptz`. Revision **history** is slice 4. |
| `infra/fake_store.rs` | Mirror update + migration in memory so the fake lane covers the same conformance cases. |
| `api/rest/{dto,handlers,routes}.rs` | The new route, the option, the response shapes, the 409 body carrying diagnostics. |
| `config.rs` | `type_update_max_rows` (default 100 000, range 1..=5 000 000), `type_update_batch` (default 2 000), `type_update_max_reported_rows` (default 50). |
| `dev/DEVIATIONS.md`, `docs/ADR/0006-…-type-update.md`, `docs/api docs` | D-031 for what is built and what is not; an ADR because this is a contract decision, citing types-registry ADR-0003/0004/0005 rather than restating them. |

Authorization: a payload rewrite is a write over tenant data and must not ride
on the registration action. New action `types.migrate` alongside
`types.register` in `domain/authz.rs`.

## 7. Phasing

| Slice | Scope | Estimate | Acceptance |
| --- | --- | --- | --- |
| 1 | `gts` wired, verdicts + diagnostics, `POST /types/compatibility`, no writes | ~1.5 d | The four edits of the deck's example classify correctly against the loaded DM; measured share of `Unknown` verdicts over all 190 node types reported |
| 2 | `on_existing: update`, in-place accept, trait recompute, refusals, fail closed on `Unknown` | ~2 d | Edits 1–2 return `200` and the 10 000 objects stay queryable and filterable; edits 3–4 return `409` naming the location |
| 3 | Migrations: step grammar, batched validate-then-write, report | ~3 d | Rename and required-field edits succeed with a migration on a 250 k-row type inside the ceiling; a migration that would invalidate rows is refused naming them |
| 4 | Revision history + `GET /types/{id}/revisions`, re-embed staleness, docs, exporter `--on-existing update` and rename-step emission between two model revisions | ~2 d | An edited model uploads with no `409` and no revision suffix; the type's revision history shows both definitions |

≈ 8–9 working days for one person including tests. Slices 1–2 alone (≈3.5 days)
remove the `409` for the majority of real edits, which is the whole product
complaint.

## 8. Tests

- **Unit** (`domain/evolution.rs`, `domain/migration.rs`): each row of §3;
  `Unknown` refused; a step path outside the alphabet refused; steps applied to a
  payload produce exactly the expected document; traits diff.
- **Fake lane** (`tests/fake_conformance.rs`): the four edits end to end;
  idempotent re-registration still converges; refusal shapes.
- **PG lane** (`tests/pg_conformance.rs`), named in the existing style:
  `a_compatible_change_updates_the_type_in_place`,
  `an_incompatible_change_is_refused_with_its_location`,
  `an_undecidable_change_is_refused`,
  `a_new_index_path_becomes_filterable_without_re_registration`,
  `a_migration_rewrites_the_payloads_of_a_large_type`,
  `a_migration_that_would_invalidate_rows_is_refused_naming_them`,
  `a_concurrent_ingest_of_the_old_shape_fails_after_the_update`.
- **Stand rehearsal**: the deck's four edits against the loaded domain model
  (revision m8, 13 925 objects, plus the 250 k-row `action-run`), timings
  recorded for the report so the slide can carry measured numbers instead of an
  estimate.

## 9. Risks and open questions

1. **How often is the verdict `Unknown`?** Our generated schemas nest the
   payload inside `allOf` branches, and the checker stops at unprovable
   locations. If a trivial edit to a mirrored type comes back `Unknown`, the
   exporter must emit the closed-envelope shape gts §4.4.1 recommends. This is
   why slice 1 is read-only and reports the share: it is a measurement, not a
   guess, and it can change the exporter before any write path exists.
2. **Who is the authority.** Local checking is a stopgap. The plan keeps the
   call site narrow (`domain/evolution.rs`) precisely so it can become a
   types-registry round trip without touching the store or the API.
3. **`force`.** The platform allows a recorded waiver only across a minor
   boundary, never within one identifier. Default: no `force` in the gear. If
   Studio wants one for its own tenant namespace, it is an ADR, not a flag.
4. **The major-version path.** A new major is simply a new type id, so the gear
   needs nothing; what needs an answer is who queries both majors and how — a
   consumer/exporter question (a pattern spanning majors), noted here so it is
   not mistaken for gear work.
5. **Auditability.** Without the slice-4 history table an update is invisible
   after the fact. If the demo needs "show me what this type looked like last
   week", slice 4 moves up.
6. **Large types.** 100 k rows is the synchronous ceiling; above it the answer is
   an asynchronous migration with progress, which is out of scope and should
   stay out until someone hits the ceiling.
7. **Index rebuild.** A newly declared `index` path is filterable immediately but
   unindexed until gears-rust #4721 lands. The two pieces of work are
   independent; neither blocks the other.

## 10. Out of scope

Automatic data copy between majors; asynchronous migrations; index DDL (#4721);
change-event publication (`emit_events` exists as a trait, publication does not);
and any change to the `--model-revision` path, which stays as the escape hatch.

---

# Addendum, 2026-09-10: what building it changed

## The measurement slice 1 exists for, taken first

Over the 188 instantiable node types the exporter emits from the Studio domain
model, "add one optional property" is:

| exporter shape | backward verdict |
| --- | --- |
| as emitted today (payload level open) | `incompatible` — 188 of 188 |
| payload materialized and closed at the leaf | `compatible` — 188 of 188 |

`Unknown` never occurred. Risk 1 of §9 was therefore wrong in its shape and
right in its consequence: the verdicts are decidable, and it is our *schema
shape* that makes the commonest edit inadmissible. gts sec 4.4 explains it in
one line — at an open object level the previous definition already accepted any
value under the new property's name, so declaring the property narrows the
accepted set. The same run classifies the deck's four PM edits against the real
`requirement` type:

| edit | open payload | closed payload |
| --- | --- | --- |
| 1 add optional `owner` | incompatible | **compatible** |
| 2 widen `status` enum | **compatible** | **compatible** |
| 3 rename `priority` → `urgency` | incompatible | incompatible (`property_removed`) |
| 4 make `owner` required | incompatible | incompatible (`required_changed`) |

## The exporter change this implies, and its limit

Close the payload at the leaf: `additionalProperties: false` with every
inherited property restated. **Only on a type with no descendants** — 181 of
the 188 are leaves — because an intermediate type that closes `payload` makes
every descendant uninstantiable, `allOf` branches being evaluated
independently. That is not a new discovery: the gear's own D-029 states the
rule. The seven non-leaf instantiable types keep the open payload and reach the
same outcome through the second ground below.

Closing the payload also makes ingest refuse an undeclared payload field, which
is a real behaviour change for producers and a decision worth taking
deliberately rather than as a side effect.

## The second ground for admission, which this plan did not have

§3 assumed one rule: the schemas decide, and `Unknown` is a refusal. That is
right for a *registry*, which holds no data. This gear holds the data, so when
the schemas cannot prove inclusion there is a different question available: does
every live row of the type validate against the candidate? `options.revalidate`
opts into asking it, and the answer is reported as
`admission_basis: "data_backed"` with the row count, never as a verdict about
the type. Two grounds, never conflated:

| ground | what it proves | what it costs |
| --- | --- | --- |
| `schema_proved` | `Valid(old) ⊆ Valid(new)` for every instance that could ever exist | no row is read |
| `data_backed` | every row this tenant has now satisfies the candidate | one scan, capped by `type_update_max_rows` |

This is what turns the 188 open-payload types from "blocked until the exporter
changes" into "admitted, at the price of a scan" — and it is also the honest
half of what a migration will be in slice 3, since a migration is a rewrite
followed by exactly this check.

## What shipped, against §4 and §7

- §4.1 `POST /types/compatibility` — as specified, plus `admissible` and
  `levels_not_evolvable_in_place`. Re-validation defaults **on** here and off on
  the write path: a dry run that skipped the row check would answer a different
  question from the one the update asks.
- §4.2 `options.on_existing` — as specified; `reject` is the default and is the
  previous behaviour byte for byte.
- §4.3 migrations — **not built**. Unchanged as a design.
- §5 — steps 1–4, 6 and 8 as written. Step 5's row pass re-validates and does
  not rewrite. Step 7: `index` needs nothing (the projection reads the path at
  query time); the tsvector and vector-epoch recomputes are not built, and
  `evolution::recompute_needed` names which trait changes will need them.
- §6 — `domain/evolution.rs` (the rule, pure), `infra/store/evolution.rs` (the
  row pass), the split conflict branch in `infra/store/types.rs`, `m0006`, the
  fake store mirroring both grounds, the DTOs and the route, three config keys.
  `domain/migration.rs` and `infra/store/migrate.rs` belong to slice 3 and do
  not exist.
- §6 authorization — no new PDP action. A re-validating update requires `write`
  on the node resource *in addition to* `admin` on the type resource and is
  served under the node scope, which is the scope ingest already writes those
  rows with. A new action would have needed a policy the deployments do not
  carry; the two existing decisions say the same thing.
- §8 — 10 unit cases in `domain::evolution`, 7 conformance cases on **both**
  stores (`a_backward_compatible_change_updates_the_type_in_place`,
  `an_incompatible_change_is_refused_with_its_location`,
  `a_changed_schema_is_still_a_conflict_by_default`,
  `a_dry_run_reports_every_verdict_and_writes_nothing`,
  `a_change_the_schemas_cannot_prove_is_admitted_when_the_rows_fit`,
  `a_change_the_stored_rows_contradict_is_refused_naming_them`,
  `a_new_index_path_becomes_filterable_without_recreating_the_type`).

## A wart this closed on the way past

The prototype's handover recorded that a type registered before v0.1.2 keeps a
stale `effective_traits` (no `index_kinds`), that an idempotent re-registration
does not refresh it, and that the only remedy was to recreate the database. In
`update` mode a byte-identical re-registration whose *resolved traits* differ
now rewrites them and bumps the revision. In `reject` mode it still converges
silently, so the default path is unchanged.
