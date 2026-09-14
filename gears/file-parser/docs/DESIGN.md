# Technical Design — File Parser

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 External Dependencies](#34-external-dependencies)
  - [3.5 Interactions & Sequences](#35-interactions--sequences)
  - [3.6 Database schemas & tables](#36-database-schemas--tables)
- [4. Additional context](#4-additional-context)
  - [Configuration](#configuration)
  - [Error Mapping](#error-mapping)
- [Appendix](#appendix)
  - [Change Log](#change-log)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

File Parser is a stateless toolkit service gear that acts as a **parsing gateway**: it accepts document uploads (or local file paths), routes each request to the first registered parser plugin that claims the file's extension, and returns structured content. All format-specific extraction logic lives in individual plugin implementations of the `FileParserBackend` trait — the gateway itself has no knowledge of any file format.

Five plugins are shipped with this version, covering PDFs, HTML, spreadsheets, presentations, Word documents, plain text, and common image formats. Additional plugins (e.g. Tika, LibreOffice, IBM Document Understanding) can be added in future without changing the gateway or the REST API.

### 1.2 Architecture Drivers

#### Functional Requirements

- Parse documents in multiple formats (PDF, HTML, XLSX, PPTX, DOCX, images, plain text) into structured blocks
- Preserve headings, paragraphs, lists, tables, code blocks, quotes, page breaks, and inline annotations
- Render structured blocks as Markdown
- Enforce path-traversal security for `parse-local` endpoints
- Report available parsers and their supported extensions via the `/info` endpoint

#### Third-party Library Constraints

`kreuzberg` ≥ 4.8.0 uses Elastic License 2.0 (EL-2.0). The dependency is pinned at `=4.9.4`. An explicit exception is declared in `deny.toml` with rationale: Gears' document parsing is not sold as a standalone competing product. Any upgrade requires re-verifying the exception still applies.

#### Architecture Decisions

- `cpt-cf-file-parser-adr-content-based-type-detection` — [ADR-0001](./ADR/0001-cpt-cf-file-parser-adr-content-based-type-detection.md): routing may be driven by *content* rather than by the caller-supplied filename extension / `Content-Type`, behind the optional `magika` feature. Records the confidence threshold and fallback policy, why detection failure is a startup failure rather than a silent degradation, and the ONNX Runtime constraints that shape the implementation.

### 1.3 Architecture Layers

```
REST API layer       (src/api/)
        ↓
Parser Gateway       (src/domain/service.rs — FileParserService)
        ↓  determines extension; routes to first matching plugin
┌─────────────────────────────────────────────────────────────────────┐
│  FileParserBackend plugins  (src/infra/parsers/)                    │
│                                                                     │
│  PlainTextParser   txt, log, md          (internal)                 │
│  KreuzbergParser   pdf, html, xlsx, pptx (kreuzberg =4.9.4)        │
│  DocxParser        docx                  (docx-rust)                │
│  ImageParser       png, jpg, webp, gif   (internal / base64)        │
│  StubParser        doc, rtf, odt, …      (fallback stub)            │
└─────────────────────────────────────────────────────────────────────┘
        ↓  each plugin returns ParsedDocument (platform IR)
Markdown renderer    (src/domain/markdown.rs)
```

> **Note**: `KreuzbergParser` additionally uses `src/infra/parsers/ir_convert.rs` to convert kreuzberg's `ExtractionResult` into the platform IR. Other plugins produce `ParsedDocument` directly.

## 2. Principles & Constraints

### 2.1 Design Principles

#### Stateless Operation

- [ ] `p1` - **ID**: `cpt-cf-file-parser-principle-stateless`

The gear does not maintain session state. Each request is fully independent. Temporary files are cleaned up after processing.

#### Format-Agnostic API

- [ ] `p1` - **ID**: `cpt-cf-file-parser-principle-format-agnostic`

The REST API is uniform regardless of input format. Format detection (extension from filename or Content-Type header) is performed by the gateway, not by individual plugins. Error handling is consistent across all formats.

#### Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-file-parser-principle-single-backend`

All format-specific extraction logic lives exclusively inside parser plugins. The `FileParserService` gateway contains no format knowledge. Each plugin implements the `FileParserBackend` trait, declares the extensions it handles, and is registered at gear startup. The gateway selects the appropriate plugin at request time.

### 2.2 Constraints

#### File Size Limit

- [ ] `p2` - **ID**: `cpt-cf-file-parser-constraint-file-size`

Maximum file size is configurable via `max_file_size_mb` (default: 100 MB). Enforced by the service layer before any plugin is invoked. Requests exceeding the limit are rejected with HTTP 413.

#### Local Path Security

- [ ] `p1` - **ID**: `cpt-cf-file-parser-constraint-local-path-security`

**ID**: [ ] `p1` `fdd-file-parser-constraint-local-path-security-v1`

<!-- fdd-id-content -->
Local file parsing (`parse-local`) validates paths before any filesystem access:
(a) paths containing `..` components are rejected outright;
(b) the requested path is canonicalized (resolving symlinks);
(c) the canonical path must be a descendant of the mandatory `allowed_local_base_dir`.
The gear fails to start if `allowed_local_base_dir` is missing or unresolvable.
Violations return HTTP 403 Forbidden. Rejected attempts are logged at `warn` level.
<!-- fdd-id-content -->

#### Supported Formats

- [ ] `p2` - **ID**: `cpt-cf-file-parser-constraint-formats`

**ID**: [ ] `p2` `fdd-file-parser-constraint-formats-v1`

<!-- fdd-id-content -->
Supported extensions are determined by the registered plugins. Requests for extensions not claimed by any plugin are rejected with HTTP 400.

Currently supported:
- `PlainTextParser`: `txt`, `log`, `md`
- `KreuzbergParser`: `pdf`, `html`, `htm`, `xlsx`, `xls`, `xlsm`, `xlsb`, `pptx`
- `DocxParser`: `docx`
- `ImageParser`: `png`, `jpg`, `jpeg`, `webp`, `gif`
- `StubParser` (fallback): `doc`, `rtf`, `odt`, `xls`, `xlsx`, `ppt`, `pptx`

**Known limitations of `KreuzbergParser` at kreuzberg 4.9.4**:
- PPTX multi-slide presentations: slides are emitted as `##` headings rather than distinct nodes; `PageBreak` blocks between slides are not produced.
- PPTX tables: structured `Table` blocks are not produced; table cell content is extracted as plain paragraphs.
<!-- fdd-id-content -->

#### kreuzberg Version Pin

- [ ] `p1` - **ID**: `cpt-cf-file-parser-constraint-version-pin`

`kreuzberg` is declared as `=4.9.4` in `Cargo.toml`. This prevents `cargo update` from silently upgrading to a newer release with a different or changed license. An explicit `deny.toml` exception permits Elastic-2.0 for this crate at this version. Any upgrade is a deliberate, reviewed action.

## 3. Technical Architecture

### 3.1 Domain Model

Core types (all in `src/domain/`):

| Type | Description |
|---|---|
| `ParsedDocument` | Top-level result: `id: Option<Uuid>`, `title: Option<String>`, `language: Option<String>` (BCP 47), `meta: ParsedMetadata`, `blocks: Vec<ParsedBlock>` |
| `ParsedMetadata` | `source: ParsedSource`, `original_filename`, `content_type`, `created_at`, `modified_at`, `is_stub: bool` |
| `ParsedSource` | `LocalPath(String)` or `Uploaded { original_name: String }` |
| `ParsedBlock` | Enum: `Heading { level: u8, inlines }`, `Paragraph { inlines }`, `ListItem { level: u8, ordered: bool, blocks }`, `CodeBlock { language, code }`, `Table(TableBlock)`, `Quote { blocks }`, `HorizontalRule`, `Image { alt, title, src }`, `PageBreak` |
| `TableBlock` | `rows: Vec<TableRow>` |
| `TableRow` | `is_header: bool`, `cells: Vec<TableCell>` |
| `TableCell` | `blocks: Vec<ParsedBlock>` (cells may contain nested block content) |
| `Inline` | `Text { text, style: InlineStyle }`, `Link { text, target, style }`, `Code { text, style }` |
| `InlineStyle` | `bold`, `italic`, `underline`, `strike`, `code` (all `bool`) |
| `DocumentBuilder` | Fluent builder for constructing `ParsedDocument`; used by all plugins |

### 3.2 Component Model

#### API Layer

- [ ] `p1` - **ID**: `cpt-cf-file-parser-component-rest`

**ID**: [ ] `p1` `fdd-file-parser-component-rest-v1`

<!-- fdd-id-content -->
REST endpoints: `/file-parser/v1/info`, `/file-parser/v1/upload`, `/file-parser/v1/upload/markdown`, `/file-parser/v1/parse-local`, `/file-parser/v1/parse-local/markdown`
<!-- fdd-id-content -->

#### Parser Gateway

- [ ] `p1` - **ID**: `cpt-cf-file-parser-component-parser-service`

**ID**: [ ] `p1` `fdd-file-parser-component-parser-v1`

<!-- fdd-id-content -->
`FileParserService` (`src/domain/service.rs`) — the parsing gateway. Holds an ordered registry of `FileParserBackend` plugins populated at gear startup, plus an optional `ContentTypeDetector` (see
`docs/ADR/0001-cpt-cf-file-parser-adr-content-based-type-detection.md`). For each request:
0. If a detector is registered (only possible with the gear built with `--features magika`), runs content-based detection on every request — including over a present-but-wrong filename extension / `Content-Type`. A detection at ≥ 0.90 confidence overrides routing **only when its label maps to an extension a registered parser actually claims** — enforced by the gateway itself (`reconcile_extension` re-checks `find_parser_by_extension`), not only by the detector implementation, so a third-party `ContentTypeDetector` that reports an unregistered extension cannot turn a request the hint would have satisfied into `no_parser_available`. A disagreement with the hint is logged at `WARN`. Below that confidence, or when the label maps to no supported extension, or with no detector registered, falls through to step 1 using the hint-derived extension unchanged.
1. Determines the file extension from the filename hint or Content-Type header.
2. Iterates the plugin registry and selects the first plugin whose `supported_extensions()` contains the extension.
3. Returns HTTP 400 if no plugin matches.
4. Delegates to the selected plugin's `parse_bytes` or `parse_local_path` method.

The gateway also enforces file size limits and path-traversal protection. It has no format-specific logic of its own. With no detector registered — the default, and the only possibility without the `magika` feature — steps 1-4 are the entire algorithm and behavior is unchanged from before content-based detection existed.
<!-- fdd-id-content -->

#### Parser Plugins

- [ ] `p1` - **ID**: `cpt-cf-file-parser-component-parser-backend`

**ID**: [ ] `p1` `fdd-file-parser-component-backend-v1`

<!-- fdd-id-content -->
Each plugin implements `FileParserBackend` (`src/domain/parser.rs`) and is registered at gear startup in `src/gear.rs`. Registration order determines priority (first match wins). Plugins currently shipped, in registration order:

| # | Plugin | Handled extensions | Backend library |
|---|---|---|---|
| 1 | `PlainTextParser` | `txt`, `log`, `md` | internal |
| 2 | `KreuzbergParser` | `pdf`, `html`, `htm`, `xlsx`, `xls`, `xlsm`, `xlsb`, `pptx` | `kreuzberg =4.9.4` (Elastic-2.0, `deny.toml` exception) |
| 3 | `DocxParser` | `docx` | `docx-rust` |
| 4 | `ImageParser` | `png`, `jpg`, `jpeg`, `webp`, `gif` | internal (base64 encoding) |
| 5 | `StubParser` | `doc`, `rtf`, `odt`, `xls`, `xlsx`, `ppt`, `pptx` | stub fallback |

Future plugins implement `FileParserBackend` and are added to the `vec![]` in `gear.rs` — no changes to the gateway or REST API are required.

Each plugin produces a `ParsedDocument` using the platform IR (`src/domain/ir.rs`). `KreuzbergParser` additionally uses `result_to_blocks` (`src/infra/parsers/ir_convert.rs`) to convert kreuzberg's `ExtractionResult` into that IR.
<!-- fdd-id-content -->

#### Markdown Renderer

- [ ] `p1` - **ID**: `cpt-cf-file-parser-component-markdown-renderer`

**ID**: [ ] `p1` `fdd-file-parser-component-markdown-v1`

<!-- fdd-id-content -->
`src/domain/markdown.rs` — converts any `ParsedDocument` to Markdown, preserving headings, lists, tables, code blocks, quotes, and inline formatting. Both eager (`render`) and streaming (`render_iter`) modes are supported.
<!-- fdd-id-content -->

### 3.3 API Contracts

#### REST API

| Endpoint | Method | Request body | Response |
|---|---|---|---|
| `/file-parser/v1/info` | GET | — | JSON: `{ "supported_extensions": { "<parser-id>": ["ext", …], … } }` |
| `/file-parser/v1/upload` | POST | `application/octet-stream` + `?filename=` | JSON: `ParsedDocResponseDto` |
| `/file-parser/v1/upload/markdown` | POST | `multipart/form-data` (field `file`) | `text/markdown` stream |
| `/file-parser/v1/parse-local` | POST | JSON `{ "file_path": "…" }` | JSON: `ParsedDocResponseDto` |
| `/file-parser/v1/parse-local/markdown` | POST | JSON `{ "file_path": "…" }` | `text/markdown` stream |

The `/upload` endpoint also accepts `?render_markdown=true` to include rendered Markdown in the JSON response alongside the structured blocks.

Example `/info` response:

```json
{
  "supported_extensions": {
    "plain_text": ["txt", "log", "md"],
    "kreuzberg": ["pdf", "html", "htm", "xlsx", "xls", "xlsm", "xlsb", "pptx"],
    "docx": ["docx"],
    "image": ["png", "jpg", "jpeg", "webp", "gif"],
    "generic_stub": ["doc", "rtf", "odt", "xls", "xlsx", "ppt", "pptx"]
  }
}
```

#### FileParserBackend Trait (Plugin Contract)

Internal Rust trait (`src/domain/parser.rs`) that every parser plugin must implement:

```rust
pub trait FileParserBackend: Send + Sync {
    fn id(&self) -> &'static str;
    fn supported_extensions(&self) -> &'static [&'static str];
    /// `resolved_content_type` is the canonical MIME for the extension the
    /// gateway used to select this backend — which may come from content
    /// detection overriding the file's own extension. When `Some`, use it
    /// instead of re-deriving a MIME from `path`.
    async fn parse_local_path(
        &self,
        path: &Path,
        resolved_content_type: Option<&str>,
    ) -> Result<ParsedDocument, DomainError>;
    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        content_type: Option<&str>,
        bytes: Bytes,
    ) -> Result<ParsedDocument, DomainError>;
}
```

Backends that read a whole local file into memory must do so through
`infra::parsers::bounded_read::read_bounded`, not `tokio::fs::read`: the
service's up-front size check reads `metadata()` and so is not atomic against
the file growing afterwards.

Plugin registration is done at gear startup in `src/gear.rs`. The gateway selects the first registered plugin whose `supported_extensions()` contains the requested extension. Future versions may add a `priority() -> i32` method to allow explicit priority-based selection when multiple plugins claim the same extension.

### 3.4 External Dependencies

| Dependency | Version | License | Role |
|---|---|---|---|
| `kreuzberg` | `=4.9.4` | Elastic-2.0 (exception in `deny.toml`) | Document extraction for PDF, HTML, XLSX, PPTX (used by `KreuzbergParser`) |
| `pdfium` | bundled via kreuzberg | BSD-3 | PDF rendering (bundled, no separate install) |
| `docx-rust` | workspace | MIT | DOCX parsing (used by `DocxParser`) |

### 3.5 Interactions & Sequences

#### Document Upload and Parse

- [ ] `p1` - **ID**: `cpt-cf-file-parser-seq-upload-and-parse`

1. Client uploads document via `POST /file-parser/v1/upload`
2. API layer reads filename from query param and Content-Type from headers; passes raw bytes to `FileParserService`
3. `FileParserService` checks file size against configured limit (default 100 MB); rejects with HTTP 413 if exceeded
4. Gateway determines the file extension: from filename first, then from Content-Type; rejects with HTTP 400 if neither is available
5. Gateway iterates its plugin registry and selects the first plugin whose `supported_extensions()` contains the extension; rejects with HTTP 400 if none match
6. The selected plugin's `parse_bytes` is called with the filename hint, content type, and raw bytes
7. Plugin extracts content and returns a `ParsedDocument` (platform IR)
8. `ParsedDocument` returned to API layer → serialised as JSON
9. For `/upload/markdown`: `MarkdownRenderer` converts `ParsedDocument` to Markdown string (or streams it)

#### Local File Parse

- [ ] `p1` - **ID**: `cpt-cf-file-parser-seq-local-file-parse`

1. Consumer sends `POST /file-parser/v1/parse-local` with JSON body `{ "file_path": "…" }`
2. API layer extracts the path string and passes it to `FileParserService::parse_local`
3. Service validates the path: rejects `..` components (HTTP 403), canonicalizes it, enforces `allowed_local_base_dir` prefix (HTTP 403)
4. Service determines extension from the canonical path; selects the first matching plugin
5. Selected plugin's `parse_local_path` is called with the canonical path
6. Plugin reads the file, extracts content, and returns `ParsedDocument`
7. Response serialised and returned

### 3.6 Database schemas & tables

File Parser is stateless and does not own any database tables or persistent storage. No schema migrations are required.

## 4. Additional context

### Configuration

```yaml
# config/server.yaml (relevant keys)
file_parser:
  max_file_size_mb: 100              # optional; default 100 MB
  allowed_local_base_dir: /data/uploads   # required; gear fails to start if absent
  detection_confidence_threshold: 0.90  # optional; default 0.90. Minimum detector confidence, in
                                      # [0.0, 1.0], at which content detection may override the
                                      # caller's filename / Content-Type hint. Below it the hint
                                      # wins. Configurable because ADR-0001 records 0.90 as an
                                      # initial value rather than a tuned one; the gear fails to
                                      # start on a value outside [0.0, 1.0] instead of clamping.
                                      # Only has effect when a detector is registered.
  magika_intra_op_threads: 2         # optional; leave unset (recommended). Only used with the
                                      # `magika` feature — ONNX Runtime intra-op thread count for
                                      # the detector's single magika::Session. Unset leaves ONNX
                                      # Runtime's own default: measured ~6 threads and ~3.5 ms per
                                      # detection on a 10-core host. Lower it only when detection
                                      # must not contend with other CPU-bound work, and note the
                                      # latency cost: ~5.6 ms at 2, ~9.3 ms at 1. There is no
                                      # session-count setting: magika::Session needs &mut self per
                                      # inference and each session brings its own thread pool and
                                      # model copy, so detection is serialized through one session
                                      # and tuned here.
```

### Error Mapping

| Condition | HTTP status |
|---|---|
| Unsupported file format / no extension | 400 Bad Request |
| No parser available for extension | 400 Bad Request |
| File too large (> configured limit) | 413 Payload Too Large |
| Path traversal attempt | 403 Forbidden |
| Local file not found | 404 Not Found |
| Parser extraction failure | 500 Internal Server Error |

## Appendix

### Change Log

| Date | Version | Author | Changes |
|------|---------|--------|---------|
| 2026-02-09 | 0.1.0 | System | Initial DESIGN for cypilot validation |
| 2026-02-17 | 0.2.0 | Security | Removed `/file-parser/v1/parse-url*` endpoints, HTTP client dependency, and `download_timeout_secs` config. Rationale: SSRF risk (issue #525). |
| 2026-02-17 | 0.3.0 | Security | Added path-traversal protections for `parse-local` endpoints: `..` rejection, path canonicalization, `allowed_local_base_dir` enforcement, symlink-escape prevention, `PathTraversalBlocked` error (HTTP 403). Added constraint `fdd-file-parser-constraint-local-path-security-v1`. |
| 2026-04-29 | 0.4.0 | Engineering | Restructured to match cypilot SDLC DESIGN template. Consolidated four format-specific parsers (`HtmlParser`, `PdfParser`, `XlsxParser`, `PptxParser`) into `KreuzbergParser` backed by `kreuzberg =4.9.4` (Elastic-2.0). Retained `DocxParser`, `ImageParser`, `PlainTextParser`, `StubParser` plugins. Documented domain model, component model, API contracts, and interaction sequences. |
| 2026-04-30 | 0.5.0 | Engineering | Rewrote to be accurate to the full gateway+plugin architecture. Corrected domain model (actual `ParsedBlock` variants, `ParsedDocument` fields, `ParsedSource`). Fixed `FileParserBackend` trait signature. Fixed `/info` response key. Added plugin registration order table. Corrected file size limit (100 MB default). Removed kreuzberg-specific wording from gateway-level descriptions. |
