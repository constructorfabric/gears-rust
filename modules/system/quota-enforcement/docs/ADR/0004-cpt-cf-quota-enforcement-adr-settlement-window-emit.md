---
status: accepted
date: 2026-05-07
---

# Settlement window emit policy — emit nothing during settlement

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Emit with `is_post_rollover` flag](#a-emit-with-is_post_rollover-flag)
  - [(b) Emit nothing during settlement; rely on period-rollover payload](#b-emit-nothing-during-settlement-rely-on-period-rollover-payload)
  - [(c) New event variant `closing-period-mutation`](#c-new-event-variant-closing-period-mutation)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-settlement-window-emit`

## Context and Problem Statement

Cross-period leases (`cpt-cf-quota-enforcement-fr-lease-commit` cross-period section) mean a lease commit / release /
TTL auto-release may fire after the wall-clock period boundary while still being attributed to the closing period.
During this *settlement window* — between `period_end` and the moment the last active lease acquired in the closing
period has resolved — counter mutations land on the closing-period counter.

The question this ADR resolves: should `quota-counter-adjusted` / `threshold-crossed` events be emitted for these
settlement-window mutations? Three options were on the table.

## Decision Drivers

- Subscriber sanity (no spurious "in the new period?" signals from closing-period mutations).
- Simplicity of the event catalogue (avoid adding new event variants or boolean flags).
- Closing-period state must remain reconstructible by sinks.

## Considered Options

- (a) Emit `quota-counter-adjusted` + `threshold-crossed` as usual, with an `is_post_rollover` boolean flag in the
  payload.
- (b) Emit nothing during the settlement window; closing-period state is surfaced via the `period-rollover` event
  payload alone (which carries `closing_consumed` and `closing_cap`).
- (c) Emit a new dedicated event variant `closing-period-mutation`.

## Decision Outcome

Chosen option: **(b) — emit nothing during the settlement window**, because closing- period state is fully
reconstructible from the `period-rollover` event payload alone, no new event kinds or flags are introduced, and
subscriber discipline stays uniform across event kinds.

### Consequences

- Cross-period commits / releases / TTL auto-releases mutate the closing-period counter but emit no
  `quota-counter-adjusted` or `threshold-crossed` events.
- `period-rollover` event is emitted once, after the last active lease attributed to the closing period resolves;
  carries `closing_consumed` and `closing_cap` for sinks to reconstruct closing state.
- Sinks needing per-mutation auditability for settlement-window operations consume the operation-log read API instead of
  notifications.

### Confirmation

Confirmed by storage-plugin tests that exercise cross-period commit / release scenarios and assert the absence of
`quota-counter-adjusted` / `threshold-crossed` events during settlement, plus presence of the consolidated
`period-rollover` payload after closure.

## Pros and Cons of the Options

### (a) Emit with `is_post_rollover` flag

- Good, because preserves event uniformity — every counter mutation produces an event.
- Bad, because every consumer of `threshold-crossed` must learn the flag and route accordingly — high cognitive load on
  subscribers.
- Bad, because subtle: a `threshold-crossed` flagged `is_post_rollover` semantically differs from one without the flag.

### (b) Emit nothing during settlement; rely on period-rollover payload

- Good, because clean event stream — events for the new period are unambiguous.
- Good, because no new event kinds or flags; existing catalogue suffices.
- Good, because closing state arrives once, atomically, in `period-rollover`.
- Bad, because per-mutation visibility for settlement operations requires the operation log; acceptable since the log
  exists anyway for audit.

### (c) New event variant `closing-period-mutation`

- Good, because explicit semantic separation.
- Bad, because event-catalogue bloat for marginal benefit.
- Bad, because `period-rollover` already carries closing state — duplication.

## More Information

Eliminates the need for new event variants for cross-period commits/releases during the settlement window.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

- `cpt-cf-quota-enforcement-fr-period-rollover` — event payload semantics.
- `cpt-cf-quota-enforcement-fr-notification-plugin` — event catalogue stability.
- DESIGN sequence `cpt-cf-quota-enforcement-seq-period-rollover`.
