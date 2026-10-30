<!-- CONFLUENCE_TITLE: [BSS]: Rating — Deferred: price adjustments (design) -->

# Rating — Deferred: price adjustments (design)

<!-- toc -->

- [Overlays: Architectural Vision](#overlays-architectural-vision)
- [Overlays: Architecture Drivers](#overlays-architecture-drivers)
- [Overlays: Architecture Layers](#overlays-architecture-layers)
- [Overlays: Design Principles](#overlays-design-principles)
- [Overlays: Constraints](#overlays-constraints)
- [Overlays: Domain Model](#overlays-domain-model)
- [Overlays: Component Model](#overlays-component-model)
- [Overlays: API Contracts](#overlays-api-contracts)
- [Overlays: Internal Dependencies](#overlays-internal-dependencies)
- [Overlays: External Dependencies](#overlays-external-dependencies)
- [Overlays: Database Schemas and Tables](#overlays-database-schemas-and-tables)
- [Overlays: Deployment Topology](#overlays-deployment-topology)
- [Overlays: Scope → Tenant-Axis Mapping (normative)](#overlays-scope--tenant-axis-mapping-normative)
- [Overlays: Stacking and the Total Order (normative)](#overlays-stacking-and-the-total-order-normative)
- [Overlays: Contract Overlay Precedence (normative)](#overlays-contract-overlay-precedence-normative)
- [Overlays: Bounded Composition — Anti-Drift Cap (normative)](#overlays-bounded-composition--anti-drift-cap-normative)
- [Overlays: Traceability](#overlays-traceability)
- [Commitments: Architectural Vision](#commitments-architectural-vision)
- [Commitments: Architecture Drivers](#commitments-architecture-drivers)
- [Commitments: Architecture Layers](#commitments-architecture-layers)
- [Commitments: Design Principles](#commitments-design-principles)
- [Commitments: Constraints](#commitments-constraints)
- [Commitments: Domain Model](#commitments-domain-model)
- [Commitments: Component Model](#commitments-component-model)
- [Commitments: API Contracts](#commitments-api-contracts)
- [Commitments: Internal Dependencies](#commitments-internal-dependencies)
- [Commitments: External Dependencies](#commitments-external-dependencies)
- [Commitments: Database Schemas and Tables](#commitments-database-schemas-and-tables)
- [Commitments: Deployment Topology](#commitments-deployment-topology)
- [Commitments: Commitment-Pool Waterfall (normative)](#commitments-commitment-pool-waterfall-normative)
- [Commitments: Reservation Flavors and Pool Precedence (normative)](#commitments-reservation-flavors-and-pool-precedence-normative)
- [Commitments: Reserved-Rate Two-Source Rule (normative)](#commitments-reserved-rate-two-source-rule-normative)
- [Commitments: Commitment Pool vs Prepaid Credit Grant (normative)](#commitments-commitment-pool-vs-prepaid-credit-grant-normative)
- [Commitments: Obligations, Reversals, and Period Boundaries (normative)](#commitments-obligations-reversals-and-period-boundaries-normative)
- [Commitments: Traceability](#commitments-traceability)
- [Coupons: Architectural Vision](#coupons-architectural-vision)
- [Coupons: Architecture Drivers](#coupons-architecture-drivers)
- [Coupons: Architecture Layers](#coupons-architecture-layers)
- [Coupons: Design Principles](#coupons-design-principles)
- [Coupons: Constraints](#coupons-constraints)
- [Coupons: Domain Model](#coupons-domain-model)
- [Coupons: Component Model](#coupons-component-model)
- [Coupons: API Contracts](#coupons-api-contracts)
- [Coupons: Internal Dependencies](#coupons-internal-dependencies)
- [Coupons: External Dependencies](#coupons-external-dependencies)
- [Coupons: Database Schemas and Tables](#coupons-database-schemas-and-tables)
- [Coupons: Deployment Topology](#coupons-deployment-topology)
- [Coupons: Placement and the FX Split (normative)](#coupons-placement-and-the-fx-split-normative)
- [Coupons: Stacking Policies (normative)](#coupons-stacking-policies-normative)
- [Coupons: applyScope Attachment and Hybrid Split-Back (normative)](#coupons-applyscope-attachment-and-hybrid-split-back-normative)
- [Coupons: Frozen Coupon Snapshot and Fail-Closed Rules (normative)](#coupons-frozen-coupon-snapshot-and-fail-closed-rules-normative)
- [Coupons: Snapshot Segment and Discount Lineage (normative)](#coupons-snapshot-segment-and-discount-lineage-normative)
- [Coupons: Traceability](#coupons-traceability)
- [Currency and FX: Architectural Vision](#currency-and-fx-architectural-vision)
- [Currency and FX: Architecture Drivers](#currency-and-fx-architecture-drivers)
- [Currency and FX: Architecture Layers](#currency-and-fx-architecture-layers)
- [Currency and FX: Design Principles](#currency-and-fx-design-principles)
- [Currency and FX: Constraints](#currency-and-fx-constraints)
- [Currency and FX: Domain Model](#currency-and-fx-domain-model)
- [Currency and FX: Component Model](#currency-and-fx-component-model)
- [Currency and FX: API Contracts](#currency-and-fx-api-contracts)
- [Currency and FX: Internal Dependencies](#currency-and-fx-internal-dependencies)
- [Currency and FX: External Dependencies](#currency-and-fx-external-dependencies)
- [Currency and FX: Database Schemas and Tables](#currency-and-fx-database-schemas-and-tables)
- [Currency and FX: Deployment Topology](#currency-and-fx-deployment-topology)
- [Currency and FX: Currency Role Separation (normative)](#currency-and-fx-currency-role-separation-normative)
- [Currency and FX: FX Policy Semantics (normative)](#currency-and-fx-fx-policy-semantics-normative)
- [Currency and FX: FX-Lock Snapshot Segment (normative)](#currency-and-fx-fx-lock-snapshot-segment-normative)
- [Currency and FX: Ordering and Precision at the FX Boundary (normative)](#currency-and-fx-ordering-and-precision-at-the-fx-boundary-normative)
- [Currency and FX: Traceability](#currency-and-fx-traceability)
- [Integration contracts: Finance FX Input Contract (normative)](#integration-contracts-finance-fx-input-contract-normative)
- [Integration contracts: Promotions Coupon Snapshot Contract (normative)](#integration-contracts-promotions-coupon-snapshot-contract-normative)
- [Integration contracts: Contracts & Agreements Input Contract (normative)](#integration-contracts-contracts--agreements-input-contract-normative)

<!-- /toc -->

**Status: deferred, not part of the implementation contract** (2026-10-12, PR #5175 design review).
Rating does not build price adjustments now: contract overrides and the anti-drift cap (namespace
04), commitments and reservations (05), coupons (06) and FX conversion (07), with their input
contracts (former DESIGN 11 §4.4, §4.5, §4.9). None has a source on `main`. The text below is kept
verbatim from DESIGN §6 as the starting point for a future design; its section addresses and
identifiers are unchanged and are listed in [DECOMPOSITION §4](../DECOMPOSITION.md#4-contract-address-index)
as deferred. It is not validated by cfs and no live document depends on it.

**Reopens when**: Contracts provides overlays, commitment pools or balances (DECISIONS R-11,
UPSTREAM_REQS `…-upreq-contracts-inputs`); Pricing ships promotions (pricing D-409,
`…-upreq-promotions-coupon-snapshots`); Finance provides pinnable FX rates (R-07,
`…-upreq-fx-rate-snapshots`). Each activation is a new engine generation and a new design review.
Until then `rating-core` keeps one fail-closed guard (DESIGN [01 §4.4](../DESIGN.md#contract-01-4-4),
`adjustment_unsupported`, and `fx_not_supported`).

<a id="contract-04"></a>

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-overlays-precedence`

<a id="contract-04-1-1"></a>

<!-- contract:04-overlays-precedence:1.1 -->
### Overlays: Architectural Vision

This contract was the **steps 4–5 evaluator**: step 4 stacked pricing `PriceOverlay`s, step 5
applied the customer/contract overlay, bounded by an anti-drift cap.

**Step 4 has no input on main.** Pricing dropped `PriceOverlay`, partner pricing and overlay
precedence with the PriceBook model (pricing PRD spec item 2; D-384: "a plan-specific exception uses
another book or SKU"; customer-group-to-book mapping is "later, by separate decision"); the legacy
overlay tables are refused at boot (`m0000_pricing_refuse_a_legacy_or_stale_schema.rs:66, 73-75`).
A price differs for a customer only because the plan revision binds another book, entry or price,
and that arrives as the binding (T-D-73). Step 4 therefore passes the step-3 amount through
unchanged; no overlay is read, stacked or recorded.

**Step 5 stays dormant.** Contract and account overlays need a Contracts source; only a first-draft
Contracts PRD exists (R-11). Until a source is added to the `EvaluationInput`, step 5 receives no
terms and passes the amount through; a line that references a contract overlay fails closed
(`contract_dimension_violation` / `pool_input_torn` family, [01 §4.4](../DESIGN.md#contract-01-4-4)).

<!-- /contract -->

<a id="contract-04-1-2"></a>

<!-- contract:04-overlays-precedence:1.2 -->
### Overlays: Architecture Drivers

<a id="contract-04-functional-drivers"></a>

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-priceoverlay-scope-mapping` | No input: `PriceOverlay` does not exist on main (§1.1). Kept for traceability; realized by Pricing binding the book/entry/price per plan revision. |
| `cpt-cf-bss-rating-fr-overlay-stacking` | No input on main (§1.1). |
| `cpt-cf-bss-rating-fr-customer-contract-overlay` | `ContractOverlayApplier` (dormant) runs after step 3 when a Contracts source exists (§4.3, R-11). |
| `cpt-cf-bss-rating-fr-bounded-composition-cap` | `CompositionCapGuard` (dormant) bounds a future step-5 stack; an uncapped stack of depth ≥ 2 fails closed (§4.4). |

<a id="contract-04-nfr-allocation"></a>

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | — | Steps 4–5 are pass-through at launch | — |
| `cpt-cf-bss-rating-nfr-audit-segregation` | Lineage | Lineage records that steps 4–5 applied nothing | Vectors |
| `cpt-cf-bss-rating-nfr-resilience` | Fail closed | A line referencing an overlay source that does not exist fails closed | Vectors |

<a id="contract-04-key-adrs"></a>

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Historical; **superseded** by T-D-73 — there is no `price_overlay` axis and no overlay on main. |

<!-- /contract -->

<a id="contract-04-1-3"></a>

<!-- contract:04-overlays-precedence:1.3 -->
### Overlays: Architecture Layers

**Contract**: `cpt-cf-bss-rating-tech-stack-ovl` (`p3`), defined in [§1.3 Namespace technology layers](../DESIGN.md#register-tech).

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | Pass-through step 4; dormant `ContractOverlayApplier`, `CompositionCapGuard`; lineage | `rating-core` module (pure Rust) |
| Infrastructure | None | — |

<!-- /contract -->

<a id="contract-04-2-1"></a>

<!-- contract:04-overlays-precedence:2.1 -->
### Overlays: Design Principles

<a id="contract-04-stack-all-survivors-tie-break-dont-exclude"></a>

#### Stack all survivors (dormant)

**Contract**: `cpt-cf-bss-rating-principle-stack-not-winner-ovl` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

**Superseded 2026-10-08 (T-D-73).** No pricing overlay exists to stack (T-D-02 obsolete). The principle survives only for a
future step-5 source: every applicable term contributes; ordering never removes a term.

<a id="contract-04-one-total-order-no-arbitrary-picks"></a>

#### One total order, no arbitrary picks

**Contract**: `cpt-cf-bss-rating-principle-total-order-ovl` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

When a step-5 source exists, its terms apply in one deterministic total order defined with that
source; the same input always stacks in the same order.

<a id="contract-04-axes-are-typed-not-inferred"></a>

#### Axes are typed, not inferred

**Contract**: `cpt-cf-bss-rating-principle-typed-axes-ovl` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

A contract term that cannot be matched from the evaluation input fails the child closed; it never
matches by similarity.

<!-- /contract -->

<a id="contract-04-2-2"></a>

<!-- contract:04-overlays-precedence:2.2 -->
### Overlays: Constraints

<a id="contract-04-publish-side-validation-is-relied-on-not-re-run"></a>

#### No publish-side overlay validation exists

**Contract**: `cpt-cf-bss-rating-constraint-publish-side-guarantees-ovl` (`p1`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

**Superseded 2026-10-08 (T-D-73).** Pricing publishes no overlays, and Rating has no validator role in the shared
`bss-approval` engine (R-12 closed); the former `rating-val-01` precedence rule has nothing to judge.

<a id="contract-04-the-overlay-segment-is-sealed"></a>

#### No overlay segment

**Contract**: `cpt-cf-bss-rating-constraint-overlay-segment-ovl` (`p1`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

The snapshot body has no overlay segment (T-D-44 as revised by T-D-73); a replay reads the stored
binding of record, which already reflects any customer-specific book or price.

<a id="contract-04-caps-clamp-or-fail--never-silently-compound"></a>

#### Caps clamp or fail — never silently compound

**Contract**: `cpt-cf-bss-rating-constraint-cap-modes-ovl` (`p1`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

For a future step-5 stack: a result beyond the configured cap clamps and records, or fails closed
under a hard cap. No cap value, mode or Finance default exists on main (PRD §15 open).

<!-- /contract -->

<a id="contract-04-3-1"></a>

<!-- contract:04-overlays-precedence:3.1 -->
### Overlays: Domain Model

**Contract**: `cpt-cf-bss-rating-entity-model-ovl` (`p1`), defined in [§3.1 Namespace domain models](../DESIGN.md#register-entities).

**Superseded 2026-10-08 (T-D-73).** `OverlayDocument`, `OverlayLine` and `StackOrderKey` described pricing overlays and
have no counterpart on main. What remains is **`OverlayLineage`** — per applied step-5 term (none at
launch): source id and version, kind, magnitude, pre-amount, post-amount; clamp record if any.
Amounts inside a future stack are exact fractions in major units (T-D-51, T-D-80).

<!-- /contract -->

<a id="contract-04-3-2"></a>

<!-- contract:04-overlays-precedence:3.2 -->
### Overlays: Component Model

**Contract**: `cpt-cf-bss-rating-component-overlays-precedence-ovl` (`p1`), defined in [§3.2 Namespace components](../DESIGN.md#register-components).

- **`Step4PassThrough`** — returns the step-3 amount unchanged and records an empty step-4 lineage
  (no `PriceOverlay` on main).
- **`ContractOverlayApplier`** — step 5, dormant (§4.3).
- **`CompositionCapGuard`** — §4.4, dormant.

The former `ScopeFilter` and `OverlayStacker` are removed.

<!-- /contract -->

<a id="contract-04-3-3"></a>

<!-- contract:04-overlays-precedence:3.3 -->
### Overlays: API Contracts

**Contract**: `cpt-cf-bss-rating-interface-stack-overlays-ovl` (`p1`), defined in [§3.3 Namespace interfaces](../DESIGN.md#register-interfaces).

Internal: `apply_overlays(&ModelLineOutcome, &ContractTerms /* empty at launch */) ->
Result<OverlaidLine, EvaluationError>`. Errors: `contract_dimension_violation` (defensive),
`composition_cap_missing` (a step-5 stack of depth ≥ 2 with no configured cap, §4.4). The former
`unresolvable_scope`, `invalid_overlay_line`, `composition_cap_exceeded` and
`rule_violation:rating-val-03` are removed with pricing overlays.

<!-- /contract -->

<a id="contract-04-3-4"></a>

<!-- contract:04-overlays-precedence:3.4 -->
### Overlays: Internal Dependencies

Upstream: [contract 01](../DESIGN.md#contract-01) (pipeline, lineage), [contract 03](../DESIGN.md#contract-03) (step-3 line amount). Downstream: [contract 05](#contract-05), [contract 06](#contract-06),
[contract 07](#contract-07).

<!-- /contract -->

<a id="contract-04-3-5"></a>

<!-- contract:04-overlays-precedence:3.5 -->
### Overlays: External Dependencies

| Dependency | What arrives | Status |
|------------|--------------|--------|
| Pricing | nothing for steps 4–5: `PriceOverlay` and partner pricing were removed (pricing PRD spec item 2; D-384) | n/a on main |
| Contracts & Agreements | Contract/account overlay terms | MISSING — first-draft PRD only; UPSTREAM_REQS G-1, R-11 |

<!-- /contract -->

<a id="contract-04-3-7"></a>

<!-- contract:04-overlays-precedence:3.7 -->
### Overlays: Database Schemas and Tables

**Contract**: `cpt-cf-bss-rating-db-none-ovl` (`p1`), defined in [§3.7 Namespace data statements](../DESIGN.md#register-db).

None. The applied set (empty at launch) is persisted in the window result lineage.

<!-- /contract -->

<a id="contract-04-3-8"></a>

<!-- contract:04-overlays-precedence:3.8 -->
### Overlays: Deployment Topology

**Contract**: `cpt-cf-bss-rating-topology-ovl` (`p3`), defined in [§3.8 Namespace topology](../DESIGN.md#register-topology).

Part of `rating-core`; nothing slice-specific.

<!-- /contract -->

<a id="contract-04-4-1"></a>

<!-- contract:04-overlays-precedence:4.1 -->
### Overlays: Scope → Tenant-Axis Mapping (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-scope-mapping-ovl`

**Superseded 2026-10-08 (T-D-73).** The scope → tenant-axis table matched pricing overlay scope classes (`Global`,
`CustomerGroup`, `Partner`, `OrgTier`, `Brand`, `Region`) that no longer exist on main. A
customer-, partner- or region-specific price is a different book, entry, dimension value or price
in the plan revision, and Pricing's `resolve` binds it (dimension `region` is seeded in the tenant's
dimension registry, pricing D-385). Rating reads it from the binding and applies no scope rule.

<!-- /contract -->

<a id="contract-04-4-2"></a>

<!-- contract:04-overlays-precedence:4.2 -->
### Overlays: Stacking and the Total Order (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-stacking-order-ovl`

**Superseded 2026-10-08 (T-D-73).** Pricing overlay stacking (T-D-02), its total order, line selection, cohort filter
(T-D-30), adjustment kinds and currency coverage have no input on main. Step 4 is a pass-through with
an empty lineage. A temporary price pair (pricing D-391) is Pricing's dated discount-like mechanism;
it reaches Rating as an ordinary binding bounded by its `ends_on`, never as an overlay.

<!-- /contract -->

<a id="contract-04-4-3"></a>

<!-- contract:04-overlays-precedence:4.3 -->
### Overlays: Contract Overlay Precedence (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-contract-overlay-ovl`

- Step 5 applies contract/account overrides after step 3 (step 4 is a pass-through on main).
- Overrides must not introduce dimensions absent from the plan revision's entries
  (`contract_dimension_violation` is the defensive runtime error).
- **Launch**: no Contracts source exists (first-draft PRD only, R-11); step 5 receives no terms and
  passes the amount through unchanged. The rules above activate when a Contracts source is added to
  the `EvaluationInput`; negotiated reserved rates would arrive the same way ([05 §4.3](#contract-05-4-3)).

<!-- /contract -->

<a id="contract-04-4-4"></a>

<!-- contract:04-overlays-precedence:4.4 -->
### Overlays: Bounded Composition — Anti-Drift Cap (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-anti-drift-cap-ovl`

- The cumulative relative change of step 5 over the step-3 amount is bounded by a configured cap
  once a step-5 source exists.
- `clamp` mode: clamp to the bound, record pre-clamp amount; `hard` mode: fail closed.
- A step-5 stack of depth ≥ 2 with no configured cap and no Finance default fails the child closed
  with `composition_cap_missing`; no line is emitted and Rating never assumes a default cap. A stack
  of depth ≤ 1 needs no cap. (This replaces the publish-time `rating-val-03` rule: there is no
  overlay publish on main and Rating has no validator role, R-12 closed.)
- `[OPEN QUESTION]` Default cap value and mode (PRD §15, Program/Finance). Nothing on main carries
  a cap; until a source does, every depth ≥ 2 stack fails closed as above (DESIGN §4.13 V27).

<!-- /contract -->

<a id="contract-04-5"></a>

<!-- contract:04-overlays-precedence:5 -->
### Overlays: Traceability

**Traces to**: `cpt-cf-bss-rating-fr-priceoverlay-scope-mapping`, `cpt-cf-bss-rating-fr-overlay-stacking`, `cpt-cf-bss-rating-fr-customer-contract-overlay`,
`cpt-cf-bss-rating-fr-bounded-composition-cap`

- **PRD**: §6.3 `fr-priceoverlay-scope-mapping`; §6.4; §17.1 steps 4–5.
- **Dependencies on main**: pricing PRD spec item 2, D-384, D-385, D-391; legacy overlay tables refused (`m0000_pricing_refuse_a_legacy_or_stale_schema.rs`).
- **Seams / contracts**: [`UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) G-1.
- **Decisions**: T-D-02 (obsolete), T-D-30 (obsolete), T-D-37, T-D-73; R-11, R-12 (closed) — [`DECISIONS.md`](../DECISIONS.md).
- **ADR**: [`../ADR/0001-cpt-cf-bss-rating-adr-scope-key-adoption.md`](../ADR/0001-cpt-cf-bss-rating-adr-scope-key-adoption.md) (superseded by T-D-73).
- **Related contracts**: [contract 03](../DESIGN.md#contract-03), [contract 05](#contract-05), [contract 10](../DESIGN.md#contract-10).

<!-- /contract -->

<a id="contract-05"></a>

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-commitments-reservations`

<a id="contract-05-1-1"></a>

<!-- contract:05-commitments-reservations:1.1 -->
### Commitments: Architectural Vision

This contract is the **step-6 evaluator** in `rating-core`. Commitments and reservations are
compositions over the steps 2–5 output, not model kinds (T-D-05): they split a line's quantity into
reserved, in-commit and overage portions priced at already-resolved rates, and surface a structured
`TrueUpObligation` for Billing.

Two inputs are needed and **neither has a source on main** (2026-10-08):

- **Reservation match and reserved rates** — which reservation covers which usage or allocated
  quantity, and at which rate. Pricing on main has no reserved rate, reservation flavor or included
  allowance on any entry or price (`PriceModel` carries none; D-467 removed included quantities);
  "reserved capacity, prepaid credit and carry-over" are "later, by separate decision" (pricing
  `DESIGN.md:58, 97-102`, `PRD.md:154-155`). "Reservation" on main means the Products SKU reference
  reservation that protects a SKU's lifecycle (ADR-0004), not capacity.
- **Commitment pools** — ordered `commitmentPools[]` with balances, owned by Contracts, which has
  only a first-draft PRD (UPSTREAM_REQS G-2, R-11).

Launch behaviour therefore is: no binding can carry a reservation, so every usage line prices
on-demand; a context that references a reservation or a pool fails the child closed
(`reservation_match_unavailable`, `pool_input_torn`). The rules in §4 are the specified target
behaviour and activate when a source is added to the `EvaluationInput`.

<!-- /contract -->

<a id="contract-05-1-2"></a>

<!-- contract:05-commitments-reservations:1.2 -->
### Commitments: Architecture Drivers

<a id="contract-05-functional-drivers"></a>

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-commitment-drawdown` | Waterfall over ordered pools (§4.1) — activates when the Contracts gear supplies pools (R-11). |
| `cpt-cf-bss-rating-fr-committed-usage` | In-commit / overage split; `TrueUpObligation` for `committed_rate` pools (§4.5) — activates with R-11. |
| `cpt-cf-bss-rating-fr-reservation-consumption-flavor` | Matched quantity at the reserved rate, remainder re-banded (§4.2) — requires a match source. |
| `cpt-cf-bss-rating-fr-capacity-charge` | `capacityCharge = reservedRate × reservedQuantity × coveredGranules` as a period-driven line (§4.2) — requires a match and a rate source; none exists on main. |
| `cpt-cf-bss-rating-fr-hybrid-pricing` | Commitment attaches to the usage line unless the plan marks it plan-level. |
| `cpt-cf-bss-rating-fr-evaluation-order` | Fixed step-6 slot; intra-step order reservation → waterfall → overage. |

<a id="contract-05-nfr-allocation"></a>

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | Step-6 evaluator | Pure arithmetic over input values | Load test |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Input freezing | Pool balances enter the input as values; no live balance read or lock | Design |
| `cpt-cf-bss-rating-nfr-resilience` | Fail-closed guards | Missing match, unknown flavor, torn pool input ⇒ typed error | Fixture |

<a id="contract-05-key-adrs"></a>

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Historical; **superseded** by T-D-73 — the rates step 6 composes over come from the binding; this contract adds no selection. |

<!-- /contract -->

<a id="contract-05-1-3"></a>

<!-- contract:05-commitments-reservations:1.3 -->
### Commitments: Architecture Layers

**Contract**: `cpt-cf-bss-rating-tech-stack-cmt` (`p3`), defined in [§1.3 Namespace technology layers](../DESIGN.md#register-tech).

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | `ReservationMatcher`, `PoolWaterfall`, `TrueUpAssembler` | `rating-core` module |
| Infrastructure | None | — |

<!-- /contract -->

<a id="contract-05-2-1"></a>

<!-- contract:05-commitments-reservations:2.1 -->
### Commitments: Design Principles

<a id="contract-05-composition-not-a-model"></a>

#### Composition, not a model

**Contract**: `cpt-cf-bss-rating-principle-composition-cmt` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

For on-demand usage, Step 6 partitions a line's quantity and prices the partitions at rates
the binding and step 5 supply; it adds no formula beyond the reservation tier-counter exclusion (§4.2). The
exceptions are the capacity charge (§4.2) and the true-up obligation (§4.5), which Step 6 defines.

<a id="contract-05-frozen-balances-thin-evaluation"></a>

#### Frozen balances, thin evaluation

**Contract**: `cpt-cf-bss-rating-principle-frozen-balances-cmt` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

Pool balances, draw order, rollover and flavor are values in the `EvaluationInput`, recorded in the
window result; the core never reads or writes a live balance.

<a id="contract-05-surface-never-post"></a>

#### Surface, never post

**Contract**: `cpt-cf-bss-rating-principle-surface-not-post-cmt` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

Step 6 produces effects (lineage) and obligations (`TrueUpObligation`), never a balance mutation or
posting.

<!-- /contract -->

<a id="contract-05-2-2"></a>

<!-- contract:05-commitments-reservations:2.2 -->
### Commitments: Constraints

<a id="contract-05-fixed-slot-fixed-intra-step-order"></a>

#### Fixed slot, fixed intra-step order

**Contract**: `cpt-cf-bss-rating-constraint-fixed-slot-cmt` (`p1`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

Always step 6; reservation precedes pools; overage is the residual. No configuration changes it.

<a id="contract-05-reversal-math-is-slice-08s"></a>

#### Reversal math is [contract 08](../DESIGN.md#contract-08)'s

**Contract**: `cpt-cf-bss-rating-constraint-reversal-boundary-cmt` (`p1`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

A correction re-evaluates the child and produces a new revision ([contract 08](../DESIGN.md#contract-08)); this contract records
per-pool draws on every revision so a Contracts consumer can compute the balance effect.

<a id="contract-05-launch-posture-single-pool-first"></a>

#### Launch posture: single pool first

**Contract**: `cpt-cf-bss-rating-constraint-launch-posture-cmt` (`p2`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

When pools arrive, the first release evaluates the single-pool case; multi-pool waterfall and
rollover use the same input shape (PRD §17.4).

<!-- /contract -->

<a id="contract-05-3-1"></a>

<!-- contract:05-commitments-reservations:3.1 -->
### Commitments: Domain Model

**Contract**: `cpt-cf-bss-rating-entity-model-cmt` (`p1`), defined in [§3.1 Namespace domain models](../DESIGN.md#register-entities).

- **`ReservationMatch`** — `match_id`, `flavor` (`Consumption | Capacity`), `reserved_quantity`,
  coverage sub-intervals `[(from, to, reserved_quantity)]`, rate source (a contract term; Pricing
  on main publishes no reserved rate). No source today.
- **`CommitmentPoolInput`** — per pool: `pool_id`, contract ref, unit (quantity | spend),
  `currency` (required for spend pools; checked against the line's currency —
  `pool_currency_mismatch`),
  `pool_type ∈ {prepaid_drawdown, committed_rate}`, remaining balance, `balance_version`, draw order,
  rollover policy, optional `overage_rate`. No source today.
- **`CommitmentEffect`** — per-pool draws in order, in-commit vs overage split, applied rates.
- **`ReservationEffect`** — matched quantity at the reserved rate, `capacityCharge`, exclusions
  applied.
- **`TrueUpObligation`** — `(amount: ExactAmount, currency, period_start, contract_ref)`; executed by
  Billing.

<!-- /contract -->

<a id="contract-05-3-2"></a>

<!-- contract:05-commitments-reservations:3.2 -->
### Commitments: Component Model

**Contract**: `cpt-cf-bss-rating-component-commitment-evaluator-cmt` (`p1`), defined in [§3.2 Namespace components](../DESIGN.md#register-components).

- **`Step6Evaluator`** — input guards and intra-step order.
- **`ReservationMatcher`** — consumption split / capacity charge (§4.2).
- **`PoolWaterfall`** — ordered drawdown (§4.1).
- **`TrueUpAssembler`** — period-end obligation on the `period_line` child (§4.5).

<!-- /contract -->

<a id="contract-05-3-3"></a>

<!-- contract:05-commitments-reservations:3.3 -->
### Commitments: API Contracts

**Contract**: `cpt-cf-bss-rating-interface-step6-evaluator-cmt` (`p1`), defined in [§3.3 Namespace interfaces](../DESIGN.md#register-interfaces).

Internal: `apply_step6(&OverlaidLine, Option<&ReservationMatch>, &[CommitmentPoolInput],
&Step6Context) -> Result<Step6Line, EvaluationError>`. `Step6Context` carries what neither the line
nor the match holds: `remainder_offset_q` — the window-cumulative post-reservation remainder before
this slice (T-D-23), taken from the child's earlier slices in the same window (`ChildQuantities`,
[contract 13](../DESIGN.md#contract-13) materializes it beside `bandOffsetQ`); and, for a capacity charge, the `period_line`
child's billing period and the granule length defined with the reservation source (none exists on
main), from which `coveredGranules` is the reservation's coverage sub-intervals clipped to that
period, counted in granules (§4.2). Errors: `reservation_match_unavailable` (the context references
a reservation and the input has no match), `reservation_rate_unresolved`, `unknown_pool_type`,
`pool_currency_mismatch`, `pool_input_torn`.

<!-- /contract -->

<a id="contract-05-3-4"></a>

<!-- contract:05-commitments-reservations:3.4 -->
### Commitments: Internal Dependencies

Upstream: [contract 03](../DESIGN.md#contract-03) (remainder band math), [contract 04](#contract-04) (step-4 pass-through; negotiated reserved rate via
step 5). Downstream: [contract 06](#contract-06) (post-commitment amount), [contract 08](../DESIGN.md#contract-08) (differences between versions),
[contract 09](../DESIGN.md#contract-09) (`period_line` children, plan-change reset).

<!-- /contract -->

<a id="contract-05-3-5"></a>

<!-- contract:05-commitments-reservations:3.5 -->
### Commitments: External Dependencies

| Dependency | What arrives | Status |
|------------|--------------|--------|
| Pricing | nothing for step 6: no reserved rate, reservation flavor or included allowance exists on main (pricing D-467; reserved capacity and prepaid credit "later") | n/a on main |
| Reservation entitlement source (OSS / Contracts) | `ReservationMatch` and reserved rates | MISSING — no gear (R-11) |
| Contracts & Agreements | pools, true-up clause, negotiated reserved rates | MISSING — first-draft PRD only; UPSTREAM_REQS G-1, G-2 (R-11) |
| Billing | executes `TrueUpObligation` | MISSING — no gear (R-05) |

<!-- /contract -->

<a id="contract-05-3-7"></a>

<!-- contract:05-commitments-reservations:3.7 -->
### Commitments: Database Schemas and Tables

**Contract**: `cpt-cf-bss-rating-db-none-cmt` (`p1`), defined in [§3.7 Namespace data statements](../DESIGN.md#register-db).

None. Effects and obligations are stored in `bss_rating__window_result.lines` / `.obligations` (DESIGN §3.7).

<!-- /contract -->

<a id="contract-05-3-8"></a>

<!-- contract:05-commitments-reservations:3.8 -->
### Commitments: Deployment Topology

**Contract**: `cpt-cf-bss-rating-topology-cmt` (`p3`), defined in [§3.8 Namespace topology](../DESIGN.md#register-topology).

Part of `rating-core`.

<!-- /contract -->

<a id="contract-05-4-1"></a>

<!-- contract:05-commitments-reservations:4.1 -->
### Commitments: Commitment-Pool Waterfall (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-waterfall-cmt`

*Activates when the Contracts gear supplies pools — R-11.*

- Pools are drawn in declared order; each absorbs quantity/spend up to its remaining balance; the
  residual is overage.
- Residual pricing (T-D-19): `overage_rate` present ⇒ flat rate, no band math; absent ⇒ banded
  on-demand rates over the post-reservation remainder quantity (pool draws do not reduce it).
- Billability (T-D-14): `prepaid_drawdown` ⇒ in-commit line due 0 with notional value in lineage;
  `committed_rate` ⇒ in-commit bills in arrears at the in-commit rate.
- Spend pools draw in price currency; a pool denominated in another currency fails closed
  (`pool_currency_mismatch`).
- Tier counter: in-commit quantity is **not** excluded from `Q` (only reservations are).
- Pool balances are values of the input and are recorded on the window result. Cross-unit balance
  sequencing and `CommitmentBalanceEffect` publication (T-D-10, T-D-27) are dormant until a Contracts
  consumer exists.

<!-- /contract -->

<a id="contract-05-4-2"></a>

<!-- contract:05-commitments-reservations:4.2 -->
### Commitments: Reservation Flavors and Pool Precedence (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-reservation-flavors-cmt`

*Requires a reservation-match source — R-11.*

- **Consumption flavor**: matched quantity prices at the reserved rate; the remainder prices at
  on-demand rates from steps 2–5. The matched quantity is excluded from pool drawdown and from the
  on-demand tier counter: the remainder is banded on a window-cumulative remainder axis starting at
  the window origin (`remainderOffset` across slices — T-D-23), never reset per slice. Steps 3–5
  re-run over the remainder (T-D-13).
- **Capacity flavor**: a period-driven line `capacityCharge = reservedRate × reservedQuantity ×
  coveredGranules`, with `reservedRate` an exact decimal in major units per billable-unit granule
  kept exact in major units like any price (T-D-74), emitted regardless of usage, never drawing pools
  (T-D-25); `coveredGranules` is the reservation's covered duration within the
  billing period, summed per sub-interval when coverage or quantity changes.

Worked example (capacity, `reservedRate` = €0.0002 per GB·h, hour granule, 30-day period = 720 h):

```text
100 GB reserved for the whole period       100 × 720 × 0.0002       = €14.40
allocated on day 20 (11 days = 264 h)      100 × 264 × 0.0002       = € 5.28
100 GB days 1–24, 200 GB days 25–30        100×576×0.0002 + 200×144×0.0002 = 11.52 + 5.76 = €17.28
```

- No reservation in the context ⇒ pure usage pricing (the only case on main).

<!-- /contract -->

<a id="contract-05-4-3"></a>

<!-- contract:05-commitments-reservations:4.3 -->
### Commitments: Reserved-Rate Two-Source Rule (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-reserved-rate-sourcing-cmt`

- Self-service reserved rates: **none on main** — Pricing publishes no reserved rate (the former
  `reservedRateNanoMinor` on a usage row has no successor; reserved capacity is "later, by separate
  decision" in pricing).
- Negotiated reserved rates: the step-5 contract overlay (no source at launch, R-11).
- Step 6 uses the post-step-5 value; a match whose rate resolves from no source fails closed
  (`reservation_rate_unresolved`).

<!-- /contract -->

<a id="contract-05-4-4"></a>

<!-- contract:05-commitments-reservations:4.4 -->
### Commitments: Commitment Pool vs Prepaid Credit Grant (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-pool-vs-grant-cmt`

| | Commitment pool | Prepaid credit grant |
|---|---|---|
| Construct | `commitmentPools[]`, step-6 waterfall | Plan-attached wallet (`grantAmount`, `creditUnit`, `expiryPolicy`) |
| Definition owner | Contracts | not defined on main (pricing defers prepaid credit, "later, by separate decision") |
| Balance owner | Contracts | Billing (ledger `CreditGrant` / `CreditApply` exist in `bss-ledger`) |
| Drawdown | Rating, step 6 | Billing, outside the per-line order |

Step 6 never draws a wallet grant. Rating documents never use the bare word "prepaid".

<!-- /contract -->

<a id="contract-05-4-5"></a>

<!-- contract:05-commitments-reservations:4.5 -->
### Commitments: Obligations, Reversals, and Period Boundaries (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-obligations-boundary-cmt`

- `TrueUpObligation` shape: `(amount: ExactAmount, currency = price currency, period_start, contract_ref)`.
- `committed_rate` quantity basis: `max(0, committedQuantity − consumedInCommitQuantity) ×
  in-commit rate`; spend basis: `max(0, committedSpend − inCommitBilledAmount)`.
  `prepaid_drawdown` pools emit no true-up.
- The waterfall cannot produce a negative component; the non-negative guard runs at step 9.
- Plan change: pool carry defaults to reset; pricing publishes no pool flag (T-D-29 superseded; no plan-change counter flag exists on main).

<!-- /contract -->

<a id="contract-05-5"></a>

<!-- contract:05-commitments-reservations:5 -->
### Commitments: Traceability

**Traces to**: `cpt-cf-bss-rating-fr-commitment-drawdown`, `cpt-cf-bss-rating-fr-reservation-consumption-flavor`, `cpt-cf-bss-rating-fr-capacity-charge`,
`cpt-cf-bss-rating-fr-committed-usage`

- **PRD**: §6.2, §6.6, §17.1 step 6, §17.3, §17.4.
- **Dependencies on main**: pricing D-467; pricing `DESIGN.md:58, 97-102`, `PRD.md:154-155` (reserved capacity, prepaid credit deferred).
- **Seams / contracts**: [`UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) G-1, G-2.
- **Decisions**: T-D-05, T-D-08 (superseded), T-D-09, T-D-10, T-D-13, T-D-14, T-D-19, T-D-23, T-D-25, T-D-27, T-D-29 (superseded), T-D-37, T-D-74; R-11 — [`DECISIONS.md`](../DECISIONS.md).
- **Related slices**: [`03-metering-models.md`](../DESIGN.md#contract-03), [`04-overlays-precedence.md`](#contract-04), [`08-retroactivity-corrections.md`](../DESIGN.md#contract-08), [`09-period-plan-change.md`](../DESIGN.md#contract-09).

<!-- /contract -->

<a id="contract-06"></a>

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-coupons`

<a id="contract-06-1-1"></a>

<!-- contract:06-coupons:1.1 -->
### Coupons: Architectural Vision

This slice is the **step-7 evaluator** in `rating-core`: it applies frozen coupon snapshots to the
post-commitment line amount, split around FX (price-currency coupons before step 8,
billing-currency coupons after). Rating owns application semantics only; coupon lifecycle,
campaigns and redemption are the Promotions domain's (PRD §5.2).

No Promotions gear or PRD exists (UPSTREAM_REQS G-3, R-11); on main promotions are owned by
Pricing and deferred (pricing D-409: `PromotionPublished` deferred, `resolve` returns no
promotion; promotion-aware renewal "comes back with promotions", D-420), so no coupon snapshot can
enter the `EvaluationInput`. **Launch behaviour**: the coupon set is empty and step 7 passes the
amount through unchanged. No binding carries a discount reference on main (`AcceptedBinding` has no
such field); a context that references a coupon fails the child closed with
`coupon_source_unavailable` — a referenced discount is never silently dropped. The rules in §4 are
the specified target behaviour and activate when a coupon-snapshot source is added.

<!-- /contract -->

<a id="contract-06-1-2"></a>

<!-- contract:06-coupons:1.2 -->
### Coupons: Architecture Drivers

<a id="contract-06-functional-drivers"></a>

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-coupon-application-order` | Step 7 after steps 4–6; billing-currency pass after step 8 (§4.1). |
| `cpt-cf-bss-rating-fr-coupon-stacking` | `exclusive_best` default, `ordered_stack` for campaign-linked sets; fail-closed set (§4.2, §4.4). |
| `cpt-cf-bss-rating-fr-hybrid-pricing` | `applyScope` attachment; `line_total` fails closed at launch (T-D-22, §4.3). |
| `cpt-cf-bss-rating-fr-snapshot-carry` | Applied coupon ids + stacking policy recorded in the snapshot body (§4.5). |
| `cpt-cf-bss-rating-fr-evaluation-order` | Fixed step-7 slot. |

<a id="contract-06-nfr-allocation"></a>

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | Step-7 evaluator | In-memory over input snapshots | Load test |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Stateless | No redemption state read or written | Design |
| `cpt-cf-bss-rating-nfr-resilience` | Fail-closed guards | Missing policy fields or unavailable source ⇒ typed error | Fixture |

<a id="contract-06-key-adrs"></a>

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Context only — the discounted rows resolve on the canonical key upstream. |

<!-- /contract -->

<a id="contract-06-1-3"></a>

<!-- contract:06-coupons:1.3 -->
### Coupons: Architecture Layers

**Contract**: `cpt-cf-bss-rating-tech-stack-cpn` (`p3`), defined in [§1.3 Namespace technology layers](../DESIGN.md#register-tech).

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | `EligibilityFilter`, `StackingResolver`, `ScopeAttacher`, two application passes | `rating-core` module |
| Infrastructure | None | — |

<!-- /contract -->

<a id="contract-06-2-1"></a>

<!-- contract:06-coupons:2.1 -->
### Coupons: Design Principles

<a id="contract-06-apply-never-own"></a>

#### Apply, never own

**Contract**: `cpt-cf-bss-rating-principle-apply-never-own-cpn` (`p2`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

Rating applies coupon snapshots and records the result; it never creates, counts or expires a
redemption.

<a id="contract-06-policy-from-the-snapshot-only"></a>

#### Policy from the snapshot only

**Contract**: `cpt-cf-bss-rating-principle-policy-from-snapshot-cpn` (`p2`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

Scope, stacking, sequence, validity and settlement currency come from snapshot fields; an absent
field fails closed.

<a id="contract-06-lineage-is-part-of-the-outcome"></a>

#### Lineage is part of the outcome

**Contract**: `cpt-cf-bss-rating-principle-lineage-first-cpn` (`p2`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

Pre/post-coupon amounts and applied ids are recorded per line and pass so Billing/Tax can choose
gross-vs-net treatment.

<!-- /contract -->

<a id="contract-06-2-2"></a>

<!-- contract:06-coupons:2.2 -->
### Coupons: Constraints

<a id="contract-06-fixed-slot-compiled-fx-split"></a>

#### Fixed slot, compiled FX split

**Contract**: `cpt-cf-bss-rating-constraint-fixed-slot-cpn` (`p2`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

Price-currency coupons at step 7, billing-currency coupons after step 8; no configuration changes
the placement.

<a id="contract-06-no-redemption-mutation-snapshot-only-replay"></a>

#### No redemption mutation, snapshot-only replay

**Contract**: `cpt-cf-bss-rating-constraint-no-redemption-mutation-cpn` (`p2`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

The applied snapshots are stored with the window result; a replay uses them, never live Promotions
state.

<a id="contract-06-promotions-contract-maturity"></a>

#### Promotions contract maturity

**Contract**: `cpt-cf-bss-rating-constraint-promotions-maturity-cpn` (`p2`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

`[DEPENDENCY GAP R-11]` No Promotions PRD or gear; Pricing owns promotions and has deferred them
(pricing D-409); PRD §17.2 is the Rating-side field list. Field
names and the snapshot delivery contract must be agreed before coupons can be rated.

<!-- /contract -->

<a id="contract-06-3-1"></a>

<!-- contract:06-coupons:3.1 -->
### Coupons: Domain Model

**Contract**: `cpt-cf-bss-rating-entity-model-cpn` (`p2`), defined in [§3.1 Namespace domain models](../DESIGN.md#register-entities).

- **`CouponSnapshot`** — `couponId`, `adjustmentType` (`percent | fixed_amount`), `value`,
  `valueCurrency` (required for `fixed_amount`), `settlementCurrency` (`price | billing`),
  `applyPerTierBand`, `applyScope` (`usage | recurring | line_total`), `stackSequence` (required and
  unique under `ordered_stack`), validity, applicability filters, redemption eligibility.
- **`StackingPolicy`** — `exclusive_best | ordered_stack`.
- **`CouponApplication`** — coupon id, pass, basis, discount, result.
- **`DiscountLineage`** — per line and pass: pre/post amounts, applied ids, policy.

<!-- /contract -->

<a id="contract-06-3-2"></a>

<!-- contract:06-coupons:3.2 -->
### Coupons: Component Model

**Contract**: `cpt-cf-bss-rating-component-coupon-evaluator-cpn` (`p2`), defined in [§3.2 Namespace components](../DESIGN.md#register-components).

- **`Step7Evaluator`** — coupon-reference guard, both passes, fail-closed checks.
- **`EligibilityFilter`** — validity at the slice start, applicability, redemption eligibility.
- **`StackingResolver`** — §4.2.
- **`ScopeAttacher`** — §4.3.

<!-- /contract -->

<a id="contract-06-3-3"></a>

<!-- contract:06-coupons:3.3 -->
### Coupons: API Contracts

**Contract**: `cpt-cf-bss-rating-interface-step7-evaluator-cpn` (`p2`), defined in [§3.3 Namespace interfaces](../DESIGN.md#register-interfaces).

Internal: `apply_coupons(&Step6Line, &[CouponSnapshot], Pass) -> Result<CouponLine,
EvaluationError>`, called once with `Pass::Price` at step 7 and once with `Pass::Billing` after
step 8. Errors: `coupon_source_unavailable`, `coupon_policy_missing`, `coupon_sequence_invalid`,
`coupon_currency_mismatch`, `coupon_scope_unsupported` (`line_total`), `coupon_incompatible_pair`,
`coupon_comparison_undefined`.

<!-- /contract -->

<a id="contract-06-3-4"></a>

<!-- contract:06-coupons:3.4 -->
### Coupons: Internal Dependencies

Upstream: [contract 05](#contract-05) (post-commitment amount), [contract 03](../DESIGN.md#contract-03) (band amounts for `applyPerTierBand`),
[contract 04](#contract-04) (partner discounts coexist). Downstream: [contract 07](#contract-07) (conversion between the passes).

<!-- /contract -->

<a id="contract-06-3-5"></a>

<!-- contract:06-coupons:3.5 -->
### Coupons: External Dependencies

| Dependency | What arrives | Status |
|------------|--------------|--------|
| Promotions | Coupon snapshots | MISSING — UPSTREAM_REQS G-3 (R-11) |
| Pricing | promotions (owner); none published — deferred (pricing D-409); bindings carry no discount reference | n/a on main |
| Billing / Tax | Consume discount lineage | MISSING gear (R-05) |

<!-- /contract -->

<a id="contract-06-3-7"></a>

<!-- contract:06-coupons:3.7 -->
### Coupons: Database Schemas and Tables

**Contract**: `cpt-cf-bss-rating-db-none-cpn` (`p2`), defined in [§3.7 Namespace data statements](../DESIGN.md#register-db).

None. Applied snapshots and lineage are stored in the window result.

<!-- /contract -->

<a id="contract-06-3-8"></a>

<!-- contract:06-coupons:3.8 -->
### Coupons: Deployment Topology

**Contract**: `cpt-cf-bss-rating-topology-cpn` (`p3`), defined in [§3.8 Namespace topology](../DESIGN.md#register-topology).

Part of `rating-core`.

<!-- /contract -->

<a id="contract-06-4-1"></a>

<!-- contract:06-coupons:4.1 -->
### Coupons: Placement and the FX Split (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-design-placement-fx-split-cpn`

- Order (former numbering, see the §6 note): binding → contract override (5) → commitment (6) →
  coupon (7) → FX (8) → emit (9); price overlays (4) no longer exist.
- `settlementCurrency = price`: step 7, price currency, before FX. `settlementCurrency = billing`:
  after step 8 on the billing-currency amount, same FX rate record.

<!-- /contract -->

<a id="contract-06-4-2"></a>

<!-- contract:06-coupons:4.2 -->
### Coupons: Stacking Policies (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-design-stacking-cpn`

- `exclusive_best` (default): one coupon per line — the lowest resulting charge. When a `line_total`
  candidate is present exclusivity widens to one coupon per plan per period (T-D-20); inert while
  `line_total` fails closed.
- `ordered_stack`: only for campaign-linked sets; folds ascending `stackSequence`, each step on the
  prior output. Missing or duplicate `stackSequence` fails closed.
- Incompatible pairs are rejected at redemption bind (Promotions); if both arrive applicable,
  evaluation fails closed.
- `[OPEN QUESTION]` Equal-benefit tie-break (default proposal: ascending `couponId`).
- `[OPEN QUESTION]` Mixed-settlement candidate sets under either policy — fail closed until pinned
  with Promotions/Finance.

<!-- /contract -->

<a id="contract-06-4-3"></a>

<!-- contract:06-coupons:4.3 -->
### Coupons: applyScope Attachment and Hybrid Split-Back (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-design-applyscope-cpn`

- `usage` / `recurring`: bind to that line.
- `line_total`: fails closed at launch (`coupon_scope_unsupported`, T-D-22) — usage and recurring
  lines are different units rated on different triggers, so no evaluation holds a plan's lines
  together. Target semantics once a plan-period assembly point exists: apply once to the combined
  total, split back pro-rata to pre-coupon amounts at full precision; if both are 0 assign the
  (zero) discount to the recurring line. `[OPEN QUESTION]` Assembly point, trigger and cascade —
  joint with Promotions.
- `applyPerTierBand = false`: discount the line total after tier math; `true`: per marginal band;
  on unbanded lines `true` behaves as total and is recorded as such.

<!-- /contract -->

<a id="contract-06-4-4"></a>

<!-- contract:06-coupons:4.4 -->
### Coupons: Frozen Coupon Snapshot and Fail-Closed Rules (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-design-fail-closed-snapshot-cpn`

Fail closed (never defaulted): coupon source unavailable while the context references a coupon; missing
`applyScope`; missing/duplicate `stackSequence` under `ordered_stack`; missing `valueCurrency` on
`fixed_amount`; a `fixed_amount` whose `valueCurrency` does not match its pass's currency; unknown
`adjustmentType`/`settlementCurrency`; `applyScope = line_total`; the §4.2 opens. Eligibility is
evaluated against snapshot fields only.

<!-- /contract -->

<a id="contract-06-4-5"></a>

<!-- contract:06-coupons:4.5 -->
### Coupons: Snapshot Segment and Discount Lineage (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-design-segment-lineage-cpn`

- Applied coupon ids + stacking policy would become a segment of the `bss_rating__snapshot` body
  once a Promotions source exists (DESIGN §4.1); no such segment exists today.
- Lineage per line and pass: pre/post amounts, applied ids. Whether a contractual floor claws back a
  coupon discount is a PRD §15 open; the lineage keeps either answer computable.

<!-- /contract -->

<a id="contract-06-5"></a>

<!-- contract:06-coupons:5 -->
### Coupons: Traceability

**Traces to**: `cpt-cf-bss-rating-fr-coupon-application-order`, `cpt-cf-bss-rating-fr-coupon-stacking`

- **PRD**: §6.8, §6.2 (hybrid attachment), §17.1 steps 7–8, §17.2.
- **Dependencies on main**: pricing D-409, D-420 (promotions deferred; resolve returns none).
- **Seams / contracts**: [`UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) G-3.
- **Decisions**: T-D-20, T-D-22; R-11 — [`DECISIONS.md`](../DECISIONS.md).
- **Related slices**: [`05-commitments-reservations.md`](#contract-05), [`07-currency-fx.md`](#contract-07), [`09-period-plan-change.md`](../DESIGN.md#contract-09).

<!-- /contract -->

<a id="contract-07"></a>

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-currency-fx`

<a id="contract-07-1-1"></a>

<!-- contract:07-currency-fx:1.1 -->
### Currency and FX: Architectural Vision

This contract is the **step-8 evaluator** in `rating-core`. It keeps three currency roles apart —
**price currency** (the binding's `price.currency`, which is its price book's single currency,
pricing D-384), **billing currency** (the sale market's currency the subscription was sold in —
`market{currency, region}` of the accepted sale, carried on the fact), **presentment currency**
(display only, outside Rating) — and is the only place a conversion could occur.

No gear provides versioned, pinnable FX rates to Rating (UPSTREAM_REQS F-1…F-3): `gears/bss/rate-provider`
is a stateless adapter supplying live rates through source plugins, and rate persistence, staleness
and snapshotting are the ledger's (rate-provider PRD §4); the ledger mints its rate snapshots only
when it posts. **Launch behaviour is native currency only** (DESIGN §2.2, R-07): a plan revision
binds one book and so one currency; a market in another currency is another book, entry or plan
revision that Pricing binds, never an FX derivation; billing currency must equal price currency. A mismatch fails the child closed with `fx_not_supported`. The FX policies in §4.2
are the specified target behaviour and activate when a pinnable rate-snapshot contract exists
(UPSTREAM_REQS J-8).

<!-- /contract -->

<a id="contract-07-1-2"></a>

<!-- contract:07-currency-fx:1.2 -->
### Currency and FX: Architecture Drivers

<a id="contract-07-functional-drivers"></a>

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-multi-currency` | Roles separated (§4.1); native currency at launch; a market's price is its own book's price, never FX-derived. |
| `cpt-cf-bss-rating-fr-fx-policy` | Two policies specified (§4.2); inactive until R-07; no implicit or provider-default rate ever. |
| `cpt-cf-bss-rating-fr-evaluation-order` | Fixed step-8 slot between the two coupon passes. |
| `cpt-cf-bss-rating-fr-coupon-application-order` | Billing-currency coupons after step 8 (§4.4). |
| `cpt-cf-bss-rating-fr-snapshot-carry` | An FX segment of the snapshot once an FX source exists (none today, R-07; §4.3). |

<a id="contract-07-nfr-allocation"></a>

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | Native check | One equality check at launch | — |
| `cpt-cf-bss-rating-nfr-resilience` | Fail-closed guard | Mismatch or missing rate record ⇒ typed error | Fixture |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Pinned inputs | Rates (when added) are values of the input | Design |

<a id="contract-07-key-adrs"></a>

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Historical; **superseded** by T-D-73 — currency belongs to the price book (D-384); each market is priced by its own book, never derived by FX. |

<!-- /contract -->

<a id="contract-07-1-3"></a>

<!-- contract:07-currency-fx:1.3 -->
### Currency and FX: Architecture Layers

**Contract**: `cpt-cf-bss-rating-tech-stack-fx` (`p3`), defined in [§1.3 Namespace technology layers](../DESIGN.md#register-tech).

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | `CurrencyRoleResolver`, `FxPolicyApplier` (inactive), billing-currency coupon pass hook | `rating-core` module |
| Infrastructure | None | — |

<!-- /contract -->

<a id="contract-07-2-1"></a>

<!-- contract:07-currency-fx:2.1 -->
### Currency and FX: Design Principles

<a id="contract-07-three-roles-one-conversion-point"></a>

#### Three roles, one conversion point

**Contract**: `cpt-cf-bss-rating-principle-currency-roles-fx` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

Price, billing and presentment currency are distinct; the only authoritative conversion is step 8,
and it runs only when billing ≠ price.

<a id="contract-07-no-unrecorded-fx"></a>

#### No unrecorded FX

**Contract**: `cpt-cf-bss-rating-principle-no-implicit-fx` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

A converted amount always carries its rate record identity; without a record, evaluation fails
closed.

<a id="contract-07-convert-at-full-precision-never-round"></a>

#### Convert at full precision, never round

**Contract**: `cpt-cf-bss-rating-principle-full-precision-conversion-fx` (`p1`), defined in [§2.1 Namespace principles](../DESIGN.md#register-principles).

Conversion is exact (rational rate × exact amount); Billing rounds to the currency scale (T-D-51).

<!-- /contract -->

<a id="contract-07-2-2"></a>

<!-- contract:07-currency-fx:2.2 -->
### Currency and FX: Constraints

<a id="contract-07-fx-tables-and-policy-are-finances"></a>

#### FX tables and policy are Finance's

**Contract**: `cpt-cf-bss-rating-constraint-finance-sor-fx` (`p1`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

Rating never sources, derives, inverts or triangulates a rate. `[DEPENDENCY GAP R-07]` No FX owner
exposes pinnable rates today; J-8 names the required change (ledger or a Finance gear exposing
`lock_rate` / `read_snapshot`).

<a id="contract-07-the-currency-region-binding-is-consumed-never-re-derived"></a>

#### The (currency, region) binding is consumed, never re-derived

**Contract**: `cpt-cf-bss-rating-constraint-binding-consumed-fx` (`p1`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

Billing currency is read from the stored fact/subscription version (the accepted sale's market,
UPSTREAM_REQS S-1); Rating never infers it from a binding.

<a id="contract-07-presentment-is-outside-rating-core"></a>

#### Presentment is outside rating-core

**Contract**: `cpt-cf-bss-rating-constraint-presentment-outside-fx` (`p1`), defined in [§2.2 Namespace constraints](../DESIGN.md#register-constraints).

Display conversion is non-authoritative and not computed here.

<!-- /contract -->

<a id="contract-07-3-1"></a>

<!-- contract:07-currency-fx:3.1 -->
### Currency and FX: Domain Model

**Contract**: `cpt-cf-bss-rating-entity-model-fx` (`p1`), defined in [§3.1 Namespace domain models](../DESIGN.md#register-entities).

- **`CurrencyRoles`** — `price_currency` (the binding's `price.currency`), `billing_currency` (the sale market's currency on the fact).
- **`FxRateRecord`** (target) — `rate_ref` (pinnable id from the FX owner), pair `(price →
  billing)`, rate, policy kind (`per_window_rate_lock | invoice_period`). No source at launch.
- **`FxApplication`** (target) — pre/post amounts, `rate_ref`, provisional flag.

<!-- /contract -->

<a id="contract-07-3-2"></a>

<!-- contract:07-currency-fx:3.2 -->
### Currency and FX: Component Model

**Contract**: `cpt-cf-bss-rating-component-conversion-fx` (`p1`), defined in [§3.2 Namespace components](../DESIGN.md#register-components).

- **`CurrencyRoleResolver`** — binds roles; equal ⇒ skip conversion; different ⇒ `fx_not_supported`
  at launch, `FxPolicyApplier` once R-07 is resolved.
- **`FxPolicyApplier`** (target) — §4.2.
- **Billing-currency coupon pass** — invokes [contract 06](#contract-06) with `Pass::Billing`.

<!-- /contract -->

<a id="contract-07-3-3"></a>

<!-- contract:07-currency-fx:3.3 -->
### Currency and FX: API Contracts

**Contract**: `cpt-cf-bss-rating-interface-convert-fx` (`p1`), defined in [§3.3 Namespace interfaces](../DESIGN.md#register-interfaces).

Internal: `apply_fx(&CouponLine, &CurrencyRoles, Option<&FxRateRecord>) -> Result<FxLine,
EvaluationError>`. Errors: `fx_not_supported` (launch), `fx_rate_missing`, `fx_pair_missing`
(exact pair direction required; no inversion).

<!-- /contract -->

<a id="contract-07-3-4"></a>

<!-- contract:07-currency-fx:3.4 -->
### Currency and FX: Internal Dependencies

Upstream: [contract 02](../DESIGN.md#contract-02) (row currency), [contract 06](#contract-06) (price-currency pass). Downstream: [contract 06](#contract-06)
(billing-currency pass), [contract 01](../DESIGN.md#contract-01) step-9 guards, [contract 09](../DESIGN.md#contract-09) (floor/cap comparison currency).

<!-- /contract -->

<a id="contract-07-3-5"></a>

<!-- contract:07-currency-fx:3.5 -->
### Currency and FX: External Dependencies

| Dependency | What arrives | Status |
|------------|--------------|--------|
| Pricing | the binding's `price.currency` and its stored scale, `price.currency.scale()` (one currency per book; D-531) | CONFIRMED on main (`pricing-sdk/src/read.rs:80-109`, `terms.rs:119-158`; UPSTREAM_REQS P-15) |
| Subscriptions | the sale market `{currency, region}` on the fact | ASSUMED (UPSTREAM_REQS S-1) |
| FX owner (ledger / Finance) | Pinnable rate snapshots | MISSING — `rate-provider` serves live rates only; snapshots are ledger-internal (UPSTREAM_REQS F-1…F-3, R-07) |

<!-- /contract -->

<a id="contract-07-3-7"></a>

<!-- contract:07-currency-fx:3.7 -->
### Currency and FX: Database Schemas and Tables

**Contract**: `cpt-cf-bss-rating-db-none-fx` (`p1`), defined in [§3.7 Namespace data statements](../DESIGN.md#register-db).

None. When FX is enabled, the rate record used is stored with the window result so replay does not
read the FX owner.

<!-- /contract -->

<a id="contract-07-3-8"></a>

<!-- contract:07-currency-fx:3.8 -->
### Currency and FX: Deployment Topology

**Contract**: `cpt-cf-bss-rating-topology-fx` (`p3`), defined in [§3.8 Namespace topology](../DESIGN.md#register-topology).

Part of `rating-core`.

<!-- /contract -->

<a id="contract-07-4-1"></a>

<!-- contract:07-currency-fx:4.1 -->
### Currency and FX: Currency Role Separation (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-currency-roles-fx`

- **Price currency**: the binding's currency (one per price book, pricing D-384); a market without a
  book of its currency is uncovered (`price_uncovered`), never FX-derived.
- **Billing currency**: the sale market's currency carried on the fact.
- **Presentment currency**: outside Rating.
- Conversion iff billing ≠ price; at launch that case fails closed (`fx_not_supported`).
- One currency per line; lines of one invoice share the billing currency.

<!-- /contract -->

<a id="contract-07-4-2"></a>

<!-- contract:07-currency-fx:4.2 -->
### Currency and FX: FX Policy Semantics (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-fx-policy-fx`

*Activates with R-07.*

- **Per-window rate lock**: the locked rate is final when rated; `rate_ref` recorded.
- **Invoice-period FX**: provisional rate on provisional evaluations; the child finalizes only with
  the close-time rate record; a later rate correction is a new revision (Billing derives any
  difference).
- The rate record must carry the exact pair direction (price → billing); no inversion or
  triangulation.
- Missing record with billing ≠ price ⇒ fail closed.

<!-- /contract -->

<a id="contract-07-4-3"></a>

<!-- contract:07-currency-fx:4.3 -->
### Currency and FX: FX-Lock Snapshot Segment (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-fx-lock-segment-fx`

- When FX activates, the `bss_rating__snapshot` body gains an FX segment holding the `rate_ref`(s)
  used; native-currency lines (all launch lines) carry none (the empty segment was removed with
  T-D-73).
- A later re-evaluation with a different rate produces a new snapshot and revision; recorded snapshots
  never change.

<!-- /contract -->

<a id="contract-07-4-4"></a>

<!-- contract:07-currency-fx:4.4 -->
### Currency and FX: Ordering and Precision at the FX Boundary (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-fx-ordering-fx`

- Order: price-currency coupons (step 7) → conversion (step 8, skipped when native) →
  billing-currency coupons → step-9 guards. The billing-currency pass runs even when conversion is
  skipped, keeping coupon placement invariant.
- Conversion computes at full precision and never rounds; Billing rounds to the currency scale.
- The minimum-fee floor compares in the binding's currency (T-D-78); under FX it would use the same rate record as step 8.

<!-- /contract -->

<a id="contract-07-5"></a>

<!-- contract:07-currency-fx:5 -->
### Currency and FX: Traceability

**Traces to**: `cpt-cf-bss-rating-fr-multi-currency`, `cpt-cf-bss-rating-fr-fx-policy`

- **PRD**: §6.9, §17.1 steps 2 and 8, §12 AC 8.
- **Dependencies on main**: pricing D-384 (one currency per book); `gears/bss/rate-provider` (live rates; snapshots ledger-owned).
- **Seams / contracts**: [`UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) F-1…F-3, P-15, S-1, J-8.
- **Decisions**: R-07, T-D-51, T-D-74 — [`DECISIONS.md`](../DECISIONS.md).
- **Related slices**: [`06-coupons.md`](#contract-06), [`08-retroactivity-corrections.md`](../DESIGN.md#contract-08), [`09-period-plan-change.md`](../DESIGN.md#contract-09).

<!-- /contract -->


<a id="contract-11-4-4"></a>

<!-- contract:11-consumer-contracts:4.4 -->
### Integration contracts: Finance FX Input Contract (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-design-finance-fx-cc`

No FX contract exists that Rating can pin (UPSTREAM_REQS §2.9): `RateProviderV1` is in the ledger
SDK and the ledger is its only consumer. A price book has one immutable currency (pricing D-384), so
every binding is in one currency; only billing currency = binding currency is supported, otherwise
`fx_not_supported`. The required future contract is a pinnable rate snapshot
`{ rate_id, base, quote, rate, as_of, provider }` readable by id (J-8).

<!-- /contract -->

<a id="contract-11-4-5"></a>

<!-- contract:11-consumer-contracts:4.5 -->
### Integration contracts: Promotions Coupon Snapshot Contract (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-design-promotions-coupons-cc`

Promotions are owned by Pricing and deferred by its owner (pricing D-409); `resolve` returns no
promotion, and the only dated discount-like mechanism is a temporary price pair (`temporary_until`),
which Rating prices like any binding and slices at its `ends_on`. Any coupon or promotion reference a
fact carries (SUB-D-29 names "promotion versions" in the pins) fails closed with
`coupon_source_unavailable`. Coupon semantics are in [contract 06](#contract-06). External contract:
UNKNOWN / EXTERNAL CONTRACT REQUIRED (UPSTREAM_REQS G-3).

<!-- /contract -->


<a id="contract-11-4-9"></a>

<!-- contract:11-consumer-contracts:4.9 -->
### Integration contracts: Contracts & Agreements Input Contract (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-contracts-input-cc`

No Contracts implementation exists (UPSTREAM_REQS G-1, G-2; first-draft PRD). At launch step 5 receives no
contract overlays and step 6 no commitment pools; no `CommitmentBalanceEffect` is published. Target
when the gear exists: price-override windows (contracts PRD `:296-307`) as the step-5 input — the
catalog scope key they were written against no longer exists in pricing, so their key is the
Contracts owner's to restate; pool set `{pool_id, unit, pool_type, balance, balance_version, draw_order,
rollover, overage_rate?}` read at a version and copied; effect protocol per DESIGN Flow F. Every
field of the pool set is UNKNOWN / EXTERNAL CONTRACT REQUIRED until Contracts specifies it.

<!-- /contract -->

