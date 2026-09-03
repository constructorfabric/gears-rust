# cf-gears-graph-storage-sdk

The public contract of the graph-storage gear, for consumers and plugin authors:

- `GraphStorageClientV1` — the in-process client trait a consuming gear resolves
  from the `ClientHub`: type registration, ingest, node reads, projection,
  search, traversal, neighbourhood, revision.
- `models` — transport-agnostic request and response shapes (`NodeSpec`,
  `EdgeSpec`, `IngestRequest`, `SearchRequest`, the element envelope).
- `plugin_api` — the plugin contracts the gear is assembled from:
  `GraphStoreV1`, `GraphEngineV1` and `EmbeddingProviderV1`, plus the
  `EmbeddingSpaceId` identity every provider must derive the same way.
- `gts` — the GTS identifiers of the base ontology and the gear's resource
  types.
- `contract` (feature `test-support`) — the executable form of the plugin
  contracts; every provider and store implementation proves itself against the
  same assertions.

```rust,ignore
let graph = ctx.client_hub().get::<dyn graph_storage_sdk::GraphStorageClientV1>()?;
```

See the gear's [DESIGN](../docs/DESIGN.md) for the API semantics.
