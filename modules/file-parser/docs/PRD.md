# PRD — File Parser

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [Document Upload](#document-upload)
  - [Format Support](#format-support)
  - [Plugin Extensibility](#plugin-extensibility)
  - [Content Extraction](#content-extraction)
  - [Markdown Rendering](#markdown-rendering)
  - [Local Path Security](#local-path-security)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [Performance](#performance)
  - [Scalability](#scalability)
  - [Reliability](#reliability)
  - [6.1 NFR Exclusions](#61-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [UC-001: Upload and Parse Document](#uc-001-upload-and-parse-document)
  - [UC-002: Parse Local File](#uc-002-parse-local-file)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [Appendix](#appendix)
  - [Change Log](#change-log)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

File Parser provides document parsing and content extraction capabilities for the CyberFabric platform. It is designed as a **parsing gateway with a plugin architecture**: the gateway exposes a stable REST API and routes each request to the appropriate parser plugin based on file extension. Plugins are self-contained implementations of the `FileParserBackend` trait; adding support for a new format or library requires only adding a new plugin — the gateway and REST API are unchanged.

### 1.2 Background / Problem Statement

Platform modules — most notably the Chat Engine and LLM Gateway — need to process user-uploaded documents as grounding material for AI responses. These documents arrive as binary uploads in varying formats (PDF, HTML, spreadsheets, presentations, Word documents, images, plain text).

The module was designed from the start with extensibility in mind: the `FileParserBackend` trait defines the plugin contract, and `FileParserService` acts as the gateway that routes requests to the correct plugin.

The previous implementation had four separate format-specific plugins (`HtmlParser`, `PdfParser`, `XlsxParser`, `PptxParser`) each backed by different libraries (`tl`, `pdf-extract`, `calamine`, `pptx-to-md`), resulting in fragmented logic, inconsistent output quality, and no support for DOCX or images. The current version consolidates the four PDF/HTML/spreadsheet/presentation plugins into a single `KreuzbergParser`, adds `DocxParser` and `ImageParser`, and retains `PlainTextParser` and `StubParser` — all within the same gateway+plugin architecture.

### 1.3 Goals (Business Outcomes)

- Provide a single, unified REST API for document content extraction regardless of input format
- Support a plugin architecture that allows new parser backends to be added without changing the REST API or the gateway
- Return structured content that preserves document semantics (headings, lists, tables, inline annotations)
- Produce Markdown output suitable for injection into LLM prompts
- Enable downstream modules to process documents without understanding format-specific details
- Parse common formats with ≥ 95% accuracy; respond in < 5 s for documents < 10 MB

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Parsed block | A unit of structured document content: `Heading`, `Paragraph`, `ListItem`, `Table`, `CodeBlock`, `Quote`, `HorizontalRule`, `Image`, or `PageBreak` |
| Inline annotation | Span-level styling within text: bold, italic, underline, strikethrough, code, or hyperlink |
| IR (Intermediate Representation) | `ParsedDocument` — the internal data model produced by any parser plugin and consumed by the Markdown renderer |
| Parser plugin | A Rust struct implementing `FileParserBackend`; declares the extensions it handles and contains all format-specific logic |
| Parser gateway | `FileParserService` — routes each request to the correct plugin; contains no format-specific logic |
| kreuzberg | Third-party Rust crate (`=4.9.4`, Elastic-2.0) used by `KreuzbergParser` to handle PDF, HTML, XLSX, and PPTX |
| `parse-local` | Endpoint family that parses files already present on the server filesystem |
| `allowed_local_base_dir` | Mandatory config field constraining which filesystem paths `parse-local` may access |
| Stub parser | `StubParser` — fallback plugin that returns a placeholder document for unsupported legacy formats |

## 2. Actors

### 2.1 Human Actors

#### API User

**ID**: `fdd-file-parser-actor-api-user`

<!-- fdd-id-content -->
**Role**: End user or developer who uploads documents and receives parsed content or Markdown.
**Needs**: Upload a document via REST, receive structured text content or Markdown in the response.
<!-- fdd-id-content -->

### 2.2 System Actors

#### Consumer Module

**ID**: `fdd-file-parser-actor-consumer`

<!-- fdd-id-content -->
**Role**: Internal platform module (e.g., Chat Engine) that calls File Parser programmatically as part of a document-processing workflow.
**Needs**: Reliable structured content extraction from files on the server filesystem (`parse-local`) or from binary payloads.
<!-- fdd-id-content -->

## 3. Operational Concept & Environment

> **Note**: Project-wide runtime, OS, architecture, and lifecycle policy are defined in the root PRD. Only module-specific deviations are documented here.

File Parser runs as a stateless HTTP service within the CyberFabric platform. Each request is fully self-contained: the module accepts an uploaded file (or a local path), routes it to the appropriate plugin, extracts content, and returns the result without persisting any state. Temporary files used by individual plugins are cleaned up after each request.

The module requires an `allowed_local_base_dir` config entry to be set at startup. If the value is missing or unresolvable, the module fails to start.

## 4. Scope

### 4.1 In Scope

- Binary and multipart file upload endpoints returning JSON-structured content
- Markdown rendering endpoint returning extracted content as Markdown
- Local-path parsing endpoints for server-side files
- Format support via registered plugins (see §5 Format Support for the current list)
- Preservation of document structure: headings, paragraphs, lists, tables, code blocks, quotes, page breaks
- Inline annotation extraction: bold, italic, underline, strikethrough, inline code, hyperlinks
- Parser info endpoint listing supported extensions per registered plugin
- Plugin architecture allowing new format backends to be added without API changes

### 4.2 Out of Scope

- OCR for scanned or image-only documents (future enhancement — kreuzberg `ocr` feature + Tesseract; separate PR)
- Document editing or modification
- Long-term document storage
- Format conversion other than Markdown rendering
- URL-based document fetching (removed due to SSRF risk, issue #525)

## 5. Functional Requirements

### Document Upload

- [ ] `p1` - **ID**: `cpt-cf-file-parser-fr-upload`

**ID**: [ ] `p1` `fdd-file-parser-fr-upload-v1`

<!-- fdd-id-content -->
System SHALL support binary file upload (`application/octet-stream` with `?filename=` query param) and multipart form upload (field name `file`). Both SHALL support optional Markdown rendering. System SHALL enforce a configurable maximum file size (default 100 MB); requests exceeding the limit SHALL be rejected with HTTP 413.

**Actors**: `fdd-file-parser-actor-api-user`
<!-- fdd-id-content -->

### Format Support

- [ ] `p1` - **ID**: `cpt-cf-file-parser-fr-formats`

**ID**: [ ] `p1` `fdd-file-parser-fr-formats-v1`

<!-- fdd-id-content -->
The gateway SHALL route each request to the first registered plugin that claims the file's extension. Requests for extensions not claimed by any plugin SHALL be rejected with HTTP 400. Supported extensions are determined by the installed plugins; the gateway has no hardcoded format list.

Currently registered plugins (in priority order):

| Plugin | Extensions |
|---|---|
| `PlainTextParser` | `txt`, `log`, `md` |
| `KreuzbergParser` | `pdf`, `html`, `htm`, `xlsx`, `xls`, `xlsm`, `xlsb`, `pptx` |
| `DocxParser` | `docx` |
| `ImageParser` | `png`, `jpg`, `jpeg`, `webp`, `gif` |
| `StubParser` (fallback) | `doc`, `rtf`, `odt`, `xls`, `xlsx`, `ppt`, `pptx` |

**Known limitations of `KreuzbergParser` at kreuzberg 4.9.4**:
- PPTX multi-slide presentations: slides are emitted as headings rather than distinct nodes; `PageBreak` blocks between slides are not produced.
- PPTX tables: structured `Table` blocks are not produced; table cell content is extracted as paragraphs.

**Actors**: `fdd-file-parser-actor-api-user`
<!-- fdd-id-content -->

### Plugin Extensibility

- [ ] `p2` - **ID**: `cpt-cf-file-parser-fr-plugin-extensibility`

**ID**: [ ] `p2` `fdd-file-parser-fr-plugin-extensibility-v1`

<!-- fdd-id-content -->
The system SHALL support registration of additional parser plugins without requiring changes to the REST API or the gateway. A new plugin SHALL only need to implement the `FileParserBackend` trait and be added to the plugin registry in `src/module.rs`. The new plugin's extensions SHALL automatically appear in the `/info` response and be routable via `/upload` and `/parse-local`.

**Actors**: `fdd-file-parser-actor-consumer`
<!-- fdd-id-content -->

### Content Extraction

- [ ] `p1` - **ID**: `cpt-cf-file-parser-fr-extraction`

**ID**: [ ] `p1` `fdd-file-parser-fr-extraction-v1`

<!-- fdd-id-content -->
System SHALL extract text content and preserve document structure (headings, paragraphs, lists, tables, code blocks, quotes, page breaks). Inline text annotations (bold, italic, underline, strikethrough, code, hyperlinks) SHALL be preserved in the parsed output.

**Actors**: `fdd-file-parser-actor-api-user`
<!-- fdd-id-content -->

### Markdown Rendering

- [ ] `p1` - **ID**: `cpt-cf-file-parser-fr-markdown`

**ID**: [ ] `p1` `fdd-file-parser-fr-markdown-v1`

<!-- fdd-id-content -->
System SHALL convert the extracted document structure to Markdown format, preserving headings, lists, formatting, tables, and code blocks. Markdown output SHALL be available both as a field in the JSON response (`?render_markdown=true`) and as a streaming `text/markdown` response from the dedicated `/markdown` endpoints.

**Actors**: `fdd-file-parser-actor-api-user`
<!-- fdd-id-content -->

### Local Path Security

- [ ] `p1` - **ID**: `cpt-cf-file-parser-fr-local-path-security`

**ID**: [ ] `p1` `fdd-file-parser-fr-local-path-security-v1`

<!-- fdd-id-content -->
System SHALL reject local file paths containing `..` traversal components. System SHALL require a mandatory `allowed_local_base_dir` configuration; the module SHALL fail to start if this field is missing or the path cannot be resolved. System SHALL canonicalize the requested path (resolving symlinks) and reject paths that do not fall under the base directory. Rejected requests SHALL return HTTP 403 and be logged at `warn` level.

**Actors**: `fdd-file-parser-actor-api-user`, `fdd-file-parser-actor-consumer`
<!-- fdd-id-content -->

## 6. Non-Functional Requirements

### Performance

- [ ] `p1` - **ID**: `cpt-cf-file-parser-nfr-response-time`

**ID**: [ ] `p1` `fdd-file-parser-nfr-response-time-v1`

<!-- fdd-id-content -->
System SHALL respond in < 5 s for documents < 10 MB and < 30 s for documents up to the configured size limit.
<!-- fdd-id-content -->

### Scalability

- [ ] `p1` - **ID**: `cpt-cf-file-parser-nfr-concurrency`

**ID**: [ ] `p1` `fdd-file-parser-nfr-concurrency-v1`

<!-- fdd-id-content -->
System SHALL support 100 concurrent parsing requests.
<!-- fdd-id-content -->

### Reliability

- [ ] `p1` - **ID**: `cpt-cf-file-parser-nfr-availability`

**ID**: [ ] `p1` `fdd-file-parser-nfr-availability-v1`

<!-- fdd-id-content -->
System SHALL maintain 99.9% uptime SLA.
<!-- fdd-id-content -->

### 6.1 NFR Exclusions

| NFR | Reason excluded |
|-----|-----------------|
| OCR accuracy SLA | OCR is out of scope for this version |

## 7. Public Library Interfaces

### 7.1 Public API Surface

REST endpoints exposed by the module:

| Endpoint | Method | Description |
|---|---|---|
| `/file-parser/v1/info` | GET | List all registered plugins and their supported extensions |
| `/file-parser/v1/upload` | POST | Upload and parse a document; returns JSON structured blocks |
| `/file-parser/v1/upload/markdown` | POST | Upload and parse a document; streams Markdown |
| `/file-parser/v1/parse-local` | POST | Parse a server-local file; returns JSON structured blocks |
| `/file-parser/v1/parse-local/markdown` | POST | Parse a server-local file; streams Markdown |

### 7.2 External Integration Contracts

No external service contracts. The module depends only on in-process Rust libraries (`kreuzberg`, `docx-rust`) and the host filesystem for `parse-local` endpoints.

## 8. Use Cases

### UC-001: Upload and Parse Document

**ID**: [ ] `p1` `fdd-file-parser-usecase-upload-parse-v1`

<!-- fdd-id-content -->
User uploads a document in a supported format and receives parsed content as structured blocks and optional Markdown.

**Actors**: `fdd-file-parser-actor-api-user`
**Preconditions**: Document is in a format claimed by a registered plugin and does not exceed the configured size limit.
**Postconditions**: Structured blocks (headings, paragraphs, tables, etc.) returned in JSON or Markdown.
<!-- fdd-id-content -->

### UC-002: Parse Local File

**ID**: [ ] `p1` `fdd-file-parser-usecase-local-parse-v1`

<!-- fdd-id-content -->
Consumer module requests parsing of a file already present on the server filesystem.

**Actors**: `fdd-file-parser-actor-consumer`
**Preconditions**: File exists under `allowed_local_base_dir` and has an extension claimed by a registered plugin.
**Postconditions**: Structured content returned; path traversal attempts rejected with HTTP 403.
<!-- fdd-id-content -->

## 9. Acceptance Criteria

| Criterion | Condition |
|---|---|
| Supported formats parsed | PDF, HTML, XLSX, PPTX, DOCX upload returns non-empty structured blocks |
| Plain text parsed | TXT/MD upload returns non-empty paragraph blocks |
| Image parsed | PNG/JPG upload returns a document with an `Image` block or base64 content |
| Markdown rendering | Upload-markdown endpoint returns valid Markdown preserving headings and tables |
| Path traversal rejection | Requests with `..` components return HTTP 403 |
| Unknown format rejection | Upload of unsupported extension returns HTTP 400 |
| Parser info endpoint | `/info` returns all registered plugins with their correct extension lists |
| File size limit | Upload exceeding configured limit returns HTTP 413 |
| Plugin extensibility | Adding a new plugin to `module.rs` causes it to appear in `/info` without other code changes |

## 10. Dependencies

| Dependency | Type | Notes |
|---|---|---|
| `kreuzberg =4.9.4` | External library (Elastic-2.0, exception in `deny.toml`) | Pinned with `=` to prevent silent upgrades; license exception documented in `deny.toml`. Used by `KreuzbergParser`. |
| `pdfium` | Bundled native library | Included via `bundled-pdfium` feature; no separate install required |
| `docx-rust` | External library (MIT) | Used by `DocxParser` for DOCX extraction |
| Host filesystem | Runtime | Required for `parse-local` endpoints |
| modkit HTTP framework | Internal | REST endpoint registration and body-size enforcement |

## 11. Assumptions

- All registered parser plugins are stateless and safe to share across concurrent requests.
- The `bundled-pdfium` feature provides a sufficiently up-to-date PDFium for production PDF parsing.
- The configured `max_file_size_mb` (default 100 MB) is sufficient for current use cases; revision requires an NFR update.
- Consumer modules calling `parse-local` are trusted to supply valid paths within `allowed_local_base_dir`.
- `kreuzberg =4.9.4` will remain available on crates.io for the foreseeable future; any upgrade is a deliberate, reviewed action.

## 12. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| kreuzberg Elastic-2.0 license restricts future use | Low | High | Version pinned with `=`; upgrade requires explicit license review; other plugins do not use EL-2.0 |
| PPTX multi-slide / table limitations persist in future kreuzberg releases | Medium | Low | Tests marked `#[ignore]` with strict assertions; re-evaluate on upgrade |
| PDFium bundled binary lags security patches | Low | Medium | Track pdfium releases; update via kreuzberg upgrade when license permits |
| Large document OOM under concurrency | Low | Medium | Configurable size limit enforced before any plugin is invoked; monitor memory usage in production |
| Plugin registration order conflict (two plugins claim same extension) | Low | Low | First-match semantics are deterministic; document registration order in `module.rs` |
| `docx-rust` does not support all DOCX features | Medium | Low | Known limitation; `DocxParser` extracts text and basic structure; rich formatting may be lost |

## Appendix

### Change Log

| Date | Version | Author | Changes |
|------|---------|--------|---------|
| 2026-02-09 | 0.1.0 | System | Initial PRD for cypilot validation |
| 2026-02-17 | 0.2.0 | Security | Removed URL parsing capability (use case `fdd-file-parser-usecase-url-parse-v1`, FR `fdd-file-parser-fr-url-v1`). Rationale: SSRF vulnerability (issue #525). |
| 2026-02-17 | 0.3.0 | Security | Added FR `fdd-file-parser-fr-local-path-security-v1` — path-traversal protections for `parse-local`. Rationale: prevent arbitrary file read via path traversal (issue #525). |
| 2026-04-29 | 0.4.0 | Engineering | Restructured to match cypilot SDLC PRD template. Consolidated four format-specific plugins into `KreuzbergParser` backed by `kreuzberg =4.9.4` (Elastic-2.0). Added `DocxParser` (DOCX), `ImageParser` (PNG/JPG/WebP/GIF), `PlainTextParser` (TXT/MD/LOG). Added inline annotation extraction. Documented kreuzberg 4.9.4 PPTX limitations. |
| 2026-04-30 | 0.5.0 | Engineering | Rewrote to accurately describe the full gateway+plugin architecture. Corrected Out of Scope (DOCX and images are in scope). Corrected file size limit (100 MB default). Updated Format Support FR to list all registered plugins in priority order. Added Plugin Extensibility FR. Corrected Acceptance Criteria, Dependencies, Assumptions, and Risks to be plugin-generic rather than kreuzberg-specific. Removed incorrect NFR exclusion for DOCX/image SLA. |
