# `CredStore` SDK

SDK crate for the `CredStore` gear, providing public API contracts for credential storage in Gears.

## Overview

Transport-agnostic interface for the `CredStore` gear:

- **`CredStoreClientV1`** — consumer-facing trait (`get`/`put`/`create`/`delete`);
  `get` returns the value plus metadata (`owner_tenant_id`, `sharing`,
  `is_inherited`, `id`, `version`, `secret_type`, `expires_at`)
- **`CredStorePluginClientV1`** — backend trait: a pure per-tenant value store
  (`get`/`put`/`delete` keyed by `tenant_id` + `key` + optional `owner_id`); no
  sharing/hierarchy/policy — that stays in the gear. Planned (ADR-0006): the
  key becomes `tenant_id`/`value_id` only, since backend entries become
  immutable and version-addressed; the plugin then knows nothing about
  references, owners, or sharing
- **`SecretRef`** / **`SecretValue`** / **`SharingMode`** / **`GetSecretResponse`** — domain models
- **`CredStoreError`** — error types for all operations
- **`CredStorePluginSpecV1`** — GTS schema for plugin registration
- **Planned (ADR-0004)**: `CredStoreClientV1` reshaped around one item shared
  by the record and its optional secret — `get`, `get_secret`, `put`, `patch`,
  `list`, `delete`:
  - `get` — metadata only (`Credential`, `read`), never the secret; always
    carries the validator a secret-blind writer needs
  - `get_secret` — the secret plus its usage fields (`Secret`: reference,
    type, expiry, secret; `read_secret`). Over REST both live at one address,
    `/credentials/{ref}`, selected by `$select=secret`; the trait keeps two
    typed methods since a caller already knows which half it needs
  - `put` — guarded create-or-replace of the record with a tri-state `secret`
    (ADR-0007): a string writes it, an explicit `null` creates or leaves the
    record without one; this is the only way to create a credential
  - `patch` — guarded partial update, RFC 7396 merge-patch semantics
    (ADR-0007): present fields replace, absent fields untouched; covers
    metadata edits, secret rotation and secret removal (`null`); never
    creates
  - `list` — OData query (`filter`, `select`, `orderby`, `limit`, `cursor`)
    over records; `secret` appears only when selected. Selecting it switches
    to **secret mode** (ADR-0005): `limit`/`cursor` rejected, results capped
    and non-paginated, filterable only by `reference in (...)` or
    `type eq`/`in`. Otherwise `list` is the plain paginated listing and never
    carries secrets
  - `delete` — guarded delete of the record and its secret

  `get`'s return type changes: `Credential.secret` becomes optional, so code
  that read the old response as a bare value fails to compile instead of
  silently reading metadata; secret readers use `get_secret` or select
  `secret` on `get`. `create` is removed (ADR-0007) — `put` under the
  create-only precondition replaces it.

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
