# `CredStore` SDK

SDK crate for the `CredStore` gear, providing public API contracts for credential storage in Gears.

## Overview

This crate defines the transport-agnostic interface for the `CredStore` gear:

- **`CredStoreClientV1`** — consumer-facing trait (`get`/`put`/`create`/`delete`);
  `get` returns the value plus metadata (`owner_tenant_id`, `sharing`,
  `is_inherited`, `id`, `version`, `secret_type`, `expires_at`)
- **`CredStorePluginClientV1`** — backend trait: a pure per-tenant value store
  (`get`/`put`/`delete` keyed by `tenant_id` + `key` + optional `owner_id`); it
  holds no sharing/hierarchy/policy — that lives in the gear. Planned,
  ADR-0006, not yet implemented: the key becomes `tenant_id`/`value_id` — no
  `key`, no `owner_id` — since backend entries become immutable, addressed
  only by version, and unique store-wide; the plugin learns nothing about
  references, owners, or sharing
- **`SecretRef`** / **`SecretValue`** / **`SharingMode`** / **`GetSecretResponse`** — Domain models
- **`CredStoreError`** — Error types for all operations
- **`CredStorePluginSpecV1`** — GTS schema for plugin registration
- Planned (ADR-0004), not yet implemented — `CredStoreClientV1` reshaped
  around one item shape shared by the record and its optional secret value:
  `get`, `get_secret`, `put`, `patch`, `list`, `delete`.
  - `get` — point read of one credential's metadata (`Credential`, never
    the value; `read`); always the source of the validator a value-blind
    writer needs
  - `get_secret` — the value with its usage envelope (`Secret`: reference,
    type, expiry, value; `read_secret`). Over REST these are one item shape
    at one address, `/credentials/{ref}`, where `$select=value` decides
    whether the value is included; the in-process trait keeps two typed
    methods instead of a field selector because a caller inside the platform
    already knows which half it needs
  - `put` — precondition-guarded create-or-replace of the record together
    with a **tri-state** `value`, in one call: a string writes it, an
    explicit `null` creates or leaves the record without one; the
    create-only precondition is how a credential is created, with or
    without a value
  - `patch` — precondition-guarded partial update following RFC 7396
    merge-patch semantics: present fields replace, absent fields are
    untouched; metadata edit, value rotate, or value remove (a `null` value)
    all go through it; never creates
  - `list` — takes an OData query (`filter`, `select`, `orderby`, `limit`,
    `cursor`) over credential records; an item's `value` field is present
    only when `select` names it. Selecting `value` switches the call into
    **value mode**, matching the REST contract one-for-one: `limit` and
    `cursor` are rejected, results are capped and non-paginated, and only
    `reference in (...)` or `type eq`/`in` may filter — no prefix or ordered
    operator over `reference` — see ADR-0004 for why that one is withheld
    rather than pending. Without `value` selected, `list` behaves exactly as
    the plain listing: paginated, never carrying values regardless of the
    caller's grants
  - `delete` — precondition-guarded delete of the record and its value

  **`get` changes meaning**: `Credential.value` is optional and present only
  when selected, so an existing caller that read the old combined response's
  value as a bare value fails to compile against the changed type rather
  than silently reading metadata; value readers use `get_secret`, or select
  `value` on `get` directly. **`create` is removed**: `put` under the
  create-only precondition is create, and its `value` may be a string or an
  explicit `null` — creating with `put` (record and, optionally, a value
  together, guarded by the create-only precondition), then editing or
  rotating with `patch`; a `patch` `value` of `null` removes the value.

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
    let response = credstore.get(security, &key).await?;

    Ok(response.map(|secret| secret.value.as_bytes().len()))
}
```

A missing or out-of-scope secret is expressed as `Ok(None)`, preventing existence
leaks. An explicit denial of the read action is returned as `CredStoreError::AccessDenied`.

## License

Apache-2.0
