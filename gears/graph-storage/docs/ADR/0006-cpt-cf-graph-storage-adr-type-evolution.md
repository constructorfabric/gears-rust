---
status: proposed
date: 2026-09-10
decision-makers: Graph Storage design review
---

# ADR-0006: A backward-compatible type change is admitted under the same identifier

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. Reject every changed schema; evolution is a new GTS version](#a-reject-every-changed-schema-evolution-is-a-new-gts-version)
  - [B. Accept any change and validate the stored rows](#b-accept-any-change-and-validate-the-stored-rows)
  - [C. Backward-compatible in place, data-backed second ground, migration for the rest](#c-backward-compatible-in-place-data-backed-second-ground-migration-for-the-rest)
- [What this amends in the documentation](#what-this-amends-in-the-documentation)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-graph-storage-adr-type-evolution`

## Context and Problem Statement

`cpt-cf-graph-storage-fr-type-registration` requires registration to reject a
changed schema under a registered identifier and to direct the caller to publish
a new GTS version. That rule was written for the case it names: producers
evolving independently against one shared graph, where an incompatible change
must not invalidate types other producers already derive from.

Loading the Studio domain model showed what it costs in the case it does not
name. A product manager edits one entity in a JSON model; the exporter
regenerates 415 types; every one of them is byte-identical except the edited
leaf, and that leaf is a `409`. The prototype worked around it by putting a
model revision into the namespace segment of every identifier, which registers a
complete parallel set per upload: no conflict, but the objects already in the
graph stay on the old revision until they are re-ingested, and the catalogue
grows by 415 types per edit. Two of the four edits a PM made in a week —
adding an optional property and widening an enum — cannot invalidate a single
stored object, and the gear refused them anyway, because it had no way to tell a
safe change from a breaking one.

Meanwhile the platform had already answered the same question for the registry
this gear caches. types-registry ADR-0003 fixes the strategy (`BACKWARD`: a
candidate is admissible when `Valid(baseline) subset-of Valid(candidate)`), the
baseline (the entity's current revision, never its history) and the posture (an
undecidable check is a refusal — the registry fails closed). ADR-0004 says a
major-only GTS id names a *mutable* logical entity and that "a content update
that is backward compatible under ADR-0003 preserves the same GTS ID"; ADR-0005
makes every admitted definition an immutable retained revision. DESIGN § 3.7
describes `gts_type` as "a cache with a foreign identity, never a second source
of truth" — and a cache that refuses what its authority accepts is a bug in the
cache.

## Decision Drivers

- `cpt-cf-graph-storage-fr-type-registration`'s stated rationale is that the
  type registry is "the contract boundary that keeps one shared graph consistent
  across producers". A backward-compatible change preserves exactly that: every
  instance and every reader that was valid stays valid.
- The PRD's own risk mitigation is "family patterns that keep older derived types
  valid" (§ 12). One direction of compatibility is what makes that true, and it
  is checkable.
- The check is not this gear's to invent: `gts` ships schema evolution as
  **OP#8** (spec sec 4.2) with a three-valued verdict and diagnostics that carry
  the offending schema location. OP#12 is derivation — a different relation over
  a different pair of schemas — and the two must not be conflated.
- Node and edge rows reference the interned `gts_type.id`, not the schema, so a
  compatible update touches no element row.
- The gear holds the tenant's data, which the registry does not. That is an
  asymmetry worth using, and worth reporting separately from a proof.
- An ontology author needs to know what an edit costs *before* making it; the
  loop "edit, upload, read the 409" is the loop the prototype was stuck in.

## Considered Options

- A. Reject every changed schema; evolution is a new GTS version (today's rule)
- B. Accept any change and validate the stored rows
- C. Backward-compatible in place, a data-backed second ground, a migration for
  the rest

## Decision Outcome

Chosen option: **"C"**, with the direction, baseline and fail-closed posture
taken from types-registry ADR-0003 rather than restated, and with the two
grounds for admission kept separate in the API because they are different
claims.

1. **The mode is per request and defaults to today's behaviour.**
   `POST /types` takes `options.on_existing: reject | update`; `reject` is the
   default and is `fr-type-registration` byte for byte. A caller that has not
   asked for evolution cannot receive it.
2. **`schema_proved`.** With `update`, a candidate whose backward verdict is
   `Compatible` replaces the stored definition under the same identifier,
   recomputes the chain-resolved traits, and advances a revision counter. No
   element row is read or written. Forward compatibility is computed and
   **reported**, never enforced — the registry's posture, and information a
   producer needs, since adding an optional property is exactly where the two
   directions disagree.
3. **`data_backed`.** `Incompatible` and `Unknown` are refusals from the schemas
   alone, as ADR-0003 requires. Because this gear holds the data, a caller may
   additionally offer the rows: with `options.revalidate`, every live row of the
   type is validated against the candidate, and the change is admitted only if
   all of them pass. This is a claim about *these rows*, not about the type; it
   is reported as `admission_basis: "data_backed"` with the number of rows read,
   is never cached or restated as a verdict, and is bounded by
   `type_update_max_rows` and by the caller's remaining deadline. A refusal names
   up to `type_update_max_reported_rows` offending keys.
4. **A dry run is part of the surface, not a debugging aid.**
   `POST /types/compatibility` runs the identical admission path and writes
   nothing, reporting per type: the state, both directional verdicts, every
   diagnostic with its schema location, which traits moved, the row count, the
   object levels a *later* edit will not be able to extend in place
   (`ContentModel::is_evolvable_in_place`), and whether the change is admissible.
5. **Everything else stays a new major.** A rename, a narrowed enum, a retyped
   or newly required property is refused unless the rows happen to satisfy it;
   moving the data is a migration, which this decision does not build.
6. **The verdict is computed locally with the same crate and the same
   direction** as the registry, so the two answers cannot diverge, and the call
   site is one function (`domain::evolution`) so it can become a types-registry
   round trip without touching the store or the API.

### Consequences

- `fr-type-registration`'s MUST is narrowed rather than dropped: it holds
  unconditionally in `reject` mode, and in `update` mode it holds for every
  change that is not proved safe. See
  [What this amends](#what-this-amends-in-the-documentation).
- A producer that was sending payloads the old definition accepted and the new
  one does not now fails at ingest. That is the point of the change and must be
  said in the API documentation rather than smoothed over: an update is a
  contract change for producers even when it is compatible for stored data.
- `data_backed` means "no stored row becomes invalid", **not** "no query
  breaks". Over an open payload level a rename is admitted — nothing stored
  becomes invalid — while every filter on the new path returns nothing, because
  the data has not moved. The ground is exactly as strong as the shape it checks
  against, which is one more reason a rename needs a migration.
- **The trait side reaches production earlier than ADR-0003 intended.** That ADR
  makes changing an annotation a type-version change with a durable index
  activation lifecycle (`requested -> building -> active`), and admits filters
  only while a path's index is `active`. Neither the lifecycle nor the capacity
  admission of `fr-index-admission` exists (DEVIATIONS D-104, D-105), so a
  declared path is filterable as soon as it is declared and served by the static
  payload GIN plus a scan (D-030). An in-place update does not create that gap,
  but it does open a second door to it: a new `index` path can now appear under
  an existing identifier. When the lifecycle lands it must gate this path as
  well, and a trait-only update must be capacity-admitted like a registration.
- A revision counter on `gts_type` is not the retained history ADR-0005
  describes. Until the history table exists, an update is invisible after the
  fact beyond "the counter moved".
- The revision-in-the-namespace workaround stays. It costs nothing, and it
  remains the answer for a full reload and for a rename until migrations exist.
- Registration authorization is unchanged (ontology administration, ADR-0003's
  interim policy). Re-validation additionally requires `write` on the node
  resource and is served under that scope: it reads the tenant's rows, and
  holding ontology administration is not holding the data.

### Confirmation

- Unit cases over the rule itself: each row of the edit-to-verdict table, an
  undecidable verdict refused, the trait diff, the refusal text naming its
  locations.
- Seven conformance cases run against **both** store implementations — the
  built-in PostgreSQL store and the in-memory fake — so a ground only one of them
  applies fails the suite: a compatible change updates in place and the stored
  objects stay queryable; an incompatible one is refused with its location; a
  changed schema is still a conflict by default; a dry run reports every verdict
  and writes nothing; a change the schemas cannot prove is admitted when the rows
  fit; one the rows contradict is refused naming them; a newly declared `index`
  path becomes filterable without recreating the type.
- Rehearsed on a stand carrying the loaded Studio domain model (1254 types,
  531 251 nodes, 637 975 edges): the four PM edits classified as expected,
  62 ms for a proved update, 270 ms for a data-backed one over 1 000 rows,
  12.9 s over 250 000, and the row ceiling refused with the count and the
  configuration key named. Numbers and the full run in
  [`dev/type-update-plan.md`](../../dev/type-update-plan.md).
- Measured before the write path existed: with the payload object level left
  open, "add one optional property" is `Incompatible` for **188 of 188**
  instantiable node types of the Studio model, and `Compatible` for 188 of 188
  with the payload closed at the leaf and the inherited members restated —
  which is authoring rule 3 of DESIGN § 3.1, not a new idea. Nothing came back
  `Unknown`.

## Pros and Cons of the Options

### A. Reject every changed schema; evolution is a new GTS version

- Good, because it is the simplest possible rule and cannot be wrong about
  compatibility, having no opinion about it.
- Good, because a consumer pinning an identifier gets a definition that never
  moves.
- Bad, because it refuses changes that provably cannot invalidate anything, so
  the caller pays a new identifier — and the migration of every object onto it —
  for adding an optional field.
- Bad, because it diverges from the registry this gear is documented as caching:
  the platform accepts under one identifier what the gear refuses.
- Bad, because the cost lands on the least prepared actor: a product manager
  editing a model, whose edit becomes 415 new types.

### B. Accept any change and validate the stored rows

- Good, because it is one mechanism, and it answers the question the owner of
  the data actually has: will anything I hold stop being valid.
- Good, because it admits the many safe changes that no schema comparison can
  prove.
- Bad, because it reads every row of the type for every change, including the
  ones that are provably free.
- Bad, because it silently redefines compatibility as a property of the current
  data: the same edit is admitted today and refused tomorrow, and nothing in the
  answer says so.
- Bad, because it contradicts the platform strategy, so the gear and the registry
  would disagree about the same pair of schemas.

### C. Backward-compatible in place, data-backed second ground, migration for the rest

- Good, because the direction, baseline and fail-closed posture are the
  platform's, so the two answers agree by construction.
- Good, because the common edits cost nothing at all: no row read, same
  identifier, objects untouched.
- Good, because the weaker claim is available *and labelled*: a caller can see
  that a change was admitted from the rows rather than proved, and how many rows
  that was.
- Good, because the dry run turns "what does this edit cost" into a question with
  an answer, which is what the ontology author's loop needs.
- Neutral, because it adds a second admission path to keep correct; both are
  covered by the same conformance cases on both stores.
- Bad, because it narrows a normative MUST and therefore needs this ADR and the
  amendments below.
- Bad, because a rename still has no answer until payload migrations exist, so
  the revision workaround has to stay for that case.

## What this amends in the documentation

Recorded here rather than by silently rewriting the statements, so a reviewer
sees exactly what changes:

1. **PRD `cpt-cf-graph-storage-fr-type-registration`** — "MUST reject
   re-registration of an existing identifier with a different schema (directing
   the caller to publish a new GTS version)" becomes conditional on
   `options.on_existing`, and remains the default. Everything else in the FR
   (idempotence on identical bytes, batch atomicity, UUIDv5 derivation) is
   untouched.
2. **DESIGN § 2 traceability** for that FR, and **§ 4** Ontology Registry
   responsibilities ("idempotent, conflict-rejecting, batch-atomic
   registration") — the registry also admits a proved-compatible change.
3. **DESIGN § 3.3** REST surface — one new operation,
   `POST /api/graph-storage/v1/types/compatibility`, and one new request option
   on `POST /types`.
4. **DESIGN § 3.7 Table `gts_type`** — two columns (`revision`, `updated_at`),
   and its note that "each registered minor version is its own row" now holds
   per *identifier*: a minor-bearing identifier is still immutable, while a
   major-only identifier's row carries successive revisions.
5. **ADR-0003 consequence** "Changing annotations is a type-version change" —
   a trait change that no instance can fail is admitted under the same
   identifier; the index activation lifecycle it describes still applies to the
   index work, when it exists.
6. **PRD § 12 risk table** — the mitigation reads "immutable schemas per GTS
   version"; the property that actually keeps older derived types valid is
   backward compatibility, which is now checked rather than assumed.

`fr-index-admission` and the index activation lifecycle are **not** amended:
they remain unimplemented deferrals (D-104, D-105), and this decision adds a
second door to the same gap rather than a new gap.

## More Information

- types-registry ADR-0003 (compatibility strategy), ADR-0004 (identifier
  mutability and the `force` waiver), ADR-0005 (retained revisions).
- `gts` 0.12 `schema_evolution` (OP#8) and `GtsStore::compare_documents`.
- [`dev/type-update-plan.md`](../../dev/type-update-plan.md) — the
  implementation plan, the measurement, and the stand rehearsal.
- [`dev/DEVIATIONS.md`](../../dev/DEVIATIONS.md) D-031 — what is built and what
  is not.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

- `cpt-cf-graph-storage-fr-type-registration` — narrows its rejection rule to a
  per-request default and defines what may be admitted instead
- `cpt-cf-graph-storage-fr-type-constraints` — chain validation is what the
  data-backed ground re-runs against the candidate
- `cpt-cf-graph-storage-contract-gts-ontology` — unchanged for the base
  ontology: a base-type schema change is still a new GTS version
- `cpt-cf-graph-storage-adr-metadata-partitioning` — amends one consequence
  about annotation changes
