---
status: accepted
date: 2026-07-23
decision-makers: Constructor Fabric Steering Committee
---

# Federated Registry Source Routing and Query Strategy

**ID**: `cpt-cf-types-registry-adr-federated-source-routing-query`

## Table of Contents

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
* Each Registry Source Plugin declares one or more validated source claims and a priority. A source claim is a GTS Identifier pattern and nothing more: it carries no entity-kind restriction, because the trailing `~` of an identifier already determines its kind and overlap is checked over the identifier space regardless of kind.
* Active source claims cannot overlap another active source claim or the managed identifier space. Priority determines consultation order; it never authorizes shadowing.
* Plugins are ordered by `(priority ASC, plugin GTS Instance Identifier ASC)`.
* Exact GTS Identifiers are routed to the single source whose claim matches the identifier. ADR-0011 constrains that routing:
  * a claim is a **rooted single-segment** wildcard pattern: it contains no `~` and carries the wildcard at a token boundary (`gts.<vendor>.*` through `gts.<vendor>.<package>.<namespace>.<type>.*`);
  * its wildcard accepts the rest of the identifier, including `~` chain separators, so the claim matching the first GTS chain segment owns every identifier chained beneath it and an externally managed derivation chain stays within one source; and
  * activation rejects a multi-segment claim because it could slice into a chain whose base is managed.
* Opaque Registry References that are not managed locally are resolved through the ordered plugin chain because the UUID does not encode its source.
* Wildcard queries select every source whose claim intersects the requested pattern.
* Federated lists use source-major order: managed storage first, followed by matching plugins in resolver order. Global field ordering across sources is not supported by this model.
* Query assistance returns a complete, bounded Registry Reference set or fails; a partial expansion is never a usable domain query constraint.
* A source failure remains distinct from `NOT_FOUND` for the affected exact or batch key; unaffected batch keys may still succeed. List, search, and query operations that require a complete page or set fail as a whole rather than reinterpret failure as exhaustion or partial success.
* The platform federation contract is total: an active source implements all of it across its whole claimed identifier space, so there is no capability set to declare at registration and none to check at activation. The contract requires:
  * batch forward and reverse resolution, retaining reverse resolution after deletion;
  * complete bounded candidate queries with opaque pagination;
  * lifecycle, ownership/visibility, and tenant-state assertions;
  * revision/hash and conditional-read semantics;
  * structured source failures; and
  * for a Type Schema result — an identifier with a trailing `~` — resolved effective schema and trait artifacts, because Types Registry does not compute them for external content and consumers cannot obtain them otherwise (ADR-0002).
* ADR-0011 keeps two operations out of the contract rather than making them optional:
  * dependency registration toward managed identifiers is excluded because ADR-0011 leaves no cross-boundary dependency to register;
  * reverse dependency-impact lookup is excluded because it could report only external dependents of an external entity, while Types Registry exposes no dependent-enumeration operation and mutation Dry Run answers the actionable impact question.
* Every applicable listed obligation is mandatory and authoritative throughout the claimed space. A claim covers both entity kinds in it; a source holding none of one kind answers `NOT_FOUND` for that kind, exactly as for any other absent identifier. The contract has no optional or advisory tier and no output may degrade to a warning instead of failing closed. Conformance is established on every response — a non-conforming or incomplete result is rejected rather than interpreted — and by conformance tests, never by an activation-time check against a plugin's own declaration.
* Registry Source Plugins are read-only with respect to Types Registry state.
* Candidate-query implementations may over-return for platform filtering, but cannot introduce false negatives or weaken correctness.

P1 adds neither a routing memo nor a circuit breaker:

* A `uuid → owning plugin` memo helps only repeated positive resolution. It cannot cache the expensive negative case because a source may register the identifier without changing the routing generation; authoritative `NOT_FOUND` still requires every source to answer.
* An open circuit breaker must remain a source failure under fail-closed semantics, not become absence. Timeouts, concurrency limits, and per-source failure classification already provide resource protection.
* Batching keeps cost proportional to plugin count rather than reference count; claim counts are expected to remain single digits.

