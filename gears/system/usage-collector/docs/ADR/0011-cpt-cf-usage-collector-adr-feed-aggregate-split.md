---
status: accepted
date: 2026-08-17
decision-makers: usage-collector spec owners
---

Created:  2026-09-09 by Virtuozzo International GmbH
Updated:  2026-09-15 by Virtuozzo International GmbH

# Charging reads the entry stream, and aggregates are a derived view

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The raw query path is not the feed](#the-raw-query-path-is-not-the-feed)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A dedicated pull feed for entries, with aggregates as a derived view](#a-dedicated-pull-feed-for-entries-with-aggregates-as-a-derived-view)
  - [Aggregates only, with the per-entry read paths removed](#aggregates-only-with-the-per-entry-read-paths-removed)
  - [Raw query polling as the feed](#raw-query-polling-as-the-feed)
  - [Push through the event broker](#push-through-the-event-broker)
- [More Information](#more-information)
  - [What the analog survey shows](#what-the-analog-survey-shows)
  - [Why effectively-once belongs to the consumer](#why-effectively-once-belongs-to-the-consumer)
  - [Related decisions](#related-decisions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-usage-collector-adr-feed-aggregate-split`

## Context and Problem Statement

The gear meters, retains, aggregates and feeds. Pricing, rating and invoice
generation sit outside its scope and belong to a downstream consumer. The gear
therefore hands off, and the question is where that hand-off happens.

Three read surfaces are candidates. The raw query path serves entries as
persisted. The aggregate path applies the declared fold over a requested range.
The feed serves entries under a subscription, in an order independent of the
covered period.

The question is architectural rather than a routing detail, because the answer
fixes what each surface must guarantee. A surface that feeds a charge owes
ordering, a snapshot, a bounded replay window, and a staleness bound a consumer
can detect. A surface that feeds a chart owes none of those.

The two audiences differ in what they tolerate. Aggregation's natural audience is
dashboards, quota evaluation and reconciliation. Such a consumer tolerates
bounded staleness and never computes an invoice. A charging consumer tolerates
neither staleness it cannot detect nor a number it cannot reproduce.

## Decision Drivers

- A charge must be reproducible — a consumer that recomputes a period obtains the
  number it charged, from the identified entries the charge derived from.
- A charge must be attributable — a dispute resolves against named entries with
  their identifiers, covered periods, correction linkage and reason codes. A
  total names nothing.
- Staleness must be detectable — a charging consumer has to prove it has seen
  everything accepted up to a point, which needs a cursor that proves
  completeness, and to spot a stalled emitter, which needs per-scope
  reconciliation metadata.
- `cpt-cf-usage-collector-fr-billing-usage-feed` — requires a deterministic,
  replay-safe pull path over entries. This decision states which consumer that
  path exists for.
- Materialisation must stay available on the aggregate path — the throughput
  profile and the retention floor put tens of billions of entries in residence.
  A per-query scan over them cannot hold p95 ≤ 500 ms.
- Plugin neutrality — every freshness obligation must be expressible as a
  per-plugin readiness gate, because the gear floor publishes no upper bound
  (`cpt-cf-usage-collector-nfr-query-freshness`).
- Bounded recovery — the replay obligation must scale with what a consumer
  subscribes to, not with platform-wide telemetry volume.
- Prior art — every surveyed metering substrate that hands off to a rating layer
  hands off entries. The surveyed systems that serve totals own rating
  themselves.

## Considered Options

- A dedicated pull feed for entries, with aggregates as a derived view — the feed
  carries entries under a subscription in feed order. The aggregate path
  becomes a derived read path with its own freshness gate.
- Aggregates only, with the per-entry read paths removed — the gear serves one
  number per meter, period and grouping. The feed, the raw query path and point
  lookup all go away.
- Raw query polling as the feed — a consumer polls the raw query path with a
  forward cursor over event time. It treats the pages as its stream.
- Push through the event broker — the gear publishes every accepted entry to the
  platform event broker, and a charging consumer subscribes.

## Decision Outcome

Chosen option: "A dedicated pull feed for entries, with aggregates as a derived
view". A charging consumer reads the feed and derives its charges from entries.

The feed carries the obligations that follow from that role. It is pull-based
over the Downstream Usage Reader Contract. A consumer declares the set of GTS
types it rates, and the Feed Gateway excludes everything else from the pages and the
cursor. Pages come in a deterministic order the storage plugin chooses. Beyond
determinism the decision promises only that an invalidation follows its target.
In particular it promises no acceptance-instant order, and a consumer detects
late arrival from the acceptance instant instead. The gateway owns the cursor
encoding, and a plugin never mints, encodes or interprets one. The opaque cursor
keeps the plugin's position off the wire, free to change and leaking no other
tenant's volume.

The storage plugin owns the feed's order and how it is realised. A position is
plugin-issued and opaque: the gateway carries it inside the cursor without
reading it, and no position value appears on the wire. The
decision fixes what the order guarantees rather than how positions are
assigned, because backends differ in what they can assign cheaply. One store
has a native counter per table. Another has no sequence at all, and would need
a coordination round-trip per write to emulate a counter per scope. One
requirement bounds that freedom without dictating a mechanism: the cursor
carrying a position is length-bounded and may not grow with a subscription's
breadth (`cpt-cf-usage-collector-fr-billing-usage-feed`). A position keyed per
tenant therefore does not qualify, since a charging consumer reads every tenant
it may read. Any key the store can compare across a whole subscription does —
its commit order, or an arrival instant served behind a safety lag — so a
backend with no native sequence is still admissible. Which key stays the
plugin's choice.

Two guarantees bind every plugin. **Completeness**: a page carries only settled
entries — entries that have converged under the plugin's dedup level
(`cpt-cf-usage-collector-adr-mandatory-idempotency`) and before which nothing
more can become visible. A cursor therefore never passes a position that is not
final, and once it has passed one, no entry becomes visible at or before it,
however many writers accept concurrently and in whatever order their writes
commit. A page that reaches the settled head of the feed returns its cursor at
that head, so a cursor's age reflects how far its consumer has read.
**Correction order**: an invalidation follows its target.

A paginated scan observes a snapshot, and the append-only ledger purchases that
property. No accepted entry is ever rewritten. A correction arrives as a later
invalidation entry at its own feed position, so a scan has nothing to
observe changing. Append-only arrivals are the one carve-out, and they always
land ahead of the cursor. That includes a late entry or a withdrawal for a
covered period the consumer has already read, so a cursor proves what the feed
has delivered, never that a covered period will not change. The consumer
handles such an arrival as an adjustment to that period.

The snapshot is therefore prefix-stable rather than frozen. A replay from a
cursor returns every entry the original scan returned, in the same order and at
the same position, extended only by entries accepted since. A consumer that
bounds the replay with a cursor it recorded later reads exactly the pages it
read before.

No second token exists beside the cursor. Because a page carries only settled
entries, a cursor already proves that everything before it is delivered and
final. A separate completeness token would restate that. A design needs one
only when its cursor can run ahead of what is final, and a cursor that runs
ahead of what is final is exactly the hole completeness closes: an entry still
being written lands behind it, and the consumer never receives it.

Two zones and one refusal govern a rewound cursor
(`cpt-cf-usage-collector-fr-billing-retention-floor`). A cursor's age is the
acceptance instant of the oldest entry of a subscribed GTS type after it — the
reader's scope does not narrow it — so it holds whatever order the plugin
chooses, and a cursor with no such entry after it is current. A cursor no older
than the operational replay horizon is served, and that is the guarantee a
charging consumer codes against. Beyond that horizon what a deployment still
retains is plugin-dependent and a consumer relies on none of it, but that
latitude is over retention rather than behaviour: a plugin refuses on no
cursor's age, and serves one whose continuation it still holds however old that
cursor is. The refusal is not a third zone: a cursor after which retention has
already removed an entry of a subscribed GTS type is refused with an actionable
error, whatever that cursor's age. Removal is read over the subscription's types
and not over the caller's compiled scope, because a plugin holds no record of
which scopes a removed entry fell in. The refusal is conservative for that
reason: it can refuse a cursor whose own scope lost nothing, and it never serves
a truncated range. It never collides with the first zone either — a removed
entry was accepted at least one replay horizon before its removal, so a cursor
the condition refuses is older than the horizon by the same measure. Age cannot
carry that decision, because a sweep clamps the age of a stale cursor to the
retention boundary — the loss and the measure of it disappear together. The Feed
Gateway never serves a silently truncated range.

The feed delivers both entries of a withdrawn pair, each at its own feed
position. The two can land in different pages, because an invalidation follows
its target in feed order and can arrive any time after it. Excluding the pair is therefore the consumer's step,
taken when it folds, and the feed never excludes it. A consumer never treats a
target as final because the page carrying it held no invalidation.

The aggregate path is a derived view. It computes the declared fold and states
nothing a consumer cannot, in principle, compute from the entries themselves. Its
audience is dashboards, quota evaluation and reconciliation.

Because the path is derived, a plugin can serve it from a pre-computed or
materialised representation. The freshness obligation is therefore a per-plugin
readiness gate (`cpt-cf-usage-collector-nfr-aggregate-freshness`) rather than a
gear-level floor. An accepted invalidation obliges a materialised aggregate to
recompute over the affected range. Such an aggregate never absorbs the withdrawal
as a further contribution, because `MAX`, `MIN` and `LATEST` admit no reversing
term.

The boundary is one rule a reader can apply.

> A consumer that computes money reads entries, not aggregates.

Stated normatively: a charging consumer reads the feed, and derives no charge
from an aggregate response. A consumer that only displays, throttles or
reconciles can read either surface.

Reading the feed does not make every meter chargeable. The consumer applies the
declared fold rather than one of its own choosing, and only `SUM` yields a
chargeable period quantity. `MAX`, `MIN` and `LATEST` are descriptive folds
(`cpt-cf-usage-collector-adr-declared-fold`). A meter declaring one of those
characterises a series and states no amount consumed.

### The raw query path is not the feed

Event time on an entry is emitter-supplied, and the live path accepts it back to
the configured past tolerance while backfill accepts its whole window. An entry
can therefore be inserted at a position a forward event-time cursor has already
passed. The consumer never receives that entry and cannot detect the omission.

The hole exists on a fully converged single node. It is therefore not a
replica-lag problem, and no plugin consistency ceiling closes it. The feed's
completeness guarantee means a newly accepted entry never becomes visible behind
a cursor already returned. The raw path stays an audit,
debugging and dispute-resolution surface (`cpt-cf-usage-collector-fr-query-raw`).

### Consequences

- A deployment whose active plugin publishes no qualifying feed-freshness ceiling
  cannot feed a charging consumer. The condition is a storage-plugin readiness
  gate, verified at review beside the retention floor.
- The replay obligation is bounded by subscription scope. The required read rate
  is at least the subscribed arrival rate multiplied by one plus the ratio of
  backlog age to recovery time. At the launch objective that is five times the
  subscribed arrival rate, and a consumer never pays for traffic it does not
  read.
- Reconciliation metadata and its watermarks let a consumer compare totals and
  spot a stalled emitter. They prove nothing about completeness: they are not
  tied to a feed cursor, and only the cursor proves what the feed has
  delivered. The gear evaluates none of them and raises no stall signal.
- The retention floor is what makes replay meaningful. It sums the backfill
  window and the operational replay horizon, so every accepted entry keeps one
  full horizon from the moment it becomes readable. A cursor past the floor is
  refused rather than silently truncated.
- Two freshness obligations exist rather than one. Feed lag and aggregate lag are
  different mechanisms with different bounds, so each plugin publishes them
  separately. A plugin quoting one number for both overstates one of them.
- The aggregate path being derived is what makes pre-aggregation legitimate. A
  charging path served from a materialised rollup is not legitimate, because a
  consumer cannot reproduce the charge from named entries.
- The Feed Gateway stays a component distinct from the Query Gateway. It serves
  its own feed order rather than covered-period order, and its snapshot and completeness
  obligations have no analogue on the query paths.
- Retaining entries keeps a recovery path open that an aggregates-only surface
  closes. A meter whose fold was bound wrongly can be re-read and reprocessed
  from the entries, inside the retention floor. An aggregate computed under the
  wrong fold is not recoverable from the aggregate.
- A feed read **names** where it begins rather than leaving it to an absent
  position. The one start this decision admits is the oldest entry the
  subscription retains, and none begins at the head: the feed serves consumers
  that compute a charge, and skipping retained history is never a correct start
  for one. A first connect therefore replays that history, and a consumer whose
  cursor store is lost recovers by reading from the same start — both absorbed
  by the deduplication every replay already obliges.
- The start argument is shaped as an open enum rather than an optional
  position, so admitting a further start mode later is a variant rather than a
  new argument. Without that shape it would be a major-version break on both
  Rust surfaces after 1.0, while the wire parameter carrying it stays additive
  either way. No further start mode is admitted here.
- **Seeking by instant is not that start mode, and would not be a cursor
  recovery path.** Feed order is the order entries settle in, while an entry's
  acceptance instant is stamped before it commits, so the two invert by the
  commit latency — the same property that already forbids resting feed order on
  a gateway timestamp. A consumer that lost its cursor and sought to its
  last-processed instant would skip every entry stamped earlier and settled
  later, which is the loss the feed exists to prevent. A seek could only ever
  reduce the cost of a bootstrap, never establish a known position, and it is
  out of scope here.

### Confirmation

- Feed contract tests covering prefix stability and replay determinism. The same
  cursor yields the same continuation, extended only by entries accepted since,
  and a replay from a cursor inside the operational replay horizon observes the
  entries the original scan observed, in the same order. A replay bounded by a
  cursor recorded later is identical, entry for entry.
- A multi-tenant cursor test. A subscription spanning many tenants, some never
  written, replays identically between two recorded cursors, and a consumer that
  keeps reading is never refused however long its subscription stays quiet.
- A completeness test, run under concurrent ingestion of
  measurements and invalidation entries through several gateway replicas. No
  page carries an entry that is not yet settled, no entry becomes visible behind
  a returned cursor, and every invalidation follows its target.
- A replay-refusal test. A cursor after which retention has removed an entry of
  a subscribed GTS type returns an actionable error rather than a short page,
  and a cursor whose continuation is intact is served whatever its age.
- A bootstrap-position test. A read that names no cursor begins at the oldest
  retained entry rather than at the head, is not refused on the retention
  floor, and returns a head cursor on a subscription that retains nothing.
- A recovery test against a subscription at the NFR envelope. A 24-hour backlog
  clears within 6 hours while entries keep arriving, with ingestion p95 inside
  its bound throughout.
- Storage-plugin release-readiness review confirming both published ceilings:
  acceptance to feed visibility, and acceptance to aggregate visibility with its
  invalidation-propagation bound.

## Pros and Cons of the Options

### A dedicated pull feed for entries, with aggregates as a derived view

The feed carries entries under a subscription in feed order. The aggregate
path serves the declared fold to consumers that never compute a charge.

- Good, because a charge stays reproducible. A consumer recomputes a period from
  named entries and obtains the number it charged.
- Good, because a dispute resolves against identified entries, with covered
  periods, correction linkage and reason codes intact.
- Good, because a feed order with completeness has no late-arrival hole. A
  newly accepted entry never lands behind a cursor already returned.
- Good, because one token carries both the resume point and the completeness
  proof, so a consumer cannot hold the two out of step.
- Neutral, because the feed shows an entry only once it is settled, so feed lag
  includes each plugin's settling time. That time falls inside the published
  feed-freshness ceiling.
- Good, because the ordering guarantee is stated as an outcome, so every backend
  can meet it in its own way. None is forced to emulate a counter per scope.
- Neutral, because completeness is a property each plugin must demonstrate
  rather than one a counter implies. Each plugin's deployment guide states how
  it holds.
- Good, because it frees the aggregate path to be materialised. Close-query cost
  then scales with buckets served rather than with entries ingested.
- Good, because subscription scoping keeps the replay obligation proportionate to
  the meters a consumer actually rates.
- Neutral, because the gear publishes two freshness bounds instead of one. Each
  bound is honest, and each is a per-plugin gate rather than a gear promise.
- Bad, because a charging consumer does more work. It manages a cursor, applies
  the declared fold on its own side, and deduplicates by entry identifier.
- Bad, because the gear retains every entry for the retention floor, which is the
  dominant storage cost under the throughput profile.

### Aggregates only, with the per-entry read paths removed

The gear serves one number per meter, period and grouping. The feed, the raw
query path and point lookup all go away.

- Good, because it is the smallest surface the gear can publish. One read shape,
  with no cursor and no snapshot obligation.
- Good, because it makes the expensive scan unnecessary. Nothing has to stream
  tens of billions of entries out of the store.
- Good, because it matches how a dashboard actually consumes usage. A chart asks
  for a total per bucket and never for the entries behind it.
- Good, because storage falls to the rollup, so the retention floor stops driving
  resident data volume.
- Bad, because the entry stream is the deliverable a rating consumer needs. A
  charge must be reproducible and attributable to identified entries, and a total
  is neither.
- Bad, because no surveyed system that feeds a billing pipeline serves aggregates
  alone. The surveyed systems that serve totals are billing engines one layer
  above, and they own rating themselves.
- Bad, because the one surveyed system that does discard entries after
  aggregating is a monitoring store. It carries no correction primitive, no
  per-entry identity and no downstream billing hand-off.
- Bad, because it removes the correction evidence together with the entries. An
  invalidation entry and its reason code then have nowhere to be read.
- Bad, because it deletes point lookup, which is what lets a correction name its
  target and an auditor resolve one identifier.

### Raw query polling as the feed

A consumer polls the raw query path with a forward cursor over event time and
treats the pages as its stream.

- Good, because it adds no surface. The raw path exists for audit and already
  returns cursor-paginated pages.
- Good, because it serves entries as persisted, with correction linkage in both
  directions.
- Bad, because a raw scan cannot offer the snapshot guarantee. The consistency
  floor allows an entry seen on one page to be absent from a later one.
- Bad, because event time is emitter-supplied, bounded only by the live path's
  past tolerance and by the backfill window. An entry can be inserted behind a
  cursor that has already passed its position.
- Bad, because that hole exists on a fully converged single node. It is not
  replica lag, so no plugin consistency ceiling closes it.
- Bad, because the consumer cannot detect the omission. A silently incomplete
  charge run is the worst outcome the gear can hand downstream.

### Push through the event broker

The gear publishes every accepted entry to the platform event broker, and a
charging consumer subscribes.

- Good, because a consumer receives entries without polling, and end-to-end lag
  falls to broker latency.
- Good, because fan-out to several consumers costs the gear little beyond the
  publication itself.
- Bad, because a downstream outage couples into ingestion. A broker that stops
  accepting publications either blocks acceptance or drops entries the ledger has
  already acknowledged.
- Bad, because the consumer does not own the cursor. An overlapping replay is
  harmless only where the consumer chooses where to resume, and a broker offset
  is the broker's state.
- Bad, because replay past the retention floor cannot be refused with an
  actionable error. Broker retention becomes a second horizon, independent of the
  one the gear publishes.
- Bad, because it introduces a broker dependency the gear otherwise does not
  carry. DESIGN declares event architecture not applicable in v1.

## More Information

### What the analog survey shows

Every surveyed system that meters and then hands off to a rating layer hands off
entries. Google Service Control accepts reported operations, deduplicates them by
operation identifier, and exposes operations rather than totals. Azure metered
billing takes usage events per resource and dimension, and leaves pricing to the
marketplace. Amberflo and m3ter both roll up internally and then export records
to a billing system.

The systems that serve aggregates as their product sit one layer above. They own
rating themselves, so a period total is their deliverable rather than an
intermediate value someone else must reproduce. Only one surveyed system discards
entries after aggregating, and it is a monitoring time-series store with no
correction primitive and no billing hand-off.

The evidence is directional rather than decisive, and one point of dissent
belongs on the record. One surveyed engine runs analytical queries straight over
raw events and pre-aggregates nothing. It does so on a columnar engine, which the
launch storage plugin is not. That is why materialisation stays legitimate here,
and why it stays confined to the derived path.

### Why effectively-once belongs to the consumer

The split costs the consumer real work. It reads a stream instead of one number
per period. It holds a cursor and applies the declared fold on its own side. It
excludes both entries of a withdrawn pair as it folds, since the feed delivers
each of them at its own position. It also deduplicates on the entry identifier
rather than relying on the gear — not because ingestion can produce a duplicate,
which the backfill window bound excludes
(`cpt-cf-usage-collector-adr-mandatory-idempotency`), but because feed delivery is
at-least-once and an overlapping replay, a resumed cursor, or a redelivered page
each present an identifier the consumer may already have rated.

That work belongs to the consumer, because only the consumer knows what it has
already charged for. The gear holds no record of a rated charge and owns no
billing period, so it cannot make the judgment at all. Placing effectively-once
at the consumer keeps the gear's own obligation to at-least-once delivery with a
stable identifier, which it can honour under replay.

### Related decisions

- `cpt-cf-usage-collector-adr-consistency-contract` — publishes the floor every
  read path shares, and its feed snapshot subsection states what the feed adds.
- `cpt-cf-usage-collector-adr-append-only-invalidation` — supplies the
  append-only property that purchases the snapshot.
- `cpt-cf-usage-collector-adr-declared-fold` — makes one period yield one number
  per meter, which is what a derived aggregate can safely serve.
- `cpt-cf-usage-collector-adr-record-identity-derivation` — supplies the
  identifier a consumer deduplicates on.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-usage-collector-fr-billing-usage-feed` — the requirement this decision
  realizes, and the surface a charging consumer reads.
- `cpt-cf-usage-collector-fr-billing-fields-on-read` — what every feed page
  carries, unstripped and with each invalidation naming the entry it withdraws.
- `cpt-cf-usage-collector-fr-reconciliation-metadata` — the metadata and
  watermarks a consumer compares totals against and uses to spot a stalled emitter.
- `cpt-cf-usage-collector-fr-billing-retention-floor` — the floor that bounds
  replay, and past which a cursor is refused.
- `cpt-cf-usage-collector-nfr-query-freshness` — the floor that publishes no
  upper bound, which is why every freshness obligation is a per-plugin readiness
  gate rather than a gear-wide claim.
- `cpt-cf-usage-collector-fr-query-raw` — the audit path, distinct from the feed
  and not a substitute for it, because a forward event-time cursor has a
  late-arrival hole.
- `cpt-cf-usage-collector-fr-query-aggregation` — the derived view and its
  audience of dashboards, quota evaluation and reconciliation.
- `cpt-cf-usage-collector-nfr-billing-feed-freshness` — the feed ceiling a
  charging deployment requires, published by the active plugin.
- `cpt-cf-usage-collector-nfr-aggregate-freshness` — the separate aggregate
  ceiling, including the invalidation-propagation bound a materialised
  representation must publish.
- `cpt-cf-usage-collector-nfr-replay-throughput` — the recovery obligation,
  bounded by subscription scope rather than by gear-wide arrival rate.
- `cpt-cf-usage-collector-principle-aggregate-asymmetry` — the design principle
  this decision codifies, where feed and raw reads paginate and the aggregate
  does not.
- `cpt-cf-usage-collector-principle-cursor-gateway-ownership` — the gateway owns
  cursor encoding, not the plugin, on the feed path as on the raw path.
- `cpt-cf-usage-collector-principle-canonical-page` — the page envelope the feed
  shares with every list path.
- `cpt-cf-usage-collector-component-feed-gateway` — the component that owns
  the subscription and the cursor, and surfaces the plugin's refusal of a cursor.
- `cpt-cf-usage-collector-contract-downstream-usage-reader` and
  `cpt-cf-usage-collector-actor-usage-consumer` — the contract and the actor this
  surface serves.
- `cpt-cf-usage-collector-seq-read-feed` and
  `cpt-cf-usage-collector-usecase-consume-billing-feed` — the sequence and the
  use case that carry the read flow this decision fixes.
