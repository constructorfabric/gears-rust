# Gear Maturity Model

Every gear declares one maturity level. The level tells consumers what they can rely on, and it
tells contributors and reviewers which rules apply to a change.

- The source of truth is the `maturity` field in `gear.gdl`.
- The state of the gear's crates must match it (§6). CI checks this.
- A gear moves up only through the transition process in §3. It never moves down.
- A `stable` gear can still ship features that are not stable yet. §4 defines how.

[CONTRIBUTING.md](../CONTRIBUTING.md) defines the pull request and versioning rules for each
level. [DESIGN_REVIEW.md](DESIGN_REVIEW.md) §3 defines how review severity depends on the level.

---

## 1. Levels

| `maturity`    | Crates                                | Promise to consumers                                                 |
|---------------|---------------------------------------|----------------------------------------------------------------------|
| `unpublished` | not published (`publish = false`)    | None. Usable only through a git dependency.                          |
| `preview`     | published, version `0.x`              | Usable. Breaking changes only in a `0.(x+1)` release, with a [migration note](../CONTRIBUTING.md#migration-notes). |
| `stable`      | published, version `>= 1.0`           | SemVer and the deprecation policy of [CONTRIBUTING.md §3](../CONTRIBUTING.md#3-versioning). |
| `deprecated`  | published, any version                | The promise of the level it had before. Names its replacement.       |

Special cases:

- A gear with `implementation = "design"` (no crate yet) is reviewed as `unpublished`.
- A `deprecated` gear is reviewed by its crate version: `0.x` as `preview`, `>= 1.0` as `stable`.
- ToolKit libraries (`libs/**`) have no `gear.gdl` and are always published. `0.x` is reviewed as
  `preview`, `>= 1.0` as `stable`. Their blast radius is the whole platform: severity adjustment
  `+1` ([DESIGN_REVIEW.md §3](DESIGN_REVIEW.md#3-severity)).
- A plugin has its own `gear.gdl` and its own level. It cannot be more mature than the host SDK
  contract it implements.
- A `stable` gear must not require a `preview` or `unpublished` gear on its default path. It may
  use one only through an optional plugin or a gated feature (§4), or with an accepted ADR.

The `implementation` field (`design`, `in-progress`, `done`) is independent of the level. It shows
how much of the specification is built; the level shows how proven the built part is.

---

## 2. Obligations

| Area | `unpublished` | `preview` | `stable` |
|---|---|---|---|
| Rust SDK API | No compatibility requirement. Any PR may break the API without a version bump or migration note; the same PR updates every caller in this repository. | Breaking change = `0.(x+1)` + `feat!` + [migration note](../CONTRIBUTING.md#migration-notes). | Breaking change = next major (`2.0`). A previous `1.x` minor marks the old item `#[deprecated]` and adds its replacement. Migration note required. |
| Wire contracts (REST, gRPC, event, GTS, MCP, config) | No compatibility requirement between commits: environments are redeployed or reset, not rolled. GTS types use major `v0`. | Compatible with the previous release (rolling deploy). GTS types use major `v1` or higher; a breaking change is the next major, served next to the old one. | Additive only. Breaking change = new path or type major; the old one keeps working ([CONTRIBUTING.md §3](../CONTRIBUTING.md#3-versioning)). |
| Persisted data | Migrations may be rewritten while no environment keeps data. | Forward-only. The previous release runs on the new schema. | Same, plus expand/contract and a tested rollback to the previous release. |
| Specifications | Short PRD (p1) and a DESIGN for contracts, data, and tenancy. | PRD and DESIGN match the code. The maturity card lists the gaps. | Full PRD, DESIGN, and ADRs. Contract and data changes are designed and reviewed before implementation. |
| Unstable features (§4) | Not applicable: no part of the gear has a promise. | Optional. A new feature may ship as `alpha` or `beta` behind its compile-time and runtime gates (§4.1). | Required for every new feature that is not proven: it ships as `alpha` or `beta` behind its gates (§4.1), never as a change to the stable surface. |
| Tests | No minimum. | Unit coverage >= 80%. Every p1 use case has an integration or e2e test. | Unit coverage >= 90%. Every p1 and p2 use case has an integration or e2e test. |
| Security, tenant isolation, secrets | Full rules. | Full rules. | Full rules. |

**Why wire contracts need compatibility earlier than the Rust API.** During every rolling deploy,
old and new instances run side by side and read the same database and topics. Mixed versions hit
wire contracts and persisted data even when no consumer upgraded. Rust consumers choose when to
upgrade a crate.

**"Previous release"** means the previous published version of the gear. A gear supports a skew
of one release in both directions: version N reads data and messages written by N-1, and N-1
keeps working on the schema migrated by N.

Pull request approvals per level are in [CONTRIBUTING.md §2.10](../CONTRIBUTING.md#210-review-process).

**Coverage** is line coverage of the gear's crates from `make coverage GEAR=<gear>`. Generated
code and test helpers are excluded. A pull request must not lower the coverage of a gear below
its level's minimum.

---

## 3. Transitions

There is no way back. A published crate is not unpublished (`cargo yank` only for a security
issue), and `1.x` never returns to `0.x`. A gear that needs a redesign after `1.0` ships it as the
next major version (§4.5).

A **contributor group** is one organization or team that has maintainers in
[MAINTAINERS.md](../MAINTAINERS.md). "Two other groups" means two groups that do not include the
gear owner.

### `unpublished`

- Every new gear starts here, with `publish = false` on every crate.
- Maintainers from two other groups review the PRD, DESIGN, and SDK at a high level: gear
  boundaries, key features, p1 scope, and the owners of the capabilities it reuses.
- Any change may break earlier behavior. Specifications and code may diverge.
- Code review by maintainers. No minimum for tests.

### `unpublished` -> `preview`

A pull request sets `maturity = "preview"`, removes `publish = false`, and sets the version to
`0.1.0`. It is a formal acceptance:

1. **Specifications.** PRD and DESIGN are final for the current scope and consistent with the
   code. The gear owner and maintainers from two other groups approve them.
2. **Contracts.** Public types live in the SDK crate: no database entities, Axum types, or
   internal enums. Contracts are versioned, tolerate unknown fields and enum variants
   (`#[non_exhaustive]`, `#[serde(other)]`), and extension points use GTS
   ([DESIGN_REVIEW.md](DESIGN_REVIEW.md) C4, C7, C8).
3. **Data.** Migrations are final. From now on they are forward-only.
4. **Security.** Security and tenant isolation are reviewed (C5, C12, C14).
5. **Tests.** The `preview` row of §2 is met.
6. **Code.** Maintainers from two other groups inspect the code.
7. **Maturity card.** The README has the card (§5): owner, consumers, known gaps.

### `preview` -> `stable`

A pull request sets `maturity = "stable"` and the version to `1.0.0`. It is a formal acceptance:

1. **Production evidence.** At least one named consumer runs the gear in production. The gear
   owner decides how long and over how many releases, and records the evidence in the promotion
   pull request.
2. **Contract stability.** No `feat!` or `BREAKING CHANGE` during that period.
3. **Quality.** No known critical or major functional, security, or performance bug. No open
   CRITICAL or HIGH review finding. Each known gap is closed or accepted in an ADR.
4. **Operations.** Limits, timeouts, metrics, alerts, and a tested rollback to the previous release
   are in place.
5. **Specifications.** Full PRD, DESIGN, and ADRs. Every remaining unstable feature is listed in
   the card with its stage (§4).
6. **Tests.** The `stable` row of §2 is met.
7. **Approval.** The gear owner and two maintainers from other groups, one of them an architect.

### `* -> deprecated`

A pull request sets `maturity = "deprecated"`, names the replacement and the migration path in the
card, and marks the SDK entry points `#[deprecated(note = "...")]` in a minor release. Contracts
keep working as before. Removal follows the deprecation policy of the level the gear had.

---

## 4. Unstable features in a `stable` gear

A `stable` gear grows. A new feature is not proven on the day it merges, but the gear's stable
promise must hold for everyone who does not use that feature. This section separates the two.
`preview` gears may use the same mechanisms to keep their default surface small.

### 4.1 Feature stages

| Stage | Built by default | Enabled by default | Promise | Production use |
|---|---|---|---|---|
| `alpha` | No: the gear's Cargo feature `unstable-<feature>` is not in `default` | No | None. May change or disappear in any release. Its data may be dropped. | No |
| `beta` | Yes: the gear's `default` features include `unstable-<feature>` | No: runtime opt-in | Breaking change only in a minor release, after one release of notice, with a [migration note](../CONTRIBUTING.md#migration-notes). Its data is migrated, never dropped silently. | Yes, by opted-in consumers |
| `ga` | Yes, no Cargo feature | Yes, no flag | The gear's stable promise. | Yes |

A feature starts at `alpha` or `beta`. It moves to `ga` (§4.4) or is removed. Small, low-risk
additive changes that pass a stable-level review may go to `ga` directly.

**Gated** means the feature's code does nothing unless someone turns it on. Every `alpha` and
`beta` feature has two gates:

1. **Compile-time gate**: the Cargo feature `unstable-<feature>`, declared on the gear crate and on
   its SDK crate. Unstable SDK items exist only with this feature, so a Rust consumer opts in
   explicitly. It is never in the SDK crate's `default` features. In the gear crate it is in
   `default` for `beta` (prebuilt servers contain the code) and not for `alpha`.
2. **Runtime gate**: the configuration flag `features.<feature>.enabled` in the gear's `config`
   section, default `false`. While it is `false`, the gear registers no route, ClientHub client,
   MCP tool, GTS type, event consumer, or background job of the feature.

Example for a `beta` feature `bulk-import` in the `file-storage` gear:

```toml
# gears/file-storage/file-storage-sdk/Cargo.toml
[features]
unstable-bulk-import = []

# gears/file-storage/file-storage/Cargo.toml
[features]
default = ["unstable-bulk-import"]          # beta: built by default; remove for alpha
unstable-bulk-import = ["file-storage-sdk/unstable-bulk-import"]
```

```yaml
# deployment configuration
gears:
  file-storage:
    config:
      features:
        bulk_import:
          enabled: true                     # default: false
```

```rust
// gear init: register the feature only when both gates are open
#[cfg(feature = "unstable-bulk-import")]
if cfg.features.bulk_import.enabled {
    bulk_import::register_routes(&mut router, &deps)?;   // /file-storage/v1beta/...
    hub.register::<dyn BulkImportUnstableApi>(bulk_import::client(&deps));
}
```

### 4.2 Isolation rules

These rules keep an unstable feature from weakening the stable gear. A violation is at least
HIGH in review (DESIGN_REVIEW.md G1, G2).

1. **Off means off.** With the feature disabled, the gear's stable contracts (SDK with default
   features, the `/v{N}` REST surface, events, GTS types, configuration), its behavior, and its
   performance are the same as without the feature. CI runs the gear's tests with default
   features and the flag off.
2. **No stable dependency on unstable code.** A `ga` path never calls into an `alpha` or `beta`
   feature. The feature depends on the stable core, not the reverse.
3. **Migrations are never gated.** Schema migrations run unconditionally, so every deployment has
   the same linear schema history. A feature's schema changes are additive: new tables, or
   nullable or defaulted columns on stable tables.
4. **Security is never gated.** Authentication, authorization, tenant isolation, and secret
   handling follow the full rules from the first `alpha` commit. A disabled or missing flag denies
   access; it never widens it.
5. **One runtime flag per feature** (§4.1). A tenant-level switch, when needed, is a setting or a
   license feature on top of the deployment flag, never instead of it.
6. **Visible usage.** The gear emits a metric per enabled unstable feature, and logs one warning
   at startup for each enabled `alpha` feature.

### 4.3 How each contract type carries an unstable feature

| Contract | Mechanism |
|---|---|
| Rust SDK | Items behind the compile-time gate `unstable-<feature>` in an `unstable` module, with `#[cfg_attr(docsrs, doc(cfg(...)))]`. New operations go on a separate trait (for example `FooUnstableApi`) that the gear registers in ClientHub only when the flag is on. Never add a method to a stable trait for an unstable feature. New variants only on `#[non_exhaustive]` enums. Semver checks run without `unstable-*` features; these items are exempt from SemVer. |
| REST | Unstable operations and fields are served only under the beta channel path `/{gear}/v{N}beta/...`, which is a superset of `/v{N}`. The `/v{N}` paths and their OpenAPI document do not change. Operations carry `x-cf-stage: alpha|beta` in OpenAPI. With the flag off, beta routes are not registered (`404`). Prerequisite: lint DE0801 in `cargo-gears` accepts `v{N}beta` (today it accepts only `v` followed by digits). |
| gRPC | A separate service or package with a `v{N}beta` suffix. Do not add unstable fields to stable messages. |
| GTS types and events | A new type, registered only when the flag is on. `alpha`: major `v0`. `beta`: major `v1` or higher, compatible within the major, and it keeps its identifier at graduation. Never add unstable fields to a stable type; use a new or derived type. Consumers opt in by subscribing to the new type. See §4.6. |
| MCP tools | A separate tool, registered only when the flag is on. Its version follows the GTS row. |
| Configuration | Keys under `features.<feature>`. Unknown keys under `features` are ignored with a warning, so removing a feature does not break existing configuration files. |
| Persisted data | Additive schema only (§4.2 rule 3). `beta` data is migrated forward like stable data. `alpha` data may be dropped only when the card says so and the release notes announce it. |

### 4.4 Lifecycle of an unstable feature

1. **Introduce.** The pull request adds the feature to the card (§5) with its stage, flag, owner,
   issue, and target release for `ga`. A `beta` feature that adds or changes a public contract
   or persisted data needs a DESIGN section or ADR, reviewed before the implementation is
   accepted (the gear is `stable`, so its flow is design-first).
2. **Change.** A change inside the feature is reviewed with the `preview` column of the severity
   table; a change to the gating, the isolation rules, or any stable surface is reviewed with the
   `stable` column (DESIGN_REVIEW.md §3).
3. **Graduate to `ga`.** In a minor release: the contract moves to the stable surface (`/v{N}`,
   default SDK features; a `beta` GTS type keeps its identifier), the flag defaults to `true`, and
   the PR records the production evidence (a named consumer, no breaking change during the period
   the gear owner chose). The beta path and
   the `unstable-<feature>` Cargo feature stay one more minor release as deprecated aliases, then
   are removed. The flag is removed one release later.
4. **Time limit.** A feature reaches `ga` or is removed within 4 minor releases or 6 months after
   it reaches `beta`, whichever is longer. Extending the limit needs an ADR. There is no permanent
   beta.
5. **Remove.** A `beta` feature is announced as deprecated in one minor release (release notes,
   `Deprecation` header on its routes, `#[deprecated]` on its SDK items) and removed in a later
   one. Its data follows the `beta` rule in §4.3.

### 4.5 Larger changes to a `stable` gear

- **Internal rewrite** (same contracts). Use branch by abstraction: put the new implementation
  behind an internal trait and a runtime flag, compare it with the old one in tests and, when
  possible, by running both on real traffic (shadow or dual-run), then switch the default and
  remove the old path. Do not keep a long-lived branch.
- **New major version.** Build the next contract (`/v{N+1}beta`, a new SDK trait, `v{N+1}` GTS
  types) as a `beta` feature inside the `N.x` line. When it graduates, release `{N+1}.0` with
  both versions served. The previous version keeps working under the deprecation policy of
  [CONTRIBUTING.md §3](../CONTRIBUTING.md#3-versioning).
- **Plan majors.** Each major release costs every consumer a migration. Batch breaking changes and
  avoid more than one major release of a gear's SDK in six months.

### 4.6 GTS major `v0`

The [GTS specification](https://github.com/GlobalTypeSystem/gts-spec) allows major `0` but gives it
no special meaning: every major, `v0` included, must stay compatible across its minor versions and
revisions, and Types Registry enforces this. GTS IDs are also stored as deterministic UUIDs, so
renaming a type changes every stored reference to it. The platform convention is therefore:

- **`v0`** only where nothing is persisted that must survive: `unpublished` gears and `alpha`
  features. A breaking change to a `v0` type resets the Types Registry and data of the
  environments that registered it; there are no consumers to protect.
- **`v1` and higher** for `preview` and `stable` gears and for `beta` features. A breaking change
  is the next major, served next to the old one.
- **Renaming `v0` to `v1`** happens once: in the pull request that promotes the gear to `preview`,
  or the feature to `beta`. The migration note lists the renamed identifiers.

---

## 5. Maturity card

A `## Maturity` section at the top of the gear's `README.md`. Delete empty fields; do not write
"none".

```markdown
## Maturity

- **Level**: `preview` (see guidelines/MATURITY.md)
- **Owner**: <GitHub team or user from CODEOWNERS>
- **Consumers**: <gears, products, vendor plugins, deployments that use it>
- **In scope now**: <the use cases this level covers>
- **Known gaps**: <temporary gaps, each with an issue link>
- **Next level**: <what is left to reach the next level, each with an issue link>

| Feature | Stage | Flag / Cargo feature | Since | Target `ga` | Issue |
|---|---|---|---|---|---|
| bulk-import | beta | `features.bulk_import` | 1.3.0 | 1.6.0 | #1234 |
```

The feature table lists only `alpha` and `beta` features. A level change updates `gear.gdl`, the
crate versions, and the card in the same pull request.

---

## 6. Consistency checks (CI)

- `unpublished` => every crate of the gear has `publish = false`.
- `preview` => published, SDK version `0.x`. `stable` => published, SDK version `>= 1.0`.
- All published crates of a gear share the major version of its SDK crate.
- A new gear starts as `unpublished`.
- A `stable` gear builds and passes its tests with default features and with
  `--features unstable-*` (all its unstable features).
- Semver checks of a `stable` crate run without `unstable-*` features.
- A `/v{N}beta` route or `unstable-*` Cargo feature in a gear has a matching row in the card.
- No SDK crate lists an `unstable-*` feature in `default`; a gear crate lists only its `beta` features there.
- An `unpublished` gear registers only `v0` GTS types; other gears register `v0` types only for `alpha` features.

---

## 7. Adopting this model

Existing gears map to the new levels as follows, before CI enforces §6:

| Today | Crate state | New level |
|---|---|---|
| `experimental` | `publish = false` | `unpublished` |
| `experimental` | published `0.x` | `preview` (a published crate cannot be unpublished) |
| `preview` | published `0.x` | `preview` |
| `implementation = "design"` | no crate | reviewed as `unpublished` |

`experimental` is renamed to `unpublished`. Prerequisite: Gearbox accepts `unpublished` as a
`maturity` value. The pull request that renames the value in every `gear.gdl` also turns on the
CI checks of §6.

---

## 8. Open questions

- A limit on breaking `0.(x+1)` releases per release train for `preview` gears.

## 9. Prerequisites in other repositories

- **Gearbox**: accept `unpublished` as a `maturity` value.
- **cargo-gears**: DE0801 (`de0801_api_endpoint_version`) accepts the version segment `v{N}beta`
  in addition to `v{N}`.
