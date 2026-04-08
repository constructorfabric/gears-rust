# CredStore

Credential storage gateway module. Discovers storage backend plugins via the types registry and routes secret operations through the selected plugin with hierarchical tenant resolution.

## Overview

The `cf-credstore` module provides:

- **Plugin discovery** — finds storage backend plugins via the types registry using a configured vendor
- **Secret routing** — delegates `get`/`put`/`delete` to the active plugin
- **Hierarchical resolution** — walks the tenant hierarchy to resolve inherited secrets
- **ClientHub integration** — registers `CredStoreClientV1` for inter-module use

This module depends on `types-registry`. All storage logic lives in the plugin (e.g. `cf-static-credstore-plugin`).

## Usage

Consumers obtain the client from `ClientHub`:

```rust
use credstore_sdk::CredStoreClientV1;

let credstore = ctx.client_hub().get::<dyn CredStoreClientV1>()?;

if let Some(resp) = credstore.get(&ctx, &SecretRef::new("my-api-key")?).await? {
    // resp.value, resp.sharing, resp.is_inherited
}
```

## Configuration

```toml
[credstore]
vendor = "x"   # GTS vendor used to discover the storage plugin
```

## License

Apache-2.0
