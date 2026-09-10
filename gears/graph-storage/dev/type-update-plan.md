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
8. Every rewritten row carries the write, not only the new bytes: stamp
   `updated_at` and `updated_by` with the migrating subject (`fr-audit-envelope`
   is read-only on write surfaces but applies to *every* write), bump
   `node.version` so a producer holding the pre-migration value cannot silently
   undo the migration through `expected_version`, and advance the tenant's graph
   revision once for the operation — a consumer holding a revision must not see
   content move underneath it without a signal. Added after reading the docs
   against this plan; see § Read against the gear's own documentation.
9. Report: verdict, diagnostics, rows scanned, rows rewritten, milliseconds.

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
| `dev/DEVIATIONS.md`, `docs/ADR/0006-…-type-evolution.md`, `docs/api docs` | D-031 for what is built and what is not; an ADR because this is a contract decision, citing types-registry ADR-0003/0004/0005 rather than restating them. |

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

---

# The stand rehearsal, 2026-09-10

Studio compose stand (PostgreSQL 19 + pgvector), the loaded domain model as it
stands: 1254 registered types across three model revisions, 531 251 nodes,
637 975 edges. The gear ran from the working tree, host-side, against that
database; `m0006` applied to it on boot (`applied=1 skipped=5`). Every call
went through the REST surface with a static admin token.

## The four PM edits, dry run, against `…cf.studio.sdlc.requirement.v1~` (1 000 rows)

| edit | backward | forward | rows read | admissible | why |
| --- | --- | --- | --- | --- | --- |
| 1 add optional `owner` | incompatible | compatible | 1 000 | **yes** (data-backed) | `$.payload property_added` — the level is open |
| 2 widen `status` with `blocked` | **compatible** | incompatible | **none** | yes (schema-proved) | 61 ms, no row touched |
| 3 rename `priority` → `urgency` | incompatible | incompatible | 1 000 | yes (data-backed) | `property_added`; the `index` trait diff is reported too |
| 4 make `owner` required | incompatible | compatible | 1 000 | **no** | `required_changed`, and `node requirement:1: "owner" is a required property` |

Timings: 61 ms for the schema-only verdict, 230–360 ms with a 1 000-row
re-validation. Every answer also carried
`levels_not_evolvable_in_place: ["$.payload", "$.payload.acceptance_criteria[]"]`
— the warning that the *next* optional-property edit at those levels will not
be free either.

## Edit 3 is where the data-backed ground shows its limit

Over the open payload the rename is *admitted*: no stored row becomes invalid,
because an open level still accepts the `priority` the rows carry. But every
`$filter=payload/urgency eq …` then returns nothing — the data has not moved.
**`data_backed` means "no stored row becomes invalid", not "no query breaks".**
That is a real limit of the ground, not a bug in it, and it is the argument for
slice 3: a rename needs a migration, and until there is one the exporter's
revision-suffix escape hatch stays the honest answer for a rename.

Closing the payload changes what the check can even see — below.

## The exporter's proposed shape, applied for real

Three writes against the same 1 000 rows, in order:

| step | outcome | basis | rows | ms |
| --- | --- | --- | --- | --- |
| 1. close the payload level (29 inherited properties restated at the leaf) | updated → revision 5 | `data_backed` | 1 000 | 270 |
| 2. add optional `owner` on the closed level | updated → revision 6 | **`schema_proved`** | **none** | 62 |
| 3. rename `priority` → `urgency` on the closed level, rows offered | **refused, 400** | — | 1 000 | 212 |

Step 3's refusal names the rows: *node `requirement:1`: Additional properties
are not allowed ('priority' was unexpected)*. So the same rename the open shape
waved through is caught by the closed one, with the objects in the way named.
This is the whole case for the exporter change in one run: **close the payload
once, at the price of one scan, and every later field addition is free while
every rename is caught.**

Two things this run taught that the plan did not know:

- Closing the leaf's own `allOf` branch is **not enough** — the inherited
  members arrive through the ancestors' branches, and a branch that closes the
  level rejects everything it does not itself declare. The first attempt was
  refused with the missing names listed off the rows (`access_level`,
  `acquisition_mode`, `confidence`, `created_at`, `external_id`, …). The
  exporter must restate the inherited properties at the leaf; here 7 own
  properties become 29.
