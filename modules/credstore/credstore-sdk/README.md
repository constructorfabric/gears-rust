# CredStore SDK

SDK crate for the CredStore module, providing public API contracts for credential storage in CyberFabric.

## Overview

This crate defines the transport-agnostic interface for the CredStore module:

- **`CredStoreClientV1`** — Async trait for consumers (get/put/delete secrets)
- **`CredStorePluginClientV1`** — Async trait for backend storage plugin implementations
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
