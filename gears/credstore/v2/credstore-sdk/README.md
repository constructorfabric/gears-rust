Updated:  2026-10-06 by Constructor Tech

# `CredStore` SDK

Temporary v2 of the credstore SDK, merged piece by piece; it replaces `cf-gears-credstore-sdk` when the v2 gear is switched on.

SDK crate for the `CredStore` gear, providing public API contracts for credential storage in Gears.

## Overview

This crate defines the transport-agnostic interface for the `CredStore` gear:

- **`CredStoreClientV1`** — consumer-facing trait
  (`get_record`/`get_secret`/`put`/`patch`/`list`/`delete`); `get_record`
  returns the record's metadata (`Credential`, never the value) and
  `get_secret` the value with its usage envelope (`Secret`)
- **`CredStorePluginClientV2`** — backend trait: a versioned value store keyed
  by `StoreKey { tenant_id, record_id }`. Required: `put` (returns the
  provider's `ValueVersion`), `get` (by version), `delete_key`. Optional:
  `destroy` (`DestroySelector::Below` / `Exactly`), declared through
  `supports_destroy`. It holds no sharing/hierarchy/policy — that lives in the
  gear (ADR-0006)
- **`SecretRef`** / **`SecretValue`** / **`SharingMode`** / **`Credential`** / **`Secret`** — Domain models
- **`CredStoreError`** — Error types for all operations; `SecretExpired` means the decisive record's secret has expired (its metadata stays readable with status `expired`; never served, never replaced by an ancestor's value); a stored version the backend can never return (lost or rotated decryption key, corrupt entry, or a version gone although the pointer did not move) surfaces as `Internal` — permanent, retrying does not help, the record must be rewritten or deleted (REST: `500`; in a `list` with `secret` selected such an item fails the request)
- **`CredStorePluginSpecV1`** — GTS schema for plugin registration

## `CredStoreClientV1`

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
  **tri-state** `secret`: a string writes it, an explicit `null` creates or
  leaves the record without one. Under the create-only precondition it is how
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

## Plugin SPI

`CredStorePluginClientV2` is a versioned value store keyed by
`StoreKey { tenant_id, record_id }`: `put` stores a new immutable version and
returns the provider's `ValueVersion`; `get` reads exactly that version
(`Ok(None)` when it is gone, `ServiceUnavailable` for a transient failure,
`Internal` for a version that exists but can never be read);
`delete_key` removes the key with all versions (idempotent); `destroy` is
optional and declared through `supports_destroy`. The gear calls both from the
request that recorded the cleanup, right after the commit, or when it heals a
live record on a later access (after a rotation, a secret removal, or a write
that lost its compare-and-set); the same version or key may be passed again,
so both calls must be idempotent.

## Usage

A `ToolKit` consumer normally obtains `CredStoreClientV1` from `ClientHub`. The SDK
itself is transport-independent, so the example accepts the resolved client directly:

```no_run
use credstore_v2_sdk::{CredStoreClientV1, CredStoreError, SecretRef};
use toolkit_security::SecurityContext;

async fn secret_length(
    credstore: &dyn CredStoreClientV1,
    security: &SecurityContext,
) -> Result<Option<usize>, CredStoreError> {
    let key = SecretRef::new("my-api-key")?;
    let response = credstore.get_secret(security, &key).await?;

    Ok(response.map(|secret| secret.secret.as_bytes().len()))
}
```

The record's metadata alone (no value; `Ok(Some(_))` even for a value-less
`declared` record) is `credstore.get_record(security, &key)`.

A missing or out-of-scope secret is expressed as `Ok(None)`, preventing existence
leaks. An explicit denial of the read action is returned as `CredStoreError::AccessDenied`.

## License

Apache-2.0
