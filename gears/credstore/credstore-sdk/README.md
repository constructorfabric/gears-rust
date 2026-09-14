# `CredStore` SDK

SDK crate for the `CredStore` gear, providing public API contracts for credential storage in Gears.

## Overview

This crate defines the transport-agnostic interface for the `CredStore` gear:

- **`CredStoreClientV1`** — consumer-facing trait (`get`/`put`/`create`/`delete`);
  `get` returns the value plus metadata (`owner_tenant_id`, `sharing`,
  `is_inherited`, `id`, `version`, `secret_type`, `expires_at`)
- **`CredStorePluginClientV1`** — backend trait: a pure per-tenant value store
  (`get`/`put`/`delete` keyed by `tenant_id` + `key` + optional `owner_id`); it
  holds no sharing/hierarchy/policy — that lives in the gear
- **`SecretRef`** / **`SecretValue`** / **`SharingMode`** / **`GetSecretResponse`** — Domain models
- **`CredStoreError`** — Error types for all operations
- **`CredStorePluginSpecV1`** — GTS schema for plugin registration

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