Measurement may reopen this choice. The benchmark profile must therefore fix plugin count and the share of references not resolved locally.

Construction details belong to [DESIGN](../DESIGN.md):

* §3.2, *Federation Router*: claim matching, ordering, response validation, pagination, query expansion, and failure mapping;
* §3.3, *Registry Source Plugin contract*: trait, models, and conditional reads; and
* §3.6: federated resolution and type-filter-expansion sequences.

### Consequences

* Types Registry does not need a per-external-entity routing or query index.
* UUID lookup latency grows with the number and ordering of plugins, so batch APIs and plugin-local caches are required for acceptable performance.
* Non-overlapping claims preserve global identity semantics but rule out overlay and failover-source models without a future ADR.
* Source-major ordering is deterministic and avoids a global merge, but it cannot provide global ordering by an entity field.
* Pagination correctness depends on stable plugin configuration and source cursor contracts.
* A kind-filtered query can no longer skip a source by declaration and passes the filter to every intersecting one. With single-digit claim counts the extra call is immaterial, and it removes a declaration the platform could not verify.
* Source-claim intersection becomes a platform prerequisite for wildcard routing, and it reduces to a pattern-containment test the platform GTS implementation is expected to provide, so no bespoke intersection algorithm is needed.
* A rooted claim captures every chained identifier beneath it, increasing the blast radius of a mis-specified claim. In exchange, overlap reduces to prefix containment.
* External and managed namespaces cannot nest, so vendor namespace planning must allocate disjoint roots.
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
* a source response that omits or degrades any output the contract requires for the returned identifier is rejected and the affected request fails closed, with no output exempt from that;
* a Source Claim is accepted as a bare GTS Identifier pattern, and a claim declaring an entity-kind restriction is rejected.

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
* Bad, because the plugin contract, its completeness guarantee, and cursor semantics become correctness-critical.
* Bad, because overlays and active/passive failover sources are excluded from P1.

### Encode source identity in Registry References

Registry References become source-qualified values instead of opaque UUIDs.

* Good, because reverse resolution can route directly to the owning source.
* Bad, because it changes the Registry Reference contract selected by ADR-0001.
* Bad, because persisted domain references become coupled to plugin identity and source replacement.
* Bad, because the reference ceases to represent one source-independent global GTS identity.

## More Information

ADR-0002 separately decides that externally managed definitions and tenant state remain source-owned and are delegated live rather than projected into Types Registry. The DESIGN allocations are listed under the decision above.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0011**: [0011-cpt-cf-types-registry-adr-managed-external-boundary.md](./0011-cpt-cf-types-registry-adr-managed-external-boundary.md) — bounds the contract: neither dependency registration nor reverse dependency-impact lookup is in it, everything that remains is mandatory, and the claim grammar is fixed as a rooted single segment.
- **ToolKit plugins**: [../../../../../docs/TOOLKIT_PLUGINS.md](../../../../../docs/TOOLKIT_PLUGINS.md)

This decision directly addresses:

* `cpt-cf-types-registry-fr-registry-federation` - selects deterministic federation without local external projections.
* `cpt-cf-types-registry-fr-registry-source-routing` - selects non-overlapping source claims and deterministic resolver order.
* `cpt-cf-types-registry-fr-id-resolution` - selects local-first forward and reverse resolution.
* `cpt-cf-types-registry-fr-type-query-assistance` - requires complete bounded source-major expansion.
* `cpt-cf-types-registry-fr-ref-tracking` - leaves the tracked dependency set entirely managed under ADR-0011, so no plugin operation contributes to it and none asks a source about dependents at all.
* `cpt-cf-types-registry-fr-tenant-availability` - preserves fail-closed behavior when an authoritative source is unavailable.
* `cpt-cf-types-registry-fr-cache-freshness-metadata` - makes plugin configuration revision and source cursors part of federation freshness.
