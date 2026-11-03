# file-parser SDK

Transport-agnostic public API for the `file-parser` gear: the `FileParserClientV1`
trait, request/response models, and error types. Consumers resolve the client via
`ClientHub::get::<dyn FileParserClientV1>()` — no dependency on `file-parser`'s
implementation crate (and its `kreuzberg` document-parsing dependency)
is needed.

See `../QUICKSTART.md`, `../docs/`, and `../file-parser/README.md` for the gear overview.
