---
status: accepted
date: 2026-10-01
decision-makers: Yehor Komarov, Igor Makarov
---

# ADR-0004: Construct stores no weight and does not rank facts

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Store no weight and do not rank](#store-no-weight-and-do-not-rank)
  - [Store a weight in Construct's own payload](#store-a-weight-in-constructs-own-payload)
  - [Ask graph storage for a weight or a scoring hook](#ask-graph-storage-for-a-weight-or-a-scoring-hook)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-construct-adr-no-weights`

## Context and Problem Statement

An earlier Construct design put an **importance weight** on nodes, edges and properties. It summarised a profile by weight: it kept the few facts that matter and dropped the rest. The weight was also the stated difference between Construct and Insight. That design assumed Construct would build its own store.

Construct now uses [graph storage](../../../graph-storage/docs/DESIGN.md) as it is, under `cpt-cf-construct-adr-graph-storage-as-is`. Graph storage has no weight on nodes, edges or attributes, and it will maintain none. [PRD.md](../PRD.md), section 4.2, puts weights and ranking of facts out of scope.

Does Construct need a weight, and what does it lose without one? This ADR records the answer, so the earlier design is not restored without the evidence that closed it.

## Decision Drivers

- **A weight needs a consumer.** A stored value is only useful if something acts on it.
- **Construct maintains what Construct stores.** Graph storage maintains no importance value and plans none.
- **One read path, no ranking step.** The Profile reader serves one subject's readable facts, grouped by entity kind and category. Nothing in it chooses between facts.
- **Measured profiles, not imagined ones.** The answer must hold against the profiles Construct serves today.
- **A product decision.** The weight was the stated difference from Insight, so dropping it is a product decision, not an engineering preference.

## Considered Options

- Store no weight and do not rank
- Store a weight in Construct's own payload
- Ask graph storage for a weight or a scoring hook

## Decision Outcome

Chosen option: "Store no weight and do not rank", because the mechanism the weight was designed for does not exist in graph storage and is not planned, the one read path has no step that needs an order, and the profiles Construct serves are small enough to return whole. Yehor Komarov took this decision on 2026-09-28.

Three facts about graph storage settle the first point. They were established with its owner on 29 and 30 September 2026.

- There is no weight on nodes, edges or attributes. A `confidence` value exists only inside a provenance attribute, which only an analysis edge requires. Graph storage never reads it, and it cannot be ordered on: `$orderby` projects nodes only, and a traversal returns edges without their payload.
- Construct may store its own number as a payload path and declare it in the type's `index` trait. A **numeric** payload path is never indexed. Only string, boolean and date-time equality use the payload index. The surface that would let a gear create such an index is raised as [#4721](https://github.com/constructorfabric/gears-rust/issues/4721) and is planned after graph storage's first release.
- A traversal takes explicit seed keys. There is no caller hook for order or score, and none is planned. A neighbourhood budget keeps the nodes with the highest degree, which is popularity inside the tenant, not importance to the subject.

So a weight cannot steer a traversal under any option. It can only sort a set that Construct already holds, and the caller can do that itself.

### Consequences

- Construct's node and edge types carry no weight field. No job computes a weight, and none refreshes one.
- The Profile reader returns the readable facts of one subject. It applies no order by importance. An application that wants an order applies it to the set it receives.
- A profile stays small through admission limits and retention, not through ranking. Today's data shows one key that grew to 2,959 values, so admission must bound the values a key may hold.
- The key set has to be bounded and owned. A number comparable across facts needs a shared key set, and today there is none.
- The DESIGN states no difference from Insight in terms of a weight. A difference stated in behaviour belongs in the PRD.
- A ranking need comes back to the PRD first. This ADR does not reserve a place for one.

### Confirmation

- A design review checks [DESIGN.md](../DESIGN.md) against this ADR.
- No Construct type declares a weight path in its `index` trait, and no write path computes one.
- The Profile reader orders its result by no stored value.

## Pros and Cons of the Options

### Store no weight and do not rank

The Profile reader returns a subject's readable facts. Construct stores no importance value.

- Good, because it matches what a profile read needs: the facts of one subject, in one category, which today is a set of single digits.
- Good, because nothing can go stale. There is no number to recompute when a fact's context changes.
- Good, because it asks graph storage for no change.
- Neutral, because an application that wants an order applies one to the set it receives.
- Bad, because importance does not survive a request. Two facts of one category and age are the same to Construct.
- Bad, because there is no decay, so retention is the only thing that removes a fact.

### Store a weight in Construct's own payload

Construct computes a number on write, stores it in the payload and declares the path in the type's `index` trait.

- Good, because a write knows more than a read. On a write Construct sees the record, its origin, the facts already stored and the subject's own signal.
- Good, because one stored number serves every reader in the same way, and a review can show it.
- Bad, because a numeric payload path is never indexed, so every order and every range is a scan. The index surface is #4721 and comes after graph storage's first release.
- Bad, because the weight still cannot steer a traversal. It sorts a set Construct already holds.
- Bad, because Construct owns the whole life of the number: compute, refresh, backfill and a common scale across types. A number that is not refreshed is wrong and still looks exact.
- Bad, because there is no common scale to normalise against while the key set is unbounded.
- Bad, because it treats the wrong defect. It would sort 2,959 values under one key instead of stopping them at admission.

### Ask graph storage for a weight or a scoring hook

- Good, because it would restore the earlier design's mechanism instead of approximating it.
- Bad, because the answer is no. Graph storage plans no weight, and it plans no caller hook for order or score.
- Bad, because it would hold Construct's design behind another team's backlog for a mechanism the measured profiles do not need.

## More Information

**What Construct's profiles look like today.** Counted on the production Construct database on 2026-09-28, as aggregates.

| | value |
|---|---|
| Facts, active | 2,611 of 2,614 rows |
| Subjects with facts | 136, in 29 tenants |
| Fact values | 33,941 |
| Facts per subject | p50 **7**, p90 36, p99 213, max 309 |
| Values per subject | p50 **13**, p90 176, p99 7,394, max 9,993 |
| Values per fact | p50 **2**, p90 14, p99 200, max **2,959** |
| Distinct keys | **1,372**; 1,222 of them used once |
| Values with a confidence | **161 of 33,941**, which is 0.5% |

The median profile is too small to prune. The large profiles are not rich: the largest single facts are one key with 2,959 values, one with 1,869 and one with 1,479. A subject does not have 2,959 values of one property. That is accumulation without a limit, and a weight would sort it rather than stop it. The key set is not a taxonomy: any writer decides the key, and most keys appear once.

The measurement describes today's service, not a profile at scale. The population is small, most of it written between February and May 2026, and four subjects in two tenants hold 80% of all values. The median is the reliable number.

**Two corrections to earlier material.** A traversal keeps nodes in the order it reaches them; it is a neighbourhood budget that keeps the nodes with the highest degree. And `confidence` has been described as what graph storage carries in place of a weight; it is not usable as one, and Construct's own data has it on 0.5% of values.

**Other topics.** This decision changes no caller's permissions and asks graph storage for no change. It does not affect deletion, retention or tenant isolation. If this decision changes, a later ADR supersedes this one.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-construct-fr-profile-read` — the read returns the readable facts and orders them by no importance value.
- `cpt-cf-construct-component-profile-reader` — applies the read rules and no ranking.
- `cpt-cf-construct-component-mcp-tools` — the read tool returns the same unranked set.
- `cpt-cf-construct-component-admission` — bounds the values a key may hold, which is what keeps a profile small.
- `cpt-cf-construct-fr-retention` — retention removes what ages out, in place of decay.
- `cpt-cf-construct-adr-graph-storage-as-is` — Construct asks graph storage for no weight and no scoring hook.
