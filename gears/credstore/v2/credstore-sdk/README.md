Updated:  2026-10-06 by Constructor Tech

# `CredStore` SDK

Temporary v2 of the credstore SDK, merged piece by piece; it replaces `cf-gears-credstore-sdk` when the v2 gear is switched on.

SDK crate for the `CredStore` gear, providing public API contracts for credential storage in Gears.

## Overview

This crate defines the transport-agnostic interface for the `CredStore` gear:

- **`CredStoreClientV2`** — consumer-facing trait
  (`get_record`/`get_secret`/`put`/`patch`/`list`/`delete`); `get_record`
  returns the record's metadata (`Credential`, never the value) and
  `get_secret` the value with its usage envelope (`Secret`)
- **`CredStorePluginClientV2`** — backend trait: a versioned value store keyed
  by `StoreKey { tenant_id, record_id }`. Required: `put` (returns the
  provider's `ValueVersion`), `get` (by version), `delete_key`. Optional:
  destroy (`DestroySelector::Below` / `Exactly`), a separate
  `CredStorePluginDestroyerV2` capability exposed through `destroyer()`; a
  plugin that implements it promises ordered versions per key. It holds no sharing/hierarchy/policy — that lives in the
  gear (ADR-0006)
- **`SecretRef`** / **`SecretValue`** / **`SharingMode`** / **`Credential`** / **`Secret`** — Domain models
- **`CanonicalError`** — the error of every consumer-trait method (see
  [Errors](#errors)); **`CredStoreError`** is an opt-in typed projection of it
- **`CredStorePluginError`** — the error of every storage-plugin method (see
  [Plugin SPI](#plugin-spi))
- **`CredStorePluginSpecV2`** — GTS schema for plugin registration

## `CredStoreClientV2`

The consumer trait has six methods over one item shape shared by the record
and its optional secret (ADR-0004, ADR-0007):

- `get_record` — point read of one credential's metadata (`Credential`, never the
  secret; `read`); the source of the validator a secret-blind writer needs.
  Named `get_record`, not `get`: in the v1 API `get` returned the secret value,
  so the rename makes every stale call site fail to compile instead of
  silently returning `Some` for a value-less (`declared`) record
- `get_secret` — the secret with its usage envelope (`Secret`: reference,
  type, expiry, secret; `read_secret`). Over REST both are one item shape at
  `/credentials/{ref}`, where `$select=secret` decides whether the secret is
  included; the in-process trait keeps two typed methods instead
- `put` — precondition-guarded create-or-replace of the record together with a
  **tri-state** `secret` (`SecretWrite`): `Set` writes it, `Null` creates the
  record without one or removes the stored value on a replace. Under the create-only precondition it is how
  a credential is created
- `patch` — precondition-guarded partial update with RFC 7396 merge-patch
  semantics: present fields replace, absent fields are untouched; metadata
  edit, secret rotation and secret removal (a `null` secret) go through it;
  never creates
- `list` — takes an `OData` query (`filter`, `select`, `orderby`, `limit`,
  `cursor`) over credential records; an item's `secret` is present only when
  `select` names it. Selecting `secret` keeps the call paginated
  (ADR-0005): `limit`, `cursor`, `orderby` and `filter` behave as without it,
  and it requires `read_secret` alone (besides `secret`, `select` may name only
  `reference`, `type`, `expires_at`); records of a type the caller may not
  `read_secret` are omitted. Without `secret` selected, `list` never carries
  secrets
- `delete` — precondition-guarded delete of the record and its secret

There is no `create` (`put` under the create-only precondition is create) and
no separate bulk-read method (`list` with `secret` selected returns the secrets of its page).

Rotating only the secret is a `patch` with only `secret`. `put` is a whole
replace: an omitted expiry is cleared and `fallback` resets to `inherit` (the
v1 `put` preserved the expiry), so use `patch` to change one field.

## Errors

Every fallible method of `CredStoreClientV2` returns `CanonicalError`, the
platform's canonical error type; the SDK defines no error type of its own in
the trait contract (platform ADR 0005, "SDK Error Surface"). Each method's
`# Errors` section lists the canonical categories it can produce, with the
reason or field code where there is one.

`CredStoreError` is an opt-in typed projection for consumers that want flat
dispatch. The conversion `From<CanonicalError>` is infallible; its variants are
`NotFound`, `AlreadyExists`, `PreconditionFailed` (an `Aborted` with
`OPTIMISTIC_LOCK_FAILURE`), `TypeConflict` (an `Aborted` with one of the three
`TYPE_*` codes), `SecretExpired`, `UnsupportedTransition`, `Invalid` (the first
field violation: `field`, a typed `field::Reason`, `detail`), `PermissionDenied`,
`Unavailable` (with an optional `retry_after`; also "no storage plugin
registered" and "the store refused the plugin's credentials"), `Internal` and
`Other { canonical }` for everything else. The enum is `#[non_exhaustive]`.

A consumer has three ways to use it. It can propagate `CanonicalError` with `?`
when it needs no dispatch. It can project at one call site with
`.map_err(CredStoreError::from)?`. Or it can implement
`From<CanonicalError> for OwnError` once, chaining through
`CredStoreError::from`, so every call site stays a plain `?`.

`SecretExpired` means the decisive record's secret has expired: its metadata
stays readable with status `expired`, and the secret is never served nor
replaced by an ancestor's value. A stored version the backend can never return
(a lost or rotated decryption key, a corrupt entry, or a version gone although
the pointer did not move) is `Internal`: permanent, retrying does not help, the
record must be rewritten or deleted (REST `500`; in a `list` with `secret`
selected such an item fails the request).

The wire codes are constants with typed enums beside them: `reason::aborted`,
`reason::failed_precondition` and `reason::permission_denied` for the
category-level reasons, and `field` for the `InvalidArgument` field-violation
reasons (`field::Reason`). `SecretRef::new` returns the canonical
`InvalidArgument` (`field::INVALID_SECRET_REF`) for a malformed reference.

## Plugin SPI

`CredStorePluginClientV2` is a versioned value store keyed by
`StoreKey { tenant_id, record_id }`: `put` stores a new immutable version and
returns the provider's `ValueVersion`; `get` reads exactly that version
(`Ok(None)` when it is gone, `CredStorePluginError::Unavailable` for a
transient failure, `Internal` for a version that exists but can never be
read);
`delete_key` removes the key with all versions (idempotent); `destroy` is
optional: a plugin that can destroy returns `Some(self)` from `destroyer()`
and implements `CredStorePluginDestroyerV2`, which promises ordered versions
per key (a `put` that starts after another `put` on the same key has returned
gets a greater version, because `Below` is safe only under that order); the
gear never calls destroy otherwise. The gear calls both from the
request that recorded the cleanup, right after the commit, or when it heals a
live record on a later access (after a rotation, a secret removal, or a write
that lost its compare-and-set); the same version or key may be passed again,
so both calls must be idempotent. Cancelling or timing out a `put` does not
undo it, and the gear treats an unanswered `put` as possibly applied, so a
plugin need not make `put` cancel-safe.

### Plugin errors

Every plugin method returns `CredStorePluginError`, which the gear lifts into
its own domain error; it never reaches consumers. It has three variants:

- `Unavailable { detail, retry_after, source }` for a timeout, a connection
  reset, a 5xx or a 429. The gear answers 503 and passes `retry_after` on as
  the retry hint.
- `CredentialsRejected { detail, source }` when the store refuses the plugin's
  own token or policy (for example a Vault 403). The gear answers 503 without a
  retry hint and writes an error log for the operator; it is never reported as
  a missing secret and never as a 403 to the caller.
- `Internal { detail, source }` for a version that exists but can never be read
  or an answer the plugin does not understand. The gear answers 500.

A version that is gone is not an error: `get` returns `Ok(None)`. Build errors
with `CredStorePluginError::unavailable(detail, retry_after)`,
`credentials_rejected(detail)` and `internal(detail)`, and attach the cause with
`with_source`.

`detail` and `source` stay in the process (logs and diagnostics), but they must
never carry secret bytes or the plugin's own credentials: the plugin redacts
whatever text the store returned before it goes into either.

### Plugin conformance suite

Any `CredStorePluginClientV2` implementation, in-tree or out-of-tree (a
customer's `PostgreSQL` plugin, say), can prove it meets the backend contract
with the suite in `credstore_v2_sdk::conformance`, behind the `conformance`
feature. Add the feature to the plugin's dev-dependencies (the generated tests
also need `tokio` with `rt` and `macros`):

```toml
[dev-dependencies]
credstore-sdk = { package = "cf-gears-credstore-v2-sdk", version = "0.1", features = ["conformance"] }
tokio = { version = "1", features = ["rt", "macros"] }
```

One macro line then generates one `#[tokio::test]` per check, each against a
fresh plugin built by the expression (it may `.await`):

```no_run
# #[cfg(feature = "conformance")]
# mod conformance_tests {
# use async_trait::async_trait;
# use credstore_v2_sdk::{
#     CredStorePluginClientV2, CredStorePluginError, SecretValue, StoreKey, ValueVersion,
# };
# use toolkit_security::SecurityContext;
# #[derive(Default)]
# struct MyPlugin;
# #[async_trait]
# impl CredStorePluginClientV2 for MyPlugin {
#     async fn put(&self, _: &SecurityContext, _: &StoreKey, _: SecretValue)
#         -> Result<ValueVersion, CredStorePluginError> { unimplemented!() }
#     async fn get(&self, _: &SecurityContext, _: &StoreKey, _: &ValueVersion)
#         -> Result<Option<SecretValue>, CredStorePluginError> { unimplemented!() }
#     async fn delete_key(&self, _: &SecurityContext, _: &StoreKey)
#         -> Result<(), CredStorePluginError> { unimplemented!() }
# }
use credstore_v2_sdk::credstore_plugin_conformance;

credstore_plugin_conformance!(MyPlugin::default());
# }
# fn main() {}
```

Outer attributes written before the expression are applied to every generated
test. A plugin whose backend needs Docker keeps the suite out of a default
`cargo test` with `#[ignore]`:

```no_run
# #[cfg(feature = "conformance")]
# mod conformance_tests {
# use async_trait::async_trait;
# use credstore_v2_sdk::{
#     CredStorePluginClientV2, CredStorePluginError, SecretValue, StoreKey, ValueVersion,
# };
# use toolkit_security::SecurityContext;
# struct MyPlugin;
# impl MyPlugin {
#     async fn connect_to_docker_backend() -> Self {
#         Self
#     }
# }
# #[async_trait]
# impl CredStorePluginClientV2 for MyPlugin {
#     async fn put(&self, _: &SecurityContext, _: &StoreKey, _: SecretValue)
#         -> Result<ValueVersion, CredStorePluginError> { unimplemented!() }
#     async fn get(&self, _: &SecurityContext, _: &StoreKey, _: &ValueVersion)
#         -> Result<Option<SecretValue>, CredStorePluginError> { unimplemented!() }
#     async fn delete_key(&self, _: &SecurityContext, _: &StoreKey)
#         -> Result<(), CredStorePluginError> { unimplemented!() }
# }
use credstore_v2_sdk::credstore_plugin_conformance;

credstore_plugin_conformance!(
    #[ignore = "needs Docker; run with `-- --ignored`"]
    MyPlugin::connect_to_docker_backend().await
);
# }
# fn main() {}
```

The checks cover exact bytes (text, binary with NUL and invalid UTF-8, empty,
64 KiB), a new immutable version per `put`, absence (`None` for a never-written
key or a never-issued version), key isolation across tenants and records,
`delete_key` (removes every version, idempotent), concurrent puts, and, when
`destroyer()` is `Some`, `destroy` (`Below`, `Exactly`, idempotency) and
ordered versions, observed through `destroy(Below(..))` because versions are
opaque. A plugin whose `destroyer()` is `None` skips those checks and is
never asked to destroy. Every check uses fresh random ids, so checks can share
one backend. See the module docs for the full list and what the suite leaves
to the plugin (`Internal` for an unreadable version, `Unavailable`, `CredentialsRejected`,
durability across a restart).

## Usage

A `ToolKit` consumer normally obtains `CredStoreClientV2` from `ClientHub`. The SDK
itself is transport-independent, so the example accepts the resolved client directly:

```no_run
use credstore_v2_sdk::{CredStoreClientV2, SecretRef};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

async fn secret_length(
    credstore: &dyn CredStoreClientV2,
    security: &SecurityContext,
) -> Result<Option<usize>, CanonicalError> {
    let key = SecretRef::new("my-api-key")?;
    let response = credstore.get_secret(security, &key).await?;

    Ok(response.map(|secret| secret.secret.as_bytes().len()))
}
```

The record's metadata alone (no value; `Ok(Some(_))` even for a value-less
`declared` record) is `credstore.get_record(security, &key)`.

A missing secret, or one the PDP's constraints exclude, is expressed as `Ok(None)`,
preventing existence leaks. A read the PDP denies outright is
`CanonicalError::PermissionDenied` (`CredStoreError::PermissionDenied`), whether or
not the record exists, since the decision is taken before any lookup. A PDP outage is
`CanonicalError::ServiceUnavailable` (`CredStoreError::Unavailable`), a miss included.

## License

Apache-2.0
