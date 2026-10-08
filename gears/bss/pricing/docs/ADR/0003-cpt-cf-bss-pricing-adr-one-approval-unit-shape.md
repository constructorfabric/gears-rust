# ADR-0003: One Approval Unit Shape Shared by Both Gears

**ID**: `cpt-cf-bss-pricing-adr-one-approval-unit-shape`

- [ ] `p1` - **ADR implementation status**

**Status:** accepted 2026-09-25 · **Deciders:** product owner · **Source:** spec §2 decision 8; §2.2; §6; §8.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)

<!-- /toc -->

## Context and Problem Statement

Materiality-dependent governance and separate publication workflows make reviewers learn multiple protocols. Book prices and plan revisions remain independently meaningful facts even when operators schedule them for the same day.

## Decision Drivers

`cpt-cf-bss-pricing-fr-approval-units`, `cpt-cf-bss-pricing-fr-publish-changes`, `cpt-cf-bss-pricing-nfr-idempotency-concurrency`, `cpt-cf-bss-pricing-nfr-audit`. The approved PriceBook model is the content authority. The decision must hold under tenant isolation,
concurrent writes and either supported database, and must remain explainable to operators and reviewers.

## Considered Options

1. Keep materiality-gated workflows.
2. Share one unit engine with separate subjects.
3. Approve a composite plan-and-money unit.

## Decision Outcome

Chosen: **Share one unit engine with separate subjects**. Use bss-approval with gear-owned tables and ApprovalSubject implementations. Pricing kinds are prices, plan_revision, promotion and migration; phase 2 implements prices. A unit copies policy quorum, stores proposed business content and fingerprint, and counts votes for one generation. Drift commits a refresh and UNIT_STALE; conditional unit version detects contention. Submitter and every item author are excluded from approval.

### Consequences

Quorum zero still records a terminal unit, audit and event. Prices and revisions have separate queues and outcomes; blocked_by is computed rather than stored. Units use conditional writes, not FOR UPDATE. Client-key replay is a separate 24-hour store; the obsolete idempotency column in spec §6 is superseded by §2.2.

### Confirmation

Exercise quorum 0/1/2, item-author SoD, duplicate votes, generation mismatch, committed stale refresh and two concurrent approvals. Verify reject/withdraw/quorum-zero audit and terminal events, plus no domain publish event on rejected or withdrawn units. These are implementation acceptance conditions; Part 2a replaces documents only.

## Pros and Cons of the Options

Materiality gates retain a second approval policy and inconsistent paths. Shared subjects centralize review semantics while each gear owns its state transaction. A composite unit obscures independent book facts and couples a rejected plan revision to already approved prices.

## More Information

[PRD](../PRD.md), [DESIGN](../DESIGN.md), [DECISIONS](../DECISIONS.md).
Source: `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout, §2 decision 8; §2.2; §6; §8.
The superseded set remains on `bss/products-backup` and in history.
