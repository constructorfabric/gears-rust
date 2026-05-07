# cf-file-storage-sdk

SDK crate for the FileStorage module. Mirrors `rust-traits.md` and exposes the
`FileStorageClient` async trait registered into ModKit's `ClientHub`.

P1 surface only. `put_file` is intentionally omitted (P3-deferred); see
`modules/file-storage/docs/rust-traits.md` and ADR-0004.
