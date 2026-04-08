# Security in Cyber Fabric

Cyber Fabric takes a **defense-in-depth** approach to security, combining Rust's compile-time safety guarantees with layered static analysis, runtime enforcement, continuous scanning, and structured development processes. This document summarizes the security measures in place across the project.

---

## Table of Contents

- [1. Rust Language Safety](#1-rust-language-safety)
- [2. Compile-Time Tenant Scoping (Secure ORM)](#2-compile-time-tenant-scoping-secure-orm)
- [3. Authentication & Authorization Architecture](#3-authentication--authorization-architecture)
- [4. Compile-Time Linting — Clippy](#4-compile-time-linting--clippy)
- [5. Compile-Time Linting — Custom Dylint Rules](#5-compile-time-linting--custom-dylint-rules)
- [6. Dependency Security — cargo-deny](#6-dependency-security--cargo-deny)
- [7. Cryptographic Stack & FIPS-140-3](#7-cryptographic-stack--fips-140-3)
- [8. Continuous Fuzzing](#8-continuous-fuzzing)
- [9. Security Scanners in CI](#9-security-scanners-in-ci)
- [10. PR Review Bots](#10-pr-review-bots)
- [11. Specification Templates & SDLC](#11-specification-templates--sdlc)
- [12. Repository Scaffolding — Cyber Fabric CLI](#12-repository-scaffolding--cyber-fabric-cli)
- [13. Opportunities for Improvement](#13-opportunities-for-improvement)

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

> Source: [`docs/arch/authorization/`](../arch/authorization/) · [`modules/system/authn-resolver/`](../../modules/system/authn-resolver/) · [`modules/system/authz-resolver/`](../../modules/system/authz-resolver/)

Cyber Fabric implements a **PDP/PEP authorization model** per NIST SP 800-162:

```
Client → AuthN Middleware → AuthN Resolver (token validation)
       → Module Handler (PEP) → AuthZ Resolver (PDP, policy evaluation)
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

### AuthN Resolver

Validates bearer tokens (JWT signature verification or introspection), extracts claims, and constructs the `SecurityContext`. Pluggable via vendor-specific implementations (OIDC/JWT plugin documented in [`AUTHN_JWT_OIDC_PLUGIN.md`](../arch/authorization/AUTHN_JWT_OIDC_PLUGIN.md)).

### AuthZ Resolver (PDP)

Evaluates authorization policies and returns **decisions + row-level constraints**. Constraints are compiled into `AccessScope` objects that translate to SQL WHERE clauses, enforcing authorization at the query level rather than just point-in-time access checks.

### Multi-Tenancy

Hierarchical multi-tenancy with tenant forest topology (see [`TENANT_MODEL.md`](../arch/authorization/TENANT_MODEL.md)):
- **Isolation by default** — tenants cannot access each other's data
- **Hierarchical access** — parent tenants may access child data (configurable)
- **Barriers** — child tenants can opt out of parent visibility (`self_managed` flag)

### GTS-Based Attribute Access Control (ABAC)

> Source: [gts-spec](https://github.com/globalTypeSystem/gts-spec/) · [`dylint_lints/de09_gts_layer/`](../../dylint_lints/de09_gts_layer/) · [`modules/system/types-registry/`](../../modules/system/types-registry/)

Cyber Fabric uses the **Global Type System (GTS)** as the foundation for attribute-based access control. GTS defines a hierarchical identifier scheme for data types and instances:

```
gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>]~
```

**How GTS enables ABAC:**

- **Token claims** — authenticated user tokens carry GTS type patterns in `token_scopes`, defining the capability ceiling for the subject (e.g., `["gts.x.core.srr.resource.v1~*"]` grants access to all SRR resource types under that schema).
- **Wildcard matching** — GTS supports segment-wise wildcard patterns (`*`), chain-aware evaluation, and attribute predicates for fine-grained policy expressions.
- **Secure ORM integration** *(under development)* — the `ScopableEntity` trait supports a `type_col` dimension. The planned flow: AuthZ Resolver (PDP) evaluates GTS type constraints → compiles them into `AccessScope` → Secure ORM translates to SQL `WHERE` clauses, automatically filtering rows by type at the database level.

**Current implementation status:**

| Component | Status |
|---|---|
| GTS identifier parsing & validation | Implemented |
| GTS type patterns in token scopes | Implemented |
| Wildcard pattern matching (`GtsWildcard`) | Implemented |
| GTS → UUID resolution (Types Registry) | Implemented |
| Domain-level type filtering (e.g., SRR) | Implemented |
| Secure ORM `type_col` auto-injection via PDP | Under development |

Custom dylint rules (`DE0901`, `DE0902`) validate GTS identifier correctness at compile time, preventing malformed type strings from entering the codebase.

## 4. Compile-Time Linting — Clippy

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

## 5. Compile-Time Linting — Custom Dylint Rules

> Source: [`dylint_lints/`](../../dylint_lints/)

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

## 6. Dependency Security — cargo-deny

> Source: [`deny.toml`](../../deny.toml) · CI job: `.github/workflows/ci.yml` (`security` job)

`cargo deny check` runs in CI and enforces:

- **RustSec advisory database** — known vulnerabilities are treated as hard errors
- **License allow-list** — only approved OSS licenses (MIT, Apache-2.0, BSD, MPL-2.0, etc.)
- **Source restrictions** — only `crates.io` allowed; unknown registries and git sources warned
- **Duplicate version detection** — warns on multiple versions of the same crate in the dependency graph

## 7. Cryptographic Stack & FIPS-140-3

The project uses `aws-lc-rs` (via `rustls`) as its primary TLS cryptographic backend. JWT validation uses `jsonwebtoken` and `aliri`.

| Layer | Library | Backend |
|---|---|---|
| TLS | `rustls` + `hyper-rustls` | `aws-lc-rs` |
| Certificate verification | `rustls-webpki` | `aws-lc-rs`, `ring` |
| JWT validation | `jsonwebtoken`, `aliri` | `sha2`, `hmac`, `ring` |
| Database TLS | `sqlx` (`tls-rustls-aws-lc-rs`) | `aws-lc-rs` |

**FIPS-140-3 status:** the current build **does not** enable FIPS mode. `aws-lc-rs` supports a `fips` feature flag that switches to the FIPS-validated AWS-LC module (`aws-lc-fips-sys`), providing a viable path to FIPS-140-3 compliance. Enabling it requires replacing `ring`-dependent libraries and building with CMake + Go toolchains. See [Opportunities for Improvement](#13-opportunities-for-improvement) for the roadmap.

## 8. Continuous Fuzzing

> Source: [`fuzz/`](../../fuzz/) · CI workflow: `.github/workflows/clusterfuzzlite.yml`

Cyber Fabric uses [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) with [ClusterFuzzLite](https://google.github.io/clusterfuzzlite/) for continuous fuzzing. Fuzzing discovers panics, logic bugs, and algorithmic complexity attacks in parsers and validators.

**Current fuzz targets:**

| Target | Priority | Component |
|---|---|---|
| `fuzz_odata_filter` | HIGH | OData `$filter` query parser |
| `fuzz_odata_cursor` | HIGH | Pagination cursor decoder (base64+JSON) |
| `fuzz_odata_orderby` | MEDIUM | OData `$orderby` token parser |
| `fuzz_yaml_config` | HIGH | YAML configuration parser |
| `fuzz_html_parser` | MEDIUM | HTML document parser |
| `fuzz_pdf_parser` | MEDIUM | PDF document parser |

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

## 9. Security Scanners in CI

Multiple automated scanners run on every pull request and/or on schedule:

| Scanner | What It Checks | Trigger |
|---|---|---|
| **[CodeQL](https://codeql.github.com/)** | Static analysis for security vulnerabilities (Actions, Python, Rust) | PRs to `main` + weekly schedule |
| **[OpenSSF Scorecard](https://scorecard.dev/)** | Supply-chain security posture (branch protection, dependency pinning, CI/CD hardness) | Weekly + branch protection changes |
| **[cargo-deny](https://embarkstudios.github.io/cargo-deny/)** | RustSec advisories, license compliance, source restrictions | Every CI run |
| **[ClusterFuzzLite](https://google.github.io/clusterfuzzlite/)** | Crash/panic/complexity bugs via fuzzing with address sanitizer | PRs to `main`/`develop` |
| **[Snyk](https://snyk.io/)** | Dependency vulnerability scanning | Configured at repository/organization level |
| **[Aikido](https://www.aikido.dev/)** | Application security posture management | Configured at repository/organization level |

The OpenSSF Scorecard badge is displayed in the project README:
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/cyberfabric/cyberfabric-core/badge)](https://scorecard.dev/viewer/?uri=github.com/cyberfabric/cyberfabric-core)

## 10. PR Review Bots

Every pull request is reviewed by automated bots before human review:

| Bot | Mode | Purpose |
|---|---|---|
| **[CodeRabbit](https://coderabbit.ai/)** | Automatic on every PR | AI-powered code review with security awareness |
| **[Graphite](https://graphite.dev/)** | Manual trigger | Stacked PR management and review automation |
| **[Claude Code](https://docs.anthropic.com/)** | Manual trigger | LLM-powered deep code review |

## 11. Specification Templates & SDLC

> Source: [`docs/spec-templates/`](../spec-templates/) · [`docs/spec-templates/cf-sdlc/`](../spec-templates/cf-sdlc/)

Cyber Fabric follows a **spec-driven development** lifecycle where PRD and DESIGN documents are written before implementation. Security is addressed at multiple points:

- **PRD template** — Non-Functional Requirements section references project-wide security baselines and automated security scans
- **DESIGN template** — dependency rules mandate `SecurityContext` propagation across all in-process calls
- **ISO 29148 alignment** — global guidelines reference `guidelines/SECURITY.md` for security policies and threat models
- **Testing strategy** — 90%+ code coverage target with explicit security testing category (unit, integration, e2e, security, performance)
- **Git/PR record** — all changes flow through PRs with review and immutable merge/audit trail

## 12. Repository Scaffolding — Cyber Fabric CLI

Cyber Fabric provides a CLI tool for scaffolding new repositories that automatically inherit the platform's security posture:

| Inherited Configuration | Description |
|---|---|
| **Compiler configuration** | `rust-toolchain.toml`, workspace lint rules (`#[deny(warnings)]`, 90+ Clippy rules at deny level), `unsafe_code = "forbid"` |
| **Custom dylint rules** | Architectural boundary enforcement (DE01xx–DE13xx series), GTS validation (DE09xx) |
| **Makefile targets** | `make deny` (cargo-deny), `make fuzz` (continuous fuzzing), `make dylint` (custom lints), `make safety` (full suite) |
| **cargo-deny configuration** | `deny.toml` with RustSec advisory checks, license allow-lists, source restrictions |

This ensures every new service or module repository starts with the same defense-in-depth baseline described in this document, eliminating configuration drift across the platform.

## 13. Opportunities for Improvement

The following areas have been identified for future hardening:

1. **FIPS-140-3 compliance** — enable the `aws-lc-rs` `fips` feature to switch TLS to the FIPS-validated AWS-LC module; replace `ring`-dependent libraries (`aliri`, `rustls-webpki`) with FIPS-capable alternatives; route JWT and hashing operations through `aws-lc-fips-sys`
2. **Security guidelines in spec templates** — add explicit security checklist sections to PRD and DESIGN templates (threat modeling, data classification, authentication requirements per feature)
3. **Security-focused dylint lints** — extend the `DE07xx` series with additional rules, such as:
   - Detecting hardcoded secrets or API keys
   - Enforcing `SecretString` usage for sensitive fields
   - Flagging raw SQL string construction
   - Validating `SecurityContext` propagation in module handlers
4. **Fuzz target expansion** — implement planned targets (`fuzz_yaml_config`, `fuzz_html_parser`, `fuzz_pdf_parser`, `fuzz_json_config`, `fuzz_markdown_parser`) and enable `fuzz_odata_filter` after [#377](https://github.com/cyberfabric/cyberfabric-core/issues/377) is resolved
5. **Kani formal verification** — expand use of the [Kani Rust Verifier](https://model-checking.github.io/kani/) for proving safety properties on critical code paths (`make kani`)
6. **SBOM generation** — add Software Bill of Materials generation to CI for supply-chain transparency
7. **Dependency update automation** — configure Dependabot or Renovate for automated dependency updates with security advisory prioritization

---

*This document is maintained alongside the codebase. For implementation-level security guidelines, see [`guidelines/SECURITY.md`](../../guidelines/SECURITY.md). For the authorization architecture, see [`docs/arch/authorization/DESIGN.md`](../arch/authorization/DESIGN.md).*
