# File Storage

Control-plane gear for file storage in the Constructor Fabric gears runtime.
Backed by `cf-gears-file-storage-sdk`, persisted via SeaORM, exposed over REST,
and integrated with the AuthZ resolver.

## Overview

The `cf-gears-file-storage` gear provides:

- **File metadata** — logical files with references, ownership and versioning
- **Signed URLs** — short-lived upload/download URLs served by a data-plane sidecar
- **AuthZ enforcement** — per-type access decisions through the AuthZ resolver (PEP flow)
- **OData listing** — filterable, cursor-paginated reads over stored files
- **ClientHub integration** — registers the file-storage client for in-process consumers

The gear owns its database schema via the database capability and exposes a REST
surface via the REST capability. Byte transfer is handled by a separate sidecar
binary (`sidecar`) so the control plane never moves blob content itself.

## Capabilities

- `db` — SeaORM-backed storage with gear-owned migrations
- `rest` — REST API for file metadata and signed URLs (OpenAPI-described)
- Gear dependencies: `authz-resolver`

## Usage (in-process)

```rust
use file_storage_sdk::FileStorageClient;

let files = hub.get::<dyn FileStorageClient>()?;
let file = files.get_file(&ctx, file_id).await?;
```
