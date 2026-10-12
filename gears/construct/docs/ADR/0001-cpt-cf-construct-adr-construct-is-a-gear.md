---
status: accepted
date: 2026-09-30
---

# ADR-0001: Construct is a gear that owns its state and the rules around the model step

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A state-owning Construct gear](#a-state-owning-construct-gear)
  - [A thin stateless facade](#a-thin-stateless-facade)
  - [No gear](#no-gear)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-construct-adr-construct-is-a-gear`

## Context and Problem Statement

Connectors send typed records about subjects. Someone must turn these records into each subject's profile and serve that profile. The profile is a graph: a subject linked to entities that carry properties. It is kept in graph storage, the platform gear that stores and embeds the profile graph. A language model proposes how a record changes the profile. Pattern checks in code and the model flag sensitive data. The model's proposal is a plan: the set of changes built in memory for one record. A sensitive-data check gives a verdict on each value the plan would store: the block, redact or allow answer. Admission, the deterministic checks on a plan, then decides what is stored. Each subject also has a personalization setting, which lets the profile be used, and can make a review request, a mark that a fact is incorrect.

Where should this logic live, and who should serve the profile?

## Decision Drivers

- **Connectors stay simple.** They send typed records and know nothing else.
- **Construct owns the rules around the model step.** These are trust, confidence, allowed entity kinds and write caps; personalization settings and who may read the profile; enforcing the sensitive-data verdicts; applying a plan in one write; and the read rules.
- **Construct owns data no other gear holds.** This is subject settings, review requests and record IDs.
- **Graph storage cannot enforce the read rules.** These rules are who may read the profile, personalization off, and hidden while under review. So applications must read through Construct.
- **Sensitive data must be blocked or redacted before storing.** Graph storage embeds text on ingest. An embedding is a vector made from the text for search.
- **Erasure spans two places.** It covers graph storage and Construct's own tables, so one gear must coordinate it.
- **The model step is small.** It is a few tools that edit a plan in memory, and one check per sensitive-data kind. A gear can run such an agent loop on its own. An agent loop is a model that calls tools in rounds. The platform needs no agent engine for it.

## Considered Options

- A state-owning Construct gear
- A thin stateless facade
- No gear: the types registry, the event broker, external workers, graph storage and product adapters

## Decision Outcome

Chosen option: "A state-owning Construct gear", because only it meets all the decision drivers above.

### Consequences

- Construct is the only writer to a subject's profile graph. It is also the only read path for it.
- Construct keeps its own tables: subject settings, review requests and record IDs.
- Construct runs the model step itself, as a small agent loop with a fixed set of tools.
- After the loop, deterministic code decides what is stored. A change is stored when its plan passed the checks and the change is in the profile.
- Construct reuses these platform parts: the types registry; the AuthN (authentication), AuthZ (authorization) and tenant resolvers; toolkit-db for its own tables; REST through the API gateway; the settings service; cluster leader election; graph storage; and OAGW, the platform's outbound API gateway, for outbound model calls. [DESIGN.md](../DESIGN.md) describes these parts in section 3.4, and graph storage in section 3.5.
- The settings service holds the tenant settings that Construct contributes.
- The full architecture is in [DESIGN.md](../DESIGN.md).

### Confirmation

- A design review checks [DESIGN.md](../DESIGN.md) against this ADR.
- Later, code review checks that no other component writes to or reads the profile graph directly.

## Pros and Cons of the Options

### A state-owning Construct gear

Construct is a gear with its own tables. It runs the model step, applies the rules, writes the profile and serves every read. See `cpt-cf-construct-design-construct-gear`.

- Good, because connectors stay simple and send only typed records.
- Good, because all rules around the model step live in one place.
- Good, because its own tables hold subject settings, review requests and record IDs.
- Good, because one read path can apply every read rule.
- Good, because it enforces the sensitive-data verdicts before any write, so graph storage never embeds blocked text.
- Good, because one gear coordinates erasure across graph storage and its own tables.
- Good, because it reuses existing platform parts, including graph storage and OAGW.
- Neutral, because it runs its own agent loop; the loop is small, so no agent engine is needed.
- Bad, because it is one more gear, with its own tables to keep.
- Bad, because Construct must build and keep the agent loop itself.

### A thin stateless facade

A gear that runs the model step and forwards the result to graph storage. It keeps no state of its own, so reads and settings would need another gear to hold their state.

- Good, because it has fewer parts: no tables of its own.
- Good, because connectors stay simple.
- Neutral, because it can still run the model step and the checks before it writes.
- Bad, because no other gear holds subject settings, review requests or record IDs, so a stateless facade has nowhere to keep them.
- Bad, because without them it cannot apply the read rules that need stored state: personalization off, and hidden while under review.
- Bad, because erasure would span graph storage and whichever gear holds settings, review requests and record IDs, so no single gear coordinates it.

### No gear

The platform parts work together without a Construct gear. Records pass through the types registry, which resolves each record's type, and the event broker, which carries the record. External workers run the model step and write to graph storage. Each product reads through its own product adapter.

- Good, because it reuses existing gears and adds no new one.
- Good, because connectors still send only typed records.
- Bad, because graph storage cannot enforce the read rules, so each product adapter would have to apply them.
- Bad, because sensitive data must be enforced before graph storage embeds it, which spreads that duty across external workers.
- Bad, because no single gear coordinates erasure.
- Bad, because subject settings, review requests and record IDs have no gear to hold them.

## More Information

- The architecture that follows from this decision is in [DESIGN.md](../DESIGN.md), sections 1 to 3.
- Graph storage is used as it is: `cpt-cf-construct-adr-graph-storage-as-is`.
- Model calls go through one small interface: `cpt-cf-construct-adr-one-model-interface`.
- Security and data: the read rules and the sensitive-data verdicts sit in one gear.
- Compliance: one gear coordinates erasure. Its 30-day removal depends on graph storage; see `cpt-cf-construct-adr-graph-storage-as-is`.
- Operations: the retention job runs on the cluster leader.
- Reliability: Construct is the only read and write path for a profile. It follows the platform's standard availability posture; PRD section 6.2 sets no higher target.
- Performance: not covered here; this decision sets no performance targets.
- Testing: see Confirmation above.
- If this decision changes, a later ADR supersedes this one.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-construct-fr-record-intake` — connectors send one typed record and know nothing else.
- `cpt-cf-construct-fr-fact-decisions` — Construct runs the model step that decides how a record changes facts.
- `cpt-cf-construct-fr-deterministic-storage` — code in Construct decides what is stored, and applies a plan in one write.
- `cpt-cf-construct-fr-profile-read` — every read goes through Construct, which applies the read rules.
- `cpt-cf-construct-fr-review-request` — Construct holds review requests and hides a fact while under review.
- `cpt-cf-construct-fr-settings` — Construct holds subject settings and applies personalization off.
- `cpt-cf-construct-fr-subject-delete` — one gear coordinates erasure across graph storage and its own tables.
- `cpt-cf-construct-fr-retention` — the same gear runs retention.
- `cpt-cf-construct-fr-sensitive-data-guardrails` — Construct enforces the verdicts before any write.
- `cpt-cf-construct-nfr-deletion-time` — one gear makes deleted data stop being served at once.
- `cpt-cf-construct-usecase-delete-all` — one gear coordinates the subject's erasure.
- `cpt-cf-construct-design-construct-gear` — the design this decision shapes.
- `cpt-cf-construct-principle-single-profile-owner` — Construct is the only writer and the only read path.
- `cpt-cf-construct-principle-model-proposes-code-decides` — the model proposes; Admission decides.
- `cpt-cf-construct-principle-verdicts-before-storage` — verdicts are enforced before graph storage embeds text.
- `cpt-cf-construct-principle-base-record-only` — connectors stay simple; Construct reads only the base record.
- `cpt-cf-construct-component-planner` — runs the agent loop inside the gear.
- `cpt-cf-construct-component-admission` — the deterministic checks the gear owns.
- `cpt-cf-construct-component-profile-reader` — the only read path.
- `cpt-cf-construct-component-subject-control` — uses the gear's own tables.
- `cpt-cf-construct-component-deletion` — coordinates erasure and retention.
