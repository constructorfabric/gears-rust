# Model Registry

Tenant-aware catalog of AI providers and models. LLM Gateway and other consumer gears resolve models for evaluation through `ModelRegistryClientV1`; tenant administrators manage their own rows through the REST API. Inheritance and shadowing across the tenant hierarchy, provider enabling, sharing modes, and owner-tenant approval are specified in the design.

- Requirements: [docs/PRD.md](docs/PRD.md)
- Design: [docs/DESIGN.md](docs/DESIGN.md)
- SDK crate: [model-registry-sdk](model-registry-sdk) (`cf-gears-model-registry-sdk`)
- Gear crate: [model-registry](model-registry) (`cf-gears-model-registry`)

## REST surface

All paths are versioned under `/model-registry/v1/` and map one-to-one to the SDK trait methods.

| Method | Path | SDK method |
|--------|------|------------|
| `GET` | `/available-models/{canonical_id}` | `resolve_model`; returns the full `ModelV1` |
| `GET` | `/available-models` | `list_available_models`; a page of full `ModelV1` records |
| `POST` | `/providers` | `create_provider` |
| `GET` | `/providers/{id}` | `get_provider` |
| `GET` | `/providers` | `list_providers` |
| `PATCH` | `/providers/{id}` | `update_provider`; requires `If-Match`; a changed `gts_type` is a type migration |
| `DELETE` | `/providers/{id}` | `delete_provider`; optional `If-Match` |
| `POST` | `/models` | `create_model` |
| `GET` | `/models/{id}` | `get_model` |
| `GET` | `/models` | `list_models` |
| `PATCH` | `/models/{id}` | `update_model`; requires `If-Match` |
| `DELETE` | `/models/{id}` | `delete_model`; optional `If-Match` |

Lists accept `$filter`, `$orderby`, `$select`, `limit` (`$top`), and `cursor` (`$skiptoken`); the filter fields are the `ModelFilterField` and `ProviderFilterField` allowlists of the SDK.

Every record carries a row `version`. Single-entry responses return it as the strong `ETag` (`"<version>"`); `PATCH` requires `If-Match` (the `ETag`, or `*` for an unconditional update) and `DELETE` accepts an optional one. A stale validator is `aborted` (409) with reason `OPTIMISTIC_LOCK_FAILURE`.

## Configuration

Keys of the gear's `config` section; absent keys take the defaults. The gear refuses to start when a check fails.

| Key | Default | Meaning |
|-----|---------|---------|
| `chain_cache_ttl_seconds` | 30 | Ancestor-chain cache TTL; 0 disables the cache; at most 300 |
| `chain_cache_max_entries` | 100000 | Chain cache capacity |
| `default_page_size` | 20 | Page size when the request has no limit |
| `max_page_size` | 100 | Largest accepted page size |
| `max_json_document_bytes` | 65536 | Size limit for each of `provider_settings`, `metadata`, `additional_info`, `default_parameters` |
| `pdp_deadline_ms` | 1000 | Deadline for one PDP decision |
| `tenant_resolver_deadline_ms` | 1000 | Deadline for one `get_ancestors` call |
| `types_registry_deadline_ms` | 3000 | Deadline for one Types Registry schema read on writes |
| `credstore_deadline_ms` | 1000 | Deadline for one CredStore record read in the `secret_ref` ownership check |
| `max_pdp_constraint_values` | 1000 | Largest value list in one PDP row-constraint filter; a larger list denies |

Dependencies: `tenant-resolver`, `authz-resolver`, `types-registry`, CredStore (writes that bind a new `secret_ref` only, through `CredStoreClientV2`; until the credstore v2 switch the client is absent and such writes fail with `503`), and the platform database (PostgreSQL in production, SQLite in dev/test).

## Development

```bash
cargo test -p cf-gears-model-registry
cargo clippy -p cf-gears-model-registry --tests
```