- A declared `index` path has to move *with* the property it names, or
  registration refuses the candidate earlier — D-030's "a declared path that
  resolves nowhere" — and the compatibility check is never reached. The message
  is precise, but it is a different gate, and a caller renaming a field edits
  two places in the model.

## At scale: 250 000 rows

The `…cf.studio.core.action_run.v1~` type from the index experiment, same edit
(add one optional property):

| | result |
| --- | --- |
| with the shipped default `type_update_max_rows: 100 000` | refused, `out_of_range`: *"has 250000 live rows; a synchronous update re-validates at most 100000 (`type_update_max_rows`)"* — the dry run reports the same as a `row_ceiling_exceeded` diagnostic with `admissible: false` |
| ceiling raised to 300 000, update for real | admitted, `data_backed`, 250 000 rows validated, **12.9 s** (dry run 15.3 s) |
| restoring the previous definition (dropping a declaration at an open level widens) | `schema_proved`, **20 ms**, no row read |

≈19 400 rows/s, so the shipped 100 000 ceiling is about **5 s** — inside the
10 s interactive deadline, and 250 000 is not. Found and fixed during the
rehearsal: the scan did not consult the caller's deadline at all, so a raised
ceiling could outlive the request that asked for it. It now checks the
remaining budget between batches and answers `Deadline`; the row ceiling bounds
the work, the budget bounds the wait.

---

# Read against the gear's own documentation, 2026-09-10

Three checks, three different answers.

## 1. Closing the payload at the leaf is the documented rule, not a deviation

DESIGN § 3.1 § Authoring rules, rule 3, verbatim: *"`allOf` branches evaluate
independently, so `additionalProperties: false` on `payload` is only safe when no
ancestor contributes payload members. Finding may close its payload; a type
derived from `reference_node` or `analysis_edge` may not, because that branch does
not see the inherited `source` or `provenance` and rejects them. Such a type
either leaves `payload` open or **restates the inherited members alongside its
own**."*

