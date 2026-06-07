---
status: accepted
date: 2026-06-02
decision-makers: Usage Collector module owners
---

# Unified plugin-DB metric catalog and gts_id reference model

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Amendment 2026-06-02 — kind from prefix and closed metadata_fields](#amendment-2026-06-02--kind-from-prefix-and-closed-metadata_fields)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep both catalogs plus all current attributes](#keep-both-catalogs-plus-all-current-attributes)
  - [Plugin-DB catalog only with gts_id referencing and dropped attributes](#plugin-db-catalog-only-with-gts_id-referencing-and-dropped-attributes)
  - [Keep the local-from-config catalog only](#keep-the-local-from-config-catalog-only)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`

## Context and Problem Statement

Four orthogonal complications have accumulated in the Usage Collector (UC)
metric specification surface as ADRs 0007, 0009, and 0010 layered onto one
another: (1) a gateway-local metric catalog loaded from configuration coexists
with the plugin-DB catalog managed via Software Development Kit (SDK) / Representational State Transfer (REST), giving two
sources of truth for what metrics exist; (2) usage records reference metrics
via a Universally Unique Identifier (UUID) derived deterministically from the metric type identifier (UUID v5 over the
type id per Generic Type System (GTS) guide §5.1), so the reference shape is a derivation rather
than a stored identity; (3) the metric specification carries `parent_type_uuid`
to model an inheritance chain across catalog rows; (4) the specification
carries `x-uc-indexable` and `abstract` (`x-gts-abstract`) complexity
attributes that mark which metadata properties are queryable and which types
may carry usage rows. Each of these was justified in isolation, but together
they enlarge the specification and SDK surface beyond what the v1 quota-reporting
consumer narrowed by commit `783abdda` actually requires. The question is
whether to keep the accumulated complexity, simplify in lockstep, or partially
revert.

## Decision Drivers

- Minimize duplicate sources of truth for the metric catalog — operators and
  downstream consumers must be able to point at one place when asking "what
  metrics exist?"
- Align metric identity with GTS schema identity so the wire reference does
  not depend on a derivation step that must be re-implemented identically on
  every consumer.
- Reduce specification surface to what v1 quota-reporting consumers
  (per commit `783abdda`) actually need; complexity attributes that exist only
  to enable deferred capabilities should not ship in v1.
- Preserve forward compatibility for catalog evolution — future ADRs may
  re-introduce indexability hints, inheritance, or abstract markers if and when
  a concrete consumer requires them, but v1 must not pay for them speculatively.
- Honor the platform GTS pattern: metric types are GTS Type Schemas (per
  ADR-0010), and their stored `gts_id` is the natural reference key.

## Considered Options

- **Keep both catalogs plus all current attributes** — preserve the
  gateway-local-from-config catalog (ADR-0007 / 0009) alongside the plugin-DB
  catalog, keep usage records referencing metrics via the uuid5-derived UUID,
  and retain `parent_type_uuid`, `x-uc-indexable`, and `abstract` on the metric
  specification unchanged.
- **Plugin-DB catalog only, with `gts_id` referencing and dropped attributes**
  — the plugin-DB catalog (managed via SDK/REST) becomes the sole metric
  catalog; usage records reference metrics via `gts_id` directly (the
  uuid5-from-type derivation is removed); the metric specification drops
  `parent_type_uuid`, `x-uc-indexable`, and `abstract`. (chosen)
- **Keep the local-from-config catalog only** — remove the plugin-DB catalog
  surface, keep the gateway-local-from-config catalog as the sole source, and
  retain or simplify the attributes around it.

## Decision Outcome

Chosen option: **"Plugin-DB catalog only, with `gts_id` referencing and dropped
attributes"**, because it is the only option that simultaneously eliminates
the duplicate catalog source, aligns the usage-record metric reference with
the GTS schema identity already stored on every catalog row, and shrinks the
specification surface to match the v1 quota-reporting consumer narrowing
(commit `783abdda`). Keeping both catalogs preserves redundancy without
adding capability; keeping only the local-from-config catalog removes the
runtime SDK/REST surface that downstream operators already depend on.

The decision pins four simplifications. They are load-bearing for the cascade
phases that follow (Phases 2-10 of the
`update-usage-collector-simplify-metric-catalog` plan) and must not be diluted
by downstream artifacts:

1. **The plugin-DB catalog (managed via SDK/REST) is the sole metric catalog.**
   The gateway-local-from-config catalog is removed. There is one place where
   metrics are declared, one place where they are looked up, and one place
   where they are deleted: the plugin's backend database, mutated via the
   gateway's SDK trait and REST surface, authorized by the Policy Decision
   Point (PDP) per ADR-0001.
2. **Usage records reference metrics via `gts_id`.** The uuid5-from-type
   derivation is removed from the wire and from the storage schema. The
   `gts_id` string that identifies a metric in the catalog is the same value
   stored on every usage record that references it. No consumer or plugin
   author needs to re-implement UUID v5 derivation to validate or join.
3. **The metric specification no longer defines `parent_type_uuid`.** Metric
   types are flat for v1; no parent pointer is carried on the catalog row, no
   inheritance chain is walked at validation time. If a future capability
   requires inheritance, it will be reintroduced by a dedicated ADR that names
   its consumer.
4. **The metric specification no longer defines `x-uc-indexable` or
   `abstract`.** Indexability hints and abstract-type markers do not appear on
   the metric specification surface. All metric types registered in the
   catalog are concrete and queryable on their declared shape; indexing
   strategy is a plugin implementation concern.
5. **`kind` is derived from the `gts_id` prefix, not declared as a separate
   trait.** The catalog row carries no `kind` column and the metric
   specification carries no `kind` trait. The value (`counter` or `gauge`) is
   the leftmost `~`-separated base-type segment of the metric's `gts_id`. The
   two reserved base kind type identifiers are
   `gts.cf.core.usage.counter.v1~` and `gts.cf.core.usage.gauge.v1~`; every
   registered metric's `gts_id` MUST begin with exactly one of those two
   prefixes, and `kind ∈ {counter, gauge}` falls out deterministically from
   that prefix.
6. **Closed `metadata_fields: Vec<String>` replaces open `metadata_schema`.**
   The catalog declares a closed list of metadata keys per metric. Only
   declared keys are accepted at ingest; there is no free-form remainder, no
   `additionalProperties: true` escape hatch, and no `extras` map. All values
   are typed as `String` at the SPI / validation layer (the catalog declares
   keys; values are conveyed as strings end-to-end). The Draft-07 JSON-Schema
   surface and the `jsonschema` runtime dependency are removed.

### Consequences

- The PRD (`PRD.md`), DESIGN (`DESIGN.md`), DECOMPOSITION (`DECOMPOSITION.md`),
  FEATUREs (`features/foundation.md`, `features/metric-lifecycle.md`,
  `features/usage-emission.md`), companion design docs (`domain-model.md`,
  `plugin-spi.md`, `sdk-trait.md`), and the OpenAPI YAML
  (`usage-collector-v1.yaml`) all carry references to the local-from-config
  catalog, to uuid5-derived metric identifiers, to `parent_type_uuid`, and to
  the dropped complexity attributes. Each of those artifacts must be revised
  in lockstep so the specification family describes one catalog model, one
  reference shape, and one attribute set. Phases 2-10 of the
  `update-usage-collector-simplify-metric-catalog` plan execute that cascade
  artifact-by-artifact.
- ADRs 0007, 0009, and 0010 are marked `superseded` with `superseded_by`
  pointing at this ADR. A one-line forward pointer is added to the body of
  each so a reader landing on those files immediately learns where the live
  decision lives. The superseded ADRs are not edited in their Decision or
  Consequences sections — they remain immutable beyond the status header and
  the forward pointer.
- ADR-0002 (`cpt-cf-usage-collector-adr-pluggable-storage`) is unchanged in
  its decision text; the catalog scope it carries was already re-expanded by
  ADR-0009 and remains so under this ADR.
- The single source of truth for the metric catalog simplifies operator
  mental model and downstream documentation: there is no longer a need to
  explain when the local catalog applies versus when the plugin catalog
  applies, or how the two are reconciled at boot.
- The `gts_id`-as-reference shape removes a class of bugs (consumers
  re-implementing UUID v5 derivation incorrectly) and removes one column from
  the storage row shape that downstream phases were going to define.
- The smaller spec and SDK surface aligns with the quota-reporting consumer
  narrowing in commit `783abdda`: v1 ships only what that consumer requires,
  with deferred complexity flagged as reserved for later ADRs that name a
  concrete consumer.
- Honest cost: `x-uc-indexable` as a documented hint is lost; downstream
  consumers that wanted to know which dimensions plugin authors should
  optimize for must now read the metric's metadata schema and decide
  per-backend. Honest cost: any external documentation, sample code, or
  partner integration that referenced the local-from-config catalog or the
  uuid5 derivation must be updated; an inventory pass is included in
  Phase 11's final consistency sweep.
- Migration mechanics from the prior model to the simplified model are a
  downstream cascade concern; this ADR does not specify migration ordering,
  which is owned by the DESIGN and FEATURE cascades (Phases 3, 5, 6).
- The 2026-06-02 amendment (simplifications 5 and 6) cascades through the
  same artifact family enumerated above — PRD, DESIGN, domain-model,
  plugin-spi, sdk-trait, DECOMPOSITION, features (`foundation.md`,
  `metric-lifecycle.md`, `usage-emission.md`), and the OpenAPI YAML
  (`usage-collector-v1.yaml`) — each of which must be revised in lockstep so
  that `kind` is presented as derived from the `gts_id` prefix (no separate
  trait, no catalog column) and `metadata_schema` is replaced by closed
  `metadata_fields: Vec<String>` (declared keys only, all values typed as
  string). Phases 2-9 of the
  `update-usage-collector-flatten-metadata-and-kind-prefix` plan execute that
  cascade artifact-by-artifact; Phase 10 performs the final consistency
  sweep.
- PRD §5.1 free-form-extras guarantee is dropped. The previously-promised
  "arbitrary-context extras" surface in PRD §5.1 is removed as an explicit
  consequence of the closed-`metadata_fields` simplification: undeclared keys
  are validation errors, not silently-preserved extras. Downstream cascade
  phases inherit this breakage and must rewrite PRD §5.1 in lockstep.
- The `jsonschema` runtime dependency that ADR-0010 introduced for the
  open-but-typed metadata schema is removed. Closed `metadata_fields` is
  validated by a small in-tree check (declared-keys membership + string
  type), so the gateway L1 validator no longer needs a Draft-07 schema
  validator on the hot path. DECOMPOSITION drops the `jsonschema` crate
  dependency and the lift of the merge core (per ADR-0010 "Code reuse — lift,
  do not depend") in lockstep.

### Amendment 2026-06-02 — kind from prefix and closed metadata_fields

This amendment extends the four original simplifications above (1-4) with two
further simplifications (5-6), without rewriting them. Status remains
`accepted`; ADR-0010 remains superseded and is not edited. The original four
bullets above are untouched.

**Rationale.** GTS guide §2.2 already encodes parent linkage in the `gts_id`
prefix as a `~`-separated, left-to-right inheritance chain (the leftmost
segment is the parent base type; the rightmost segment is the leaf), so a
separately-declared `kind` trait on the catalog row carries no information
the prefix does not already pin. Replacing the open Draft-07
`metadata_schema` with a closed `metadata_fields: Vec<String>` cuts the
`jsonschema` runtime dependency, simplifies validation to a declared-keys
membership check, and removes the open-extras attack surface (undeclared
keys are now validation errors instead of silently-preserved extras). Both
simplifications align the v1 surface with the quota-reporting consumer
narrowing pinned by commit `783abdda`.

**Pinned invariants (for downstream cascade phases to quote verbatim):**

- **Simplification 5 — `kind` derived from `gts_id` prefix.** The catalog
  carries no `kind` column and the metric specification declares no `kind`
  trait. `kind ∈ {counter, gauge}` is derived from the leftmost
  `~`-separated base-type segment of a registered metric's `gts_id`. The two
  reserved base kind type identifiers are
  `gts.cf.core.usage.counter.v1~` and `gts.cf.core.usage.gauge.v1~`; every
  registered metric's `gts_id` MUST begin with exactly one of those two
  prefixes. A `gts_id` that does not begin with one of the two reserved
  prefixes is a registration validation error.
- **Simplification 6 — closed `metadata_fields` replaces open
  `metadata_schema`.** The catalog declares a closed list of metadata keys
  per metric as `metadata_fields: Vec<String>`. Only declared keys are
  accepted at ingest; undeclared keys are validation errors. There is no
  free-form remainder, no `additionalProperties: true` escape hatch, and no
  `extras` map. All values are typed as `String` at the SPI / validation
  layer (the catalog declares keys; values are conveyed as strings
  end-to-end). The Draft-07 JSON-Schema surface and the `jsonschema` runtime
  dependency are removed.

**Cascade scope.** This amendment cascades through PRD, DESIGN,
domain-model, plugin-spi, sdk-trait, DECOMPOSITION, features
(`foundation.md`, `metric-lifecycle.md`, `usage-emission.md`), and the
OpenAPI YAML (`usage-collector-v1.yaml`). The PRD §5.1 free-form-extras
guarantee is dropped as an explicit cost; downstream cascade phases inherit
this breakage and must rewrite PRD §5.1 in lockstep. Phases 2-9 of the
`update-usage-collector-flatten-metadata-and-kind-prefix` plan execute the
artifact-by-artifact cascade; Phase 10 performs the final consistency sweep.

### Confirmation

Compliance is confirmed through (a) cross-artifact `cfs --json validate` PASS
across every modified usage-collector artifact at the end of Phase 11; (b) the
downstream phase handoffs (`out/phase-02-prd-impact.md` through
`out/phase-10-openapi-changes.md`) producing matching change summaries that
each cite the four simplifications above verbatim; (c) PR review on branch
`usage-collector/simplified-specs` once the full cascade lands, confirming
that no artifact still references the local-from-config catalog, the
uuid5-from-type derivation, `parent_type_uuid`, `x-uc-indexable`, or
`abstract`.

## Pros and Cons of the Options

### Keep both catalogs plus all current attributes

Preserve the gateway-local-from-config catalog (ADR-0007 / 0009) alongside
the plugin-DB catalog, keep usage records referencing metrics via the
uuid5-derived UUID, and retain `parent_type_uuid`, `x-uc-indexable`, and
`abstract` on the metric specification unchanged.

- Good, because the option requires no specification changes and no cascade
  rework; the current state is preserved as-is.
- Good, because every attribute that exists has a documented rationale in its
  originating ADR (0007 / 0009 / 0010); the option keeps those rationales in
  force without further interpretation.
- Bad, because two catalog sources of truth remain in the specification —
  operators must reason about when each applies and how they reconcile, and
  documentation must explain both.
- Bad, because usage records continue to reference metrics via a derived
  UUID, which every consumer that wants to join usage rows back to catalog
  rows must re-derive from the type id; mismatches between derivations are
  silent and corrupt downstream aggregation.
- Bad, because `parent_type_uuid`, `x-uc-indexable`, and `abstract` enlarge
  the spec and SDK surface without a v1 consumer named in commit `783abdda`'s
  narrowed scope; cost is paid speculatively against deferred capabilities.

### Plugin-DB catalog only with gts_id referencing and dropped attributes

The plugin-DB catalog (managed via SDK/REST) is the sole metric catalog;
usage records reference metrics via `gts_id`; the metric specification drops
`parent_type_uuid`, `x-uc-indexable`, and `abstract`.

- Good, because there is exactly one place where metrics exist: the plugin
  database, mutated via the gateway's SDK/REST surface under PDP
  authorization. Operator and consumer mental model collapses to one shape.
- Good, because the usage-record metric reference is the same `gts_id` string
  that appears on every catalog row; no derivation is needed at any consumer
  to join the two, and no class of "we derived the UUID slightly differently"
  bugs can occur.
- Good, because the specification surface shrinks to match the v1
  quota-reporting consumer narrowing in commit `783abdda`; v1 ships only what
  that consumer requires.
- Good, because the GTS schema identity (the `gts_id` already stored on every
  metric type schema per the platform GTS pattern) is the natural reference
  key; the simplification aligns the wire shape with the platform invariant
  rather than papering over it with a derivation.
- Bad, because every artifact in the usage-collector specification family
  carries language about the local catalog, the uuid5 derivation, or the
  dropped attributes; the cascade rework across PRD, DESIGN, DECOMPOSITION,
  three FEATUREs, three companion docs, and the OpenAPI YAML is real work
  (Phases 2-10 of the plan).
- Bad, because `x-uc-indexable` as a documented hint to plugin authors is
  lost; plugin authors choosing an indexing strategy must derive their hint
  list from the metric's metadata schema rather than reading the hint
  directly off the spec.
- Bad, because ADRs 0007, 0009, and 0010 must be marked superseded, with
  status-header edits and forward pointers on each (this ADR's Phase 1
  artifact-edit work covers exactly that).
- Neutral, because future capabilities (inheritance, indexability hints,
  abstract markers) are not foreclosed — they may be reintroduced by a
  dedicated ADR that names its concrete consumer at the time of need.

### Keep the local-from-config catalog only

Remove the plugin-DB catalog surface, keep the gateway-local-from-config
catalog as the sole source, and retain or simplify the attributes around it.

- Good, because the gateway carries no plugin SPI catalog surface; plugin
  authors implement only the usage-record path.
- Good, because catalog mutation requires no PDP round-trip — it happens at
  boot from configuration files under whatever access controls the deploy
  environment provides.
- Bad, because runtime metric registration via SDK or REST disappears;
  downstream operators that depend on registering metrics dynamically lose
  that capability, which contradicts the runtime-registration story already
  shipped in the public API and the SDK trait.
- Bad, because the catalog has no durable persistence beyond the
  configuration file; recovering from operator misconfiguration requires
  editing files and restarting, rather than calling a REST endpoint.
- Bad, because the option reintroduces exactly the ADR-0007 status quo that
  ADR-0009 already reverted on referential-integrity grounds; the underlying
  rationale for ADR-0009 has not changed and would re-apply.

## More Information

- ADR-0007 (`cpt-cf-usage-collector-adr-gateway-local-metric-catalog`) — superseded by this ADR. Introduced the gateway-local catalog loaded from configuration.
- ADR-0009 (`cpt-cf-usage-collector-adr-catalog-plugin-referential-integrity`) — superseded by this ADR. Restored the plugin-DB catalog surface and the referential-integrity FK; this ADR retains the plugin-DB catalog as the sole catalog and removes the local-from-config catalog entirely.
- ADR-0010 (`cpt-cf-usage-collector-adr-gts-typed-metric-metadata`) — superseded by this ADR. Introduced GTS-typed metric metadata including `parent_type_uuid`, `x-uc-indexable`, and `abstract`; this ADR drops those three attributes from the metric specification. The 2026-06-02 amendment further simplifies the two remaining elements of ADR-0010's surface that this ADR had initially preserved: `kind` (formerly a GTS trait under `x-gts-traits` per ADR-0010) is now derived from the `gts_id` prefix and is no longer a declared trait or catalog column; the open Draft-07 `metadata_schema` (formerly the gateway L1 validation surface) is replaced by a closed `metadata_fields: Vec<String>` with declared-keys-only membership and all-string values. ADR-0010 remains read-only and is not re-edited.
- Commit `783abdda docs(usage-collector): narrow downstream consumer to quota reporting` — narrows the v1 downstream consumer to quota reporting; the simplification adopted here matches the spec surface to that narrowed scope.
- Commit `03f177d9 docs(usage-collector): model metrics as GTS schemas with typed per-metric dimensions` — pins metrics as GTS Type Schemas with a `gts_id`; the `gts_id`-as-reference decision in this ADR sits directly on top of that modelling commit.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
