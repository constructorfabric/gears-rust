---
status: accepted
date: 2026-09-29
---

# ADR-0008: Adopt PriceBook with explicit initial-binding contracts


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Explicit PriceBook contracts (chosen)](#explicit-pricebook-contracts-chosen)
  - [Orders-owned translation and policy](#orders-owned-translation-and-policy)
  - [Renewal as hold; optional totals](#renewal-as-hold-optional-totals)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-lifecycle-adr-pricebook-contracts`

## Context and Problem Statement

The integration baseline is `diffora/bss/products` at `16705a243f44e2a48eeb6ec903d2be34c854cf0d`.
Its Pricing model exposes plan revisions, item chain matrices and immutable approved prices.
Lifecycle's prior frontier-based snapshot no longer describes that model. Subscriptions SUB-D-29
and Rating T-D-37 have adopted PriceBook, with consumer adapters deferred. The SDK and reciprocal
contracts required by Orders are not all implemented or agreed.

## Decision Drivers

- The price evaluated for approval must be the price accepted for initial activation.
- A renewal pin may walk to an `all` successor; price immutability alone is not a price hold.
- Preserve one transition writer, audit/idempotency semantics and fail-closed admission.
- SDK contracts must carry seller scope and all consumer-required fields.

## Considered Options

- Adopt PriceBook and specify the missing initial-binding and prospective-purchase contracts.
- Keep the retired catalog model behind an Orders-owned translation/policy layer.
- Treat renewal resolve as a submit-to-activation hold and make missing totals optional.

## Decision Outcome

Choose explicit PriceBook contracts. One assessment freezes one revision per line and a common
assessment date; Pricing owns purchase eligibility, Rating owns monetary evaluation, Subscriptions
owns atomic activation and subsequent renewal. Lifecycle stores their evidence with the order
version and publishes a complete SDK for Workflow.

The Orders-side design is adopted. **Counterpart adoption and implementation are open prerequisites**;
this ADR does not accept policy on behalf of Pricing, Products, Rating or Subscriptions. D-150–D-158
and `UPSTREAM_REQS.md` distinguish local decisions from joint proposals. Missing required contracts
refuse admission rather than fall back to old APIs or substitute local commercial rules.

### Consequences

- Replace the line reference triple with plan revision and selected items; replace the old pin with `OrderPin`.
- Define initial acceptance separately from renewal, with an absolute activation deadline and
  receiver-side enforcement. Product must supply duration policy; state TTLs are not a substitute.
- Retain required totals/TCV, three charge kinds and explicit exclusions.
- Preserve the opaque overlap-key policy pending its owner's replacement definition; never infer `plan_id`.
- Keep immutable version reads and bounded event projections; full expanded pins are not duplicated in events.
- Preserve existing state transitions; expiry during fulfillment follows compensation, not a new expiry edge.

**Amended 2026-09-30 (D-159–D-168).** Where a seam already exists on `16705a243`, the contracts
above are pulled onto it rather than replaced by a new one:

- `OrderPin` nests `items[].chains[]` exactly as resolve answers the matrix (D-159); the six
  catalog predicates that the existing reads can answer are consumed through `PricingReadV1` over
  those reads, and only the residual verdict remains an ask (D-161).
- Pricing access is the D-424 pattern, a `bss-orders.system` subject in the seller tenant (D-160).
- Initial acceptance is verified by a pinned comparison at activation: Subscriptions sends the
  accepted bindings as pins to the ordinary resolve and refuses when a consumed slot's price moved;
  no new resolve mode, receipt or clock agreement (D-162, amended in round 2). The deadline is a
  locally derived early check, not a producer output. The driver "SDK contracts must carry seller
  scope" is amended by D-160: the seller is the subject's tenant, never an API parameter.
- The overlap key remains Subscriptions' SUB-G1 registry key, with the paid recurring item's SKU
  as the proposed PriceBook derivation (D-163).
- SKU protection is inherited from the revision's `plan_item` references; no Orders reservation (D-164).
- Provisioning maps onto Subscriptions' three instants and its `applied` status (D-165); the approval
  owner is Workflow's adapter over `cf-gears-bss-approval` (D-166); the Rating request uses resolve's
  vocabulary and PriceBook's two periods (D-167); the Ledger is not the billing chain (D-168).

### Confirmation

Static validation covers the design, linked feature contracts, reason vocabulary and reciprocal
amendment proposals. Runtime confirmation requires the producer SDKs plus tests for successor prices,
price-end boundaries, holds, concurrent activation, cross-tenant reads and idempotent retries.
No runtime integration is claimed by this document change.

## Pros and Cons of the Options

### Explicit PriceBook contracts (chosen)

- Good: matches the available model and makes missing operations reviewable.
- Bad: order taking remains blocked until owners implement the required seams.

### Orders-owned translation and policy

- Good: superficially minimizes changes to the order shape.
- Bad: recreates removed concepts and forks sellability; no producer can verify the resulting promises.

### Renewal as hold; optional totals

- Good: reuses an existing resolver signature.
- Bad: may change prices before activation and cannot satisfy required TCV approval input.

## More Information

The counterpart asks are registered in `UPSTREAM_REQS.md` §2.1, §2.2, §2.6 and §2.10, each citing the
inspected Pricing/Products code at `16705a243`, the Subscriptions and Rating decisions, and Workflow
`e0c24ba50`.

## Traceability

- [DESIGN](../DESIGN.md#contract-03-4-3), [Decisions](../DECISIONS.md#pricebook-seam-remediation-2026-09-29)
- [PRD](../PRD.md), [Upstream requirements](../UPSTREAM_REQS.md)
- `cpt-cf-bss-orders-lifecycle-fr-order-submit`
- `cpt-cf-bss-orders-lifecycle-nfr-order-snapshot-integrity`
- `cpt-cf-bss-orders-lifecycle-fr-order-amendment`
