<!-- CONFLUENCE_TITLE: [BSS]: FX Rate Provider (Adapter Gear) — Product Requirements -->
<!-- Related: ./DESIGN.md, ../../ledger/docs/PRD.md, ../../ledger/docs/design/06-fx-multicurrency.md | Owners: @vstudzinskyi (BSS Billing Platform team) -->

# PRD — FX Rate Provider (Adapter Gear)

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Rate Feed](#51-rate-feed)
  - [5.2 Sources & Fallback](#52-sources--fallback)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

> **Implementation revision (2026-07-23) — `*-plugin` pattern.** The requirements
> below are unchanged, but the gear was built on the platform's plugin pattern:
> each rate source (ECB, http-json) is a **source plugin** that self-registers in
> the types-registry, a **core gear** discovers + composes them (ordered by each
> plugin's `priority`), and the ledger discovers the composite **lazily each sync
> tick**. So "config-driven source assembly / ordered fallback" (FR
> `cpt-cf-bss-rate-provider-fr-config-onboarding` / `-ordered-source-fallback`) is
> realized by plugin discovery + `priority`, and "strict config validation" (FR
> `cpt-cf-bss-rate-provider-fr-strict-config-validation`) is per-plugin (an
> http-json plugin requires a `mapping` + https `base_url`) with cross-gear
> alignment by a shared `vendor`. See `DESIGN.md` § Implementation revision.

## 1. Overview

### 1.1 Purpose

The **FX rate-provider gear** supplies the Billing Ledger with **live foreign-exchange
reference rates**. It is a stateless fetch-only adapter: it retrieves the latest published
rates from configured external sources (ECB primary; further feeds by configuration) and
hands them to the ledger through the ledger-owned `RateProviderV1` contract. It stores
nothing, exposes no REST surface, and performs no accounting.

### 1.2 Background / Problem Statement

The ledger's multi-currency posting (ledger PRD § Money, Rounding & Foreign Exchange)
requires a live rate feed to translate transaction currency into functional currency, but
the ledger deliberately declares the provider integration out of its own scope — it ships
only the consuming seam (`RateProviderV1`, `RateSyncJob`, the local rate store, and the
fail-safe that blocks FX posts when no rate is available). Until an adapter implements
that seam, every FX post blocks (`FX_RATE_UNAVAILABLE`). This gear closes that gap.

### 1.3 Goals (Business Outcomes)

- **Unblock multi-currency billing**: with the adapter deployed and one source configured,
  cross-currency invoice posting works without manual rate seeding.
- **Auditable rates**: every synced rate is traceable to a named provider and its original
  publication timestamp — no fabricated or silently stale values.
- **Cheap provider onboarding**: a new plain REST rate feed is added by configuration
  only, with no code change and no redeployment of consuming modules.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Reference rate | An FX rate published by an institution (e.g. ECB) for a currency pair on a given date. |
| Functional currency | The tenant's accounting currency the ledger translates into. |
| Direct pair | A pair the provider itself publishes (ECB publishes EUR→X only). |
| Rate document | The whole set of pairs one source returns for one fetch. |
| Provenance | Which concrete source actually served the rates recorded by the ledger. |
| Fail-safe by absence | On provider failure the ledger blocks FX posts rather than guessing a rate. |

## 2. Actors

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-bss-rate-provider-actor-platform-operator`

- **Role**: Configures rate sources (order, endpoints, credentials) and operates the
  platform deployment; reacts to feed-freshness alarms raised by the ledger.
- **Needs**: Config-only source management, clear startup validation errors, fetch metrics.

#### Finance Controller / Auditor

**ID**: `cpt-cf-bss-rate-provider-actor-finance-auditor`

- **Role**: Signs off FX treatment; audits which rate was applied to which posting.
- **Needs**: Deterministic rate conversion and per-rate provider/publication-time provenance.

### 2.2 System Actors

#### Billing Ledger (`RateSyncJob`)

**ID**: `cpt-cf-bss-rate-provider-actor-ledger-rate-sync`

- **Role**: Sole consumer. Periodically pulls the latest rate document through
  `RateProviderV1` and upserts it into the ledger's local rate store.

#### ECB Reference-Rate Feed

**ID**: `cpt-cf-bss-rate-provider-actor-ecb-feed`

- **Role**: Primary external source — free, EUR-based daily reference rates.

#### Bank / PSP Rate Feed (future)

**ID**: `cpt-cf-bss-rate-provider-actor-bank-psp-feed`

- **Role**: Fallback / settlement-evidence source, onboarded by configuration when
  procured by ops (DESIGN decision O-10).

## 3. Operational Concept & Environment

Runtime, OS, and lifecycle policy follow the repository-level platform defaults
([`guidelines/`](../../../../guidelines/)); the consuming seam is defined by the parent
[ledger PRD](../../ledger/docs/PRD.md).

### 3.1 Gear-Specific Environment Constraints

- Requires **outbound HTTPS egress** to configured provider endpoints (unusual for gears —
  most have no external egress).
- No database and no per-tenant state: rates are global reference data; the ledger owns
  all persistence and the per-tenant fan-out.

## 4. Scope

### 4.1 In Scope

- Fetching the latest published rate document from configured external sources.
- Ordered cross-source fallback at fetch time with true-source provenance.
- Implementing the ledger's `RateProviderV1` contract (fetch, health, provider identity).
- Config-driven source assembly, including no-code onboarding of plain REST JSON feeds.
- Deterministic conversion of published decimal rates into the contract's fixed-precision
  integer representation.

### 4.2 Out of Scope

- Rate persistence, staleness marking, snapshotting, per-tenant fan-out — ledger-owned.
- Currency translation, triangulation / pair inversion — ledger-owned (DESIGN O-3).
- Pricing-side FX and rate-lock governance — Catalog module.
- Provider commercial contracts and credential procurement — ops.
- Manual / break-glass rate ingest — the ledger's own seed endpoint.

## 5. Functional Requirements

### 5.1 Rate Feed

#### Live rate feed via the ledger contract

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-fr-live-rate-feed`

The gear **MUST** supply the latest published FX rates to the Billing Ledger through the
ledger-owned `RateProviderV1` contract, returning the whole published table when no
specific pairs are requested.

- **Rationale**: The ledger requirement `cpt-cf-bss-ledger-fr-multi-currency-fx`
  (ledger PRD § Multi-currency & FX) needs a live feed; this gear is that feed.
- **Actors**: `cpt-cf-bss-rate-provider-actor-ledger-rate-sync`

#### Provider publication time

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-fr-provider-publication-time`

Every returned rate **MUST** carry the provider's original publication timestamp (UTC) —
never the fetch time. On non-publication days the last published rate is returned with its
original timestamp unchanged.

- **Rationale**: The ledger's staleness policy
  (`cpt-cf-bss-ledger-fr-fx-rate-source-failure`) is only meaningful against true
  publication time; stamping fetch time would mask stale feeds.
- **Actors**: `cpt-cf-bss-rate-provider-actor-finance-auditor`

#### Direct pairs only

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-fr-direct-pairs-only`

The gear **MUST** emit only pairs the serving source natively publishes. A pair the source
cannot serve is omitted — never synthesized, inverted, or merged from another source.
Cross-base derivation is the ledger's triangulation concern.

- **Rationale**: Keeps every synced document single-source-coherent for audit and keeps
  rate-math ownership in one place (DESIGN O-1 / O-3).
- **Actors**: `cpt-cf-bss-rate-provider-actor-finance-auditor`

### 5.2 Sources & Fallback

#### Ordered source fallback

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-fr-ordered-source-fallback`

The gear **MUST** try configured sources in their configured order and return the first
whole successful rate document. Only when **all** sources fail may it report failure to
the caller.

- **Rationale**: Realizes the fallback algorithm the ledger design expects at fetch
  time ([ledger FX design § rate-source fallback](../../ledger/docs/design/06-fx-multicurrency.md)),
  where the ledger cannot do it itself.
- **Actors**: `cpt-cf-bss-rate-provider-actor-ledger-rate-sync`

#### True-source provenance

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-fr-source-provenance`

The gear **MUST** report the identity of the source that actually served the most recent
successful document, so ledger rate rows and snapshots record the true upstream — never a
generic adapter name.

- **Rationale**: Auditors must be able to answer "whose rate was applied here".
- **Actors**: `cpt-cf-bss-rate-provider-actor-finance-auditor`

#### Config-driven source onboarding

- [ ] `p2` - **ID**: `cpt-cf-bss-rate-provider-fr-config-onboarding`

Adding, removing, or reordering rate sources **MUST** be a configuration change. A plain
REST JSON rate feed **MUST** be onboardable with no code change.

- **Rationale**: Provider procurement is an ops decision (DESIGN O-10); billing must not
  need a release to switch or add a feed.
- **Actors**: `cpt-cf-bss-rate-provider-actor-platform-operator`
- **As implemented:** "configuration change" means deploying/configuring a source-plugin
  gear with a `vendor` + `priority`, not editing one shared list — see DESIGN.md §3.2.

#### Strict configuration validation

- [ ] `p2` - **ID**: `cpt-cf-bss-rate-provider-fr-strict-config-validation`

Invalid *per-plugin* configuration — e.g. the http-json plugin's `mapping` missing, or its
`api_key` missing while `auth != none` — **MUST** fail that plugin gear's own startup
loudly, not surface at first fetch.

- **Rationale**: A misconfigured rate feed discovered at fetch time silently degrades
  billing; startup is the cheapest place to fail for what a single gear's own `init()` can
  check.
- **Actors**: `cpt-cf-bss-rate-provider-actor-platform-operator`
- **As implemented — narrower than the original wording:** there is no single "source
  list" or "source kind" anymore (see DESIGN.md's Implementation-revision note and §3.2),
  so **an unknown source kind and an empty source list are no longer things to validate at
  all** — a `RateProviderV1` source is a plugin crate, not a config value, so there's no
  "unknown kind" to reject. Zero matching source plugins for the configured vendor is now a
  **first-fetch runtime error**, not a startup failure (§3.2's "Empty result is a runtime
  error" — deliberately, so plugin registration order never has to be enforced). A
  `vendor` mismatch between the core gear and a source plugin is not rejected anywhere
  either — it is filtered out **silently**, with no log line at all (a *matching*-vendor
  instance whose scoped client can't be resolved *is* logged as a warning — the two cases
  are handled differently). Only the http-json plugin's own two config checks above are
  still startup-time failures today.

## 6. Non-Functional Requirements

Project-wide NFR baselines follow the repository [`guidelines/`](../../../../guidelines/)
and the parent [ledger PRD](../../ledger/docs/PRD.md) §7; gear-specific NFRs below.

### 6.1 Gear-Specific NFRs

#### Off the posting path

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-nfr-off-posting-path`

A provider outage **MUST NOT** affect posting latency or availability: the gear is
consumed only by the ledger's background sync job, never on the posting path.

- **Threshold**: Zero posting-path invocations; provider downtime affects FX posts only
  through the ledger's own fail-safe (block, not guess).
- **Rationale**: Hard isolation requirement inherited from the ledger's post-path NFRs.

#### Fail-safe by absence

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-nfr-fail-safe-absence`

When no source can serve, the gear **MUST** return an error — never fabricated, partial,
or silently stale data — so the ledger blocks FX posts and alarms instead of posting a
wrong rate.

- **Threshold**: 100% of all-sources-failed fetches surface as errors to the caller.
- **Rationale**: `cpt-cf-bss-ledger-fr-fx-rate-source-failure` forbids silent fallback;
  the adapter must not undermine it upstream.

#### Deterministic rate conversion

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-nfr-deterministic-conversion`

Converting a published decimal rate into the contract's fixed-precision integer form
**MUST** be deterministic: the same published value always yields the same integer, using
exact-decimal arithmetic with banker's rounding (half-to-even) and explicit overflow
errors.

- **Threshold**: Golden-vector equality on repeated conversion, including exact half-way
  decimals; overflow / non-numeric input always errors, never truncates.
- **Rationale**: Matches the ledger rounding default
  (`cpt-cf-bss-ledger-fr-money-rounding-scale`); auditors must be able to reproduce rates.

#### Fetch latency

- [ ] `p2` - **ID**: `cpt-cf-bss-rate-provider-nfr-fetch-latency`

A fetch against one source **MUST** complete one bounded attempt within its configured
timeout; a successful fetch completes fast enough that feed freshness holds within one
ledger sync tick.

- **Threshold**: p95 ≤ 2 s per source (draft; confirm against ECB response times);
  worst-case composite duration bounded by the sum of configured per-source timeouts.
- **Rationale**: Background job budget; G10 pairs must not cross the ledger's 24 h
  staleness window under normal operation.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

### 6.2 NFR Exclusions

- **Horizontal scalability**: not applicable — one lightweight fetch per ledger sync tick;
  no request fan-in.
- **Data durability**: not applicable — the gear is stateless; durability is the ledger's.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### `RateProviderV1` implementation

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-interface-rate-provider-v1`

- **Type**: Rust trait implementation (`bss-ledger-sdk` `RateProviderV1`,
  GTS `gts.cf.bss.ledger.rate-provider.v1`), registered in the platform `ClientHub`.
- **Stability**: stable
- **Description**: The gear's only consumable surface — latest-rates fetch, health probe,
  and serving-provider identity.
- **Breaking Change Policy**: The contract is owned by the ledger SDK; this gear never
  changes it unilaterally.

### 7.2 External Integration Contracts

#### Provider feed contract

- [ ] `p2` - **ID**: `cpt-cf-bss-rate-provider-contract-provider-feed`

- **Direction**: required from external rate providers.
- **Protocol/Format**: HTTPS GET returning a parseable rate document (ECB daily XML, or
  JSON for config-mapped REST feeds) that includes a publication timestamp.
- **Compatibility**: Feed format changes are absorbed in this gear (parser / mapping
  config); consumers are insulated by the `RateProviderV1` contract.

## 8. Use Cases

#### Fallback serve with provenance

- [ ] `p2` - **ID**: `cpt-cf-bss-rate-provider-usecase-fallback-serve`

**Actor**: `cpt-cf-bss-rate-provider-actor-ledger-rate-sync`

**Preconditions**:
- Two sources configured in order (primary, fallback); primary is unreachable.

**Main Flow**:
1. The ledger sync job requests the latest rates.
2. The gear tries the primary source; the attempt fails within its timeout.
3. The gear tries the fallback source and receives a whole rate document.
4. The gear returns that document and reports the fallback source as the serving provider.
5. The ledger stores the rates stamped with the fallback source's identity.

**Postconditions**:
- Ledger rate rows record the fallback provider and the provider's publication time.

**Alternative Flows**:
- **All sources fail**: the gear returns the last error; the ledger alarms and FX posts
  block (fail-safe by absence).

## 9. Acceptance Criteria

- [ ] With the adapter deployed and ECB configured, one ledger sync tick populates the
  ledger's rate store and a cross-currency invoice post locks a rate (no
  `FX_RATE_UNAVAILABLE`).
- [ ] With all sources down, FX posts block, the feed-freshness alarm fires, and non-FX
  posting is unaffected.
- [ ] After a primary-source outage with a healthy fallback, synced rows record the
  fallback provider's identity.
- [ ] Re-fetching an identical published document yields byte-identical integer rates.
- [ ] An http-json plugin configured without a `mapping`, or with `auth != none` and no
  `api_key`, fails that plugin's own gear startup with a clear error.
- [ ] With zero source plugins registered for the core gear's configured `source_vendor`,
  the first `RateSyncJob` tick's `fetch_latest` errors — never a startup crash — and a
  source plugin registering by a later tick self-heals it. There is no "unknown
  kind"/"empty list"/"precedence mismatch" startup check anymore; those concepts don't
  exist in the current design (see DESIGN.md's Implementation-revision note).

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `bss-ledger-sdk` | Owns the `RateProviderV1` contract and its types | p1 |
| `types-registry` / `types-registry-sdk` | Plugin discovery + registration substrate every crate here uses (as implemented — plugin pattern, see DESIGN.md) | p1 |
| Billing Ledger (`RateSyncJob`) | Sole consumer; pulls and persists the rates | p1 |
| ECB reference-rate feed | Primary external source (free daily publication) | p1 |
| Ledger triangulation companion change | Required for non-EUR-functional tenants (DESIGN §4) | p1 |
| Bank / PSP rate feed | Future fallback source; procurement owned by ops | p3 |

## 11. Assumptions

- The ECB daily reference-rate feed remains publicly available at no cost.
- Source configuration is operator-supplied and platform-trusted (not tenant input).
- The ledger sync job remains a single non-concurrent ticker per deployment (DESIGN O-7a).

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| ECB feed outage with no fallback configured (v1 is ECB-only) | FX posts block until feed recovers | Fail-safe by absence + ledger alarm; fallback mechanism is config-ready (deploy/configure another source-plugin gear) |
| EUR-based pairs only until ledger triangulation/inversion lands | Non-EUR-functional tenants cannot post FX | Sequencing tracked as a hard companion dependency (DESIGN §4) |
| Provider format drift breaks parsing | Fetch fails, feed goes stale | Whole-document parse failure surfaces as an error → fallback / alarm, never partial data |

## 13. Open Questions

- Concrete bank / PSP fallback feed and credentials — ops procurement (DESIGN O-10).
- Finance / audit sign-off on banker's-rounding conversion (DESIGN O-4).

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md) — self-contained technical design for this gear.
- **Upstream PRD**: [`../../ledger/docs/PRD.md`](../../ledger/docs/PRD.md) — § Money,
  Rounding & Foreign Exchange (`cpt-cf-bss-ledger-fr-multi-currency-fx`,
  `cpt-cf-bss-ledger-fr-fx-rate-source-failure`,
  `cpt-cf-bss-ledger-fr-money-rounding-scale`).
- **Consuming design**:
  [`../../ledger/docs/design/06-fx-multicurrency.md`](../../ledger/docs/design/06-fx-multicurrency.md)
  — the fetch-time rate-source-fallback algorithm this gear realizes.
