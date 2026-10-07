---
status: accepted
date: 2026-05-24
---

Created:  2026-05-22 by Virtuozzo International GmbH
Updated:  2026-09-17 by Virtuozzo International GmbH

# Mandatory idempotency key on every ingestion entry

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Same-key outcomes](#same-key-outcomes)
  - [Guarantee levels](#guarantee-levels)
  - [Idempotency horizon](#idempotency-horizon)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Mandatory idempotency key, plugin-enforced deduplication](#mandatory-idempotency-key-plugin-enforced-deduplication)
  - [Optional key with best-effort deduplication](#optional-key-with-best-effort-deduplication)
  - [Server-generated key](#server-generated-key)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-usage-collector-adr-mandatory-idempotency`

## Context and Problem Statement

At-least-once delivery is the operational baseline for REST and SDK callers.
Retries are routine, so duplicate submissions are expected.

Each GTS type declares one aggregation fold from a closed set, and a duplicate
harms every fold. Under `SUM` a retry inflates the accrued total, with no way to
detect or correct it. Under any other fold a duplicate observation poisons
consumers that derive counts, distinct observation windows, or rate-of-change
signals from raw entries. Deduplication is therefore required for correctness
whatever the fold.

The question is whether the ingestion contract requires an idempotency key on
every entry, merely encourages one, or relies on backend deduplication after the
fact. The answer shapes the contract obligations, the rejection error surface,
the plugin's storage schema, and how a calling gear retries a transient failure.

A same-key collision is not uniformly safe to absorb. An exact-equality retry
that re-sends identical content is the benign case. A key reused with different
content is a caller bug, and the gear must surface it rather than drop it.

## Decision Drivers

- `cpt-cf-usage-collector-fr-idempotency` — the ingestion contract requires an
  idempotency key, and the gear delegates deduplication to the active plugin.
  Retry safety must hold for every declared fold, so a calling gear needs one
  retry pattern rather than a fold-dependent one.
- `cpt-cf-usage-collector-fr-aggregation-fold` — a retry under a `SUM` fold
  inflates the accrued total.
- `cpt-cf-usage-collector-nfr-ingestion-latency` — enforcement sits on the
  synchronous ingestion path and must fit the 200 ms p95 budget.
- `cpt-cf-usage-collector-principle-fail-closed` — a keyless measurement must draw a
  deterministic rejection, never silent acceptance.

## Considered Options

- Mandatory idempotency key, plugin-enforced deduplication — the contract
  requires the key, and the gear rejects a keyless measurement. The active plugin
  enforces the dedup identity at the storage layer, at a concurrency level it
  declares.
- Optional key with best-effort deduplication — a caller can supply a key. The
  plugin deduplicates when one is present, and stores the entry as-is when it is
  absent.
- Server-generated key — the gear derives a fingerprint from the entry's own
  content, and a caller supplies no key.

## Decision Outcome

Chosen option: "Mandatory idempotency key, plugin-enforced deduplication". It is
the only option that makes at-least-once delivery safe for every declared fold
without a fold-dependent retry strategy in every calling gear.

Every entry carries a caller-supplied key on the REST, SDK, and Plugin SPI
contracts, because a measurement and an invalidation ride the same ingestion
path. The gear rejects a keyless entry of either kind with a deterministic
error. A measurement carries the caller's own key. An invalidation carries its
target's key, as part of the faithful copy that identifies the target, so every
key admissible under the ordinary key rules is admissible on either kind.

The entry type is what places at most one invalidation per entry inside this
decision. An invalidation copies its target's tenant, type, key, and covered
period, and declares its own entry type, so every withdrawal of one entry has
one dedup identity, distinct from the target's. Two of them then resolve by the
same-key outcomes below rather than by a store-side rule of their own
(`cpt-cf-usage-collector-adr-append-only-invalidation`).

The dedup identity is the tenant, the GTS type, the idempotency key, both
bounds of the covered period, and the entry type. The active plugin enforces it
at the storage layer, under the guarantee levels below, and the gear keeps no
dedup table of its own. A plugin that enforces the first five alone treats every
invalidation as a collision with its target, so the entry type binds every
plugin exactly as the other five do. Two submissions under one key that cover
different periods, or that differ in entry type, are therefore distinct
entries, not a conflict.

That identity is deliberately minimal, so the key carries what it leaves out.
Resource and subject are compared on a collision but are not part of the
identity, which puts one obligation on a caller: a key that varies with
everything the identity omits, repeated exactly on a retry.

### Same-key outcomes

A same-key submission inside the horizon that meets a converged entry resolves
into one of two outcomes. Converged is defined under the guarantee levels below.

It is an exact-equality retry when every caller-supplied canonical field outside
the identity matches the stored entry: quantity, resource, subject, metadata, and
reason code. The plugin absorbs the submission and returns the stored entry, and
the gear acknowledges it as accepted. No surface carries a separate duplicate
outcome. A caller that must tell a replay from a first write reads the returned
acceptance instant, which is the stored entry's.

It is a canonical-field mismatch when any of those fields differs, a
metadata-only difference included. The plugin reports a conflict that carries the
existing entry's identifier, and the gear rejects the submission fail-closed in
the conflict (`Aborted`) error category
(`cpt-cf-usage-collector-principle-canonical-errors`). Against a converged entry
the second write is never silently dropped.

Entry type is not compared, because it is part of the identity. A measurement
and its invalidation share tenant, type, key, and covered period and differ in
entry type, so they never collide. The identifier of the withdrawn entry is not
compared either. The gear derives it from the invalidation's own identity
fields, so it cannot differ unless one of those does, and then the identity
differs too.

For an invalidation the two outcomes read as a withdrawal rule. The faithful copy
fixes every other compared field to the target's, so a second invalidation of
one entry differs, if at all, only in its reason code. The same reason code is an
exact-equality retry and returns the stored invalidation. A different one is a
conflict, which the gear reports as an already-invalidated target rather than as
a key reuse, because the key is the target's by construction. A conflict whose
stored entry has a different entry type from the submission's cannot arise from
a conforming plugin, and the gear reports it as an internal error rather than as
an already-invalidated target.

### Guarantee levels

Two writes under one identity can reach the store before either has seen the
other: through two gateway replicas, past a lagging replica, or as a timed-out
insert that commits after its caller was told to retry. Not every backend can
decide such a pair synchronously, because a store with no uniqueness constraint
learns of the second write only after both have landed. The decision therefore
fixes the guarantee rather than the mechanism. It decides every such pair by the
store's own **commit order**: the order in which the store makes writes under
one identity durable, as the plugin's own state records it. A replicated
insert-block number is one such order, and so is the row order inside one
coalesced backend write. A gateway timestamp never is, so clock skew between
replicas plays no part.

The **survivor** of an identity is its first write in commit order. An identity
has **converged** once its survivor is final — no write preceding it in commit
order can still become visible — the survivor is visible to every dedup check
the plugin runs, and no persist call under the identity that did not see the
survivor is still to return its outcome. The survivor is then a converged entry,
until retention frees the identity no other write under it ever is, and every
later write is decided against it. A plugin establishes convergence from its own
commit or replication state, never from elapsed time. Each plugin publishes a
**convergence bound**, the longest time from an acknowledgement to convergence.
An identity that converges later than the bound is a conformance defect, counted
on a metric the plugin's guide names.

The floor binds every plugin:

- One identity is one entry. Every read path, fold, reconciliation counter and
  watermark, materialised aggregate, and the feed shows the survivor and nothing
  else under that identity. A read before convergence may show a write that
  proves not to be first, never two.
- No write displaces a converged survivor, because every later write follows it
  in commit order. The guarantee is tied to the moment the plugin returns its
  outcome, not to when a write arrived or began its dedup check, and gateway
  forwarding after that moment does not count. No acknowledgement the plugin
  returns after an identity has converged accepts divergent content under it: a
  later write with identical content is absorbed and returns the stored entry,
  and a divergent one draws the conflict. An acknowledgement returned before
  convergence may be of a write that is later discarded. A write whose caller has already been answered,
  such as a timed-out insert that commits late, is discarded, and counted when
  its content diverges.
- Two submissions under one identity inside one ingestion request never race.
  The Ingestion Gateway decides the later against the earlier accepted one
  before dispatch.

Above the floor, each plugin declares one of two levels in its published
consistency profile (`cpt-cf-usage-collector-design-consistency-contract`):

- `linearizable` — the convergence bound is zero. Every write is decided against
  every write before it in commit order as it commits: an identical one is
  absorbed, and a divergent one draws the conflict.
- `eventual` — a write can be acknowledged before the plugin knows whether an
  earlier write under its identity precedes it. When one does, the later write
  is discarded once the identity converges, and the plugin counts every divergent
  discard on a metric it names. The feed never returns a discarded write, because
  an entry is settled only once it has converged. A materialised aggregate or
  reconciliation figure that counted one recomputes on convergence.

An `eventual` acknowledgement can therefore disagree with later reads. For an
identical write the difference is the acceptance instant and the origin marker,
and nothing measured is lost. For a divergent write the difference is the
content, and a later retry of that write draws the conflict. That is the one case
in which the gear drops a divergent write without a conflict. Only a caller
defect reaches it — one key reused with different content, acknowledged before
the identity has converged — because a genuine retry repeats its content. The
collision metric keeps the defect visible to the operator, but the caller loses the
conflict it would otherwise receive. No meter is gated on the level, charging
meters included: a conforming emitter never reaches the race.

An invalidation resolves its target only once the target's identity has
converged, so no faithful copy is taken from a write the store later discards.
The lookup never reports an acknowledged, retained target as missing, and reaches a definite answer within
the plugin's published bounds
(`cpt-cf-usage-collector-adr-append-only-invalidation`).

### Idempotency horizon

Retention bounds the horizon. It is not unbounded. A dedup identity stays visible
to later submissions for at least as long as the referenced GTS type's retention
policy keeps that entry. The horizon runs from the covered period's end, not from
acceptance, so it is per-meter. A storage plugin must honour the floor above
over at least that span.

The horizon is a floor rather than an exact boundary. A deployment never has to
retain a dedup identity beyond the data it protects, and a purge or archive of an
entry frees its dedup identity with it. A purge runs on the plugin's own
schedule, so an aged entry can sit in the store after its horizon ends, and its
dedup identity stays live for as long as it does.

No admissible submission reaches the far side of the floor. The covered period
enters the dedup identity, and it is also what the ingestion bounds are checked
against — the same two timestamps serve both purposes. No path admits a period
ending further back than the backfill window
(`cpt-cf-usage-collector-adr-backfill-isolation`), and every meter's retention is
at least that window plus one replay horizon
(`cpt-cf-usage-collector-fr-billing-retention-floor`). The window therefore sits
strictly inside retention on every meter rather than merely within it, and a
submission old enough for its counterpart to have been purged is refused on the
bound, before deduplication is consulted at all. The horizon is a floor the
ingestion surface cannot walk off.

No caller therefore carries a post-horizon obligation, because on the ingestion
side there is no past the horizon. A consumer still deduplicates on the entry
identifier (`cpt-cf-usage-collector-fr-record-identity`), for the at-least-once
feed delivery that is its own reason.

### Consequences

- Every entry carries an idempotency key on every surface, and a caller cannot
  omit it from either kind. An invalidation repeats its target's key.
- A calling gear adopts one retry pattern. The same key with identical content is
  a safe retry, and the same key with different content is a conflict.
  Fold-dependent retry logic disappears from every emitter.
- A caller that reuses a key with different content receives a deterministic
  conflict rejection whenever it is answered after the identity has converged,
  and always under `linearizable`. Under `eventual` a reuse answered before that
  convergence can be
  acknowledged and then discarded, so that defect surfaces on the plugin's
  collision metric rather than to the caller.
- The active plugin owns the deduplication primitive and the conflict path. The
  gear maintains no dedup table. The plugin's level is part of its published
  profile, and a consumer that relies on `linearizable` couples itself to that
  plugin.
- At most one invalidation per entry needs no store-side rule of its own. Two
  withdrawals of one entry share one identity — the target's tenant, type, key,
  and covered period with the invalidation entry type — so the floor and the
  declared level govern them exactly as they govern two measurements.
- A plugin keys everything that dedups on the identity — a uniqueness
  constraint, the read-back of a conflicting entry, an in-request collapse — on
  all six parts or on the entry identifier, which covers them. Once an entry is
  withdrawn, the first five parts match two entries.
- A keyless measurement draws a deterministic rejection through the same error
  contract as any other validation failure.
- A caller chooses its own measurement keys. The contract bounds the key's
  length and forbids control characters so that the identifier derivation stays
  injective. No token shape is enforced otherwise, on either kind. A caller that
  generates an opaque key, such as a ULID or a UUIDv7, must therefore store it
  before the first send.
- An imported entry already carries a partly spent horizon, because retention
  runs from the covered period's end rather than from import time. What remains is
  never exhausted while the entry is still submittable: an import reaching past
  the backfill window is refused on the bound, so a re-run either lands inside
  the horizon and is deduplicated, or does not land. There is no third case in
  which the gear admits a silent duplicate.

### Confirmation

- Ingestion contract tests that reject, on every surface, a keyless measurement
  and a keyless invalidation.
- The `record-and-invalidation-distinct-identity` plugin SPI contract test: a
  measurement, then its invalidation under the same key and covered period, then
  a retry of the measurement. All three are accepted, the retry is absorbed, and
  a read returns exactly two entries.
- A test that a second invalidation of one entry is absorbed under the same
  reason code and rejected as an already-invalidated target under a different
  one.
- Duplicate-submission tests over both arms and every declared fold. An
  exact-equality retry yields an accepted acknowledgement carrying the stored
  entry, and a same-key submission with one differing canonical field yields a
  conflict rejection.
- A test that two resources measured in one period under one key yield one
  accepted entry and one conflict.
- Plugin SPI conformance tests for the floor. One identifier folds, reads, and
  counts in reconciliation at most once. A retry against a converged entry is
  absorbed, and a divergent submission against one is a conflict. Two
  same-identity submissions inside one request resolve as a retry or a conflict
  at either level.
- A race conformance test that drives an identical pair and a divergent pair on
  one identity through several gateway replicas before the identity converges.
  A plugin declaring `linearizable` absorbs the identical pair and yields one
  acceptance and one conflict for the divergent one. A plugin declaring
  `eventual` yields, for each pair, one survivor — the first in commit order —
  and no read path returning two. The feed delivers only the survivor, and the
  divergent pair adds one collision count.
- A late-commit conformance test: an insert reported as a transient failure,
  stamped with an earlier acceptance instant, commits after the identity has
  converged on a retry with different content. The converged entry and its feed delivery
  stay unchanged at either level, and the discard adds one collision count.
- A test that walks a submission's covered period from inside the backfill window
  to beyond it, asserting the far side yields a bound rejection rather than any
  deduplication outcome.
- A plugin SPI conformance test for the horizon, driving the plugin directly
  rather than the ingestion route, which refuses an over-aged period on the
  bound before deduplication is consulted. A replay inside the horizon resolves
  to the stored entry or a conflict. Beyond it the test asserts only that the
  submission draws one of three outcomes, selected by whether the plugin has
  purged the counterpart: acceptance as a new entry once it has, the stored
  entry while it has not and every canonical field matches, or a conflict while
  it has not and one differs. The gear guarantees no single one, because the
  purge schedule is the plugin's.

## Pros and Cons of the Options

### Mandatory idempotency key, plugin-enforced deduplication

Every entry carries a caller-supplied key: a measurement its own, and an
invalidation its target's. The plugin enforces the dedup identity at the storage
layer, at the level it declares.

- Good, because the ingestion path never consults the declared fold. The contract
  carries no fold-dependent special case, so an emitter uses one retry pattern
  for every GTS type it emits.
- Good, because the deduplication primitive lives at the storage layer, where it
  is cheapest and most correct.
- Good, because stating the guarantee rather than a uniqueness constraint admits a
  backend that has none, such as a columnar store, at the `eventual` level.
- Good, because a conflict on key reuse with different content keeps a caller bug
  visible — to the caller under `linearizable`, and to the operator through the
  collision metric under `eventual`. Billing and other consumers never see two
  entries behind one identity.
- Good, because a keyless measurement fails closed deterministically, which matches
  the fail-closed principle.
- Neutral, because a caller must generate the key and must repeat it on a retry.
  The contract bounds the key's length and charset and enforces no token shape.
- Bad, because under `eventual` a concurrent key reuse with different content is
  acknowledged and then discarded, so the caller that sent it never learns of it.
- Bad, because a mandatory contract field is a breaking-change risk if it is
  relaxed later. `cpt-cf-usage-collector-adr-contract-stability` governs that
  risk.

### Optional key with best-effort deduplication

A caller can supply a key. The plugin deduplicates when one is present, and
otherwise stores the entry as-is.

- Good, because the contract is permissive and costs a casual caller less effort.
- Bad, because duplicate safety then depends on caller discipline instead of a
  guarantee, which is the gap that `cpt-cf-usage-collector-fr-idempotency` exists
  to close.
- Bad, because without a guaranteed key, emitter retry logic becomes
  fold-dependent again.
- Bad, because dashboards and billing pipelines downstream lose the guarantee
  that every entry is dedup-protected.

### Server-generated key

The gear derives a key from the entry's attribution, timestamp, and quantity. A
caller supplies none.

- Good, because a caller needs to generate and persist no key.
- Bad, because the derivation cannot separate a legitimate replay from a
  coincidental repeat that shares attribution, timestamp, and quantity. A
  low-cardinality quantity makes this likely.
- Bad, because retries from one emitter can carry different timestamps, and
  therefore different derived keys, which defeats the purpose.
- Bad, because the derivation logic then lives in the gear and becomes a
  maintenance burden tied to the deduplication primitive.

## More Information

Related decisions:

- `cpt-cf-usage-collector-adr-pluggable-storage` — the SPI that enforces the
  deduplication primitive.
- `cpt-cf-usage-collector-adr-caller-supplied-attribution` — the attribution
  fields that enter the dedup boundary.
- `cpt-cf-usage-collector-adr-record-identity-derivation` — the entry identifier
  that a consumer deduplicates on across at-least-once feed delivery.
- `cpt-cf-usage-collector-adr-backfill-isolation` — the window bound that keeps
  every admissible submission inside the horizon.
- `cpt-cf-usage-collector-adr-append-only-invalidation` — the invalidation entry
  whose reason code joins the canonical-field set, and whose entry type in the
  dedup identity bounds withdrawal to one per entry.
- `cpt-cf-usage-collector-adr-consistency-contract` — the published plugin
  profile that carries the declared dedup level.
- `cpt-cf-usage-collector-adr-contract-stability` — governs any later relaxation
  of the mandatory field.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-usage-collector-fr-idempotency` — the mandatory idempotency key on the
  ingestion contract.
- `cpt-cf-usage-collector-fr-aggregation-fold` — a retry under a `SUM` fold
  inflates the accrued total.
- `cpt-cf-usage-collector-fr-record-quantity` — the quantity that a retry
  duplicates.
- `cpt-cf-usage-collector-nfr-ingestion-latency` — keeps enforcement on the
  synchronous ingestion path inside the 200 ms p95 budget.
- `cpt-cf-usage-collector-principle-idempotency-by-key` — the design principle
  that this decision codifies.
- `cpt-cf-usage-collector-principle-canonical-errors` — the error contract that
  carries the conflict rejection.
- `cpt-cf-usage-collector-interface-plugin` — the SPI surface that enforces the
  dedup identity.
- `cpt-cf-usage-collector-design-consistency-contract` — the deployment-guide
  profile in which each plugin declares its dedup level.
- `cpt-cf-usage-collector-fr-record-invalidation` — the invalidation entry that
  repeats its target's key, and whose at-most-one rule follows from the dedup
  identity.
- `cpt-cf-usage-collector-fr-record-identity` — the entry identifier that a
  consumer deduplicates on across at-least-once feed delivery.
- `cpt-cf-usage-collector-fr-usage-windows` — the covered period whose bounds
  enter the dedup identity.
- `cpt-cf-usage-collector-fr-billing-retention-floor` — the per-meter retention
  that sets the horizon.
