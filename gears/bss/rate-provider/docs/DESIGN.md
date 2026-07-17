<!-- CONFLUENCE_TITLE: [BSS]: FX Rate Provider (Adapter Gear) — Technical Design -->
<!-- Related: ../../ledger/docs/DESIGN.md, ../../ledger/docs/design/06-fx-multicurrency.md, ../../ledger/docs/PRD.md | Owners: @vstudzinskyi (BSS Billing Platform team) -->

# Technical Design — FX Rate Provider (Adapter Gear)

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional context](#4-additional-context)
  - [Security & AuthZ](#security--authz)
  - [Feature metrics](#feature-metrics)
  - [Testing architecture](#testing-architecture)
  - [Decision register](#decision-register)
  - [Companion ledger change (hard dependency, from O-3)](#companion-ledger-change-hard-dependency-from-o-3)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-design-main`

> **Canonical design entry point.** This document is the FX rate-provider gear's technical
> design and the anchor for spec traceability. The gear is small enough that the design is
> **self-contained** — there is no slice set; component, contract, and sequence detail is
> normative here.
>
> **Status**: DRAFT — decisions recorded (O-1 & O-3 decided; all other defaults accepted
> 2026-07-08). Ready for implementation planning. The O-3 companion `bss-ledger` change
> (§4 "Companion ledger change") is a linked hard dependency.
>
> **Implementation revision (2026-07-23) — `*-plugin` pattern.** The gear was
> built on the platform's plugin pattern (types-registry `PluginV1` instances +
> `ClientHub` scoped registration + host-side discovery), which revises three
> decisions below. This note is authoritative where it conflicts with the older
> prose:
>
> - **Two crates → four.** Instead of one composite gear with a config `sources[]`
>   list, each source is its own **plugin gear** — `bss-rate-provider-ecb-plugin`
>   and `bss-rate-provider-http-json-plugin` — and a **core gear**
>   (`bss-rate-provider`) discovers and composes them. Shared source utilities
>   (conversion, error mapping, fetch metrics) and the source-plugin GTS spec live
>   in `bss-rate-provider-sdk`.
> - **O-1 revised — scoped plugin registration, two layers.** *Level 1:* each
>   source plugin registers a `PluginV1<RateProviderSourcePluginSpecV1>` instance
>   in the types-registry and a scoped `RateProviderV1` client (`priority` = the
>   fallback order). The core gear lists those instances, orders by `priority`,
>   resolves each via `get_scoped`, and composes them (first whole successful
>   document; last-served provenance — unchanged). *Level 2:* the core gear
>   registers its composite as a `PluginV1<RateProviderPluginSpecV1>` (spec in
>   `bss-ledger-sdk`) + scoped `RateProviderV1`; the ledger discovers **that**.
> - **O-7 revised — no `deps` edge.** The ledger discovers the composite **lazily
>   on every `RateSyncJob` tick** (types-registry `list_instances` → vendor/priority
>   select → `get_scoped`), falling back to `UnconfiguredRateProviderV1`. A
>   late-registered adapter self-heals on the next tick, so the startup-ordering
>   concern is gone and the ledger stays decoupled (schedulable without the adapter).
> - **O-12 revised — per-plugin config, not `sources[]`.** Source assembly is now
>   plugin discovery ordered by each plugin's `priority`; the `fx.provider_order` /
>   `sources[]` cross-gear order check is replaced by matching `vendor` between the
>   core gear, its source plugins, and the ledger's `fx.provider_vendor`. Unknown
>   `kind` no longer exists (each source is a distinct plugin crate). Config
>   validation is per plugin (http-json requires `mapping` + https `base_url`).
>
> The domain model (§3.1), the fetch-only/direct-pairs/deterministic-conversion
> principles (§2.1), the metrics (§4), and the O-3 triangulation boundary are
> unchanged.

## 1. Architecture Overview

### 1.1 Architectural Vision

The **FX rate-provider gear** is a **stateless adapter**: it implements the ledger's
`RateProviderV1` contract (`bss-ledger-sdk`, GTS `gts.cf.bss.ledger.rate-provider.v1`) and
registers an `Arc<dyn RateProviderV1>` into the platform `ClientHub`, so the ledger's
background `RateSyncJob` resolves a live adapter instead of the fail-safe
`UnconfiguredRateProviderV1` default. The registered instance is a
**`CompositeRateProvider`** that tries its ordered sources in the PRD-ratified provider
order (2026-06-10) and returns the **first whole document** that succeeds (all-or-nothing
per source, never a merge; O-1). The fallback **mechanism** ships in v1, but the **v1
configuration is ECB-only** (O-10): the bank / PSP feed is a *future* source, added later
by deploying/configuring one more source-plugin gear (its own crate, its own `vendor` +
`priority`) — no change to this gear's code. It performs no persistence, no HTTP surface,
and no accounting logic.

**This gear only fetches rates.** The ledger declares the FX rate provider out of its own
scope ("The FX rate provider itself (integration, feeds) — external; the ledger consumes
rates and snapshots them",
[`../../ledger/docs/design/06-fx-multicurrency.md`](../../ledger/docs/design/06-fx-multicurrency.md))
and already ships the consuming seam: the `RateProviderV1` SDK trait, the `RateSyncJob`
that pulls it, the `ledger_fx_rate` local store, the lock-time `RateSource`, staleness /
provider-precedence resolution, and the immutable `rate_snapshot`. Everything ledger-owned
stays there and is NOT restated here:

- Functional-currency **translation** and the dual-column balance.
- **Triangulation** through EUR (X→EUR→Y) — ledger-owned (O-3); requires the companion
  ledger change (§4 "Companion ledger change").
- **Staleness** rules (G10 > 24 h; others ≤ 7 d) and `stale` marking.
- **Provider precedence / fallback-order** resolution over the local store.
- **`rate_snapshot`** freezing, `ledger_fx_rate` upsert, per-tenant fan-out.
- Realized / unrealized FX, revaluation runs.
- The `RateSyncJob` tick cadence and its `FX_SNAPSHOT_MISSING` alarm (ledger-side).

Also out of scope: **pricing-side FX / rate-lock governance** (Catalog module) and
**provider commercial contracts / credential procurement** (ops).

### 1.2 Architecture Drivers

Requirements from the ledger [`PRD.md`](../../ledger/docs/PRD.md) and the FX slice design
that significantly shape this gear.

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-ledger-fr-multi-currency-fx` | The ledger needs a live rate feed to translate transaction currency into functional currency; this gear supplies it through the fixed `RateProviderV1` seam — implement `provider_id()`, `fetch_latest()`, `health()` (§3.3). |
| `cpt-cf-bss-ledger-fr-fx-rate-source-failure` | Provider outage must never produce a silent wrong rate. The composite returns the last `RateProviderError` when **all** sources fail; the ledger job then alarms and FX posts block (`FX_RATE_UNAVAILABLE`) — fail-safe by absence (§2.2). |
| Rate-source fallback ([ledger FX design](../../ledger/docs/design/06-fx-multicurrency.md)) | The ledger resolves precedence over its **local store**; cross-source fallback at fetch time is this gear's `CompositeRateProvider` — ordered sources, first whole successful document, true-source provenance (§3.2). |
| Provider onboarding without code change | Plugin-discovery source assembly: the core gear discovers registered source-plugin instances by `vendor`, ordered by `priority`; a plain REST feed is onboarded by configuring `bss-rate-provider-http-json-plugin` alone (no code), a new provider *family* costs one new plugin crate (§3.2). |

#### NFR Allocation

| NFR theme | Allocated to | Design Response |
|-----------|--------------|-----------------|
| Post-path isolation (hard) | Consumption model | `fetch_latest` is called only by the background `RateSyncJob`, never on the posting path; a provider outage fails the job (ledger alarms), never a post. |
| Feed freshness | Fetch path + ledger tick | A successful fetch SHOULD complete within one `rate_sync_tick` (ledger default 1 h) so G10 pairs never cross the 24 h staleness window under normal operation. |
| Fetch latency | Sources + HTTP client | `fx_provider_fetch_duration_seconds{provider}` p95 ≤ 2 s **per source** (draft; confirm against ECB response times). One bounded attempt per source per call, no unbounded retry. The composite's worst case is the **sum of the configured per-source `timeout_ms`** (every source down); this is acceptable because the fetch runs only inside the background `RateSyncJob`, never on the posting path — no shared total deadline is imposed in v1. |
| Availability | Ledger fail-safe | Best-effort; the ledger's fail-safe (block, not guess) absorbs adapter downtime. |

#### Key Decisions

The load-bearing decisions are recorded in the decision register (§4 "Decision register");
the two that shape the architecture:

| Decision | Summary |
|----------|---------|
| **O-1 — Composite adapter, no merge** | The ledger resolves exactly one `RateProviderV1` (a scoped `ClientHub` lookup as implemented — see §1.3) and stamps every synced row with that single `provider_id()`, so per-provider registrations do not work today. This gear registers ONE composite (`DiscoveringRateProvider` wrapping a `CompositeRateProvider`) that returns the first whole successful document — a snapshot period stays single-source-coherent for audit. Source provenance is preserved via the last-served index (§3.2). |
| **O-3 — Ledger owns triangulation** | The adapter emits **only the source's native direct pairs** (ECB's EUR pairs) — no cross-rate synthesis here. Cross-base rates (X→EUR→Y) are computed ledger-side in `RateSource`; enabling the ledger's deferred triangulation is a hard companion dependency (§4). |

### 1.3 Architecture Layers

Four crates, not one — each source plugin self-registers; the core gear discovers and
composes them:

```text
Source-plugin gears   Each source is its OWN gear crate. Its init() builds the shared HTTP
(bss-rate-provider-    client, registers a PluginV1<RateProviderSourcePluginSpecV1>
  ecb-plugin,          instance in types-registry (vendor + priority in the instance JSON),
  -http-json-plugin)   and register_scoped::<dyn RateProviderV1>(gts_id) in ClientHub.
       │
       ▼
Core gear init()      Registers ITSELF as a PluginV1<RateProviderPluginSpecV1> instance +
(bss-rate-provider)    scoped RateProviderV1 (the one the ledger discovers). Does NOT
                       discover sources yet — that is deferred (see below).
       │
       ▼
DiscoveringRateProvider   The registered instance. On the FIRST fetch_latest call: lists
(lazy, self-healing)      types-registry instances of RateProviderSourcePluginSpecV1,
                          keeps the ones whose vendor == source_vendor, sorts by priority,
                          resolves each via get_scoped, and builds a CompositeRateProvider —
                          cached in a OnceCell. A failed discovery is NOT cached, so the
                          next tick retries (self-heals if a source registered late).
       │
       ▼
CompositeRateProvider   impl RateProviderV1 · ordered sources · first whole successful
(selection)             document · last-served provenance — source-agnostic
       │
       ▼
Sources            EcbRateProvider (bss-rate-provider-ecb-plugin: XML fetch/parse) ·
(one per plugin)   HttpJsonRateProvider (bss-rate-provider-http-json-plugin: generic
                   GET-JSON + field mapping)
       │
       ▼
HTTP client        toolkit-http (hyper + rustls), outbound HTTPS only, built once per
                   source plugin via the shared bss-rate-provider-sdk::http_client helper
                   → ECB eurofxref-daily.xml (primary) · bank/PSP feed (fallback, post-v1 O-10)
```

Shared source utilities (exact-decimal conversion, HTTP-error mapping, fetch metrics, the
shared HTTP client builder, the `PluginV1` registration helper) live in
`bss-rate-provider-sdk`, used by every source plugin and the core gear alike.

The ledger-side `RateSyncJob` (outside this boundary) resolves the registered composite
instance the same way this gear resolves its own sources — a types-registry lookup +
scoped `ClientHub` get, matched by the ledger's `fx.provider_vendor` — then calls
`fetch_latest(ctx, &[], request_id)` once per tick, then reads `provider_id()` for the row
stamp. (The exact ledger-side resolution code is out of this gear's boundary and not
re-verified here — see the ledger's own design docs.)

## 2. Principles & Constraints

### 2.1 Design Principles

#### Fetch-only adapter

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-principle-fetch-only`

The gear fetches rates and nothing else: no persistence, no translation, no triangulation,
no staleness marking, no snapshotting — those are ledger-owned (§1.1). `fetch_latest` MUST
be side-effect-free and safe to call repeatedly (the ledger job is idempotent).

#### Config-driven source assembly

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-principle-config-driven-assembly`

The active sources and their fallback order are config — the composite is assembled lazily
by discovery, never hardcoded. Each source is its own deployed plugin gear with a `vendor` +
`priority` in its config; add a source by deploying/configuring its plugin gear, remove one
by un-configuring it, reorder the fallback chain by changing `priority` values. A new
provider *family* costs one new plugin crate implementing `RateProviderV1`; a new *simple
REST feed* costs zero code — configure `bss-rate-provider-http-json-plugin` with a
`mapping`.

#### All-or-nothing per source

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-principle-all-or-nothing-source`

Fallback triggers only when a source returns `Err` for the whole fetch — never per missing
pair, never a cross-source merge. A pair absent from the chosen source's document is simply
absent (the ledger treats it as no rate). A snapshot period stays single-source-coherent
for audit (O-1).

#### Direct pairs only

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-principle-direct-pairs-only`

A source emits only its natively published pairs. A requested pair the source cannot serve
is **omitted** (not an error), never synthesized — cross-base derivation is the ledger's
triangulation concern (O-3).

#### Deterministic conversion

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-principle-deterministic-conversion`

`rate → rate_micro` conversion parses the published decimal into an **exact decimal
representation** (never binary floating point) and rounds with banker's rounding
(half-to-even), matching the platform ledger rounding default, so a re-fetch of the same
published rate yields the same integer (§3.2, O-4).

#### Provider time, not fetch time

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-principle-provider-as-of`

`as_of` MUST be the provider's publication timestamp normalized to UTC — never `now()`.
On non-publication days (weekends / TARGET holidays) the last published rate is returned
with its original `as_of`, so the ledger's staleness rule still applies.

### 2.2 Constraints

#### Fixed SDK contract

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-constraint-fixed-sdk-contract`

The `RateProviderV1` trait, `ProviderRate`, `CurrencyPair`, and `RateProviderError` are
**already defined** in `bss-ledger-sdk` and MUST NOT be changed without a ledger-side
change (GTS `gts.cf.bss.ledger.rate-provider.v1`).

#### Never on the posting path

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-constraint-off-posting-path`

`fetch_latest` is called only by the background `RateSyncJob`. A provider outage fails the
job (ledger alarms), never a post. The adapter MUST NOT retry indefinitely; one bounded
attempt per call — the ledger job schedules the next tick.

#### Stateless

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-constraint-stateless`

The adapter holds no DB and no per-tenant state; a provider publishes **global** rates and
the ledger fans them out per tenant. The only in-memory state is the composite's
last-served source index (interior mutability, no persistence).

#### Fail-safe by absence

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-constraint-fail-safe-by-absence`

If the adapter is not registered, the ledger uses `UnconfiguredRateProviderV1` → the local
store stays empty → FX posts block (`FX_RATE_UNAVAILABLE`), never a silent wrong rate.

#### Rate precision

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-constraint-rate-micro-precision`

`ProviderRate.rate_micro` is the functional-per-unit-transaction multiplier × 1e6, `i64`
(O-5: kept for v1; revisit for high-unit / crypto pairs — any change is an SDK change).
Overflow / non-finite values MUST map to `RateProviderError::Internal`, never a silent
truncation.

#### Secrets handling

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-constraint-secrets`

Any provider API key MUST come from config `${VAR}` expansion or the platform CredStore —
never hardcoded, never logged, never in this document. Implemented as `secrecy::SecretString`
(`bss-rate-provider-http-json-plugin`'s `api_key: Option<SecretString>`, the same pattern
used by `authn-resolver`'s plugin configs) so an accidental `Debug`/`tracing` dump of the
config struct is redacted rather than printing the key — only `expose_secret()` reveals it,
at the one call site that builds the outbound auth header. ECB has no credential (public
feed). **Caveat:** this only redacts the *typed* config once deserialized into Rust; it does
not protect the gear's raw config section from `toolkit`'s own effective-config dump feature
(`toolkit::bootstrap::config::dump`), which serializes the pre-deserialization JSON — a
platform-level gap outside this gear, not fixed here.

## 3. Technical Architecture

### 3.1 Domain Model

The domain types are **inherited from `bss-ledger-sdk`** (`rate_provider.rs`) and NOT
redefined here (constraint `cpt-cf-bss-rate-provider-constraint-fixed-sdk-contract`).

#### Type: `CurrencyPair`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `base` | string (ISO 4217) | Yes | Transaction currency |
| `quote` | string (ISO 4217) | Yes | Functional currency |

#### Type: `ProviderRate`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `base` | string | Yes | Base (transaction) currency |
| `quote` | string | Yes | Quote (functional) currency |
| `rate_micro` | int64 | Yes | Functional-per-unit-base × 1e6 (fixed precision) |
| `as_of` | timestamp (UTC) | Yes | Provider publication time; drives ledger staleness |

#### Enum: `RateProviderError`

| Value | Description |
|-------|-------------|
| `PairUnavailable { base, quote }` | Provider does not publish this pair |
| `Unreachable(msg)` | Network / DNS / timeout |
| `UpstreamStatus(u16)` | Non-success HTTP status |
| `InvalidPair(msg)` | Malformed / unknown currency code |
| `Internal(msg)` | Parse / conversion fault |

### 3.2 Component Model

Each component carries a stable `cpt-cf-bss-rate-provider-component-{slug}` ID.

#### Plugin discovery & per-plugin configuration

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-component-source-factory`

There is no `build_source` factory and no `kind` field — each source is its own gear crate
implementing `RateProviderV1` directly, and "assembly" is types-registry discovery, not a
config-driven factory loop:

- Each source-plugin gear's `init()` registers a `PluginV1<RateProviderSourcePluginSpecV1>`
  instance (carrying its own `vendor` + `priority` in the instance JSON) and a scoped
  `RateProviderV1` client keyed by that instance's GTS id
  (`bss_rate_provider_sdk::registration::register_rate_provider_plugin`, shared by every
  plugin gear including the core gear itself).
- The core gear's `DiscoveringRateProvider` (built at `init()`, discovery deferred to the
  first `fetch_latest`) lists all `RateProviderSourcePluginSpecV1` instances, **keeps only
  the ones whose `vendor` matches its own configured `source_vendor`**, sorts the survivors
  by `priority` (lower = tried first; a tie is broken by registry list order and logged as
  a warning, never a startup failure), and resolves each via `get_scoped`. A vendor match
  whose scoped client isn't registered is logged and excluded, not fatal.
- **Empty result is a *runtime* error, not a startup failure.** If zero source plugins match
  the configured vendor, `discover()` returns `RateProviderError::Unreachable` from that
  *tick's* `fetch_latest` — it is **not** checked or rejected at any `init()`, because
  discovery itself doesn't run until the first fetch (deliberately, so plugin registration
  order across gears never matters — see O-7 revised). This is a real behavior change from
  the original (pre-plugin-pattern) design, which required a startup-time empty-list check
  — there is no equivalent of "unknown `kind`" or "empty `sources[]`" to validate anymore,
  since there is no `kind` and no list.
- **Self-healing is narrower than "every tick" — only the all-empty case retries.** A
  *failed* discovery (zero matches) is never cached, so the next tick retries. But a
  *successful* discovery — even over only a subset of the intended source plugins, because
  the rest hadn't registered yet — is cached **permanently** in the `OnceCell` for the
  process's lifetime; a source plugin registering after that point is never picked up
  without a restart (§3.8 "Deployment Topology"). Operationally: whichever source plugins
  have registered by the time of the very first `RateSyncJob` tick are the ones that serve
  for the rest of that process's life.
- There is likewise no `fx.provider_order` list to align — cross-gear alignment between the
  core gear, its source plugins, and the ledger is a shared **`vendor` string** match (O-12
  revised), not an ordered-list comparison.

**Module config — one block per gear, not one list:**

```yaml
gears:
  bss-rate-provider:                    # the core/composite gear
    config:
      vendor: "cf.bss"                  # what THIS composite advertises to the ledger
      priority: 100
      source_vendor: "cf.bss"           # which source plugins this gear composes
      id: "bss-rate-provider"           # provider_id() before the first successful discovery

  bss-rate-provider-ecb-plugin:
    config:
      id: "ecb"                         # stable provider_id stamped on synced rows
      vendor: "cf.bss"                  # MUST match the core gear's source_vendor
      priority: 100                     # lower = tried first
      base_url: "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml"
      timeout_ms: 5000

  bss-rate-provider-http-json-plugin:   # ILLUSTRATIVE fallback — not part of v1 (O-10: v1 ships ECB-only)
    config:
      id: "bank-x"
      vendor: "cf.bss"
      priority: 200
      base_url: "${BANK_X_URL}"
      timeout_ms: 5000
      api_key: "${BANK_X_KEY}"          # SecretString — never logged
      auth: bearer                     # none | bearer | header-key
      mapping:
        base: "USD"                    # v1: literal base only, no JSON-path base
        rates: "rates"                 # dotted path, not a JSON-path/JSONPath expression
        rate: "value"
        as_of: "date"
```

**Config fields, by gear (no shared `SourceConfig` type — each plugin's config is its own
struct; the `id`/`vendor`/`priority` shape is a convention every plugin repeats, not a
common base type):**

| Gear | Field | Type | Required | Description |
|------|-------|------|----------|-------------|
| all three | `id` (core: no `provider_id` role — see below) | string | Yes | Core gear: fallback `provider_id()` before first discovery. Plugins: the stable `provider_id` stamped on synced rows. |
| all three | `vendor` (core), `source_vendor` (core, source-selection), `vendor` (plugins) | string | Yes | The matching key across core ↔ plugins ↔ ledger `fx.provider_vendor`. |
| all three | `priority` | i16 | Yes | Fallback order (lower tried first); duplicates across plugins are logged, not rejected. |
| plugins only | `base_url` | string | Yes | Source endpoint; MUST be `https://` (checked at that plugin's `init()`). |
| plugins only | `timeout_ms` | u64 | No (5000) | Outbound per-attempt HTTP timeout. |
| http-json only | `api_key` | `SecretString`, optional | No | `${VAR}` / CredStore expansion upstream; redacted from `Debug`. Required (gear `init()` fails loud) when `auth != none`. |
| http-json only | `auth` | enum `none` \| `bearer` \| `header-key` | No (`none`) | How `api_key` is presented. |
| http-json only | `mapping` | struct, optional | **Yes for http-json** — its `init()` fails loud if absent | `base` (literal only), `rates`/`rate`/`as_of` (dotted paths). |

**Adding a provider:**

- *Simple REST feed* → deploy/configure `bss-rate-provider-http-json-plugin` with a
  `mapping` and a `vendor` matching the core gear's `source_vendor`. **No code.**
- *New family (quirky format/auth)* → implement `RateProviderV1` in a new plugin crate,
  register it the same way (`register_rate_provider_plugin`), give it a matching `vendor`.

#### `CompositeRateProvider`

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-component-composite`

**Not itself the registered `ClientHub` instance** — `DiscoveringRateProvider` (see §1.3) is
what the core gear registers; it builds and caches exactly one `CompositeRateProvider` on
first use. `CompositeRateProvider` wraps the **ordered** `Vec<Arc<dyn RateProviderV1>>`
that discovery produced and does the fallback the ledger cannot (the ledger stamps one
`provider_id` per sync pass). Source-agnostic — it never names a concrete source.
Configuration: none of its own — the try order **is** the priority order `discover()`
established, fixed for the lifetime of the cached instance (a `priority` config change on a
source plugin takes effect on the *next* discovery, i.e. after a restart, not the next tick,
because a *successful* discovery is cached for good in the `OnceCell`).

**State:** `last_served: AtomicUsize` — index of the source that produced the most recent
successful document (default `0` = primary). Interior mutability only; no persistence.

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `fetch_latest` | `ctx`, `pairs`, `request_id` | `Vec<ProviderRate>` | Try sources **in order**; return the **first** that yields `Ok(document)` **whole** (no merge). On success, record its index in `last_served`. If **all** sources fail, return the last `RateProviderError` (the ledger job then raises `FX_SNAPSHOT_MISSING`). |
| `provider_id` | — | `&str` | Return `sources.get(last_served)`'s id — the **real** source that served last (`"ecb"` / `"bank-x"`), so `ledger_fx_rate.provider` and `rate_snapshot.provider` record the true upstream. Uses `.get()`, never a bare index — see the empty-list rule below. |
| `health` | `ctx`, `request_id` | `()` | `Ok(())` if **any** source is healthy (ordered probe). |

**Behavioral rules:**

- **Provenance correctness depends on call order.** `provider_id()` reflects
  `last_served`, which is set during `fetch_latest`. This is correct **because**
  `RateSyncJob` calls `fetch_latest` before `provider_id` in the same pass
  (rate_sync.rs:111 then :149 — verified against the current file). A single non-concurrent
  ticker + `AtomicUsize` makes this race-free. Flagged as a residual coupling (O-7a): if the
  ledger job is ever refactored or made concurrent, revisit — or push a ledger change so
  `ProviderRate` carries its own source id. Assumption is noted in code + tests.
- **Startup default.** Before any successful fetch, `last_served = 0`; `provider_id()`
  returns the primary source's id.
- **Empty-list safety.** The constructor only `debug_assert!`s non-emptiness (a defensive
  invariant check that compiles out in release), so `provider_id()` MUST NOT index the
  vector directly. It uses `sources.get(last_served)`, falling back to the sentinel `"none"`
  (matching the ledger's own `UnconfiguredRateProviderV1` sentinel) if the list is ever
  empty — never a panic. Covered by a dedicated unit test constructing the struct directly
  (bypassing the debug-only guard) to exercise this exact case.

#### `EcbRateProvider` (source plugin `bss-rate-provider-ecb-plugin`)

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-component-ecb-source`

HTTP fetch + XML parse + `rate_micro` conversion + error mapping over the ECB daily feed.
Dependencies: its own `toolkit_http::HttpClient` (built once in this plugin's `init()` via
the shared `bss_rate_provider_sdk::http_client::build_source_http_client` helper — not a
process-wide shared client), its own `EcbPluginConfig` (`id` default `"ecb"`, `vendor`,
`priority`, `base_url` default = the ECB daily feed, `timeout_ms` default `5000`).

**No `format` field exists.** v1 ships **XML-only** (`parse_ecb_xml`) — there is no `format`
config knob and no SDMX parser in the implementation; O-2's "SDMX optional" / "Frankfurter
allowed for dev" were never built. Add a `format` field only if a non-XML feed is actually
needed.

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `provider_id` | — | `&str` | Returns the configured stable id. |
| `fetch_latest` | `ctx`, `pairs: &[CurrencyPair]`, `request_id` | `Vec<ProviderRate>` | GET latest feed → parse → convert. `pairs = &[]` ⇒ return the **whole** published table. Requested pairs the source cannot serve are **omitted** (not an error). Map transport failures to `Unreachable` / `UpstreamStatus`. |
| `health` | `ctx`, `request_id` | `()` | Overrides the trait's default (which would delegate to a full `fetch_latest(&[])`) with a cheap `HEAD` probe against the same feed URL — never re-parses the published table just to check reachability. |

**ECB payload handling:**

- **Direct pairs published by ECB are EUR-based** (EUR→X). A `CurrencyPair` whose
  `base`/`quote` is not directly published is **omitted** — never synthesized (O-3). In
  particular the **inverse leg X→EUR is NOT emitted here** (e.g. `USD→EUR` for a USD
  transaction under an EUR functional currency): deriving it by **deterministic inversion**
  of the stored EUR→X rate is part of the ledger's triangulation (O-3; §4 "Companion
  ledger change").
- **Non-publication days** (weekends / TARGET holidays): return the last published rate
  with its original `as_of` (staleness is the ledger's call).
- **Cadence assumption:** ECB publishes once per TARGET business day ~16:00 CET; on
  non-publication days the last published rate is returned (its `as_of` unchanged, so the
  ledger's staleness rule still applies).

#### `HttpJsonRateProvider` (source plugin `bss-rate-provider-http-json-plugin`)

- [ ] `p2` - **ID**: `cpt-cf-bss-rate-provider-component-http-json-source`

A configurable GET-JSON source so a plain REST rate feed is onboarded by **config alone**.
Covers the common "fetch a JSON document of rates, map fields" shape; NOT for quirky
sources (ECB XML above, or a PSP settlement feed with signed auth — those get their own
plugin crate). Dependencies: its own `toolkit_http::HttpClient` (same shared builder helper
as ECB); its own `HttpJsonPluginConfig` incl. `mapping` + `auth` + `api_key`.

**Configuration (`HttpJsonPluginConfig`, in addition to `id`/`vendor`/`priority`/
`base_url`/`timeout_ms`):**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `mapping.base` | string, **literal only** | — | Base currency literal (e.g. `"USD"`) — v1 has no path-based/multi-base support, only a fixed literal. |
| `mapping.rates` | dotted path (`a.b.c`, object traversal only — not JSONPath) | — | The object of quote→entry pairs. |
| `mapping.rate` | field name within each entry | — | The numeric/string rate field. |
| `mapping.as_of` | dotted path | — | Publication timestamp (RFC 3339); parsed to UTC. One document-level timestamp is applied to every returned rate — there is no per-entry `as_of`. |
| `api_key` | `Option<SecretString>` | `None` | `${VAR}` / CredStore expansion upstream; redacted from `Debug`. `init()` fails loud if `auth != none` and this is absent. |
| `auth` | enum `none` \| `bearer` \| `header-key` | `none` | How `api_key` is presented (`Authorization: Bearer …` / a fixed `X-API-Key` header). |

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `fetch_latest` | `ctx`, `pairs`, `request_id` | `Vec<ProviderRate>` | GET `base_url` (with `auth`) → parse JSON → apply `mapping` → convert each to `ProviderRate`. `pairs = &[]` ⇒ whole document. An entry that fails mapping is skipped (counted), never fabricated; a document where **zero** entries map ⇒ `RateProviderError::Internal` (behavioral rules below). |
| `provider_id` | — | `&str` | The configured `id`. |
| `health` | `ctx`, `request_id` | `()` | **Not overridden** — uses the trait's default, i.e. a full `fetch_latest(&[])`. There is no cheap HEAD/minimal-GET probe for this source (unlike ECB); a health check re-parses the whole document. |

**Behavioral rules:**

- **Base-currency shape.** Many free feeds are single-base. Config states the base; a
  requested pair whose base ≠ the feed base is **omitted**, never synthesized here (O-3).
- **Deterministic mapping.** An unresolvable field ⇒ skip that entry with a counted
  warning; a wholesale parse failure ⇒ `RateProviderError::Internal`. A syntactically
  valid document from which **zero entries map** MUST also return
  `RateProviderError::Internal` — returning `Ok([])` would read as success, suppress the
  composite fallback, and let the ledger mark the sync pass successful without refreshing
  a single rate.
- **Scope (O-11):** v1 = single-base JSON feeds, simple field paths,
  `none` / `bearer` / `header-key` auth; richer transforms (multi-base, JSON-path dialects,
  custom date/number formats) deferred.

#### `rate → rate_micro` conversion

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-component-rate-micro-conversion`

ECB quotes ~5 significant digits. Convert `rate` (decimal) to
`rate_micro = round(rate × 1_000_000)` using **banker's rounding (half-to-even)** to match
the platform ledger rounding default, so a re-fetch of the same published rate yields the
same integer (O-4: accepted; final sign-off with Finance/audit still to be obtained).
The published decimal string MUST be parsed into an **exact decimal representation** —
**never a binary `f64`**, whose nearest-representable value can mis-round exact half-way
decimals under half-to-even. Implemented with `rust_decimal::Decimal`
(`Decimal::from_str_exact` → `checked_mul` by `1_000_000` → `round_dp_with_strategy(0,
MidpointNearestEven)` → `to_i64()`), not an arbitrary-precision `BigDecimal` — `Decimal`'s
fixed 96-bit mantissa is sufficient for FX-rate magnitudes and is the crate already used
elsewhere in this codebase. Overflow / non-finite / non-numeric values MUST map to
`RateProviderError::Internal` (never a silent truncation) — verified down to the exact
`i64::MAX`/`i64::MIN` boundary in the unit tests.

#### Gear wiring

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-component-gear-init`

Three separate `#[toolkit::gear(...)]`-macro gears, each with its own `init()` and its own
workspace/module registration — not one `init()` running a factory:

- **`bss-rate-provider-ecb-plugin`** / **`bss-rate-provider-http-json-plugin`**: build their
  own `toolkit_http::HttpClient`, construct their source, then call the shared
  `bss_rate_provider_sdk::registration::register_rate_provider_plugin` helper to publish a
  `PluginV1<RateProviderSourcePluginSpecV1>` instance + scoped `RateProviderV1` client.
- **`bss-rate-provider`** (core): builds a `DiscoveringRateProvider` (no HTTP client of its
  own — discovery only) and registers it via the *same* shared helper, keyed by
  `RateProviderPluginSpecV1` instead.
- All three share the `deps = [types_registry]` gear-macro edge (the types-registry gear
  must exist as a dependency for the `#[toolkit::gear]` macro's init-ordering, though the
  registration calls happen against `ClientHub` at runtime, not via that edge).

Bank / PSP settlement feed: onboard via the generic `bss-rate-provider-http-json-plugin` if
it is a plain REST feed, else a dedicated new plugin crate for signed/settlement auth
(concrete feed is O-10: v1 = ECB-only; bank/PSP added later as one more deployed plugin
gear, not a `sources[]` entry).

### 3.3 API Contracts

This gear exposes **no external REST surface** and produces **no events**. It is consumed
in-process by the ledger via the `RateProviderV1` trait resolved from `ClientHub`
(GTS `gts.cf.bss.ledger.rate-provider.v1`).

| Surface | Direction | Contract | Notes |
|---------|-----------|----------|-------|
| `RateProviderV1::fetch_latest` | inbound (from ledger) | SDK trait | One round-trip per tick; `&[]` = whole table. |
| `RateProviderV1::health` | inbound (from ledger) | SDK trait | Reachability probe. |
| ECB / bank feed | outbound | HTTPS GET | External provider; see §4 "Security & AuthZ". |

**Relationship to the ledger's manual ingest.** The `RateProviderV1` pull driven by
`RateSyncJob` is the **PRIMARY** rate path. The ledger separately exposes a **SECONDARY**
manual/seed path — `POST /bss-ledger/v1/fx/rates` (ledger-owned, `(ledger, provision)` PEP
gate) — that upserts one rate directly into `ledger_fx_rate`. This gear does **not** own or
replace that endpoint; the two are complementary (automated feed vs manual break-glass /
bootstrap).

**Events.** Provider-outage signalling is the ledger's `RateSyncJob`, which emits
`billing.ledger.invariant.alarm` with `alarmCategory = fx-snapshot-missing` (Critical) when
a **configured** provider fails to fetch. The adapter only returns a `RateProviderError`;
the ledger decides the alarm.

An optional debug/liveness HTTP endpoint is deferred (O-6: metrics only for v1).

### 3.4 Internal Dependencies

- **`bss-ledger-sdk`** — the `RateProviderV1` trait and its types (`rate_provider.rs`); the fixed contract this gear implements.
- **`bss-rate-provider-sdk`** — this gear's own shared internal crate: the source-plugin GTS spec, exact-decimal conversion, HTTP-error mapping, fetch metrics, the shared HTTP-client builder, and the `register_rate_provider_plugin` registration helper used by all four crates.
- **`types-registry-sdk` / `types-registry`** — every plugin gear (including the core gear) registers a `PluginV1` instance here and the core gear queries it (`list_instances`) to discover source plugins; not used in the pre-plugin-pattern design.
- **ToolKit `ClientHub`** — cross-gear registry; each gear `register_scoped`s its `RateProviderV1` under its own GTS instance id (never an unscoped registration).
- **`toolkit-http`** — outbound HTTPS client (hyper + rustls under the hood); each source plugin builds its **own** instance via the shared builder helper — not one client shared by all sources.
- **`secrecy`** — `SecretString` for the http-json plugin's `api_key` (ECB has no credential).
- **Platform OTel meter** — each of the three gears wires its own `OtelFetchMetrics` handle at `init()` from the process-global meter (§4 "Feature metrics"); the same instrument names coalesce across gears.

### 3.5 External Dependencies

- **ECB reference rates** — primary source; free, published once per TARGET business day (`eurofxref-daily.xml`).
- **Bank / PSP feed** — fallback / settlement evidence; deferred to ops (O-10), onboarded via config when available.
- **Billing Ledger (`bss-ledger`)** — the sole consumer: `RateSyncJob` pulls `fetch_latest` and stamps rows with `provider_id`; `RateSource` and the FX stores consume the synced rates ([`../../ledger/docs/design/06-fx-multicurrency.md`](../../ledger/docs/design/06-fx-multicurrency.md)).

### 3.6 Interactions & Sequences

#### Sync tick → fetch → stamp

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-seq-sync-tick-fetch`

Once per tick the ledger `RateSyncJob` resolves its registered `RateProviderV1` (a scoped
`ClientHub` lookup matched by vendor against the core gear's `PluginV1<RateProviderPluginSpecV1>`
instance — the exact ledger-side call is out of this gear's boundary and not re-verified
here), calls `fetch_latest(ctx, &[], request_id)` (whole table), then reads `provider_id()`
for the row stamp and upserts into `ledger_fx_rate`. **Within this gear**, that first
`fetch_latest` call is also the trigger for `DiscoveringRateProvider`'s lazy, once-only
discovery of its own source plugins (§3.2) — so the very first tick after a fresh deploy
does strictly more work (a types-registry list + N scoped resolves) than every tick after.
The caller context is `SecurityContext::anonymous()` (system context) — no PEP gate on this
internal cross-gear plugin call.

#### Source fallback with provenance

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-seq-source-fallback`

Primary source fails (`Err` for the whole fetch) → the composite tries the next source in
priority order → the first `Ok(document)` is returned **whole** and its index recorded in
`last_served` → the subsequent `provider_id()` reports the serving source's real id, so the
synced rows record the true upstream.

#### All sources fail → ledger alarm

- [ ] `p1` - **ID**: `cpt-cf-bss-rate-provider-seq-all-sources-fail`

Every source returns `Err` → the composite returns the last `RateProviderError` → the
ledger job raises `FX_SNAPSHOT_MISSING` (`billing.ledger.invariant.alarm`,
`alarmCategory = fx-snapshot-missing`, Critical). The local store keeps its last synced
rates; staleness marking and post blocking are the ledger's call.

### 3.7 Database schemas & tables

**None.** The adapter is stateless — no tables, no migrations. The persisted FX state is
owned by the ledger:

- `ledger_fx_rate` — the local "latest known rates" store (`RateSyncJob` upsert target).
- `rate_snapshot` — the immutable per-lock frozen rate.

Both are defined in the ledger FX / Foundation designs
([`../../ledger/docs/design/06-fx-multicurrency.md`](../../ledger/docs/design/06-fx-multicurrency.md),
[`../../ledger/docs/design/01-repository-foundation.md`](../../ledger/docs/design/01-repository-foundation.md))
and are NOT part of this gear.

### 3.8 Deployment Topology

Three stateless gear crates at `gears/bss/rate-provider/{rate-provider,plugins/ecb-plugin,
plugins/http-json-plugin}` (placement per O-8; ECB's default `provider_id = "ecb"`),
deployed in-process with the platform gear set — no standalone service, no DB. **Startup
ordering (O-7, as implemented, superseding the original resolution below):** there is no
`deps` edge between the core gear and its source plugins, and none is planned. The core
gear's discovery is deferred to the first `fetch_latest` (the ledger's serve phase, after
every gear's `init()` has run), so registration order among the three rate-provider gears
never matters *for whether the composite can be built at all*. See §3.2's "Self-healing is
narrower than every tick" for the operationally-important caveat: a discovery that succeeds
over only a subset of the intended source plugins is cached for the process's lifetime, not
retried — a slower self-heal than the original design's framing implied.

## 4. Additional context

### Security & AuthZ

- **Caller context:** the ledger calls `fetch_latest` with `SecurityContext::anonymous()`
  (system context, not a per-request user). No PEP gate on this trait — it is an internal
  cross-gear plugin call, not a tenant-scoped resource.
- **No tenant data:** rates are global reference data; the adapter never sees tenant PII
  and never writes tenant-scoped rows (the ledger does the RLS-scoped fan-out).
- **Outbound TLS:** HTTPS via `toolkit-http` (hyper + rustls). ECB is public/unauthenticated;
  paid providers need an API key (see constraint `cpt-cf-bss-rate-provider-constraint-secrets`).
- **Outbound URL validation:** every `base_url` MUST be `https://`, checked at that plugin's
  own `init()` (`bss_rate_provider_sdk::http_client::build_source_http_client`, `anyhow::ensure!`
  on the scheme prefix) — fails loud on plain `http`. **There is no loopback / private-network
  address check on `base_url` itself** — that specific control is not implemented; a
  misconfigured `base_url` pointed at an internal host is only caught by network reachability
  at fetch time, not rejected up front. (An earlier draft of this document claimed such a
  check existed "unless explicitly allow-listed for dev" — it does not, and there is no
  allow-list config either.)
- **Redirect safety (this part IS implemented, inherited, not gear-specific code):** the
  shared `toolkit-http` client's *default* `RedirectConfig` (`same_origin_only: true`,
  `strip_sensitive_headers: true`, `allow_https_downgrade: false`) is what every source
  plugin gets by not overriding it — cross-host redirects are refused unless allow-listed,
  `Authorization`/`Cookie` are stripped on any redirect that does cross an origin, and an
  HTTPS→HTTP downgrade is blocked. Config is operator-supplied (not tenant input), so this
  is defense-in-depth against misconfiguration or a compromised/malicious upstream, not a
  tenant-facing SSRF surface.
- **Provider authenticity:** trusting the provider feed's authenticity is upstream/ops.

### Feature metrics

All metrics exposed as Prometheus scrape targets. (Provider **fallback** selection is
measured ledger-side as `ledger_fx_provider_fallback_total{provider}`, emitted at lock time
by `RateSource`; the adapter measures the fetch itself.)

| Vector | Metric | Description | Target Threshold |
|--------|--------|-------------|------------------|
| **Efficiency** | `fx_provider_fetch_duration_seconds{provider}` | Outbound fetch+parse latency | p95 ≤ 2 s |
| **Performance** | `fx_provider_rates_returned{provider}` | Pairs returned per successful fetch | — |
| **Reliability** | `fx_provider_fetch_errors_total{provider,kind}` | Fetch failures by `RateProviderError` kind | — |
| **Reliability** | `fx_provider_last_success_timestamp{provider}` | Unix time of last successful fetch (feed-freshness gauge) | — |
| **Security** | `fx_provider_upstream_status_total{provider,status}` | Upstream HTTP status distribution | — |

**Instrumentation ownership.** These are the **adapter's own** instruments (the fetch
happens inside the source, out of the ledger's sight), so this gear MUST own a metrics
handle — it does not piggy-back on the ledger's meter. Wire it at `init()` from the
platform OTel meter; each source records under its own `provider_id` label. The
`{provider}` label is the source `provider_id` (`"ecb"` / `"bank-x"`); for the composite,
the source that actually served.

### Testing architecture

**Correction vs. the original draft below:** there is no `FakeHttpTransport` mock-transport
abstraction anywhere in the implementation. Unit tests call the pure parsing/mapping/
conversion functions directly with byte/JSON fixtures (no HTTP layer involved at all);
HTTP-layer behavior is covered by integration tests against a *real* in-process `axum`
server over loopback, not a fake transport trait. There is also a whole test category the
original draft didn't anticipate — discovery/vendor-filtering/priority-ordering/caching —
covered by a `ClientHub` + mock-registry integration test, not a unit test, since it
exercises real cross-gear wiring.

| Level | Database | Network | What is real | What is mocked |
|---|---|---|---|---|
| **Unit** | None | None | Parser (`parse_ecb_xml`), field mapping (`map_json_document`/`json_lookup`), `rate_micro` conversion, HTTP-error mapping, `provider_id` | Nothing — pure functions called directly with fixture bytes/JSON, no transport abstraction |
| **Component integration** (`tests/discovery.rs`) | None | None | A real `ClientHub` + `DiscoveringRateProvider`; fake `RateProviderV1` sources registered scoped | `TypesRegistryClient` — `types-registry-sdk::testing::MockTypesRegistryClient`, a real test-util the SDK ships |
| **HTTP integration** (`tests/ecb_integration.rs`, `tests/http_json_integration.rs`) | None | Real in-process `axum` server over loopback | `EcbRateProvider` / `HttpJsonRateProvider` end-to-end, incl. auth headers, timeouts, connection failure | The real external ECB / bank endpoint |
| **API** | N/A | In-process | Trait-level: `fetch_latest`/`health` contract behavior | — (no REST surface) |
| **E2E** | Real (ledger) | Real HTTPS | Adapter registered in ClientHub; ledger `RateSyncJob` populates `ledger_fx_rate`; an EUR-invoice-under-USD post locks a rate | Optionally the live ECB feed (gated) |

**Unit tests** (`*_tests.rs` files next to the code under test, per this repo's
`de1101_tests_in_separate_files` convention):

| What to test | Verification target |
|---|---|
| Parse ECB daily XML fixture → `(NaiveDate, Vec<(String, String)>)` | All EUR pairs decoded; `as_of` = publication date UTC; a duplicate currency or a second distinct date is logged and the first occurrence wins, never an error |
| `rate_micro` conversion determinism | `round(rate×1e6)` half-to-even (`rust_decimal`) over exact decimal parsing (no `f64` path); half-way golden vectors incl. negative; exact `i64::MAX`/`i64::MIN` boundary and one-past-boundary overflow |
| Requested pair not published / case-insensitive pair match / inverse leg (X→EUR) requested | Omitted from result (not an error); a lowercase-cased request still matches; the inverse leg is never synthesized |
| Upstream 5xx / network failure / malformed payload | `UpstreamStatus` / `Unreachable` / `Internal` respectively, via `map_http_error` |
| `InvalidUri` mapping never echoes the raw URL | `Internal` message contains the structured `kind` + `reason`, never the URL (which may carry a spliced-in secret) |
| `provider_id` | Returns configured id |
| Zero-mappable `http-json` document (empty `rates` object, or every entry individually fails to map) | `RateProviderError::Internal` in both cases, never `Ok([])` — the composite fallback must trigger |
| Generic `http-json` mapping → `Vec<ProviderRate>` | `mapping` resolves base/quote/rate/as_of; an unmappable entry is skipped (counted), never fabricated |
| Composite fallback order + provenance; all-sources-fail; **empty source list** | Secondary document returned whole on primary failure; last error returned + `provider_id()` still resolves when all fail; `provider_id()` returns the `"none"` sentinel (not a panic) when the source list is empty |
| Each gear's built instance id parses as a valid GTS id | `assert_registration_builds_valid_gts_id` (shared helper, one `#[test]` per gear) |

**Not yet covered (gap, not by design):** the http-json plugin's own `init()`-time
validation — `mapping` required, `api_key` required when `auth != none` — has no dedicated
unit/integration test; it is only exercised implicitly by manual/E2E deployment.

**Component-integration tests** (`tests/discovery.rs`; real `ClientHub`, no network, no DB):

| What to test | Verification target |
|---|---|
| Composes in priority order, reports true provenance | Lower `priority` served first; `provider_id()` names the real serving source |
| Falls back to the next priority on failure | Fallback source serves; provenance updates |
| Filters out a different-vendor source, even at a lower priority | Vendor mismatch excludes it regardless of priority |
| No matching-vendor source registered | `fetch_latest` errors |
| Discovery is cached across ticks | A second `fetch_latest` does not re-list instances (`OnceCell`) |
| Concurrent first fetches discover only once | Two concurrent first callers are deduped into one discovery pass, not one each |

**HTTP-integration tests** (`tests/ecb_integration.rs`, `tests/http_json_integration.rs`; a
real in-process `axum` server, no DB):

| What to test | Setup | Verification target |
|---|---|---|
| Full fetch over local server | Serve the ECB XML fixture / a JSON feed body | `fetch_latest(&[])` returns the full table/document |
| Whole-table vs specific pairs, incl. out-of-base omission | Serve full feed | A requested pair returns only that pair; an unpublished/wrong-base pair is omitted, never synthesized |
| ECB `health` probe (HEAD) | Serve 200 / 503 | `Ok(())` vs `UpstreamStatus(503)` — verifies the HEAD override, not a full re-fetch |
| Auth headers (`bearer` / `header-key`) | Server 401s unless the exact header is present | Correct header sent; a configured-but-keyless `bearer` sends **no** header and the server 401s (never silently unauthenticated in a way that looks successful) |
| Upstream 5xx / connection refused | Serve 503 / drop the listener after binding | `UpstreamStatus(503)` / `Unreachable` |

**Not yet covered (gap, not by design):** no test drives the request past `timeout_ms` to
verify a slow-but-reachable upstream maps to `Unreachable` rather than hanging — only
outright connection refusal is exercised.

**API tests:** no REST surface — the "contract" tests are the trait-level behaviors covered
at Unit/Integration. If a debug endpoint is ever added (O-6), add RFC 9457 error tests then.

**E2E tests** (planned location: `testing/e2e/modules/bss-ledger/`, extends the FX suite):

| What to test | Marker | Verification target |
|---|---|---|
| Adapter registered → ledger sync populates store | `@pytest.mark.smoke` | After a `RateSyncJob` tick, `GET /fx/rate-snapshots` path is servable; a cross-currency post locks a rate (no `FX_RATE_UNAVAILABLE`) |
| Provider unreachable → post blocks | — | With the adapter down, an EUR-under-USD post returns `FX_RATE_UNAVAILABLE` (fail-safe), and `fx-snapshot-missing` alarm fires |
| Live ECB fetch (gated) | `@pytest.mark.external` | A real ECB fetch returns a non-empty EUR table |

**What must NOT be mocked:**

| Component | Why |
|---|---|
| `rate_micro` conversion | Money precision — must be exact and deterministic against real parsing |
| The `RateProviderV1` contract behavior (`&[]` semantics, omit-on-unavailable) | The ledger job relies on it verbatim |
| Ledger fail-safe (block on empty store) — E2E | Proves "block, not guess" end to end |

**NFR verification mapping:**

| NFR | Test level | How verified |
|---|---|---|
| Post-path isolation | E2E | Provider down → posts still fast; only FX posts block |
| Fetch latency p95 ≤ 2 s | Integration + load | Timed fetch against local server; sample live ECB |
| Deterministic conversion | Unit | Golden-vector tests over the conversion function |
| Feed freshness | E2E | Sync tick populates store within the tick window |

### Decision register

| Ref | Item | Resolution | Owner |
|-----|------|------------|-------|
| **O-1** | Multiple providers vs single `dyn RateProviderV1` | ✅ **DECIDED — composite adapter, no merge.** ONE `CompositeRateProvider` registered; ordered sources; first whole document; provenance via last-served index (§3.2). Variant (b) — a ledger-side scoped multi-provider loop — stays a future option if per-pair fallback is ever needed. Residual coupling → O-7a. **REVISED 2026-07-23 (plugin rework): each source is a scoped `PluginV1` and the composite is itself a discovered plugin — see the Implementation-revision note at the top.** | Architecture |
| **O-2** | ECB source & format | ✅ **Accepted (2026-07-08):** direct ECB daily XML for prod; Frankfurter allowed for dev; SDMX optional. **As implemented:** XML-only — no `format` config field, no Frankfurter/SDMX code path shipped. Add if a non-XML feed is ever actually needed. | PM + Architecture |
| **O-3** | Triangulation ownership | ✅ **DECIDED (2026-07-08) — the ledger owns triangulation.** The adapter emits only native direct pairs; cross-base rates are computed ledger-side in `RateSource`. Companion ledger change required (below). | Architecture |
| **O-4** | Conversion rounding mode | ✅ **Accepted (2026-07-08):** banker's rounding (half-to-even), matching the ledger default; final sign-off with Finance/audit still to be obtained. | PM + Finance |
| **O-5** | `rate_micro` precision sufficiency | ✅ **Accepted (2026-07-08):** keep ×1e6 (6 dp) for v1; revisit for high-unit / crypto pairs (any change is an SDK change). | Architecture |
| **O-6** | Debug/observability endpoint | ✅ **Accepted (2026-07-08):** metrics only for v1 — no debug HTTP endpoint; ops rely on metrics + the trait `health`. | Team |
| **O-7** | Gear vs plugin & startup order | ✅ **Accepted (2026-07-08):** rely on the fail-safe + next tick; verify startup ordering during implementation (add a ledger `deps` edge if ordering proves unreliable). **REVISED 2026-07-23 (plugin rework): no `deps` edge — the ledger discovers the composite lazily each rate-sync tick, so a late adapter self-heals.** | Architecture |
| **O-7a** | Composite provenance coupling (from O-1) | ✅ **Accepted for v1;** assumption noted in code + tests. `provider_id()` reflects the last-served source, correct only while `RateSyncJob` calls `fetch_latest` before `provider_id` in one pass (true today: rate_sync.rs:111 then :149, single ticker). If the job is refactored or made concurrent, revisit — or push a ledger change so `ProviderRate` carries its own source id. | Architecture |
| **O-8** | Crate placement & naming | ✅ **Accepted (2026-07-08):** `gears/bss/rate-provider`, `provider_id = "ecb"` (confirm against gear conventions at implementation). | Team |
| **O-9** | Jira / slice linkage | ✅ **Accepted (2026-07-08):** create a Technical task under the Slice-5 FX epic (VHP-1853 / VHP-1986 family), linked to the O-3 companion ledger ticket — action pending. | PM |
| **O-10** | Bank / PSP fallback source | ✅ **Accepted (2026-07-08):** v1 = ECB-only; bank/PSP added later as a `sources[]` entry (generic `http-json` if a plain REST feed, else a dedicated `kind` for signed/settlement auth). **As implemented (plugin rework):** added later as one more deployed plugin gear configured with a matching `vendor` (the existing `bss-rate-provider-http-json-plugin` if a plain REST feed, else a new plugin crate for signed/settlement auth). Concrete feed + credentials deferred to ops. | PM + Ops |
| **O-11** | Generic `http-json` mapping grammar | ✅ **Accepted (2026-07-08):** v1 = single-base JSON feeds, simple field paths, `none` / `bearer` / `header-key` auth; richer transforms deferred. | Architecture |
| **O-12** | `init()` config-validation strictness | ✅ **DECIDED (2026-07-17):** fail `init()` loud on an unknown `kind`, an empty `sources[]`, or a `sources[]` order that does not match the ledger `fx.provider_order` — a mismatch would let the composite fetch one provider while the ledger's precedence resolution prefers another's stored rate. **REVISED 2026-07-23 (plugin rework): no `sources[]` — source assembly is plugin discovery ordered by each plugin's `priority`; cross-gear alignment is a matching `vendor` (core gear ↔ source plugins ↔ ledger `fx.provider_vendor`).** | Architecture |

### Companion ledger change (hard dependency, from O-3)

O-3 puts triangulation in the ledger, so this gear ships **direct pairs only**. Enabling
the ledger's deferred triangulation is therefore a **hard dependency**, tracked as a
separate `bss-ledger` work item — NOT part of this gear:

- **Where:** `bss-ledger` `infra/fx/rate_source.rs` — today `resolve()` reads direct pairs
  only (a documented TODO); it MUST compute `X → EUR → Y` (via the configured bridge
  currency) when no direct pair exists. This **includes deriving the `X → EUR` leg by
  deterministically inverting** the stored `EUR → X` rate — the adapter emits only ECB's
  native EUR-based pairs, so without ledger-side inversion no non-EUR-base pair (e.g.
  `USD→EUR`) can resolve at all.
- **Snapshot:** the resulting `rate_snapshot` MUST record `triangulated_via` (the bridge
  currency) — the column already exists on `ledger_fx_rate_snapshot`.
- **Determinism:** the bridge path + rounding MUST be deterministic and
  auditor-reproducible (banker's rounding per O-4).
- **Sequencing:** this adapter can ship first — EUR-functional / EUR-base tenants already
  work with direct pairs. Non-EUR-functional tenants are unblocked only once the ledger
  triangulation lands. Track the two as linked tickets (O-9).

## 5. Traceability

- **PRD (this gear)**: [`PRD.md`](./PRD.md) — the adapter's own product requirements
  (`cpt-cf-bss-rate-provider-fr-*` / `-nfr-*`), derived from the ledger PRD below.
- **Upstream PRD**: [`../../ledger/docs/PRD.md`](../../ledger/docs/PRD.md) — § Multi-currency and
  foreign exchange, § FX rate-source failure and staleness
  (`cpt-cf-bss-ledger-fr-multi-currency-fx`, `cpt-cf-bss-ledger-fr-fx-rate-source-failure`).
- **Consuming design**:
  [`../../ledger/docs/design/06-fx-multicurrency.md`](../../ledger/docs/design/06-fx-multicurrency.md)
  (the ledger side: `RateSource`, staleness, snapshots, the rate-source-fallback
  algorithm, and the frozen rate-snapshot state) and
  [`../../ledger/docs/design/01-repository-foundation.md`](../../ledger/docs/design/01-repository-foundation.md)
  (functional columns, currency-scale registry).
- **Code seam (existing)**: `bss-ledger-sdk` `rate_provider.rs` (`RateProviderV1` trait);
  `bss-ledger` `infra/jobs/rate_sync.rs`, `infra/fx/rate_source.rs`, `config.rs`
  (`FxConfig`), `module.rs` (ClientHub resolution).
- **Provenance**: authored from the architecture-repo draft
  `DESIGN-billing-fx-module-202607011613` (vhp-architecture, `docs/bss/design/`), which
  itself traces to `PRD-billing-ledger-balances-202604041200`,
  `DESIGN-billing-ledger-balances-202606091200` (slices 01 / 06), and
  `ADR-platform-persistence-layer-202601221200`.
