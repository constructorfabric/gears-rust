---
status: proposed
date: 2026-08-12
---

# Declarative GTS projection contracts for quota enforcement

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The contract model](#the-contract-model)
  - [Projection contents](#projection-contents)
  - [Caller attribution and authorization](#caller-attribution-and-authorization)
  - [Resolution, validation, and failure surface](#resolution-validation-and-failure-surface)
  - [Policy checking and bootstrap consistency](#policy-checking-and-bootstrap-consistency)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Opaque payload plus size cap only](#a-opaque-payload-plus-size-cap-only)
  - [(b) Registered and enforced projection contracts](#b-registered-and-enforced-projection-contracts)
  - [(c) Registered projections for documentation only](#c-registered-projections-for-documentation-only)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-projection-contracts`

## Context and Problem Statement

Quota Enforcement (QE) currently describes an evaluation request as a metric, an amount, and
two JSON objects: caller-authored `request.metadata` and operator-authored `quota.metadata`.
A byte-size limit is the only structural contract. The platform therefore cannot enumerate
which attributes a metric owner supplies, and a Policy expression such as
`quota.metadata.region == request.metadata.region` can silently miss because a key is absent,
misspelled, or of the wrong type. The request remains callable but is not governable.

The license-resolver precedent separates a Gear's domain model from a registered GTS
projection: an abstract core-owned base is refined by a derived, Gear-owned contract, and the
runtime value is validated against that contract. QE needs the same boundary while preserving
two existing properties: a single counter may be shared by several calling services, and the
subject id used for consumption is derived from `SecurityContext`, never asserted by the
caller.

## Decision Drivers

* The registry must answer which owner projections and metrics exist without reading Gear
  source.
* Cross-service consumption of one shared counter must remain expressible.
* Malformed requests must fail closed before Engine dispatch, distinctly from a legitimate
  `Denied` decision.
* Attribute references in operator CEL must be checked when a Policy is saved, not fail
  silently at runtime.
* Registry availability and latency must not enter the p95 ≤ 100 ms evaluation path.
* Caller-supplied data must not weaken server-derived subject identity or become an
  authorization substitute.
* Request and Quota attributes legitimately have different schemas and ownership.

## Considered Options

* (a) **Opaque payload plus size cap only** — retain the current unschematized metadata bags.
* (b) **Registered and enforced projection contracts** — derive concrete subject/resource
  projections from QE bases, validate request instances before Engine dispatch, and validate
  Quota attributes when Quotas are written.
* (c) **Registered projections for documentation only** — publish schemas but do not validate
  runtime values.

## Decision Outcome

Chosen option: **(b), registered and enforced projection contracts**.

### The contract model

QE owns three abstract (`x-gts-abstract`) base types — `gts.cf.core.qe.subj.v1~`,
`gts.cf.core.qe.res.v1~`, and `gts.cf.core.qe.quota_attrs.v1~` — plus the concrete
scope-discriminator type `gts.cf.core.qe.subject_scope.v1~`. Only concrete derived contracts
are instantiable. A representative subject projection is
`gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~`; an owner may publish both user- and
tenant-scope projections.

This replaces the illustrative `gts.cf.qe.subject.type.v1~cf.qe.subject.user.v1`, which has
three defects:

* its root is `cf.qe.*` rather than core-owned `cf.core.qe.*`;
* its last segment lacks the trailing `~`, making it an instance where QE needs a type;
* its derived segment is owned by QE rather than by the metric-owning Gear.

The decisions that follow:

* **The metric owner declares the projection — never each caller.** Any authorized caller may debit
  directly through QE using that projection; the owner publishes the contract but does not proxy
  calls. Caller-specific projections would fragment one shared budget into N counters.
* **The subject projection is the identity a Quota binds to.** Applicable-Quota lookup is
  keyed by `(projection_type, subject_id, metric)`; several Quotas may share that key.
* **Scope cascade requires every applicable owner projection to resolve** — the owner's
  `user.v1~` and `tenant.v1~`, for example — so user- and tenant-scoped Quotas reach the
  Engine together.
* **Only the projection type travels on the wire.** The subject `id` is always resolved
  server-side from `SecurityContext`, and consumption DTOs MUST NOT accept one. QE has no
  type-level subject operation, so the resolved id is required and non-nil; an anonymous
  context — including the nil UUIDs from `SecurityContext::anonymous()` — is rejected before
  projection resolution. This preserves
  `cpt-cf-quota-enforcement-principle-server-derived-identity` rather than importing
  license-resolver's optional caller-supplied id literally.
* **Callers pay an explicit projection cost**, copying the relevant fields out of their domain
  objects. That DTO-style boundary decouples domain evolution from contract evolution. An
  owner must therefore design a projection usable by every caller, and adding a required
  property is breaking for all of them.
* **Metric identity is unchanged.** Metrics stay registry-owned. Naming a metric beneath its
  owning Gear is a candidate for the open cross-gear namespace question, and Usage Collector
  may adopt the same model later, but neither is required here.

### Projection contents

The abstract bases carry the common properties `type`, `id`, and `metadata`:

| Property | At runtime |
|----------|------------|
| `type` | A `GtsTypeId` naming the concrete derived projection. |
| Subject `id` | Populated by QE after server-side resolution; never supplied by a consumption caller. |
| Resource `id` | Optional — the contract may describe a resource class or a concrete resource. Descriptive input only; not part of the P1 counter key. |
| `metadata` | The projection's extension object, refined by the derived type. |

* **`metadata` is required on the wire.** Omitting it is non-conforming and MUST NOT be
  defaulted to `{}`; callers send an empty object when the projection declares no properties.
* **Each subject projection declares its subject scope** as a required `scope` trait in the
  base's `x-gts-traits-schema`. Its value is a `GtsInstanceId` narrowed to
  `gts.cf.core.qe.subject_scope.v1~*`; P1 defines the well-known instances
  `gts.cf.core.qe.subject_scope.v1~cf.core.qe.user.v1` and
  `gts.cf.core.qe.subject_scope.v1~cf.core.qe.tenant.v1`. QE reads the registry-validated
  effective trait and compares the instance ids directly. Encoding scope in the type-id name
  segment would force string parsing, which `guidelines/GTS.md` conventions 14 and 15 rule out.
* **Each subject projection declares its admitted metrics** through an inherited
  `x-gts-traits` value whose entries are `GtsTypeId`s narrowed via `x-gts-ref` to the platform
  metric base. `x-gts-ref` is **pattern-level only** — it validates that the value is a
  well-formed GTS id under the declared prefix, and nothing more. Three checks it does *not*
  perform, all of which QE therefore owns:
  * that the referenced metric is registered — checked at bootstrap when the catalog is built;
  * that the referenced type is genuinely *derived* from the metric base, since a narrowed
    `x-gts-ref` is a prefix match, not a derivation check;
  * that debiting the metric is permitted — checked at Gateway ingress against the catalog.

  A request naming an unadmitted metric fails closed. The registry thereby becomes an
  inventory of which metrics are metered against which owner and scope. Narrowing the set is a
  breaking contract change.
* **A metric is admitted by at most one projection per `(metric, scope)` pair, per
  deployment.** One owner may own several metrics and one projection may admit several, so the
  constraint binds the pair, not the projection or the owner. Two projections admitting the
  same metric at the same scope would make the applicable-Quota set ambiguous and fragment one
  budget into two counters. QE rejects that at bootstrap rather than resolving it by
  precedence, because a silent winner would make the debited counter depend on configuration
  order. Distinct scopes of one owner admitting the same metric is the intended arrangement —
  it is what makes scope cascade expressible.
* **The resource projection carries identity plus schematized properties only.** It completes
  the request contract for resource-aware selection but does not enter the counter key. A
  Quota on a named resource is expressible today through properties; whether resource becomes
  a counter-key axis remains an Open Question.
* **Operator-authored `quota.metadata` is a separate contract family**, derived from the
  abstract base `gts.cf.core.qe.quota_attrs.v1~`. The metric owner publishes and versions it
  alongside its projections; operators populate instances at Quota create/update. The request
  projection schema MUST NOT be reused here — a Quota may carry `regions: string[]` against a
  request's `region: string`, plus arbitration fields such as `weight` with no request-side
  counterpart.
* **Contracts validate shape, not business meaning.** Semantic value rules and attribute
  interpretation stay with the Engine and Policy layer; QE core does not become an
  attribute-query engine. Contract documents and Policy configs must not carry secrets or
  sensitive runtime values.

### Caller attribution and authorization

* **`caller_type: GtsTypeId` is self-declared diagnostic metadata.** QE excludes it from
  Policy/Engine input and MUST NOT use it for authorization, Policy branching, quota
  apportionment, Quota selection, Debit Plans, or counter/allocation keys.
* **Permission to debit stays with PDP and `token_scopes`.** Caller-dependent enforcement requires
  a future server-derived, authenticated service identity; `caller_type` cannot provide one.

### Resolution, validation, and failure surface

Contracts are resolved and snapshotted **outside** the evaluation transaction:

* **Bootstrap** builds an immutable in-process `ProjectionContractCatalog` for the
  deployment's configured projections; Gateway validates evaluation requests against that local
  snapshot. `types-registry` remains authoritative; QE's catalogue is only a validated local
  snapshot. A new contract version becomes evaluable only in a deployment generation whose
  bootstrap catalogue includes it. P1 has no runtime refresh path.
* **`QuotaManagementService`** owns the `TypesRegistryClient` and a bounded LRU cache for
  metric, projection, and Quota-attribute contract lookups. Quota creation validates the
  registry contract, not the active evaluation catalogue, so replacement Quotas can be staged
  before cutover.
* **`PolicyService`** resolves the contracts a Policy references and snapshots their schema
  and version with the immutable Policy version.
* **Registration happens in `types-registry`.** QE gains no registration endpoint.

**`EvaluationOrchestrator` performs no live registry lookup, deliberately.** Four reasons, each
independently sufficient:

* The canonical pipeline — subject resolution → idempotency lookup → locked applicable-Quota
  read → Policy lookup → Engine evaluation → Debit-Plan validation → mutation →
  idempotency/outbox persistence → commit — has no registry step today.
* The `ProjectionContractCatalog` is process-local and the `TypesRegistryClient` cache belongs
  to Quota CRUD, so there is no hot-path cache to reuse.
* Hot-path metric-mode enforcement already comes from storage errors (`MetricNotRegistered` /
  `MetricNotQuotaGated`).
* A registry call would put an external dependency under the latency budget and, because QE
  fails closed, couple evaluation availability to registry availability.

This is consistent with `cpt-cf-quota-enforcement-adr-metadata-snapshot-timing`: stored Quota
attributes are captured with the locked Quota row.

| Surface | Validation point | Rule |
|---------|------------------|------|
| `quota.metadata` | Quota create/update | Resolve the metric owner's snapshotted Quota-attribute contract and validate once before persistence. Stored metadata is not revalidated during evaluation. |
| Request subject/resource projections and `caller_type` | Gateway ingress of every write and preview operation, including each batch item | Require registered concrete types, required `metadata`, schema conformance, non-nil server-derived subject id, and admitted metric. Structurally validate `caller_type`, then exclude it from Policy/Engine input. |

A request with an unregistered projection, schema mismatch, missing metadata, or inadmissible
metric maps to `InvalidArgument` / HTTP 400 with a stable field-level reason. It returns a
platform-canonical error, never `Decision::Denied`; the two surfaces remain mutually exclusive.
A Quota/Policy referring to a contract that exists but is not usable in that lifecycle state
maps to `FailedPrecondition`. No new response envelope is introduced.

### Policy checking and bootstrap consistency

**At Policy create/update**, `PolicyService` hands the Engine validator the snapshotted request
and Quota-attribute schemas. The `cel` validator parses and type-checks property and projection
references, returning line/column diagnostics. It also checks relationships per-contract
validation cannot prove:

* paired-key type disagreement;
* non-intersecting value domains, which make a Quota permanently inert;
* operator/cardinality mismatch — comparing `quota.regions: string[]` with
  `request.region: string` using `==` rather than membership.

**At bootstrap**, QE registers its abstract bases, the scope-discriminator type and its P1
well-known instances, then validates a closed consistency set:

* every compiled `SubjectProjectionResolver` names a registered, concrete owner projection
  derived from the QE subject base;
* every projection configured for resolution has exactly one resolver;
* every admitted metric reference resolves to a registered type that is genuinely derived from
  the metric base — neither check is covered by `x-gts-ref`, which is pattern-level only;
* no two configured projections admit the same metric at the same declared scope;
* no resolver accepts an anonymous/nil identity.

QE reads each projection's registry-validated effective `scope` trait and compares its
`GtsInstanceId` directly.

Any mismatch fails gear bootstrap. Owner projections not configured in that deployment stay
discoverable and may receive pre-staged Quotas, but are not evaluated by that generation.

### Consequences

**Compatibility rule.** Adding an optional property is non-breaking. Remove / rename / retype /
narrow — including narrowing the admitted-metric set — requires a new contract version. Adding
a *required* property likewise requires a new version, because every existing caller populates
the owner's contract.

**Breaking-version replacement.** QE provides no projection alias or Quota/counter migration verb.
Replacement Quotas get new ids and counters, with no carried-forward consumption. P1 uses this
ordered procedure:

1. Register the replacement contracts, Quotas, and Policy drafts, then verify them while the old
   generation serves.
2. At a consumption-period boundary, stop old-generation admission and drain in-flight evaluations.
   This cutover barrier MUST NOT exceed 30 seconds and counts against the availability budget.
3. Atomically activate the complete set of affected Policy versions under expected-version checks.
   Missing, extra, invalid, or conflicting replacements abort the transaction.
4. Route traffic only after the new generation's catalogue, Quotas, and active Policies form one
   compatible set. On failure or timeout, restore the old Policy set and routing before admission.
5. Deactivate the old Quotas; allocation deactivation resolves active leases.

Mixed-generation evaluation traffic is forbidden because the generations use independent counters.
Replacement at scale depends on P2 Bulk Quota CRUD.

* Attribute mistakes become save-time or ingress errors instead of silent Quota-selection
  misses.
* Cross-service shared counters remain intact because the metric owner supplies the stable
  projection identity.
* Callers take on projection mapping and contract-version upgrades.
* Quota writes and Policy writes depend on registry availability; the evaluation hot path does
  not.
* Quota attributes remain a separate operator-owned instance surface, with snapshot timing
  unchanged.
* This is the inbound counterpart of the strict Engine boundary in
  `cpt-cf-quota-enforcement-adr-evaluation-engine`: Engines receive only validated inputs, and
  QE still validates their outputs before mutation.

### Confirmation

Confirmed by documentation review against the GTS conventions and the license-resolver
base/derived implementation, then by implementation tests when code lands: type/instance and
abstract-base checks; missing-metadata rejection; server-derived, non-nil subject resolution;
admitted-metric rejection; Quota write validation without hot-path revalidation; Policy
pair-check diagnostics; bootstrap mismatch failure; and proof that two callers using one
owner's projection resolve the same shared applicable-Quota set.

## Pros and Cons of the Options

### (a) Opaque payload plus size cap only

* Good, because it adds no registry or projection work.
* Good, because callers may send arbitrary fields without contract versioning.
* Bad, because the metering surface cannot be discovered or reviewed from the registry.
* Bad, because key, type, and cardinality mismatches silently change selection behavior.
* Bad, because every Engine must improvise validation and diagnostics.

### (b) Registered and enforced projection contracts

* Good, because the request and operator surfaces are discoverable, versioned, and
  enforceable.
* Good, because owner-declared identity preserves one shared counter across calling services.
* Good, because Policy property references and pair compatibility are checked before
  activation.
* Good, because validation failures are uniform fail-closed errors ahead of Engine dispatch.
* Bad, because owners govern schemas and every caller maintains projection mappings.
* Bad, because Quota and Policy writes acquire a registry dependency and schema-cache
  lifecycle.

### (c) Registered projections for documentation only

* Good, because it provides a registry inventory with less runtime work than option (b).
* Bad, because nothing keeps the published schema and actual payload aligned.
* Bad, because it retains option (a)'s silent selection failures while presenting false
  contract confidence.
* Bad, because an unvalidated contract is not a contract.

## More Information

The naming, one-level derivation, traits, abstract types, and typed-id rules come from
[`guidelines/GTS.md`](../../../../../guidelines/GTS.md). The reference decisions are
[`license-resolver` ADR-0001](../../../license-resolver/docs/ADR/0001-cpt-cf-license-resolver-adr-gts-resource-identity.md)
and
[`license-resolver` ADR-0003](../../../license-resolver/docs/ADR/0003-cpt-cf-license-resolver-adr-typed-licensing-contracts.md).
The reference SDK in PR #4458 confirms abstract bases, required wire `metadata`, `GtsTypeId`
fields, and `x-gts-ref` narrowing; QE intentionally differs by keeping subject id
server-derived and all registry access off its hot path.

license-resolver registers a contract through the module that wants enforcement for that
resource, and it holds no shared state — so it never had to separate the party that declares a
contract from the party that calls it. QE does: several services debit one counter, so the
declaring party is pinned to the metric owner and a metric is admitted at one scope by one
projection. That is an extension of the pattern to a stateful gear, not a departure from it.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-quota-enforcement-fr-projection-contracts`
* `cpt-cf-quota-enforcement-fr-contract-validation`
* `cpt-cf-quota-enforcement-fr-subject-type-registry`
* `cpt-cf-quota-enforcement-fr-attribute-based-quota-selection`
* `cpt-cf-quota-enforcement-fr-quota-resolution-policy`
* `cpt-cf-quota-enforcement-principle-declarative-projection-contracts`
* `cpt-cf-quota-enforcement-principle-server-derived-identity`
* `cpt-cf-quota-enforcement-principle-strict-engine-boundary`
* `cpt-cf-quota-enforcement-adr-metadata-snapshot-timing`
* `cpt-cf-quota-enforcement-adr-evaluation-engine`
