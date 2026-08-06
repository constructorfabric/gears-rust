---
status: accepted
date: 2026-07-23
decision-makers: Constructor Fabric Steering Committee
---

# Federated Registry Source Routing and Query Strategy

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Persist a central external routing index](#persist-a-central-external-routing-index)
  - [Query all plugins in parallel](#query-all-plugins-in-parallel)
  - [Ordered resolver chain with source claims](#ordered-resolver-chain-with-source-claims)
  - [Encode source identity in Registry References](#encode-source-identity-in-registry-references)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-types-registry-adr-federated-source-routing-query`

## Context and Problem Statement

ADR-0002 selects live delegation for Externally Managed Entities. Types Registry therefore needs a deterministic way to select Registry Source Plugins for exact identifiers, resolve opaque Registry Reference UUIDs whose source is unknown, and query patterns that may span managed storage and several plugins without introducing a central projection of external entities.

The routing model must preserve the global GTS namespace, produce complete results, and distinguish authoritative absence from source failure. It should also avoid requiring every source to participate in every exact lookup or introducing global merge semantics across heterogeneous sources.

## Decision Drivers

* Managed entities must resolve locally without plugin calls.
* External UUID resolution must work without a central UUID-to-source index.
* Priority must provide deterministic lookup order without permitting identifier shadowing.
* Batch operations must avoid one plugin call per entity.
* Federated queries must not silently omit results or present failed sources as authoritative absence.
* Every plugin result must be verifiable against the global GTS identity and Registry Reference invariants.
* The first federation model should avoid global k-way sorting across heterogeneous plugins.

## Considered Options

* Persist a central external routing index.
* Query all plugins in parallel.
* Use an ordered resolver chain with source claims.
* Encode source identity in Registry References.

## Decision Outcome

Chosen option: "ordered resolver chain with non-overlapping source claims", because it preserves opaque global Registry References, avoids a central projection of external identity mappings, routes exact identifiers to one owning source, and provides deterministic federation semantics.

The architectural rules of the selected model are:

* Managed storage is consulted first.
* Each Registry Source Plugin declares validated source claims, served entity kinds, and a priority.
* Active source claims cannot overlap another active source claim or the managed identifier space. Priority determines consultation order; it never authorizes shadowing.
* Plugins are ordered by `(priority ASC, plugin GTS Instance Identifier ASC)`.
* Exact GTS Identifiers are routed to the single source whose claim matches the identifier. Under ADR-0011 a claim pattern is a **rooted single-segment** wildcard pattern: exactly one segment, no `~`, with the wildcard at a token boundary inside it (`gts.<vendor>.*` through `gts.<vendor>.<package>.<namespace>.<type>.*`). The owning claim of an identifier is therefore selected from its **first segment alone**, and because a wildcard segment accepts every remaining segment including the chain separator, an externally managed entity's whole derivation chain lies inside one claim and is served by one plugin. That is what makes the managed and externally managed identifier spaces disjoint rather than merely rule-separated. A multi-segment claim is rejected at activation, because it would slice into a chain whose base segment may be managed.
* Opaque Registry References that are not managed locally are resolved through the ordered plugin chain because the UUID does not encode its source.
* Wildcard queries select every source whose claim intersects the requested pattern.
* Federated lists use source-major order: managed storage first, followed by matching plugins in resolver order. Global field ordering across sources is not supported by this model.
* Query assistance returns a complete, bounded Registry Reference set or fails; a partial expansion is never a usable domain query constraint.
* A required source failure is not `NOT_FOUND`, source exhaustion, or a partial success. Operations that require a complete result fail closed.
* An active source must satisfy the complete platform capability contract for every entity kind it claims. For a claimed Type Schema kind that contract includes producing the resolved effective schema and the effective trait artifacts, because Types Registry will not compute them for external content and a consumer has no way to obtain them otherwise (ADR-0002). ADR-0011's closed boundary bounds that contract by removing two candidate capabilities outright rather than by grading them. Dependency registration toward managed identifiers is not a capability at all, because the boundary leaves nothing to register. Neither is reverse dependency-impact lookup — not merely because managed deletion is decided from local state, but because the boundary empties it and nothing consumes what is left: it could only ever report external dependents of an externally managed entity, never anything about a Managed Entity, and Types Registry exposes no operation on either plane that enumerates dependents, since a caller asking what a change would break is answered by the Dry Run of that mutation. **The profile therefore has no optional or advisory tier**: every capability in it is mandatory for each claimed entity kind and authoritative in its result, so no plugin output degrades with a warning in place of failing closed. That is one rule to conform to rather than two, and it leaves nothing that a plugin author must read the contract to discover they may skip. Registry Source Plugins are read-only with respect to Types Registry state. Optional optimizations may over-return candidates for platform filtering, but cannot introduce false negatives or weaken correctness.

The ordered walk carries no memo and no circuit breaker, and P1 adds neither. A `uuid → owning plugin` memo would speed up only a repeated **positive** resolution, while the case that costs most is the negative one — a reference held by no source walks the whole chain, because `NOT_FOUND` requires every source to answer authoritatively. That case cannot be memoized: a source may register the identifier at any time and nothing signals it, since the routing generation moves only when claims do. A circuit breaker changes no outcome either, because under fail-closed an open breaker must still yield a source failure rather than absence; it is resource protection, and timeouts, concurrency limits, and per-source failure classification already live in the plugin client adapter. Batching keeps the cost proportional to the number of plugins rather than to the number of references, and claim counts are single digits by design. Revisit if measurement against the benchmark profile shows otherwise; that profile must therefore fix the plugin count and the share of references not resolved locally.

The detailed plugin capability profile, resolution algorithms, pagination contract, continuation-token contents, response validation, query-assistance expansion, and failure outcomes are specified in [DESIGN](../DESIGN.md): §3.3, *Registry Source Plugin contract*, for the trait, its models, and the obligations a conditional read puts on a plugin; §3.2, *Federation Router*, for claim matching, ordering, and the platform invariants every response is checked against; and §3.6 for the federated resolution and type-filter-expansion sequences.

### Consequences

* Types Registry does not need a per-external-entity routing or query index.
* UUID lookup latency grows with the number and ordering of plugins, so batch APIs and plugin-local caches are required for acceptable performance.
* Non-overlapping claims preserve global identity semantics but rule out overlay and failover-source models without a future ADR.
* Source-major ordering is deterministic and avoids a global merge, but it cannot provide global ordering by an entity field.
* Pagination correctness depends on stable plugin configuration and source cursor contracts.
* Source-claim intersection becomes a platform prerequisite for wildcard routing, and it reduces to a pattern-containment test the platform GTS implementation is expected to provide, so no bespoke intersection algorithm is needed.
* A rooted claim is broader than a complete-identifier claim, so it captures every chained identifier beneath it and the blast radius of a mis-specified claim grows. In exchange, claim overlap becomes trivially decidable — for this grammar the platform matcher's containment test reduces to one field list being a prefix of the other — and a vendor's external namespace and the managed identifier space cannot nest, which vendor namespace planning must account for.
* Registry Source Plugins must provide a richer, completeness-preserving registry contract than ordinary ToolKit plugins — but a read-only one.

### Confirmation

This decision is confirmed when:

* managed entities resolve without plugin calls;
* exact identifiers select at most one matching source;
* overlapping source claims and managed/source namespace conflicts are rejected;
* unresolved UUID batches consult each plugin at most once;
* invalid UUID-to-GTS-ID mappings and out-of-claim results are rejected;
* source-major traversal is deterministic across managed storage and plugins;
* query assistance never returns a partial Registry Reference set;
* `NOT_FOUND` is returned only after every source required to establish absence answers authoritatively;
* a Source Claim is rejected at activation unless its pattern is a rooted single segment carrying a wildcard at a token boundary, and a claim also covers every identifier chained beneath what it covers;
* Types Registry exposes no plugin-callable operation that creates, modifies, or withdraws registry state;
* conformance tests prove that external candidate query results have no false negatives;
* a plugin missing any capability of the profile cannot activate a claim for the entity kind that needs it, and no capability is exempt from that — there is no output a plugin may omit or degrade.

## Pros and Cons of the Options

### Persist a central external routing index

Types Registry stores external UUID-to-source and GTS-ID-to-source mappings while plugins retain entity content.

* Good, because UUID reverse resolution can select one source directly.
* Good, because lookup latency does not grow with the number of plugins.
* Bad, because Types Registry acquires externally owned identity state and synchronization responsibilities.
* Bad, because stale routing data can disagree with the authoritative source.
* Bad, because imports, source replacement, and disaster recovery must preserve and reconcile the index.

### Query all plugins in parallel

Types Registry broadcasts every unresolved UUID and federated query to every source and merges the responses.

* Good, because it does not require source ordering for lookup latency.
* Good, because independent sources can be queried concurrently.
* Bad, because every request loads every plugin even when one source owns the identifier.
* Bad, because conflicts and partial source failures require complex merge semantics.
* Bad, because global sorting and pagination require a stable k-way merge contract.

### Ordered resolver chain with source claims

Types Registry checks managed storage first, routes exact identifiers by non-overlapping claims, and consults plugins in deterministic order when the source cannot be inferred from an opaque reference.

* Good, because exact identifiers normally select one plugin without broadcasting.
* Good, because Types Registry does not persist external entity mappings or query projections.
* Good, because non-overlapping claims preserve one global owner for every identifier.
* Good, because source-major traversal provides deterministic pagination without global sorting.
* Bad, because opaque UUID lookup latency grows with the number of consulted plugins.
* Bad, because plugin capability, completeness, and cursor contracts become correctness-critical.
* Bad, because overlays and active/passive failover sources are excluded from P1.

### Encode source identity in Registry References

Registry References become source-qualified values instead of opaque UUIDs.

* Good, because reverse resolution can route directly to the owning source.
* Bad, because it changes the Registry Reference contract selected by ADR-0001.
* Bad, because persisted domain references become coupled to plugin identity and source replacement.
* Bad, because the reference ceases to represent one source-independent global GTS identity.

## More Information

The normative design elaboration is [DESIGN](../DESIGN.md) §3.2 and §3.3, as listed under the decision above. ADR-0002 separately decides that externally managed definitions and tenant state remain source-owned and are delegated live rather than projected into Types Registry.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0011**: [0011-cpt-cf-types-registry-adr-managed-external-boundary.md](./0011-cpt-cf-types-registry-adr-managed-external-boundary.md) — bounds the capability profile: neither dependency registration nor reverse dependency-impact lookup is in it, every capability that remains is mandatory, and the claim grammar is fixed as a rooted single segment.
- **ToolKit plugins**: [../../../../../docs/TOOLKIT_PLUGINS.md](../../../../../docs/TOOLKIT_PLUGINS.md)

This decision directly addresses:

* `cpt-cf-types-registry-fr-registry-federation` - selects deterministic federation without local external projections.
* `cpt-cf-types-registry-fr-registry-source-routing` - selects non-overlapping source claims and deterministic resolver order.
* `cpt-cf-types-registry-fr-id-resolution` - selects local-first forward and reverse resolution.
* `cpt-cf-types-registry-fr-type-query-assistance` - requires complete bounded source-major expansion.
* `cpt-cf-types-registry-fr-ref-tracking` - leaves the tracked dependency set entirely managed under ADR-0011, so no plugin capability contributes to it and none asks a source about dependents at all.
* `cpt-cf-types-registry-fr-tenant-availability` - preserves fail-closed behavior when an authoritative source is unavailable.
* `cpt-cf-types-registry-fr-cache-freshness-metadata` - makes plugin configuration revision and source cursors part of federation freshness.
