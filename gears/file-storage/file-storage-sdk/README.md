# cf-gears-file-storage-sdk

Public API surface (client trait, model types, GTS constants, error envelope) for the
`file-storage` gear. See `gears/file-storage/docs/DESIGN.md`.

**Status**: Level 1 implemented. `FileStorageClientV1` (`src/api.rs`) covers every control-plane operation —
create/presign, conditional get, list, metadata update, delete, download-URL issuance, version
listing/presign/bind/delete, multipart upload (initiate/introspect/complete/abort), ownership transfer, backend
migration/discovery, and policy/retention-rule administration. The in-process implementation
(`FileStorageLocalClient`, in the impl crate) calls the same services the REST handlers call, under the caller's own
`SecurityContext`. Not implemented: a Level 2 seekable reader/writer that proxies the presign+sidecar-transfer
two-step *inside* the SDK — a caller still `PUT`s/`GET`s bytes against the sidecar itself, over the signed URLs this
trait hands back. See `gears/file-storage/docs/DESIGN.md`'s `sdk-facade` component for the Level 1/2 split.
