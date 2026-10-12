---
status: accepted
date: 2026-09-30
---

# ADR-0002: Construct uses graph storage as it is

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Use graph storage as it is](#use-graph-storage-as-it-is)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-construct-adr-graph-storage-as-is`

## Context and Problem Statement

Construct stores each subject's profile graph in [graph storage](../../../graph-storage/docs/DESIGN.md). Graph storage in its v1 has fixed properties:

- A batch of writes lands whole or not at all. This is a batch-atomic write.
- Each node write may carry an expected version. This is compare-and-set: the write succeeds only if the stored version still matches. A mismatch rejects the whole batch.
- Deletes are soft deletes. A soft delete leaves a tombstone, a marker that hides the node from every read.
- A tombstoned node key cannot be reused before purge (the final removal of soft-deleted data). Purge and tenant offboarding come later in graph storage.
- Graph storage embeds text on ingest (a write into graph storage). An embedding is a vector made from text for search.
- Node keys come from the producer and are unique per tenant.
- Every ingest request carries a tenant- and producer-scoped idempotency key. An identical retry returns the recorded outcome without touching graph state.

A few Construct terms are used below. A plan is the set of changes Construct builds in memory for one record or one MCP (Model Context Protocol) call. The planner is the component that builds it with a language model. The Profile reader is Construct's one read path for the profile. The profile is a tree with the subject node as its root, and the profile version is the root node's version. A sensitive-data check gives a verdict on each value a plan would store: block, redact or allow. Admission is the set of deterministic checks on a plan, and an admitted plan is one that passed them. [DESIGN.md](../DESIGN.md), sections 3.1 and 3.2, defines them.

How does Construct work with these properties?

## Decision Drivers

- **Whole plan or nothing.** A plan lands whole or not at all.
- **No mixed records.** Two records for one subject never mix.
- **Verdicts before storage.** Nothing reaches storage before the sensitive-data verdicts are enforced, because graph storage embeds text on ingest.
- **Read rules.** Graph storage cannot enforce Construct's read rules.
- **Soft delete only.** Graph storage only soft-deletes in v1, and a deleted node key cannot be reused before purge.

## Considered Options

This decision is constrained, and no other option is compared here.

- Use graph storage as it is

## Decision Outcome

Chosen option: "Use graph storage as it is", because its batch-atomic write and its compare-and-set meet the first two drivers. Construct handles the verdicts and the read rules itself, and it accepts the soft-delete limit with the consequences listed below.

Construct writes a plan only if the profile has not changed since the planner read it. On a conflict, graph storage rejects the write and the planner runs again, up to a small limit.

### Consequences

- Construct applies each admitted plan in one write, with the profile version the plan was built on.
- Construct enforces the sensitive-data verdicts before any write.
- The Profile reader applies the read rules, so applications never read graph storage directly.
- Re-adding a deleted entity needs a new node key.
- Removal from all storage within 30 days depends on graph storage's purge, which comes after its v1. So Construct's first stable release needs graph storage's purge and tenant offboarding. Only then do erasure, retention and tenant exit meet the 30-day removal.
- Tenant exit follows the platform's tenant offboarding protocol. Graph storage's own offboarding comes after its v1.
- Every write that changes a profile also writes the root node with its expected version. So graph storage's per-node compare-and-set covers the whole profile.
- Each write carries graph storage's idempotency key, and a plan's node keys are fixed when the plan is built. So storing the same changes again gives the same facts.

### Confirmation

- A design review checks [DESIGN.md](../DESIGN.md) against this ADR.
- Later, tests send two records for one subject at once. The result must equal storing them one after the other.

## Pros and Cons of the Options

### Use graph storage as it is

Construct uses graph storage's v1 write, compare-and-set and soft delete, and asks for no change. See `cpt-cf-construct-constraint-graph-soft-delete-only`.

- Good, because the batch-atomic write lets a plan land whole or not at all.
- Good, because compare-and-set keeps two records for one subject from mixing.
- Good, because Construct asks graph storage for no change.
- Good, because Construct supplies the node keys, so it can map each value the model sees back to its node key.
- Neutral, because a write conflict makes the planner run again, up to a small limit. Past the limit, the record is dropped, with an audit event.
- Neutral, because graph storage cannot enforce the read rules, so Construct's Profile reader applies them.
- Neutral, because graph storage embeds text on ingest, so Construct must enforce the verdicts before it writes.
- Bad, because a deleted node key cannot be reused before purge, so a returning entity needs a new key.
- Bad, because removal from all storage within 30 days waits for graph storage's purge, which comes after its v1.
- Bad, because tenant offboarding also waits for graph storage.

## More Information

- Graph storage's own design: [graph storage DESIGN](../../../graph-storage/docs/DESIGN.md) and [graph storage PRD](../../../graph-storage/docs/PRD.md).
- Construct's use of it: [DESIGN.md](../DESIGN.md), section 3.5, and the sequences in section 3.6.
- Construct is the only writer and read path of the profile graph: `cpt-cf-construct-adr-construct-is-a-gear`.
- Data and compliance: the 30-day removal and tenant offboarding depend on graph storage, as the Consequences say.
- Reliability: a plan lands whole or not at all; a conflict reruns the planner.
- Security: this decision does not change who may call graph storage.
- Performance: not covered here; Construct inherits graph storage's performance.
- Operations and integration: no change is asked of graph storage.
- If this decision changes, a later ADR supersedes this one.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-construct-fr-deterministic-storage` — a plan is stored fully or not at all, and two records never mix.
- `cpt-cf-construct-fr-fact-decisions` — a replace removes the old value in the same write.
- `cpt-cf-construct-fr-profile-read` — reads go through the Profile reader, never graph storage directly.
- `cpt-cf-construct-fr-sensitive-data-guardrails` — verdicts are enforced before graph storage embeds text.
- `cpt-cf-construct-fr-subject-delete` — deletes are soft deletes; a returning entity gets a new node key.
- `cpt-cf-construct-fr-retention` — retention deletes through graph storage's soft delete.
- `cpt-cf-construct-nfr-deletion-time` — removal from all storage within 30 days waits for graph storage's purge.
- `cpt-cf-construct-usecase-fact-replaced` — the old and new values change in one write.
- `cpt-cf-construct-usecase-delete-all` — erasure depends on graph storage's purge for full removal.
- `cpt-cf-construct-principle-whole-plan-or-nothing` — realized by the batch-atomic write.
- `cpt-cf-construct-principle-no-mixed-records` — realized by compare-and-set.
- `cpt-cf-construct-principle-verdicts-before-storage` — required because graph storage embeds text on ingest.
- `cpt-cf-construct-principle-single-profile-owner` — graph storage cannot apply the read rules.
- `cpt-cf-construct-constraint-graph-soft-delete-only` — the constraint this decision accepts.
- `cpt-cf-construct-component-profile-writer` — writes each plan in one write with its profile version.
- `cpt-cf-construct-component-planner` — runs again on a write conflict.
- `cpt-cf-construct-component-profile-reader` — applies the read rules.
- `cpt-cf-construct-component-deletion` — waits for purge to remove data from all storage.
