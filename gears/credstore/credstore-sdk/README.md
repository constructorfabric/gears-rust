# `CredStore` SDK

SDK crate for the `CredStore` gear, providing public API contracts for credential storage in Gears.

## Overview

This crate defines the transport-agnostic interface for the `CredStore` gear:

- **`CredStoreClientV1`** — consumer-facing trait (`get`/`put`/`create`/`delete`);
  `get` returns the value plus metadata (`owner_tenant_id`, `sharing`,
  `is_inherited`, `id`, `version`, `secret_type`, `expires_at`)
- **`CredStorePluginClientV2`** — backend trait: a versioned value store keyed
  by `StoreKey { tenant_id, record_id }`. Required: `put` (returns the
  provider's `ValueVersion`), `get` (by version), `delete_key`. Optional:
  `destroy` (`DestroySelector::Below` / `Exactly`), declared through
  `supports_destroy`. It holds no sharing/hierarchy/policy — that lives in the
  gear (ADR-0006)
- **`SecretRef`** / **`SecretValue`** / **`SharingMode`** / **`GetSecretResponse`** — Domain models
- **`CredStoreError`** — Error types for all operations; `SecretExpired` means the decisive record's secret has expired (its metadata stays readable with status `expired`; never served, never replaced by an ancestor's value)
- **`CredStorePluginSpecV1`** — GTS schema for plugin registration

## `CredStoreClientV1`

The consumer trait has seven methods over one item shape shared by the record
and its optional secret (ADR-0004, ADR-0007):

- `get` — point read of one credential's metadata (`Credential`, never the
  secret; `read`); the source of the validator a secret-blind writer needs
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
  `select` names it. Selecting `secret` switches the call into **secret mode**
  (ADR-0005): `limit` and `cursor` are rejected, results are capped and not
  paginated, and only `reference in (...)` or `type eq`/`in` may filter.
  Without `secret` selected, `list` is paginated and never carries secrets
- `delete` — precondition-guarded delete of the record and its secret

There is no `create` (`put` under the create-only precondition is create) and
no separate bulk-read method (`list` in secret mode is the bulk read).

## Usage

A `ToolKit` consumer normally obtains `CredStoreClientV1` from `ClientHub`. The SDK
itself is transport-independent, so the example accepts the resolved client directly:

```no_run
use credstore_sdk::{CredStoreClientV1, CredStoreError, SecretRef};
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

A missing or out-of-scope secret is expressed as `Ok(None)`, preventing existence
leaks. An explicit denial of the read action is returned as `CredStoreError::AccessDenied`.

## License

Apache-2.0
