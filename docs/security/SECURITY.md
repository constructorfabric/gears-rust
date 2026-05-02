# Security in Cyber Fabric

Cyber Fabric takes a **defense-in-depth** approach to security, combining Rust's compile-time safety guarantees with layered static analysis, runtime enforcement, continuous scanning, and structured development processes. This document summarizes the security measures in place across the project.

---

## Table of Contents

- [1. Rust Language Safety](#1-rust-language-safety)
- [2. Compile-Time Tenant Scoping (Secure ORM)](#2-compile-time-tenant-scoping-secure-orm)
- [3. Authentication & Authorization Architecture](#3-authentication--authorization-architecture)
- [4. Credentials Storage Architecture](#4-credentials-storage-architecture)
- [5. Outbound API Gateway (OAGW)](#5-outbound-api-gateway-oagw)
- [6. Compile-Time Linting — Clippy](#6-compile-time-linting--clippy)
- [7. Compile-Time Linting — Custom Dylint Rules](#7-compile-time-linting--custom-dylint-rules)
- [8. Dependency Security — cargo-deny](#8-dependency-security--cargo-deny)
- [9. Cryptographic Stack & FIPS-140-3](#9-cryptographic-stack--fips-140-3)
- [10. Continuous Fuzzing](#10-continuous-fuzzing)
- [11. Security Scanners in CI](#11-security-scanners-in-ci)
- [12. PR Review Bots](#12-pr-review-bots)
- [13. Specification Templates & SDLC](#13-specification-templates--sdlc)
- [14. Repository Scaffolding — Cyber Fabric CLI](#14-repository-scaffolding--cyber-fabric-cli)
- [15. Opportunities for Improvement](#15-opportunities-for-improvement)

---

## 1. Rust Language Safety

Rust eliminates entire categories of vulnerabilities at compile time:

| Vulnerability Class | How Rust Prevents It |
|---|---|
| Null pointer dereference | No null — `Option<T>` forces explicit handling |
| Use-after-free / double-free | Ownership system with borrow checker |
| Data races | `Send`/`Sync` traits enforced at compile time |
| Buffer overflows | Bounds-checked indexing; slices carry length |
| Uninitialized memory | All variables must be initialized before use |
| Integer overflow | Checked in debug builds; explicit wrapping/saturating in release |

Additional Rust-specific project practices:
- **`#[deny(warnings)]`** — all compiler warnings are treated as errors in CI (`RUSTFLAGS="-D warnings"`)
- **`#[deny(clippy::unwrap_used)]` / `#[deny(clippy::expect_used)]`** — panicking on `None`/`Err` is forbidden in production code
- **No `unsafe` without justification** — Clippy pedantic rules surface unnecessary `unsafe` usage

## 2. Compile-Time Tenant Scoping (Secure ORM)

> Source: [`libs/modkit-db-macros`](../../libs/modkit-db-macros/) · [`guidelines/SECURITY.md`](../../guidelines/SECURITY.md) · [`docs/modkit_unified_system/06_authn_authz_secure_orm.md`](../modkit_unified_system/06_authn_authz_secure_orm.md)

Cyber Fabric provides a **compile-time enforced** secure ORM layer over SeaORM. The `#[derive(Scopable)]` macro ensures every database entity explicitly declares its scoping dimensions:

```rust
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "users")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
}
```

**Key compile-time guarantees:**

- **Explicit scoping required** — every entity must declare all four dimensions (`tenant`, `resource`, `owner`, `type`). Missing declarations cause a compile error.
- **No accidental bypass** — `clippy.toml` configures `disallowed-methods` to block direct `sea_orm::Select::all()`, `::one()`, `::count()`, `UpdateMany::exec()`, and `DeleteMany::exec()`. All queries must go through `SecureSelect`/`SecureUpdateMany`/`SecureDeleteMany`.
- **Deny-by-default** — empty `AccessScope` (no tenant IDs, no resource IDs) produces `WHERE 1=0`, denying all rows.
- **Immutable tenant ownership** — updates cannot change `tenant_id` (enforced in `secure_insert`).
- **No SQL injection** — all queries use SeaORM's parameterized query builder.

## 3. Authentication & Authorization Architecture

> Source: [`docs/arch/authorization/`](../arch/authorization/) · [`modules/system/authn-resolver/`](../../modules/system/authn-resolver/) · [`modules/system/authz-resolver/`](../../modules/system/authz-resolver/) · [`modules/system/tenant-resolver/`](../../modules/system/tenant-resolver/)

Cyber Fabric implements a **PDP/PEP authorization model** per NIST SP 800-162, extended with **OpenID AuthZEN 1.0** constraint semantics (see [ADR-0001](../arch/authorization/ADR/0001-pdp-pep-authorization-model.md)):

```
Client → AuthN Middleware → AuthN Resolver (token validation)
       → Module Handler (PEP) → AuthZ Resolver (PDP, policy evaluation)
       → Constraints compiled to AccessScope
       → Database (query with WHERE clauses from constraints)
```

### SecurityContext

Every authenticated request produces a `SecurityContext`:

```rust
pub struct SecurityContext {
    subject_id: Uuid,
    subject_type: Option<String>,
    subject_tenant_id: Uuid,           // every subject belongs to a tenant
    token_scopes: Vec<String>,         // capability ceiling (["*"] = unrestricted)
    bearer_token: Option<SecretString>, // redacted in Debug, never serialized
}
```

`bearer_token` is stored as `Secret<String>` — redacted in `Debug`/`Display`, never serialized or logged. Introspection caches key by `sha256(token)`, not the raw token.

### AuthN Resolver

> Source: [`OIDC AuthN Plugin DESIGN.md`](../../modules/system/authn-resolver/plugins/oidc-authn-plugin/docs/DESIGN.md) · [ADR-0002](../arch/authorization/ADR/0002-split-authn-authz-resolvers.md) · [ADR-0003](../arch/authorization/ADR/0003-authn-resolver-minimalist-interface.md)

Validates bearer JWTs via OIDC discovery and JWKS, extracts claims, and constructs the `SecurityContext`. AuthN and AuthZ are **split into independent resolver modules** (ADR-0002) with pluggable vendor-specific implementations.

The current **OIDC AuthN plugin** supports:

- **JWT tokens** — local validation via OIDC discovery → JWKS → signature verification (`kid`, `exp`, optional `aud`), with configurable claim mapping to `SecurityContext` fields.
- **Opaque tokens** — out of scope for this plugin; non-JWT bearer tokens are rejected.
- **S2S identity** — `exchange_client_credentials` (OAuth2 client credentials grant) for service-to-service calls, producing the same `SecurityContext` pipeline.
- **Caching** — JWKS cached with refresh and bounded stale serving; S2S tokens cached with TTL bounded by `min(token_exp - now, configured_ttl)`.

### AuthZ Resolver (PDP) — AuthZEN with Constraint Extensions

> Source: [`DESIGN.md`](../arch/authorization/DESIGN.md) · [`AUTHZ_USAGE_SCENARIOS.md`](../arch/authorization/AUTHZ_USAGE_SCENARIOS.md)

Plain AuthZEN point-in-time `true/false` decisions are insufficient for LIST queries with pagination. The design extends AuthZEN with **`context.constraints`** — a predicate DSL so the PEP receives **SQL-friendly filters** in O(1) PDP calls per query instead of per-row evaluation.

**Constraint predicates:**

| Predicate | Purpose |
|---|---|
| `eq(property, value)` | Exact match (e.g., `owner_id`) |
| `in(property, values)` | Set membership |
| `in_tenant_subtree(root_id, barrier_mode, status)` | Hierarchical tenant scoping via `tenant_closure` |
| `in_group(group_ids)` | Resource group membership |
| `in_group_subtree(group_ids)` | Hierarchical group membership |

Constraints **OR** across alternatives, **AND** predicates within each constraint. PEP compiles constraints into `AccessScope` → SecureORM translates to SQL `WHERE` clauses.

**Fail-closed PEP enforcement** — PEP denies access on:
- Missing or `false` decision
- Unreachable PDP
- Missing constraints when `require_constraints: true`
- Unknown predicates or property names
- Empty predicate lists, empty `in`/`group_ids` values, or empty `constraints: []`

**Capability negotiation** — PEP sends `capabilities`, `supported_properties`, and `require_constraints` with each evaluation request. The PDP must not emit unsupported property names and must degrade gracefully (expand groups to explicit `in` lists, or deny).

**Token scopes as capability ceiling** — `effective_access = min(token_scopes, user_permissions)`. First-party apps typically carry `["*"]` (unrestricted).

**Deny contract** — `decision: false` must include a `deny_reason` with a GTS `error_code`. Details are logged for audit but never exposed to clients (generic 403 only, no policy leakage).

**404 vs 403 for point reads** — Constrained queries returning 0 rows yield 404, preventing existence leakage.

**TOCTOU mitigation** — For UPDATE/DELETE, PEP prefetches the target attribute and uses `eq` predicates in the WHERE clause so the mutation is atomic with the authorization check.

### Multi-Tenancy — Tenant Resolver

> Source: [`TENANT_MODEL.md`](../arch/authorization/TENANT_MODEL.md) · [`modules/system/tenant-resolver/`](../../modules/system/tenant-resolver/)

Hierarchical multi-tenancy with a **single-root tree topology** (exactly one tenant with no parent; all others descend from it):

- **Isolation by default** — every resource carries `owner_tenant_id` as the primary partition key; tenants cannot access each other's data.
- **Hierarchical access** — parent tenants may access child data. Subject tenant (home identity) and context tenant (operational scope) are distinguished, enabling scoped admin patterns.
- **Barriers** — child tenants set `self_managed = true` to create a privacy barrier. Parents cannot see that subtree's business data by default; `BarrierMode` controls per-resource-type relaxation (e.g., billing ignores barriers while tasks respect them).
- **`tenant_closure` table** — materialized `(ancestor_id, descendant_id, barrier, descendant_status)` enables efficient `in_tenant_subtree` predicate compilation to SQL subqueries.

**Tenant Resolver** is a plugin-based system module providing tenant graph operations (get, ancestors, descendants, `is_ancestor`) via `TenantResolverClient`:

- **Plugin architecture** — gateway discovers a backend plugin by GTS vendor string; routes all calls through `TenantResolverPluginClient`. Built-in plugins: `static-tr-plugin` (in-memory tree from config), `single-tenant-tr-plugin` (enforces `ctx.subject_tenant_id()` as the only tenant).
- **Barrier-aware traversal** — ancestor/descendant walks respect `self_managed` barriers and use visited sets for cycle safety.
- **Status filtering** — queries filter by `TenantStatus`; filtering a suspended parent excludes its entire subtree.
- **PIP role** — Tenant Resolver serves as a **Policy Information Point (PIP)**: the AuthZ plugin queries it for hierarchy data when building tenant constraints.

### Resource Groups

> Source: [`RESOURCE_GROUP_MODEL.md`](../arch/authorization/RESOURCE_GROUP_MODEL.md)

Optional M:N, tenant-scoped resource grouping that acts as a **PIP** alongside the tenant hierarchy:

- Groups enable attribute-based grouping of resources for authorization (e.g., project groups, organizational units).
- The AuthZ plugin queries group membership/hierarchy when building `in_group` / `in_group_subtree` predicates.
- `ResourceGroupReadHierarchy` supports hierarchical group traversal.
- Group constraints are always paired with tenant predicates — defense in depth prevents cross-tenant leakage through group membership alone.

### GTS-Based Attribute Access Control (ABAC)

> Source: [gts-spec](https://github.com/globalTypeSystem/gts-spec/) · [`dylint_lints/de09_gts_layer/`](../../tools/dylint_lints/de09_gts_layer/) · [`modules/system/types-registry/`](../../modules/system/types-registry/)

Cyber Fabric uses the **Global Type System (GTS)** as the foundation for attribute-based access control. GTS defines a hierarchical identifier scheme for data types and instances:

```
gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>]~
```

**How GTS enables ABAC:**

- **Token claims** — authenticated user tokens carry GTS type patterns in `token_scopes`, defining the capability ceiling for the subject (e.g., `["gts.x.core.srr.resource.v1~*"]` grants access to all SRR resource types under that schema).
- **Wildcard matching** — GTS supports segment-wise wildcard patterns (`*`), chain-aware evaluation, and attribute predicates for fine-grained policy expressions.
- **Authorization resources** — PDP evaluations reference GTS-typed resources (e.g., `gts.x.core.oagw.proxy.v1~:invoke` for outbound gateway proxy access).
- **Secure ORM integration** *(under development)* — the `ScopableEntity` trait supports a `type_col` dimension. The planned flow: AuthZ Resolver (PDP) evaluates GTS type constraints → compiles them into `AccessScope` → Secure ORM translates to SQL `WHERE` clauses, automatically filtering rows by type at the database level.

**Current implementation status:**

| Component | Status |
|---|---|
| GTS identifier parsing & validation | Implemented |
| GTS type patterns in token scopes | Implemented |
| Wildcard pattern matching (`GtsWildcard`) | Implemented |
| GTS → UUID resolution (Types Registry) | Implemented |
| Domain-level type filtering (e.g., SRR) | Implemented |
| GTS-typed authorization resources | Implemented |
| Secure ORM `type_col` auto-injection via PDP | Under development |

Custom dylint rules (`DE0901`, `DE0902`) validate GTS identifier correctness at compile time, preventing malformed type strings from entering the codebase.

## 4. Credentials Storage Architecture

> Source: [`modules/credstore/`](../../modules/credstore/) · [`modules/credstore/docs/DESIGN.md`](../../modules/credstore/docs/DESIGN.md)

Cyber Fabric provides a **plugin-based credential storage gateway** for managing secrets across the platform. The architecture separates the gateway (routing, authorization) from storage backends (plugin implementations).

```
Consumer → CredStoreClientV1 → Gateway Service → GTS Plugin Discovery
         → CredStorePluginClientV1 (vendor backend)
```

### Secret Material Handling

The SDK enforces secure handling of secret material at the type level:

| Protection | Mechanism |
|---|---|
| **Memory safety** | `SecretValue` wraps `Vec<u8>` with `zeroize` on `Drop` — secret bytes are wiped from memory when no longer needed |
| **Log safety** | `Debug` and `Display` implementations on `SecretValue` emit `[REDACTED]` — secrets cannot leak through logging |
| **Serialization safety** | `SecretValue` does not implement `Serialize`/`Deserialize` — secrets cannot be accidentally persisted or transmitted |
| **Key validation** | `SecretRef` validates keys as `[a-zA-Z0-9_-]+` (max 255 chars, no colons) — prevents injection via key names |
| **Anti-enumeration** | Failed lookups return `Ok(None)`, not a distinct "forbidden" error — prevents existence probing |

### Scoping Model

Credentials are scoped along three visibility levels:

- **Private** — `(tenant, owner, key)`: only the owning subject within a tenant can access.
- **Tenant** — `(tenant, key)`: any subject within the tenant can access.
- **Shared** — tenant-scoped with descendant visibility via gateway hierarchy walk-up.

### Plugin Isolation

The gateway enforces authorization via `SecurityContext`; plugins are **single-tenant-level adapters** that handle storage only. Built-in reference plugin: `static-credstore-plugin` (YAML-defined, in-memory — development/test use).

### Planned Encryption at Rest

The credential storage design specifies **AES-256-GCM** encryption with **per-tenant keys** and a `KeyProvider` abstraction supporting both `DatabaseKeyProvider` (co-located keys) and `ExternalKeyProvider` (Vault/KMS integration) for key–data separation.

## 5. Outbound API Gateway (OAGW)

> Source: [`modules/system/oagw/`](../../modules/system/oagw/) · [`modules/system/oagw/docs/DESIGN.md`](../../modules/system/oagw/docs/DESIGN.md)

OAGW is a **centralized outbound API gateway** built on [Pingora](https://github.com/cloudflare/pingora). All platform traffic to external HTTP services is routed through OAGW, enforcing security and observability policies via a **Control Plane / Data Plane** architecture.

### Authorization (Platform PEP)

Every proxy request is authorized via `PolicyEnforcer` before routing:

```rust
self.policy_enforcer
    .access_scope_with(
        &ctx,
        &resources::PROXY,                      // gts.x.core.oagw.proxy.v1~
        actions::INVOKE,
        None,
        &AccessRequest::new()
            .require_constraints(false)
            .context_tenant_id(ctx.subject_tenant_id()),
    )
    .await?;
```

Ancestor bind flows (descendant reusing parent upstream aliases) have separate authorization actions: `bind`, `override_auth`, `override_rate`, `add_plugins`.

### Credential Isolation

Outbound authentication credentials are **never stored in OAGW configuration**. Auth configs reference secrets via `cred://` URIs, resolved through `CredStoreClientV1` under the caller's `SecurityContext`. OAuth2 client-credentials tokens are cached with keys scoped by `(tenant, subject, auth_method)`, preventing cross-tenant token reuse.

### Auth Plugins

Per-upstream/route authentication plugins modify outbound requests:

| Plugin | Mechanism |
|---|---|
| `noop` | No authentication (pass-through) |
| `api-key` | Injects API key from credential store |
| `oauth2-client-credentials` | Client credentials grant (form/basic), token caching with tenant isolation |

### Request Hardening

| Control | Protection |
|---|---|
| **Path traversal** | Alias extracted from first path segment; only suffix is normalized |
| **Body size** | Configurable cap (default 100 MB) |
| **Content validation** | `Content-Type` and `Transfer-Encoding` validated before forwarding |
| **Hop-by-hop headers** | Stripped per HTTP spec; internal headers controlled |
| **CORS** | OPTIONS preflight returns 204 without upstream resolution; real requests validate `Origin`/method against config |
| **Rate limiting** | Per-upstream/route limits enforced in the data plane |
| **Error isolation** | `X-OAGW-Error-Source` header distinguishes gateway errors from upstream errors; RFC 9457 problem responses |

## 6. Compile-Time Linting — Clippy

> Source: [`Cargo.toml` (workspace.lints.clippy)](../../Cargo.toml) · [`clippy.toml`](../../clippy.toml)

The project enforces **90+ Clippy rules at `deny` level**, including the full `pedantic` group. Security-relevant highlights:

| Rule | Why It Matters |
|---|---|
| `unwrap_used`, `expect_used` | Prevents panics in production (denial-of-service) |
| `await_holding_lock`, `await_holding_refcell_ref` | Prevents deadlocks in async code |
| `cast_possible_truncation`, `cast_sign_loss`, `cast_precision_loss` | Prevents silent data corruption |
| `integer_division` | Prevents silent truncation |
| `float_cmp`, `float_cmp_const` | Prevents incorrect equality checks |
| `large_stack_arrays`, `large_types_passed_by_value` | Prevents stack overflows |
| `rc_mutex` | Prevents common concurrency anti-patterns |
| `regex_creation_in_loops` | Prevents ReDoS-adjacent performance issues |
| `cognitive_complexity` (threshold: 20) | Keeps code reviewable and auditable |

**`clippy.toml` additionally enforces:**
- `disallowed-methods` blocking direct SeaORM execution methods (must use Secure wrappers)
- `disallowed-types` blocking `LinkedList` (poor cache locality, potential DoS amplification)
- Stack size threshold of 512 KB
- Max 2 boolean fields per struct (prevents boolean blindness)

## 7. Compile-Time Linting — Custom Dylint Rules

> Source: [`dylint_lints/`](../../tools/dylint_lints/)

Project-specific architectural lints run on every CI build via `cargo dylint`. These enforce design boundaries that generic linters cannot:

| ID | Lint | Security Relevance |
|---|---|---|
| **DE0706** | `no_direct_sqlx` | Prohibits direct `sqlx` usage — forces all DB access through SeaORM/SecORM |
| DE0103 | `no_http_types_in_contract` | Prevents HTTP types leaking into contract layer |
| DE0301 | `no_infra_in_domain` | Prevents domain layer from importing `sea_orm`, `sqlx`, `axum`, `hyper`, `http` |
| DE0308 | `no_http_in_domain` | Prevents HTTP types in domain logic |
| DE0801 | `api_endpoint_version` | Enforces versioned API paths (`/{service}/v{N}/{resource}`) |
| DE1301 | `no_print_macros` | Forbids `println!`/`dbg!` in production code (prevents info leakage) |

The architectural lints in the `DE03xx` series enforce **strict layering** (contract → domain → infrastructure), preventing accidental coupling that could undermine security boundaries.

## 8. Dependency Security — cargo-deny

> Source: [`deny.toml`](../../deny.toml) · CI job: `.github/workflows/ci.yml` (`security` job)

`cargo deny check` runs in CI and enforces:

- **RustSec advisory database** — known vulnerabilities are treated as hard errors
- **License allow-list** — only approved OSS licenses (MIT, Apache-2.0, BSD, MPL-2.0, etc.)
- **Source restrictions** — only `crates.io` allowed; unknown registries and git sources warned
- **Duplicate version detection** — warns on multiple versions of the same crate in the dependency graph

## 9. Cryptographic Stack & FIPS-140-3

The project uses `aws-lc-rs` (via `rustls`) as its primary TLS cryptographic backend. JWT validation uses `jsonwebtoken` and `aliri`.

| Layer | Library | Backend |
|---|---|---|
| TLS | `rustls` + `hyper-rustls` | `aws-lc-rs` |
| Certificate verification | `rustls-webpki` | `aws-lc-rs`, `ring` |
| JWT validation | `jsonwebtoken`, `aliri` | `sha2`, `hmac`, `ring` |
| Database TLS | `sqlx` (`tls-rustls-aws-lc-rs`) | `aws-lc-rs` |

**FIPS-140-3 support:** the application can be built with FIPS-140-3 approved cryptography by enabling the `fips` feature flag:

```sh
cargo build -p cf-server --features fips
```

This switches the underlying cryptographic module from `aws-lc-sys` to `aws-lc-fips-sys` — the FIPS-validated AWS-LC module (NIST Certificate #4816). At startup, the FIPS crypto provider is installed as the process-wide default before any TLS, database, JWT, or other cryptographic operations occur. Runtime assertions verify that TLS configurations are operating in FIPS mode; the application fails fast if FIPS mode is expected but not active.

**Build requirements for FIPS:** CMake, Go, C compiler, C++ compiler. These are needed to build the AWS-LC FIPS module with its required integrity checks.

**Important:** enabling the `fips` feature does not automatically make the entire application FIPS-140-3 compliant. Full compliance also depends on the deployment environment, operating system, and adherence to the AWS-LC security policy constraints. The `ring` crate remains in the dependency graph via `pingora-rustls` (certificate hashing only, not TLS crypto) and `aliri` (token lifecycle); these do not participate in TLS cryptographic operations.

## 10. Continuous Fuzzing

> Source: [`fuzz/`](../../tools/fuzz/) · CI workflow: `.github/workflows/clusterfuzzlite.yml`

Cyber Fabric uses [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) with [ClusterFuzzLite](https://google.github.io/clusterfuzzlite/) for continuous fuzzing. Fuzzing discovers panics, logic bugs, and algorithmic complexity attacks in parsers and validators.

**Fuzz targets:**

| Target | Priority | Component | Status |
|---|---|---|---|
| `fuzz_odata_filter` | HIGH | OData `$filter` query parser | Implemented |
| `fuzz_odata_cursor` | HIGH | Pagination cursor decoder (base64+JSON) | Implemented |
| `fuzz_odata_orderby` | MEDIUM | OData `$orderby` token parser | Implemented |
| `fuzz_yaml_config` | HIGH | YAML configuration parser | Planned |
| `fuzz_html_parser` | MEDIUM | HTML document parser | Planned |
| `fuzz_pdf_parser` | MEDIUM | PDF document parser | Planned |

**CI integration:**
- **On pull requests:** ClusterFuzzLite runs with address sanitizer for 10 minutes per target
- **On main branch / nightly:** Extended 1-hour runs per target
- Crash artifacts and SARIF results uploaded for triage

**Local usage:**
```bash
make fuzz          # Smoke test all targets (30s each)
make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=300
make fuzz-list     # List available targets
```

## 11. Security Scanners in CI

Multiple automated scanners run on every pull request and/or on schedule:

| Scanner | What It Checks | Trigger |
|---|---|---|
| **[CodeQL](https://codeql.github.com/)** | Static analysis for security vulnerabilities (Actions, Python, Rust) | PRs to `main` + weekly schedule |
| **[OpenSSF Scorecard](https://scorecard.dev/)** | Supply-chain security posture (branch protection, dependency pinning, CI/CD hardness) | Weekly + branch protection changes |
| **[cargo-deny](https://embarkstudios.github.io/cargo-deny/)** | RustSec advisories, license compliance, source restrictions | Every CI run |
| **[ClusterFuzzLite](https://google.github.io/clusterfuzzlite/)** | Crash/panic/complexity bugs via fuzzing with address sanitizer | PRs to `main`/`develop` |
| **[Dependabot](https://docs.github.com/en/code-security/dependabot)** | Dependency alerts (including malware), security updates, version updates | Continuous (repository-level) |
| **[Snyk](https://snyk.io/)** | Dependency vulnerability scanning | Configured at repository/organization level |
| **[Aikido](https://www.aikido.dev/)** | Application security posture management | Configured at repository/organization level |

The OpenSSF Scorecard badge is displayed in the project README:
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/cyberfabric/cyberfabric-core/badge)](https://scorecard.dev/viewer/?uri=github.com/cyberfabric/cyberfabric-core)

## 12. PR Review Bots

Every pull request is reviewed by automated bots before human review:

| Bot | Mode | Purpose |
|---|---|---|
| **[CodeRabbit](https://coderabbit.ai/)** | Automatic on every PR | AI-powered code review with security awareness |
| **[Graphite](https://graphite.dev/)** | Manual trigger | Stacked PR management and review automation |
| **[Claude Code](https://docs.anthropic.com/)** | Manual trigger | LLM-powered deep code review |

## 13. Specification Templates & SDLC

> Source: [`docs/spec-templates/`](../spec-templates/) · [`docs/spec-templates/cf-sdlc/`](../spec-templates/cf-sdlc/)

Cyber Fabric follows a **spec-driven development** lifecycle where PRD and DESIGN documents are written before implementation. Security is addressed at multiple points:

- **PRD template** — Non-Functional Requirements section references project-wide security baselines and automated security scans
- **DESIGN template** — dependency rules mandate `SecurityContext` propagation across all in-process calls
- **ISO 29148 alignment** — global guidelines reference `guidelines/SECURITY.md` for security policies and threat models
- **Testing strategy** — 90%+ code coverage target with explicit security testing category (unit, integration, e2e, security, performance)
- **Git/PR record** — all changes flow through PRs with review and immutable merge/audit trail

## 14. Repository Scaffolding — Cyber Fabric CLI

Cyber Fabric provides a CLI tool for scaffolding new repositories that automatically inherit the platform's security posture:

| Inherited Configuration | Description |
|---|---|
| **Compiler configuration** | `rust-toolchain.toml`, workspace lint rules (`#[deny(warnings)]`, 90+ Clippy rules at deny level), `unsafe_code = "forbid"` |
| **Custom dylint rules** | Architectural boundary enforcement (DE01xx–DE13xx series), GTS validation (DE09xx) |
| **Makefile targets** | `make deny` (cargo-deny), `make fuzz` (continuous fuzzing), `make dylint` (custom lints), `make safety` (full suite) |
| **cargo-deny configuration** | `deny.toml` with RustSec advisory checks, license allow-lists, source restrictions |

This ensures every new service or module repository starts with the same defense-in-depth baseline described in this document, eliminating configuration drift across the platform.

## 15. Opportunities for Improvement

The following areas have been identified for future hardening:

1. **FIPS-140-3 compliance (remaining work)** — the `fips` feature flag enables FIPS-validated TLS via `aws-lc-fips-sys`. Remaining: replace `ring`-dependent libraries (`aliri` for token lifecycle, `pingora-rustls` for certificate hashing) to eliminate `ring` from the dependency graph; route JWT and hashing operations through `aws-lc-fips-sys`
2. **Secure ORM type-column auto-injection** — the `ScopableEntity` trait supports a `type_col` dimension, but automatic GTS type constraint injection from PDP → `AccessScope` → SQL `WHERE` is under development
3. **Tenant Resolver access-control plugins** — the `Unauthorized` error variant is reserved in the SDK, but no production plugin enforces caller-vs-target authorization (the static plugin allows any caller to query any configured tenant; the single-tenant plugin uses identity matching only). A policy-backed plugin would enforce fine-grained tenant visibility
4. **Security guidelines in spec templates** — add explicit security checklist sections to PRD and DESIGN templates (threat modeling, data classification, authentication requirements per feature)
5. **Security-focused dylint lints** — extend the `DE07xx` series with additional rules:
   - Detecting hardcoded secrets or API keys
   - Enforcing `SecretString` / `SecretValue` usage for sensitive fields
   - Flagging raw SQL string construction
   - Validating `SecurityContext` propagation in module handlers
6. **Fuzz target expansion** — current implemented targets cover OData parsers (`fuzz_odata_filter`, `fuzz_odata_cursor`, `fuzz_odata_orderby`). Planned targets: `fuzz_yaml_config`, `fuzz_html_parser`, `fuzz_pdf_parser`, `fuzz_json_config`, `fuzz_markdown_parser`
7. **Kani formal verification** — expand use of the [Kani Rust Verifier](https://model-checking.github.io/kani/) for proving safety properties on critical code paths (`make kani`)
8. **SBOM generation** — add Software Bill of Materials generation to CI for supply-chain transparency

---

*This document is maintained alongside the codebase. For implementation-level security guidelines, see [`guidelines/SECURITY.md`](../../guidelines/SECURITY.md). For the authorization architecture, see [`docs/arch/authorization/DESIGN.md`](../arch/authorization/DESIGN.md).*
