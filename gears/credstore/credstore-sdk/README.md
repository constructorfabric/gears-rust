# CredStore SDK

SDK crate for the CredStore gear, providing public API contracts for credential storage in Gears.

## Overview

This crate defines the transport-agnostic interface for the CredStore gear:

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

### Getting the client

```rust
use credstore_sdk::CredStoreClientV1;

let credstore = hub.get::<dyn CredStoreClientV1>()?;
```

### Retrieving a secret

```rust
if let Some(resp) = credstore.get(&ctx, &SecretRef::new("my-api-key")?).await? {
    let bytes = resp.value.as_bytes();
}
```

Access denial is expressed as `Ok(None)`, not as an error — this prevents secret enumeration.

## License

Apache-2.0