That is the exporter change, word for word, including the part the stand
rehearsal discovered by failing: our leaves' ancestors *do* contribute payload
members, so the naive close is the unsafe case the rule warns about and
restating them is the documented remedy. Rule 2 ("derived types extend
`payload`, nothing else") is untouched. The node base declares
`payload: { "type": "object", "additionalProperties": true }` — a permission, not
an obligation; DESIGN's own phantom family narrows the same level to
`maxProperties: 0`.

One consequence to state out loud rather than discover: on a closed type, ingest
starts refusing an undeclared payload field. For a typed graph that is arguably
the point, but it is a producer-visible contract change and belongs in the same
decision as the exporter flag.

## 2. The in-place update contradicts a normative MUST — deliberately, and now on paper

PRD `fr-type-registration` says registration *"**MUST** reject re-registration of
an existing identifier with a different schema (directing the caller to publish a
new GTS version)"*, and the same rule is restated in DESIGN § 2's traceability
row, in § 4's component responsibilities ("idempotent, conflict-rejecting"), in
§ 3.7's note that each registered minor version is its own row, and in the § 12
risk table. `update` mode narrows all five.

The defence is in the requirement's own rationale — the registry is "the contract
boundary that keeps one shared graph consistent across producers", and a
backward-compatible change preserves exactly that — plus the platform's own
answer for the registry this table caches (types-registry ADR-0003/0004/0005).
But a MUST is not amended by a deviations entry, so
[`docs/ADR/0006`](../docs/ADR/0006-cpt-cf-graph-storage-adr-type-evolution.md)
now carries the decision (`accepted` 2026-09-10, with the width of the agreement
behind it stated in the ADR), and each of the five places has an
amendment note pointing at it. Two smaller doc gaps closed at the same time: the
REST table in § 3.3 was normative and lacked the new operation, and the
`gts_type` table lacked `revision` and `updated_at`.

Not amended, on purpose: `fr-index-admission` and ADR-0003's index activation
lifecycle. They are unimplemented (D-104, D-105) and an in-place update is a
second door to that same gap, not a reason to redefine it.

## 3. Migrations: the docs are silent about the rewrite and loud about three things around it

Nothing forbids the gear writing payloads — ingest does it — but § 5 above omits
three obligations that apply to *any* write of an element, and a migration is
one:

1. **The audit envelope** (`fr-audit-envelope`): every element carries the
   subject and timestamp of its last update, read-only on every write surface.
   A migration that rewrites payload must stamp `updated_at` / `updated_by` with
   the subject that ran it, or the rows silently claim their last writer was the
   producer.
2. **The graph revision.** A delete "**MUST** increment the tenant's graph
   revision", label attach/detach must too, and ingest advances it whenever
   stored state actually changed — precisely so that "two reads at one revision
   can never observe different content". A migration changes stored content, so
   it must advance the revision; otherwise a consumer holding a revision sees the
   graph move underneath it without a signal.
3. **`node.version`.** It is the compare-and-set target a producer may pass as
   `expected_version`. If a migration rewrites a payload without bumping it, a
   producer holding the pre-migration version overwrites the migrated row and
   the migration is silently undone. It must bump, which also means a migration
   can legitimately break a concurrent producer's CAS — the right outcome, and
   one the API documentation has to say.

None of the three is hard; all three are the kind of thing that is cheap now and
a data-integrity bug later. They are added to § 5 as steps of the migration
pass.

---

# The exporter's half, applied 2026-09-10

`dm2gts` now closes the payload level on every type nothing derives from — 181
of 190 node types and all 225 edge types — restating each type's inherited
payload members in its own branch, per DESIGN § 3.1 authoring rule 3. The 7
intermediates and 2 abstract types stay open, because a branch that closes the
level rejects everything it does not itself declare and would make their
descendants uninstantiable. `--open-payload` restores the previous shape.
`register.mjs` grew the three modes the gear now offers (`--dry-run`,
`--on-existing update`, `--revalidate`) plus `--chunk`.

## Applying it to a graph that already holds data

| | result |
| --- | --- |
| dry run, schemas only (`--dry-run`, no `--revalidate`) | 406 of 415 types `incompatible`, none admissible — closing an open level *is* a narrowing |
| dry run offering the rows (`--revalidate`, one type per request) | 405 admissible `data_backed`, 1 refused (`action_run`, 250 000 rows, over the 100 000 ceiling), 9 `unchanged`; **866 270 rows** read in 94 s |
| applied for real, ceiling raised to 300 000, per-type chunks | **406 updated `data_backed`, 9 unchanged, 1 116 270 rows re-validated in 105 s** — and **no stored row contradicted the closed shape** |

The last line is worth more than the timing: the transition doubles as an audit
that the loader and the model agree about what a payload may contain. Had any
producer ever written an undeclared field, this is where it would have surfaced,
named by node key.

## What the same four PM edits cost afterwards

| edit | before the transition | after |
| --- | --- | --- |
| add an optional field `owner` | `incompatible`, 1 000 rows read, 357 ms, admitted only from the data | **`compatible`, 22 ms, no row read** |
| widen `status` with `blocked` | `compatible`, 61 ms | `compatible`, 18 ms |
| rename `priority` → `urgency` | admitted from the data — and every query on `urgency` returns nothing | **refused twice over**: `$.payload property_removed in a closed model`, plus `node requirement:1: Additional properties are not allowed ('priority' was unexpected)` |
| make `owner` required | refused naming the row | refused naming the row |

## The bound this ran into

The whole-model transition cannot be one atomic request: `api-gateway` caps a
synchronous request at 30 s (`SYNC_TIMEOUT`, a constant), and raising the gear's
own `deadline_interactive_secs` to 300 does not move it — the batch came back
`504 Request exceeded 30s timeout` after 31 s. So the caller chunks, giving up
"all 415 types or none" in exchange for an operation that is convergent
(re-running answers `unchanged` for whatever landed). A *fresh* deployment
registers the closed shape with no rows to check and never meets any of this;
the cost belongs to migrating a graph that is already loaded.

That 30 s is also the number `type_update_max_rows` should be sized against,
not the gear's own deadline. At ~19 000 rows/s in-gear the shipped default of
100 000 is ~5 s, which is why it stays.
