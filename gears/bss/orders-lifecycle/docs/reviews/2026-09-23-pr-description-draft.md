# Replacement PR description — draft, not published

For PR #4775. The live description was checked on 2026-09-23; its inventories, review status,
platform assumptions and validation claims predate this branch. Replace the human-authored
description with the text below; preserve any bot-owned release-note block separately.

## Summary

Orders Lifecycle technical design: a shared transition engine, Foundation and seven capability
slices, architecture decisions and explicit upstream requirements. No Orders runtime is delivered.

The engine owns authorization integration, idempotency, state/version guards, commercial writes,
transactional audit and platform outbox enqueue. Public operations use PolicyEnforcer and scoped
toolkit persistence. Local audit follows Pricing's transactional pattern with immutable actor
references. Event publication uses the managed Event Broker SDK producer, not an Orders drain.

## Review focus

- Foundation's create/existing-order branches, mutable draft revision, idempotency recovery and
  overlap-claim transaction composition.
- Actual SDK GTS/event envelope and canonical error mappings.
- Version-bound acceptance, Workflow pre-dispatch cancellation fence, recheck ordering and
  acknowledgement-only Lifecycle line projection.
- Shared read authorization/logging, assessment diagnostics, and generation/policy-bound expiry.
- Full graph lifetime analysis: 74 TTL-covered dwell entries at baseline caps; no unconditional
  wall-clock lifetime claim without finite policy bounds and bounded scheduler delay.

## Remaining integration and Product work

Pricing fixed-version predicate/composition SDKs, Subscriptions cardinality/enforcement, Payments
and Workflow execution contracts, deployed PDP policy/grants, and broker runtime/retry/recovery/
observability prerequisites remain explicit. Required race, scope and wire tests remain future
implementation acceptance work. Product-owned TTLs, latency evidence and disclosed PRD divergences
remain open. Asynchronous publication alone does not prove the PRD latency target impossible.

## Review evidence

The local High and Medium disposition documents map the external review register to the current
contracts and identify remaining upstream work. Documentation checks cover TOCs, local links,
event-table structure, obsolete-contract searches and whitespace. They are not runtime tests or
proof of deployed enforcement. This change adds a mechanical invariant checker, positive/negative fixtures and a docs CI job; local validation is distinct from an observed remote CI run.
