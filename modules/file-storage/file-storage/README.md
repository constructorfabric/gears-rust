# cf-file-storage

P1 implementation of the FileStorage module.

See `modules/file-storage/docs/` for the full SDLC trail (PRD, DESIGN,
DECOMPOSITION, FEATURE specs, ADRs, and the Rust contract). The crate's
public API surface lives in `cf-file-storage-sdk`; this crate is the
ModKit module that registers the SDK trait into `ClientHub`, owns the
SQL schema migration, the backend roster (S3-compatible only in P1),
and the REST endpoints documented in `docs/openapi.yaml`.

## P1 scope

- Single backend kind: `s3-compatible` (per ADR-0004; `local` and `webdav`
  are reserved P3 enum variants and have no concrete adapter).
- Presign-first lifecycle for uploads (per ADR-0003 + ADR-0004), with
  self-healing reconciliation on `read_file` and idempotent late-arrival
  `change_status`.
- No `put_file` (proxy upload) endpoint or SDK method — overwrites go
  through a fresh `create_presigned_url` against the row's stable
  `backend_object_key`.
