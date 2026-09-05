---
status: accepted
date: 2026-08-17
decision-makers: usage-collector spec owners
---

# Historical import on an isolated, origin-marked route

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A dedicated origin-marked route, isolated from live ingestion](#a-dedicated-origin-marked-route-isolated-from-live-ingestion)
  - [One ingestion path with wide backdating](#one-ingestion-path-with-wide-backdating)
  - [An operator bulk loader writing to the storage plugin directly](#an-operator-bulk-loader-writing-to-the-storage-plugin-directly)
- [More Information](#more-information)
  - [Prior art](#prior-art)
  - [Why the three bounds are asymmetric](#why-the-three-bounds-are-asymmetric)
  - [The cost to an operator](#the-cost-to-an-operator)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-usage-collector-adr-backfill-isolation`

## Context and Problem Statement

Two retroactive pressures meet on the ingestion path. Usage sometimes has to be
imported for periods already past, when a meter is onboarded late or an emitter
was down. An emitter can also submit a covered period dated into the future,
whether by clock skew or by defect.

Both pressures need a bound. An unbounded retroactive reach places an unbounded
recomputation obligation on any materialised aggregate, and a bulk import competes
with live traffic for the same capacity.

The question is where historical import belongs. It can share the live ingestion
path, run on its own isolated route, or bypass the gear through a plugin-side
loader.

## Decision Drivers

- `cpt-cf-usage-collector-fr-backfill` — a dedicated bulk-import path with a
  bounded backfill window, 90 days by default. It also requires an origin marker
  on every imported entry, workload isolation from live ingestion, and validation
  identical to the live path.
- `cpt-cf-usage-collector-fr-live-future-time-bound` — the live path must bound
  the covered period on both sides. It rejects a period ending further into the
  future than a configurable tolerance, 5 minutes by default. It also rejects one
  starting further into the past than a second tolerance, 48 hours by default.
- `cpt-cf-usage-collector-fr-billing-retention-floor` — the retention floor is the
  backfill window plus one replay horizon, so the two decisions cannot drift
  apart.
- `cpt-cf-usage-collector-fr-record-invalidation` — an invalidation carries the
  period it withdraws, so the path's bounds govern it as they govern a
  measurement.
- `cpt-cf-usage-collector-fr-idempotency` — an imported entry arrives with its
  dedup horizon already partly spent, because retention runs from the covered
  period rather than from import.
- `cpt-cf-usage-collector-fr-rate-limiting` — ingestion quotas apply per calling
  gear, and per calling gear and tenant pair, across every ingestion path
  including backfill.
- `cpt-cf-usage-collector-nfr-workload-isolation` — a bulk workload must not
  degrade ingestion p95 latency.
- `cpt-cf-usage-collector-nfr-throughput-profile` — the load envelope a
  concurrent-load confirmation test runs against.

## Considered Options

- A dedicated origin-marked route, isolated from live ingestion — historical
  import runs on its own bulk-import path. Every entry it accepts carries an
  origin marker, persisted and returned on every read path.
- One ingestion path with wide backdating — a single ingestion path accepts both
  live and historical entries. How far back the covered period reaches is the only
  thing that separates them.
- An operator bulk loader writing to the storage plugin directly — an operator
  tool writes historical entries straight to the active storage plugin. The path
  bypasses the gear.

## Decision Outcome

Chosen option: "A dedicated origin-marked route, isolated from live ingestion". It
bounds both pressures without forcing the live path to accept an unbounded reach.

Historical import runs on a dedicated route, isolated from live ingestion at the
gear. The Ingestion Gateway owns both routes and applies identical validation on
each. Every imported entry carries an origin marker recording the path it arrived
on, and that marker appears on every read path.

The live path bounds the covered period on both sides
(`cpt-cf-usage-collector-fr-live-future-time-bound`). It refuses a period ending
further into the future than a configurable tolerance, 5 minutes by default. It
refuses a period starting further into the past than a second tolerance, 48 hours
by default. That rejection names the dedicated backfill route as the path such a
submission belongs on.

The backfill window, 90 days by default, bounds the route rather than a kind of
entry. Both retroactive directions travel it: an import moves usage in, a
withdrawal takes it out. Widening the window moves both together.

The retention floor is the backfill window plus one replay horizon, 125 days at
the launch defaults (`cpt-cf-usage-collector-fr-billing-retention-floor`). The two
terms are summed rather than maximised, because retention runs from the covered
period. The sum leaves an entry imported at the far edge of the window a full
replay horizon of deduplication and replay.

**The covered-period bounds are a property of the path, not of the entry kind.**
An invalidation copies its target's period, and that period is checked exactly as
a measurement's own is (`cpt-cf-usage-collector-fr-record-invalidation`). A
withdrawal on the live path reaches 48 hours back, and one reaching further
travels the backfill route for its 90 days.

Both entries of a pair carry one period, so one bound still governs both: the one
the route the withdrawal travelled applies. A target aged past the backfill window
can therefore no longer be withdrawn at all, absent the elevated authorization
reaching beyond it — the same horizon past which no import lands, and what leaves
a closed aggregate stable.

### Consequences

- An imported entry arrives with its idempotency horizon already partly spent,
  because retention runs from the covered period rather than from the moment of
  import. The retention floor still leaves it a full replay horizon of
  deduplication, 35 days at the launch defaults.
- Re-running an import over entries whose periods have aged past the retention
  floor draws no guaranteed outcome
  (`cpt-cf-usage-collector-adr-mandatory-idempotency`). A re-admitted entry
  carries the same derived identifier as the entry it re-creates, and only
  consumer-side deduplication distinguishes that re-admission from new
  consumption.
- The origin marker lets a consumer tell imported history from live consumption.
  This matters when a charge has already been raised for a period. A consumer that
  rates the feed handles a backfilled entry as batch catch-up rather than as
  current consumption.
- Workload isolation for the backfill route is a gear-level obligation. Backend
  pool isolation stays a plugin deployment obligation. That isolation covers the
  bulk withdrawal an emitter defect produces.
- The backfill route is exposed on the SDK trait as well as on REST. It is the
  only route reaching past the live past bound, and a defect is normally found
  days later rather than hours later. Confining it to REST would turn every such
  correction into an operator escalation.
- The ingestion quota is not isolated. It applies per calling gear, and per
  calling gear and tenant pair, across every path. A bulk import therefore spends
  the same allowance as live emission.
- An emitter recovering from an outage longer than the live past bound moves its
  catch-up to the backfill route. Late live data inside that bound still works on
  the path the emitter already calls, and the rejection names the route for
  anything older.
- An emitter that discovers a gap older than the configured window cannot close it
  on its own. The import needs the elevated authorization the backfill path
  requires beyond that window.

### Confirmation

- A test asserting the live path rejects a covered period ending beyond the future
  bound.
- A test asserting the live path rejects a covered period starting beyond the past
  bound, with an error naming the backfill route. The same test asserts that the
  backfill route admits that period inside its own window.
- A test asserting an invalidation copying a period older than the live past
  bound is rejected on the live path with an error naming the backfill route, and
  accepted on that route carrying the origin marker.
- A test asserting an imported entry carries its origin marker on every read path.
- A concurrent load test asserting a bulk import does not degrade live ingestion
  beyond the `cpt-cf-usage-collector-nfr-throughput-profile` envelope.

## Pros and Cons of the Options

### A dedicated origin-marked route, isolated from live ingestion

Historical import runs on its own bulk-import path. Every entry it accepts carries
an origin marker, persisted and returned on every read path. The route is isolated
from live ingestion workload and validated identically to the live path.

- Good, because it bounds the recomputation obligation on materialised aggregates.
  An import job stays inside its own configured window, and it cannot reach
  further back without elevated authorization.
- Good, because the bound belongs to the route rather than to the entry kind.
  One rule covers import and withdrawal, the origin marker stays truthful for a
  correction of history, and a bulk withdrawal inherits the same isolation.
- Good, because workload isolation keeps a bulk catch-up job from degrading
  live-path service-level objectives.
- Good, because the origin marker lets a consumer separate imported history from
  live consumption. That distinction matters once a charge has already been raised
  for a period.
- Neutral, because an operator maintains two integrations, one for live emission
  and one for backfill, each against its own route. Both routes draw on the same
  ingestion quota.
- Neutral, because a withdrawal found after the live past bound costs a route
  switch. The rejection names the route, and both surfaces carry it.
- Bad, because the isolated route is an additional surface to build, document, and
  keep in step with the live path's validation rules.

### One ingestion path with wide backdating

A single ingestion path accepts both live and historical entries, distinguished
only by how far back a covered period reaches.

- Good, because it is one surface and one contract, with nothing for an emitter to
  choose between.
- Good, because an emitter needs no knowledge of which route to use. Late data
  works on the path the emitter already calls.
- Bad, because an unbounded retroactive reach places an unbounded recomputation
  obligation on any materialised aggregate.
- Bad, because a bulk import competes with live ingestion for the same backend
  pool, so a catch-up job degrades live metering. Both options share one ingestion
  quota, so workload isolation is the whole of what separates them.

### An operator bulk loader writing to the storage plugin directly

An operator tool writes historical entries straight to the active storage plugin,
bypassing the gear entirely.

- Good, because a one-time platform migration of years of history legitimately
  exceeds any bounded window. A direct loader is also the fastest way to move that
  volume.
- Good, because an operator-run loader consumes no ingestion quota. It therefore
  cannot degrade the live-path service-level objectives.
- Good, because the operation is bounded in time and auditable outside the gear,
  as a one-off operator action rather than a standing path.
- Bad, because it bypasses PDP authorization, identifier derivation, and
  validation. An entry loaded that way is not comparable to an entry the gear
  accepted.
- Bad, because the gear cannot vouch for such an entry on any read path. A
  downstream consumer cannot tell where it came from, and no audit trail runs
  through the gear.

## More Information

### Prior art

Bounded live acceptance with historical import on a separate route is a pattern
surveyed marketplace metering systems already use. Azure Marketplace and AWS
Marketplace both enforce a hard backdating limit measured in hours. Both also
route historical import through a path distinct from live ingestion. That
precedent treats a hard bound as a deliberate constraint rather than a
limitation.

### Why the three bounds are asymmetric

Each of the three bounds protects something different. The live path reaches 5
minutes forward and 48 hours back. The backfill route reaches 90 days back through
its window, and every retroactive entry travels that route to get there — an
import and a withdrawal alike.

The future bound protects against a defective emitter opening a period that does
not yet exist. The past bound keeps the origin marker able to separate imported
history from current consumption: without it, a very old measurement arrives
marked as live. The backfill window protects the recomputation surface.

Making the bound a property of the path keeps those protections intact for a
correction. A withdrawal of a closed period *is* a retroactive touch of history.
Exempting it would re-open the marker gap the past bound exists to close, and
leave the bulk emitter-defect workload on the live path.

The past bound also carries a purge-on-arrival guard that the backfill path needs
in its own right. `cpt-cf-usage-collector-fr-backfill` refuses a window wider than
the retention its storage profile guarantees, because an entry admitted outside
retention is purge-eligible on arrival. A default of 48 hours covers emitter
outage and retry lag, which is what genuinely late live data is. Anything older is
history, and history belongs on the route that marks it and isolates its load.

### The cost to an operator

Onboarding a late meter is a two-step exercise. The operator registers the type,
then runs the backfill import against its own route. That import job carries its
own elevated-authorization rule beyond the configured window, and it draws on the
same ingestion quota as live emission.

Correcting old data costs a route switch rather than an escalation. An emitter
resubmits its withdrawals against the backfill route, on REST or the SDK, and the
live-path rejection names that route.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-usage-collector-fr-backfill` — the requirement this decision realizes.
- `cpt-cf-usage-collector-fr-live-future-time-bound` — the two bounds the live
  path enforces, forward and back.
- `cpt-cf-usage-collector-fr-billing-retention-floor` — the floor derived from the
  backfill window plus one replay horizon.
- `cpt-cf-usage-collector-fr-record-invalidation` — the other retroactive
  direction, bounded by the route it travels rather than by its kind.
- `cpt-cf-usage-collector-fr-idempotency` — the partly-spent horizon an imported
  entry carries.
- `cpt-cf-usage-collector-fr-rate-limiting` — the quota shared across every
  ingestion path. A bulk import consumes the same allowance as live emission,
  which is a further reason to run it on a route whose load is isolated.
- `cpt-cf-usage-collector-nfr-workload-isolation` — the NFR this isolation answers
  at the gear.
- `cpt-cf-usage-collector-nfr-throughput-profile` — the envelope the concurrent
  test measures against.
- `cpt-cf-usage-collector-component-ingestion-gateway` — the component that owns
  both routes.
- `cpt-cf-usage-collector-seq-backfill-import` and
  `cpt-cf-usage-collector-usecase-backfill` — the sequence and use case.
