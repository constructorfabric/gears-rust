# Security Guideline

Short secure-coding rules for gear code. The full security model is in
[docs/security/SECURITY.md](../docs/security/SECURITY.md); the AuthN, AuthZ and Secure ORM data path is in
[06_authn_authz_secure_orm.md](../docs/toolkit_unified_system/06_authn_authz_secure_orm.md).

## Input

1. Deserialize request input into typed DTOs through `toolkit::api::rest::extract::{Json, Path, Query}`, not axum's
   built-in extractors. A failed extraction (oversized body, wrong `Content-Type`, schema mismatch) renders as an RFC 9457
   problem (`400` for path and query, `413`/`415`/`422` for JSON bodies) instead of axum's plain-text rejection.
2. Check business invariants in the domain layer and return `InvalidArgument` (HTTP `400`), as described in
   [docs/arch/errors/DESIGN.md](../docs/arch/errors/DESIGN.md).
3. Declare every route through `OperationBuilder` with `.authenticated()` (tenant bearer token),
   `.platform_authenticated()` (internal platform token), or `.anonymous()` only for routes that are public by design. Authentication runs in the API Gateway middleware; handlers receive a ready `SecurityContext`.

## Data access

4. Obtain the `AccessScope` for resource access from `PolicyEnforcer` (`authz-resolver-sdk`), which compiles PDP
   constraints into the scope. Do not build an `AccessScope` by hand in production code. Explicit scopes
   (`AccessScope::for_tenant`, `for_tenants`, `for_resources`) are for repository tests; `AccessScope::allow_all()` is
   allowed only in the prefetch step of the GET/UPDATE/DELETE patterns in the 06 doc, with its compensating checks.
5. Treat every `EnforcerError` as a refusal and map it to a domain error. Do not expose PDP details to the client.
6. Derive `Scopable` on every SeaORM entity and declare all four dimensions in `#[secure(...)]`: tenant, resource,
   owner and type, each as `*_col = "..."` or `no_*`. Use `#[secure(unrestricted)]` only for global tables with no
   scoping columns.
7. Query through the secure builders (`.secure().scope_with(scope)`, from `toolkit_db::secure::SecureEntityExt`) on a
   `DBRunner`. `clippy.toml` lists `sea_orm::Select::all`/`one`/`count`, `UpdateMany::exec` and `DeleteMany::exec` as
   disallowed methods to catch bypasses.
8. No raw SQL outside migrations.

## Secrets

9. Never commit secrets. Store per-tenant and per-user credentials in the `credstore` gear (`CredStoreClientV1`).
   Its `SecretValue` zeroizes on drop, redacts `Debug`/`Display` and does not implement `Serialize`.
10. For secrets in gear configuration, write a `${VAR}` placeholder in YAML, mark the field `#[expand_vars]` on a
    `#[derive(ExpandVars)]` config struct, and load it with `GearCtx::config_expanded`. Hold the value in
    `secrecy::SecretString`, the only redacting type `#[expand_vars]` supports, or expand into a `String` and wrap it
    afterwards.
11. Never log secrets, tokens or credentials, including inside error messages.

## Code

12. No `unwrap()`/`expect()` outside tests: `clippy::unwrap_used` and `clippy::expect_used` are denied in the workspace.
    `unsafe_code` is forbidden.
13. New outbound HTTP code uses `toolkit_http::HttpClient`
    ([ADR toolkit/0001](../docs/adrs/toolkit/0001-toolkit-hyper-tower-http-client.md)), which carries the shared
    timeout, retry, body-limit and redirect (SSRF) policies. Calls to third-party services are meant to go through the
    `oagw` gear ([Security Overview §5](../docs/security/SECURITY.md#5-outbound-api-gateway-oagw)).
14. TLS uses `rustls` with the `aws-lc-rs` provider. Do not enable a second provider such as `ring` (for example via a
    crate's `*-rustls-ring` feature): two active providers make rustls 0.23 panic at runtime, as the TLS comments in the
    root `Cargo.toml` explain. For FIPS builds (`--features fips`) see
    [Security Overview §9](../docs/security/SECURITY.md#9-cryptographic-stack--fips-140-3).
15. When you add or update dependencies, run `make security`: `cargo deny check` against `deny.toml` (advisories,
    licenses, bans, sources) plus the FIPS ban check against `deny-fips.toml`. CI runs `make deny` in the `security`
    job; the FIPS check is local only. Run `make dylint` (`cargo gears lint --dylint`) for the architecture lints,
    which CI also runs.
