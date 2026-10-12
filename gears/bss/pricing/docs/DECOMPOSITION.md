<!-- CONFLUENCE_TITLE: [BSS]: Pricing — PriceBook Decomposition -->
<!-- Related: ./PRD.md, ./DESIGN.md, ./DECISIONS.md | Owners: BSS Pricing team -->

# Decomposition: BSS Pricing — PriceBook

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Foundation - HIGH](#21-foundation---high)
  - [2.2 Books & Entries - HIGH](#22-books--entries---high)
  - [2.3 Prices, Windows & Dimension - HIGH](#23-prices-windows--dimension---high)
  - [2.4 Plans - HIGH](#24-plans---high)
  - [2.5 Approvals - HIGH](#25-approvals---high)
  - [2.6 Promotions & Migrations - HIGH](#26-promotions--migrations---high)
  - [2.7 Read Contract & Events - HIGH](#27-read-contract--events---high)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

Seven features match seven design slices and own 58 DoDs. Part 2a defines every phase in full, with all
implementation boxes unchecked. Phase 2b demolishes the legacy code; phase 2c builds foundation, books/entries,
prices/reference recovery, prices approvals and core events. Phase 3 adds plans (the owner defers promotions, D-409, migration requests and retirement, D-410, and the sold-as bundle and grants, D-411);
phase 4 adds resolution, pinned reads and consumer goldens; quote is not built (D-415). [DESIGN](DESIGN.md) defines the architecture.

The spec is `docs/superpowers/specs/2026-09-24-pricebook-model-design.md`, especially §2.2, §5–§8, §12–§13.
[DECISIONS](DECISIONS.md) D-384–D-433 records the living rules and explicit no-listener/outbox decisions.

## 2. Entries

### 2.1 [Foundation](features/foundation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-feature-foundation`

- **Type**: Core
- **Phases**: 2c.
- **Purpose**: Provide the fresh schema, scoped repositories, conditional approval Store, canonical errors, replay, audit and live toolkit outbox infrastructure.
- **Depends On**: shared bss-approval and toolkit infrastructure.
- **Scope**: 8 implementation DoDs in the linked FEATURE; APIs and storage in slice 01.
- **Out of scope**: Consumer adapter implementation, actual Subscriptions movement and deployed-data conversion. Later-phase behavior remains unimplemented at the phase 2 integration point.
- **Design slice**: [01-foundation.md](design/01-foundation.md).
- **Requirements**: `cpt-cf-bss-pricing-nfr-authz`, `cpt-cf-bss-pricing-nfr-audit`, `cpt-cf-bss-pricing-nfr-tenant-isolation`, `cpt-cf-bss-pricing-nfr-two-backends`, `cpt-cf-bss-pricing-nfr-idempotency-concurrency`.
- **Architecture**: `cpt-cf-bss-pricing-component-books`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-constraint-two-backends`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`.

### 2.2 [Books & Entries](features/books-entries.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-feature-books-entries`

- **Type**: Core
- **Phases**: 2c.
- **Purpose**: Author currency books and unique SKU entries, maintain dimensions/settings and export book facts; hand entry creation/removal to the reservation service.
- **Depends On**: `cpt-cf-bss-pricing-feature-foundation`.
- **Scope**: 7 implementation DoDs in the linked FEATURE; APIs and storage in slice 02.
- **Out of scope**: Consumer adapter implementation, actual Subscriptions movement and deployed-data conversion. Later-phase behavior remains unimplemented at the phase 2 integration point.
- **Design slice**: [02-books-entries.md](design/02-books-entries.md).
- **Requirements**: `cpt-cf-bss-pricing-fr-dimension-registry`, `cpt-cf-bss-pricing-fr-price-book`, `cpt-cf-bss-pricing-fr-entry-key`, `cpt-cf-bss-pricing-fr-book-export`, `cpt-cf-bss-pricing-fr-settings`.
- **Architecture**: `cpt-cf-bss-pricing-component-books`, `cpt-cf-bss-pricing-component-reservations-client`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-reserve-before-write`, `cpt-cf-bss-pricing-constraint-one-replay-store`, `cpt-cf-bss-pricing-seq-reserve-write-confirm`.

### 2.3 [Prices, Windows & Dimension](features/prices-windows-dimension.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-feature-prices-windows-dimension`

- **Type**: Core
- **Phases**: 2c.
- **Purpose**: Implement immutable money chains, models, temporary pairs and price floors; protect every entry reference with durable reservation recovery.
- **Depends On**: `cpt-cf-bss-pricing-feature-books-entries`.
- **Scope**: 11 implementation DoDs in the linked FEATURE; APIs and storage in slice 03.
- **Out of scope**: Consumer adapter implementation, actual Subscriptions movement and deployed-data conversion. Later-phase behavior remains unimplemented at the phase 2 integration point.
- **Design slice**: [03-prices-windows-dimension.md](design/03-prices-windows-dimension.md).
- **Requirements**: `cpt-cf-bss-pricing-fr-price`, `cpt-cf-bss-pricing-fr-chain-windows`, `cpt-cf-bss-pricing-fr-pair-guard`, `cpt-cf-bss-pricing-fr-min-fee`, `cpt-cf-bss-pricing-fr-temporary-pair`, `cpt-cf-bss-pricing-fr-reference-protocol`.
- **Architecture**: `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-component-reservations-client`, `cpt-cf-bss-pricing-principle-reserve-before-write`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-constraint-two-backends`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-seq-reserve-write-confirm`, `cpt-cf-bss-pricing-seq-temporary-pair`.

### 2.4 [Plans](features/plans.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-feature-plans`

- **Type**: Core
- **Phases**: 3.
- **Purpose**: Publish independent revision structure against book coverage, preserving existing pins; author items, clone and retirement prerequisites. Retirement is deferred (D-410), and so are the sold-as bundle and grants (D-411).
- **Depends On**: `cpt-cf-bss-pricing-feature-prices-windows-dimension`, `cpt-cf-bss-pricing-feature-approvals`.
- **Scope**: 9 implementation DoDs in the linked FEATURE; APIs and storage in slice 04.
- **Out of scope**: Consumer adapter implementation, actual Subscriptions movement and deployed-data conversion. Later-phase behavior remains unimplemented at the phase 2 integration point.
- **Design slice**: [04-plans.md](design/04-plans.md).
- **Requirements**: `cpt-cf-bss-pricing-fr-plans`, `cpt-cf-bss-pricing-fr-reference-protocol`.
- **Architecture**: `cpt-cf-bss-pricing-component-plans`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-reservations-client`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-reserve-before-write`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-seq-blocked-revision`, `cpt-cf-bss-pricing-seq-reserve-write-confirm`.

### 2.5 [Approvals](features/approvals.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-feature-approvals`

- **Type**: Core
- **Phases**: 2c for prices; 3 for the other subjects.
- **Purpose**: Compose price batches and govern every pricing subject with shared quorum, author separation, generation refresh and atomic terminal outcomes.
- **Depends On**: `cpt-cf-bss-pricing-feature-prices-windows-dimension`.
- **Scope**: 8 implementation DoDs in the linked FEATURE; APIs and storage in slice 05.
- **Out of scope**: Consumer adapter implementation, actual Subscriptions movement and deployed-data conversion. Later-phase behavior remains unimplemented at the phase 2 integration point.
- **Design slice**: [05-approvals.md](design/05-approvals.md).
- **Requirements**: `cpt-cf-bss-pricing-fr-publish-changes`, `cpt-cf-bss-pricing-fr-approval-units`, `cpt-cf-bss-pricing-fr-events`.
- **Architecture**: `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`, `cpt-cf-bss-pricing-seq-publish-changes`, `cpt-cf-bss-pricing-seq-temporary-pair`, `cpt-cf-bss-pricing-seq-blocked-revision`.

### 2.6 [Promotions & Migrations](features/promotions-migrations.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-feature-promotions-migrations`

- **Type**: Core
- **Phases**: 3.
- **Purpose**: Version dated percentage promotions and approve explicit subscription migration requests; retain period-aware previews without executing consumer-owned movement. Wholly deferred by the owner: promotions (D-409), migration requests and retirement (D-410).
- **Depends On**: `cpt-cf-bss-pricing-feature-plans`, `cpt-cf-bss-pricing-feature-approvals`.
- **Scope**: 7 implementation DoDs in the linked FEATURE; APIs and storage in slice 06.
- **Out of scope**: Consumer adapter implementation, actual Subscriptions movement and deployed-data conversion. Later-phase behavior remains unimplemented at the phase 2 integration point.
- **Design slice**: [06-promotions-migrations.md](design/06-promotions-migrations.md).
- **Requirements**: `cpt-cf-bss-pricing-fr-promotions`, `cpt-cf-bss-pricing-fr-migrations`.
- **Architecture**: `cpt-cf-bss-pricing-component-promotions`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-constraint-no-row-locks`.

### 2.7 [Read Contract & Events](features/read-contract-events.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-feature-read-contract-events`

- **Type**: Supporting
- **Phases**: 4 for reads (quote is not built, D-415); 2c for core events; 3 for added events.
- **Purpose**: Deliver reproducible resolution matrices, pinned-price reads and Studio quote, plus typed transactional events and consumer goldens. The Studio quote is not built (D-415).
- **Depends On**: `cpt-cf-bss-pricing-feature-plans`, `cpt-cf-bss-pricing-feature-promotions-migrations`, `cpt-cf-bss-pricing-feature-approvals`.
- **Scope**: 8 implementation DoDs in the linked FEATURE; APIs and storage in slice 07.
- **Out of scope**: Consumer adapter implementation, actual Subscriptions movement and deployed-data conversion. Later-phase behavior remains unimplemented at the phase 2 integration point.
- **Design slice**: [07-read-contract-events.md](design/07-read-contract-events.md).
- **Requirements**: `cpt-cf-bss-pricing-fr-resolve`, `cpt-cf-bss-pricing-fr-price-read`, `cpt-cf-bss-pricing-fr-quote`, `cpt-cf-bss-pricing-fr-events`.
- **Architecture**: `cpt-cf-bss-pricing-component-read-contract`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-constraint-two-backends`.

## 3. Feature Dependencies

```mermaid
flowchart LR
  foundation --> books-entries --> prices-windows-dimension --> approvals
  approvals --> plans --> promotions-migrations --> read-contract-events
  approvals --> read-contract-events
```

This graph orders delivery of the full feature set. Core typed events in entry 2.7 are an explicit phase 2
subset and depend only on foundation, prices and approvals; their delivery does not wait for phase 3/4 reads.
The rest of entry 2.7 depends on plans/promotions. Document numbering places plans before approvals for reader
continuity; implementation builds the shared approval core first. Reservations and confirmation/release retry
must integrate with Products before phase 2 is mergeable. Pricing requests migrations in phase 3; Subscriptions
execution and confirmation of retirement remain separate consumer work.
