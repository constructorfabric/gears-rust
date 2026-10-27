# cf-gears-quota-enforcement-sdk

Public, transport-agnostic contract of the `quota-enforcement` gear.

## What this crate provides

- `QuotaEnforcementStoragePluginV1`: the storage plugin contract. One trait,
  a closed `StorageError` enum, and the thirteen invariants (I1 to I13) that
  every implementation upholds.
- Domain types and closed enums that the contract references: `Quota`,
  `QuotaSnapshot`, `DebitPlan`, `Decision`, `IdempotencyScope`,
  `NotificationEvent`, `MutationResult`, policy records, and pagination types.
- The GTS plugin spec used for discovery: `QuotaEnforcementStoragePluginSpecV1`.
- GTS resource identifiers for the canonical error envelope.

Singleton coordination for the sweepers is not a contract of this SDK. The
gear consumes the platform `cluster` gear's leader election, and the operator
selects the backend in the cluster profile YAML (ADR-0006).

## Client contracts

Three `#[toolkit::contract]` traits serve callers: `QuotaManagerApi` (Quota
lifecycle and credit), `QuotaEnforcementApi` (consumption, leases, snapshots)
and `QuotaOperatorApi` (policies). Resolve one from `ClientHub`; whether it is
the in-process implementation or a REST client is the deployment's wiring,
not the caller's concern:

```rust
let quotas = hub.get::<dyn QuotaManagerApi>()?;
let id = quotas.create_quota(&ctx, spec).await?;
```

Feature `rest-client` adds the generated REST clients (`*RestClient`, plus a
directory-resolving wrapper); `rest-server` adds the route registration the
gear hosts under `/quota-enforcement/v1` (internal, authenticated, one `POST`
per method). Over REST the caller's bearer token travels with the call, so QE
authorizes the original caller.

## Test support

Enable the `test-util` feature to get `quota_enforcement_sdk::testing`. It
holds a complete in-memory double of the storage plugin contract. The double
is for tests only.

## Design source

`gears/system/quota-enforcement/docs/DESIGN.md`, section 3.3, defines the
contract. `docs/features/foundation.md` defines the foundation scope.
