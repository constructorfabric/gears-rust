# Security in Cyber Ware

Cyber Ware takes a **defense-in-depth** approach to security, combining Rust's compile-time safety guarantees with layered static analysis, runtime enforcement, continuous scanning, and structured development processes. This document summarizes the security measures in place across the project.

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
- [14. Repository Scaffolding — Cyber Ware CLI](#14-repository-scaffolding--cyber-ware-cli)
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

Cyber Ware provides a **compile-time enforced** secure ORM layer over SeaORM. The `#[derive(Scopable)]` macro ensures every database entity explicitly declares its scoping dimensions:

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

Cyber Ware implements a **PDP/PEP authorization model** per NIST SP 800-162, extended with **OpenID AuthZEN 1.0** constraint semantics (see [ADR-0001](../arch/authorization/ADR/0001-pdp-pep-authorization-model.md)):

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

Cyber Ware uses the **Global Type System (GTS)** as the foundation for attribute-based access control. GTS defines a hierarchical identifier scheme for data types and instances:

```
gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>]~
```

**How GTS enables ABAC:**

- **Token claims** — authenticated user tokens carry GTS type patterns in `token_scopes`, defining the capability ceiling for the subject (e.g., `["gts.cf.core.srr.resource.v1~*"]` grants access to all SRR resource types under that schema).
- **Wildcard matching** — GTS supports segment-wise wildcard patterns (`*`), chain-aware evaluation, and attribute predicates for fine-grained policy expressions.
- **Authorization resources** — PDP evaluations reference GTS-typed resources (e.g., `gts.cf.core.oagw.proxy.v1~:invoke` for outbound gateway proxy access).
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

Cyber Ware provides a **plugin-based credential storage gateway** for managing secrets across the platform. The architecture separates the gateway (routing, authorization) from storage backends (plugin implementations).

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
        &resources::PROXY,                      // gts.cf.core.oagw.proxy.v1~
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

> Source: [FIPS PRD](fips/PRD.md) · [ADRs in `docs/security/fips/adrs/`](fips/adrs/) · [`libs/modkit/src/bootstrap/crypto.rs`](../../libs/modkit/src/bootstrap/crypto.rs) · [`libs/rustls-corecrypto-provider/`](../../libs/rustls-corecrypto-provider/) · [`deny-fips.toml`](../../deny-fips.toml)

### Default (non-FIPS) cryptographic stack

The project uses `aws-lc-rs` (via `rustls`) as its primary TLS cryptographic backend. JWT validation uses `jsonwebtoken`.

| Layer | Library | Backend |
|---|---|---|
| TLS | `rustls` + `hyper-rustls` | `aws-lc-rs` |
| Certificate verification | `rustls-webpki` | `aws-lc-rs`, `ring` |
| JWT validation | `jsonwebtoken` | `sha2`, `hmac`, `ring` |
| Database TLS | `sqlx` (`tls-rustls-aws-lc-rs`) | `aws-lc-rs` |

### FIPS-140-3 build (`--features fips`)

Cyber Ware applications can be built with FIPS 140-3 validated cryptography by enabling the `fips` feature flag on `cyberware-modkit`, `cyberware-modkit-http`, and any binary that ships TLS. A single feature flag selects a per-target CMVP-validated backend behind one shared `rustls 0.23` TLS state machine:

| Target | Validated module | How it routes |
|---|---|---|
| Linux (x86_64, aarch64) | **AWS-LC FIPS Provider v2** — CMVP cert [#4816](https://csrc.nist.gov/projects/cryptographic-module-validation-program/certificate/4816) | `rustls/fips` + `aws-lc-fips-sys`; activated via target-gated shim `cyberware-rustls-fips-shim` |
| macOS (any arch) | **Apple corecrypto User-Space Module** — per-macOS-major CMVP cert (search by *"Apple corecrypto Module"* on the [CMVP database](https://csrc.nist.gov/projects/cryptographic-module-validation-program/validated-modules/search)) | In-tree `cyberware-rustls-corecrypto-provider` over `Security.framework` + `CommonCrypto` |
| Windows (x86_64) | **Microsoft Windows CNG** (`bcrypt.dll`) — per-Windows-build CMVP cert | Community `rustls-cng-crypto` (caret-pinned `0.1.x`); requires OS-level FIPS-mode via `HKLM\System\CurrentControlSet\Control\Lsa\FipsAlgorithmPolicy = 1` |

```sh
cargo build -p cyberware-server --features fips
```

`modkit::bootstrap::init_crypto_provider` is invoked automatically as the first step of `init_procedure` (used by `run_server` and `run_migrate`) — no explicit setup in `main()`. The function dispatches per `cfg(target_os, feature = "fips")` and installs the per-OS provider once via `OnceLock`. Subsequent calls return the cached first-call result.

**Build prerequisites:**

| Target | Toolchain |
|---|---|
| Linux + `fips` | C toolchain, `cmake`, `perl`, `go` (required by `aws-lc-fips-sys` build script — module integrity checks) |
| macOS + `fips` | Xcode Command Line Tools + Rust toolchain. No `cmake` / `perl` / `go`. The per-target shim excludes `aws-lc-fips-sys` from the macOS build graph entirely. |
| Windows + `fips` (native) | MSVC + Windows SDK. No `cmake` / `perl` / `go`. CNG is loaded at runtime from `bcrypt.dll`. |
| Windows + `fips` (cross-compile from Linux/macOS) | `cargo install cargo-xwin` plus `ninja`. See `make check-windows-fips`. |

### Algorithm scope on the wire

Under `--features fips`, the `ClientHello` and `ServerHello` offer **only** FIPS-Approved algorithms. No ChaCha20-Poly1305, X25519, X25519MLKEM768 / post-quantum hybrids, Ed25519, MD5, or SHA-1 (outside the SP 800-131A legacy signature-verify allowance).

| Category | Algorithms |
|---|---|
| TLS versions | TLS 1.2, TLS 1.3 (no TLS 1.0/1.1). **macOS is TLS 1.3-only** under FIPS — see TLS 1.2 PRF note below. |
| TLS 1.3 cipher suites | `TLS_AES_128_GCM_SHA256`, `TLS_AES_256_GCM_SHA384` |
| TLS 1.2 cipher suites | `ECDHE_{ECDSA,RSA}_WITH_AES_{128,256}_GCM_SHA{256,384}` (×4 — Linux + Windows only) |
| Key exchange | NIST P-256, P-384 ECDHE |
| Signature verify | ECDSA P-256/P-384/P-521, RSA-PSS, RSA PKCS#1 v1.5 (SHA-256/384/512) |
| Signature sign (server-side / mTLS) | Same scope as verify; routed through the validated module (`SecKeyCreateSignature` on macOS, `BCryptSignHash` on Windows, `EVP_PKEY_sign` on Linux) |
| Hash / HMAC / HKDF | SHA-256, SHA-384 (HKDF is an Approved KDF per NIST SP 800-56C) |
| TLS 1.2 Extended Master Secret (RFC 7627) | **required** (`require_ems = true`) per NIST SP 800-52 Rev. 2 §3.5 |
| RSA modulus floor | ≥ 2048 bits (NIST FIPS 186-5 §5.1) — enforced at server-side key load on macOS |

### Build-time dependency-graph policy

A FIPS build must not pull in a non-FIPS-validated crypto crate, even transitively. We enforce this at **dependency-resolution time** via [`cargo-deny`](https://embarkstudios.github.io/cargo-deny/): under `--features fips`, the build fails if any banned crypto crate appears in `cargo tree`. This is the Rust analogue of Go 1.25's `fips140=only` runtime gate — Rust has no language-level hook for "refuse this crypto call", so we cut it off one step earlier, before the offending code ever links. Configured via [`deny-fips.toml`](../../deny-fips.toml); rationale in [ADR 0005](fips/adrs/0005-fips-dependency-policy.md).

```sh
make fips-policy        # Standalone — runs cargo deny check bans --config deny-fips.toml
make security           # Runs both `deny` (license/advisory) and `fips-policy`
```

**Phase A** (shipped) bans crates not currently in the graph — zero-pain regression gate: future PRs adding `md2`/`md4`/`ripemd`, `chacha20poly1305`/`salsa20`, the Curve25519 family (`x25519-dalek`, `ed25519-dalek`, …), alternative TLS frameworks (`openssl`, `boring`, `native-tls`), or alternative rustls CryptoProviders (`rustls-symcrypt`, `rustls-mbedcrypto-provider`, `rustls-openssl`, `rustls-rustcrypto`, `rustls-graviola`, `rustls-wolfcrypto-provider`, `boring-rustls-provider`) all fail the gate.

**Phase B** (pending transitive cleanup) is documented inline in `deny-fips.toml` — `ring`, non-FIPS `aws-lc-rs`, `chacha20`, `md-5`, `sha1`, `blake2`/`blake3`, `aes`, `hmac`, `hkdf`, etc. — currently pulled by upstream deps (`pingora-rustls`/`ureq`, rustls's default features, `rand`). Each moves to Phase A as its upstream pull-through is replaced. **Tracking**: [ADR 0005 §"Phasing"](fips/adrs/0005-fips-dependency-policy.md) and [FIPS PRD §13 TODO-7](fips/PRD.md#13-open-questions) — promotion to Phase A is the unit of work; no per-crate sub-tickets today.

### Runtime Operational Environment validation

A FIPS 140-3 claim is only valid when the running OS version lies inside the **Operational Environment (OE)** listed on the active CMVP certificate. The OE check has different shapes per OS:

| Target | Runtime gate | Behaviour on mismatch |
|---|---|---|
| macOS | `cyberware_rustls_corecrypto_provider::oe::validate_oe()` reads `kern.osproductversion` via `sysctlbyname` and matches against [`SUPPORTED_OE_MACOS_MAJOR`](../../libs/rustls-corecrypto-provider/src/oe.rs). Fires as a side-effect of the first `fips_provider()` call. | Under `--features fips`: **panic** (deliberate fail-closed — cannot be silently caught by an intermediate `Result`-handling caller). Override via `CYBERWARE_FIPS_OE_OVERRIDE=1` for CI on pre-release macOS only. |
| Linux | Not yet implemented at runtime. | OE coverage verified manually per release (CMVP cert search for cert #4816). Tracked as **TODO-8** in [FIPS PRD §13](fips/PRD.md#13-open-questions). |
| Windows | OS-level FIPS-mode flag check (via `rustls-cng-crypto`'s empty-provider gate). Build-number-vs-CMVP-OE check not yet implemented at runtime. | If `FipsAlgorithmPolicy = 0`: bootstrap refuses to install the provider. Build-number OE check tracked as **TODO-8**. |

### How to verify a build is FIPS-conformant

```sh
# 1. Wire-level — what the ClientHello actually offers:
cargo run -p cyberware-fips-probe --features fips -- --url https://www.howsmyssl.com/a/check
# Expected: given_cipher_suites = AES-GCM only, given_named_groups = secp256r1/secp384r1,
#           post_quantum_key_agreement: false, [OK] No ChaCha20 in ClientHello.

# 2. Dep-graph regression (cheap, no compile step):
cargo tree --target aarch64-apple-darwin -p cyberware-example-server --features fips \
  -e features | grep -E 'corecrypto|aws-lc-fips'
# Expected on macOS: cyberware-rustls-corecrypto-provider present, aws-lc-fips-sys ABSENT.

cargo tree --target x86_64-unknown-linux-gnu -p cyberware-example-server --features fips \
  -e features | grep 'aws-lc-fips'
# Expected on Linux: aws-lc-fips-sys present.

cargo tree --target x86_64-pc-windows-msvc -p cyberware-example-server --features fips \
  -e features | grep -E 'cng-crypto|aws-lc-fips'
# Expected on Windows: rustls-cng-crypto present, aws-lc-fips-sys ABSENT.

# 3. Linkage smoke — confirm only OS-supplied crypto framework is loaded:
otool -L target/release/cyberware-server | grep -E 'aws|crypto|ssl|ring'      # macOS — expect only Security.framework
dumpbin /imports target\release\cyberware-server.exe | findstr /i "bcrypt aws"  # Windows — expect only bcrypt.dll

# 4. Dep-graph policy regression (rejects non-FIPS crypto crates):
make fips-policy

# 5. Wire-shape regression for our macOS provider:
cargo test -p cyberware-rustls-corecrypto-provider --features fips --test fips_provider_invariants
```

See [`examples/cyberware-fips-probe/README.md`](../../examples/cyberware-fips-probe/README.md) for the full four-layer verification chain (linkage, runtime, wire-level, cert-validation).

### What this does NOT claim

- **Cyber Ware itself is not on the CMVP Validated Modules list.** The validated modules are Apple corecrypto, AWS-LC FIPS Provider, and Microsoft Windows CNG. Cyber Ware is a *consumer* of those modules.
- **CMVP OE-coverage is the deployment's responsibility.** A FIPS claim is void if the running OS version is not inside the cert's OE. The macOS runtime gate is fail-closed; Linux + Windows OE coverage is verified manually per release.
- **`CryptoProvider::fips() = true` is a runtime witness, not just design intent.** On macOS it reflects the OE check (`oe::fips_witness_ok`); on Windows, the OS FIPS-mode flag. On Linux, runtime OE-validation is not yet implemented; OE coverage is verified manually per release via the §release-checklist CMVP-cert search.
- **TLS 1.2 PRF on macOS is not CAVS-listed.** Apple corecrypto exposes generic HMAC primitives but not a CAVS-listed dedicated TLS PRF (unlike `aws-lc-fips`'s `tls_prf::Algorithm`). Consequence: `fips_provider()` on macOS is TLS-1.3-only; customers requiring TLS 1.2 on macOS+FIPS must accept that those connections do not carry a FIPS claim.
- **JWT signature validation does not go through the FIPS path.** `jsonwebtoken` uses `ring` / non-FIPS `aws-lc-rs` for RSA / ECDSA verification on bearer tokens. Treat tokens as authentication context, not as data covered by the cryptographic claim. Out of scope today; tracked as **TODO-7** in [FIPS PRD §13](fips/PRD.md#13-open-questions). Cleanup is gated by `deny-fips.toml` Phase B promotion.
- **Non-FIPS crypto crates remain in the final binary on macOS+fips.** `ring` is pulled in transitively by `pingora-rustls`, `pingora-pool`, and `ureq`; non-FIPS `aws-lc-rs` is pulled in by rustls's default feature set; `chacha20` is pulled in by the `rand` ecosystem. These are **not invoked** on the TLS data plane (the installed `CryptoProvider` routes every TLS primitive through the validated module) but the symbols are linked into the binary. Linkage smoke (above) confirms no non-validated shared libraries appear at runtime.
- **Server-side TLS keys load from PEM/DER bytes by default.** The bytes transit user-space memory before reaching `SecKeyCreateWithData` / `BCryptImportKeyPair` / `EVP_PKEY_new`. Strict-FIPS auditors operating under "no plaintext CSPs outside the boundary" require a Keychain / NCrypt / HSM flow; tracked as **TODO-1**.

### Deep references

- **[FIPS PRD](fips/PRD.md)** — full strategy: requirements (FRs, NFRs), ecosystem constraints, alternatives we rejected (OpenSSL FIPS Provider 3.1.2, Go 1.25, `native-tls`), per-OS rationale, verification gates, open TODOs.
- **[ADR 0001 — macOS FIPS via custom corecrypto CryptoProvider](fips/adrs/0001-macos-fips-via-corecrypto-provider.md)** — why we built `cyberware-rustls-corecrypto-provider` over Apple corecrypto rather than `native-tls` or "Linux-only FIPS".
- **[ADR 0002 — FIPS feature flag via target-conditional shim crate](fips/adrs/0002-fips-feature-target-conditional-shim.md)** — empty `cyberware-rustls-fips-shim` crate that encodes per-target Cargo feature activation.
- **[ADR 0003 — Windows FIPS via `rustls-cng-crypto`](fips/adrs/0003-windows-fips-via-rustls-cng-crypto.md)** — why `rustls-cng-crypto` over `rustls-symcrypt` today, plus documented migration trigger.
- **[ADR 0004 — macOS server-side TLS via corecrypto](fips/adrs/0004-macos-server-side-tls-via-corecrypto.md)** — server-side TLS / mTLS signing via `SecKeyCreateSignature`; SEC1-publicKey-only EC key load that preserves the FIPS boundary; honest TLS-1.2-PRF posture.
- **[ADR 0005 — Workspace-level FIPS dependency policy via cargo-deny](fips/adrs/0005-fips-dependency-policy.md)** — `deny-fips.toml`, `make fips-policy`, Phase A/B/C plan.

## 10. Continuous Fuzzing

> Source: [`fuzz/`](../../tools/fuzz/) · CI workflow: `.github/workflows/clusterfuzzlite.yml`

Cyber Ware uses [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) with [ClusterFuzzLite](https://google.github.io/clusterfuzzlite/) for continuous fuzzing. Fuzzing discovers panics, logic bugs, and algorithmic complexity attacks in parsers and validators.

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
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/cyberfabric/cyberware-rust/badge)](https://scorecard.dev/viewer/?uri=github.com/cyberfabric/cyberware-rust)

## 12. PR Review Bots

Every pull request is reviewed by automated bots before human review:

| Bot | Mode | Purpose |
|---|---|---|
| **[CodeRabbit](https://coderabbit.ai/)** | Automatic on every PR | AI-powered code review with security awareness |
| **[Graphite](https://graphite.dev/)** | Manual trigger | Stacked PR management and review automation |
| **[Claude Code](https://docs.anthropic.com/)** | Manual trigger | LLM-powered deep code review |

## 13. Specification Templates & SDLC

> Source: [`docs/spec-templates/`](../spec-templates/) · [`docs/spec-templates/cyberware-sdlc/`](../spec-templates/cyberware-sdlc/)

Cyber Ware follows a **spec-driven development** lifecycle where PRD and DESIGN documents are written before implementation. Security is addressed at multiple points:

- **PRD template** — Non-Functional Requirements section references project-wide security baselines and automated security scans
- **DESIGN template** — dependency rules mandate `SecurityContext` propagation across all in-process calls
- **ISO 29148 alignment** — global guidelines reference `guidelines/SECURITY.md` for security policies and threat models
- **Testing strategy** — 90%+ code coverage target with explicit security testing category (unit, integration, e2e, security, performance)
- **Git/PR record** — all changes flow through PRs with review and immutable merge/audit trail

## 14. Repository Scaffolding — Cyber Ware CLI

Cyber Ware provides a CLI tool for scaffolding new repositories that automatically inherit the platform's security posture:

| Inherited Configuration | Description |
|---|---|
| **Compiler configuration** | `rust-toolchain.toml`, workspace lint rules (`#[deny(warnings)]`, 90+ Clippy rules at deny level), `unsafe_code = "forbid"` |
| **Custom dylint rules** | Architectural boundary enforcement (DE01xx–DE13xx series), GTS validation (DE09xx) |
| **Makefile targets** | `make deny` (cargo-deny), `make fuzz` (continuous fuzzing), `make dylint` (custom lints), `make safety` (full suite) |
| **cargo-deny configuration** | `deny.toml` with RustSec advisory checks, license allow-lists, source restrictions |

This ensures every new service or module repository starts with the same defense-in-depth baseline described in this document, eliminating configuration drift across the platform.

## 15. Opportunities for Improvement

The following areas have been identified for future hardening:

1. **FIPS-140-3 — non-TLS crypto cleanup** — the `--features fips` build routes the **TLS data plane** through a CMVP-validated module on Linux, macOS, and Windows (see §9). Open items, tracked in the [FIPS PRD §13](fips/PRD.md#13-open-questions):
   - **TODO-7** — JWT signature validation (`jsonwebtoken`) currently uses `ring` / non-FIPS `aws-lc-rs`. Audit the surface and either replace upstream, fork, or restrict JWT to symmetric HMAC. Build-time floor enforced via [`deny-fips.toml`](../../deny-fips.toml) Phase B promotion.
   - **TODO-8** — Runtime Operational Environment validation on Linux + Windows (macOS already has a sysctl-based fail-closed gate). Today OE coverage on Linux + Windows is verified manually per release via CMVP cert search.
   - **TODO-1** — Keychain / NCrypt / HSM-stored private keys for server-side TLS (today's PEM/DER load is acceptable for development and most production deployments where filesystem permissions guard the key).
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
