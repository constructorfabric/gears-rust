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

Eight plugins are shipped with this version, covering PDFs, HTML, spreadsheets, presentations, Word documents, plain text, common image formats, and a stub for legacy formats. Spreadsheets, HTML, PPTX and DOCX are read by the crate's own code; PDF is read through libpdfium, which the build embeds. Additional plugins (e.g. Tika, LibreOffice, IBM Document Understanding) can be added in future without changing the gateway or the REST API.

### 1.2 Architecture Drivers

#### Functional Requirements

- Parse documents in multiple formats (PDF, HTML, XLSX, PPTX, DOCX, images, plain text) into structured blocks
- Preserve headings, paragraphs, lists, tables, code blocks, quotes, page breaks, and inline annotations
- Render structured blocks as Markdown
- Enforce path-traversal security for `parse-local` endpoints
- Report available parsers and their supported extensions via the `/info` endpoint

#### Third-party Library Constraints

Every input document is attacker-controlled. The parsers must not size anything from counts or dimensions the file declares, and their work must stay linear in the input. General-purpose libraries were tried and dropped for this reason: calamine allocates from declared sizes and decodes every formula, and html5ever has spec algorithms that are super-linear on hostile input. Spreadsheets, HTML, PPTX and DOCX are therefore read with small dependencies only (`zip`, `quick-xml`, `cfb`, `encoding_rs`).

PDF is the exception: it is read through libpdfium, a native library that `build.rs` downloads from bblanchon/pdfium-binaries at a pinned build, verifies by SHA-256 and embeds into the binary. libpdfium is BSD-3-Clause and contains third-party components under their own licences; binaries built with the crate must carry `file-parser/THIRD_PARTY_NOTICES.md`. libpdfium is not a Rust crate, so `cargo deny` does not check it.

#### Architecture Decisions

- `cpt-cf-file-parser-adr-content-based-type-detection` — [ADR-0001](./ADR/0001-cpt-cf-file-parser-adr-content-based-type-detection.md): routing may be driven by *content* rather than by the caller-supplied filename extension / `Content-Type`, behind the optional `magika` feature. Records the confidence threshold and fallback policy, why detection failure is a startup failure rather than a silent degradation, and the ONNX Runtime constraints that shape the implementation.
- `cpt-cf-file-parser-adr-own-document-parsers` — [ADR-0002](./ADR/0002-cpt-cf-file-parser-adr-own-document-parsers.md): file-parser reads spreadsheets, HTML, PPTX and DOCX with its own parsers on zip, quick-xml, cfb and encoding_rs, and PDF through a directly bound, checksum-pinned libpdfium, instead of kreuzberg and docx-rust. Records why xberg, calamine and html5ever were rejected and the PDF engine's timeout limitation.
- `cpt-cf-file-parser-adr-own-html-parser` — [ADR-0003](./ADR/0003-cpt-cf-file-parser-adr-own-html-parser.md): why HTML is read by the crate's own linear tokenizer and tree builder, not html5ever, with timings of both on hostile inputs from html5ever's issue tracker.

### 1.3 Architecture Layers

```
REST API layer       (src/api/)
        ↓
Parser Gateway       (src/domain/service.rs — FileParserService)
        ↓  determines extension; routes to first matching plugin
┌─────────────────────────────────────────────────────────────────────┐
│  FileParserBackend plugins  (src/infra/parsers/)                    │
│                                                                     │
│  PlainTextParser    txt, log, md             (internal)             │
│  SpreadsheetParser  xlsx, xls, xlsm, xlsb    (own readers, cfb)     │
│  HtmlParser         html, htm                (own tokenizer)        │
│  PptxParser         pptx                     (ooxml)                │
│  PdfParser          pdf                      (libpdfium)            │
│  DocxParser         docx                     (ooxml)                │
│  ImageParser        png, jpg, webp, gif      (internal / base64)    │
│  StubParser         doc, rtf, odt, …         (fallback stub)        │
│                                                                     │
│  shared: ooxml (ZIP package reader), inlines, limits, bounded_read  │
└─────────────────────────────────────────────────────────────────────┘
        ↓  each plugin returns ParsedDocument (platform IR)
Markdown renderer    (src/domain/markdown.rs)
```

> **Note**: Every plugin produces `ParsedDocument` directly. The shared modules in `src/infra/parsers/` are: `ooxml` (ZIP package reader for DOCX, PPTX and XLSB/XLSX parts, with `ParseLimits` on every part it reads), `inlines` (inline runs and link targets), `limits` (`ParseLimits` and the document-wide text, table-cell and nesting budgets), and `bounded_read` (bounded read of a local file).

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
- `SpreadsheetParser`: `xlsx`, `xls`, `xlsm`, `xlsb`
- `HtmlParser`: `html`, `htm`
- `PptxParser`: `pptx`
- `PdfParser`: `pdf`
- `DocxParser`: `docx`
- `ImageParser`: `png`, `jpg`, `jpeg`, `webp`, `gif`
- `StubParser` (fallback): `doc`, `rtf`, `odt`, `xls`, `xlsx`, `ppt`, `pptx` (`xls`, `xlsx` and `pptx` are handled by the earlier parsers)

**Current limitations**:
- No OCR. Image-only (scanned) PDFs produce no text. Invisible text layers that OCR tools add to PDFs are read.
- PDF structure (headings, lists, paragraphs, columns) is found heuristically from glyph size and position, and can be wrong.
- Tables are not reconstructed from PDFs; table text comes out as lines.
- PDF superscripts (footnote markers, exponents) split their paragraph: the raised text becomes a paragraph of its own between the two halves.
- PDF running headers and footers are only recognised on upright text; on pages whose text is drawn rotated they are kept on every page.
- PDF text tilted about 3-10° from the rest of its page (a stamp, say) may break into fragments that mix with the lines around it.
- PPTX SmartArt text comes out in the order of the diagram's data part, which can differ from the order the diagram shows it in.
- DOCX and PPTX notes and comments, DOCX headers and footers, and PPTX footer, date and slide-number placeholders are not extracted.
<!-- fdd-id-content -->

#### libpdfium Version Pin

- [ ] `p1` - **ID**: `cpt-cf-file-parser-constraint-version-pin`

libpdfium is pinned to one build of bblanchon/pdfium-binaries (`PDFIUM_BUILD = "7961"` in `build.rs`), with a SHA-256 digest per target archive. `build.rs` refuses an archive whose digest does not match. `pdfium-render` is pinned at `=0.9.5` with the `pdfium_7881` feature, the highest feature not above the pinned build; a newer feature asks for symbols the build lacks and binding fails. Moving to another pdfium build means changing `PDFIUM_BUILD` and the digest table in `build.rs`, the `pdfium_NNNN` feature in the workspace `Cargo.toml`, and regenerating the e2e goldens (`testing/e2e/suites/file_parser/generate_file_parser_golden.py`) in the same change.

Targets without a pinned archive (musl included: a static binary cannot `dlopen` the shared library) build without a bundled library and need `FILE_PARSER_PDFIUM_RUNTIME_LIB` at runtime.

#### Parse Limits

- [ ] `p1` - **ID**: `cpt-cf-file-parser-constraint-parse-limits`

The spreadsheet, HTML, PPTX, PDF and DOCX parsers enforce `ParseLimits` (`src/infra/parsers/limits.rs`, public, `#[non_exhaustive]`). A document over a limit fails with a parse error that names the limit. The gear uses the defaults; each parser accepts other values through `with_limits`.

| Field | Default | Bounds |
|---|---|---|
| `max_zip_entries` | 10 000 | Entries in a ZIP container |
| `max_entry_bytes` | 64 MiB | Inflated size of one ZIP entry |
| `max_total_uncompressed_bytes` | 500 MiB | Inflated size of all entries of one container, declared and read |
| `max_compression_ratio` | 100 | Inflated / compressed ratio above which an entry is treated as a ZIP bomb |
| `ratio_check_floor_bytes` | 1 MiB | Entries smaller than this skip the ratio check |
| `max_nesting_depth` | 1024 | Element nesting in XML parts (HTML keeps its own, smaller caps and flattens deeper content) |
| `max_table_cells` | 100 000 | Table cells in the whole document; each table is charged rows x columns |
| `max_table_columns` | 16 384 | Columns in one table |
| `max_sheets` | 1 000 | Sheets in a workbook, or slides in a presentation |
| `max_html_nodes` | 500 000 | Elements and text nodes of one HTML document |
| `max_pdf_pages` | 2 000 | Pages of one PDF |
| `max_pdf_chars_per_page` | 200 000 | Characters pdfium reports on one PDF page |
| `pdf_timeout` | 60 s | Wall-clock time for one PDF once the PDF engine is taken; also the longest wait for the engine |
| `max_text_bytes` | 100 MiB | Text extracted from one document, plus 64 bytes for each block, inline run and PDF line, so a document of millions of tiny pieces is bounded too |

A table (or a whole sheet) that does not fit the remaining cell budget is replaced by a note paragraph instead of failing the document; the note is charged to `max_text_bytes` like any paragraph, its full text included. A nested table counts the cells its enclosing tables have built so far. The paragraphs that fill table cells (each spreadsheet cell, and the empty cells padded into a grid) are charged their text but not the per-block cost: their number is bounded by `max_table_cells`.

The `ooxml` package reader applies the ZIP limits to every part it reads; parts that are never read (media, embedded objects) are not checked. Relationship targets that escape the package root are dropped. XML is read without DTD entity expansion: only the predefined entities and numeric references are resolved.

pdfium is not thread-safe across documents, so all PDF work runs on one dedicated thread that owns the pdfium binding. Documents queue for it and their callers wait asynchronously. At most 64 documents wait; further ones, and documents that wait longer than `pdf_timeout`, are rejected as busy (429 with `Retry-After`). `pdf_timeout` is checked between pdfium calls only; a single call cannot be interrupted, so a hostile PDF can keep the engine busy past the timeout. The engine's part of the answer still comes within `pdf_timeout` of waiting plus `pdf_timeout` and 2 s of reading (122 s with the defaults); arranging the lines into headings, lists and paragraphs then runs on the blocking pool, bounded by the page, character and text limits. The thread is not one of Tokio's blocking-pool threads, so runtime shutdown does not wait for it.

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

| # | Plugin | Id | Handled extensions | Backend |
|---|---|---|---|---|
| 1 | `PlainTextParser` | `plain_text` | `txt`, `log`, `md` | internal |
| 2 | `SpreadsheetParser` | `spreadsheet` | `xlsx`, `xls`, `xlsm`, `xlsb` | own readers: XLSX/XLSM XML, XLSB binary records, BIFF8/BIFF5 `.xls` via `cfb` |
| 3 | `HtmlParser` | `html` | `html`, `htm` | own linear tokenizer and small tree builder |
| 4 | `PptxParser` | `pptx` | `pptx` | `ooxml` package reader |
| 5 | `PdfParser` | `pdf` | `pdf` | libpdfium through `pdfium-render` |
| 6 | `DocxParser` | `docx` | `docx` | `ooxml` package reader |
| 7 | `ImageParser` | `image` | `png`, `jpg`, `jpeg`, `webp`, `gif` | internal (base64 encoding) |
| 8 | `StubParser` | `generic_stub` | `doc`, `rtf`, `odt`, `xls`, `xlsx`, `ppt`, `pptx` | stub fallback |

Future plugins implement `FileParserBackend` and are added to the `vec![]` in `gear.rs` — no changes to the gateway or REST API are required.

Each plugin produces a `ParsedDocument` using the platform IR (`src/domain/ir.rs`). Output per format:

- **Plain text** (`.txt`, `.log`, `.md`): the text as verbatim code blocks, so nothing in it is read as Markdown or HTML; `.md` blocks get the `markdown` language, others none. `\r\n` becomes `\n`; everything else, blank lines and indentation included, is kept. Text up to 16 KiB (`CODE_BLOCK_BYTES`) is one block. Longer text is cut at the last blank line that keeps the block within 16 KiB; a stretch without one is cut at the last line break within 16 KiB, a longer line is kept whole up to 64 KiB and cut at a character boundary past that. The blank lines at a cut stay at the end of the block before it, past the 16 KiB, so the blocks put together give back the text with `\r\n` turned into `\n`. Only the first 64 KiB of the remaining text are looked at for each cut, so a long line costs no more than its length. The text is decoded and split on the blocking pool, like the other formats. Each block is charged its text plus the per-block cost to `max_text_bytes`. Whitespace-only text gives no blocks.
- **Spreadsheets**: one level-2 heading per sheet (the sheet name) and one table over the bounding box of its non-empty cells; the first row is the header. Hidden and very hidden sheets are read too, and their heading says so: `Name (hidden)`, `Name (very hidden)`. Cells show values as Excel displays them; formulas contribute their cached result.
- **HTML**: headings, paragraphs, styled runs, links (`http`, `https`, `mailto`, `ftp`, `tel` and relative targets), lists, tables, quotes, preformatted text as code, rules and images. Scripts, styles, form controls, embedded media and hidden elements are skipped. The IR has only block images, so an `<img>` inside a paragraph splits it and an image inside a link loses the link. The encoding is taken from a byte order mark, the `Content-Type` charset or a `<meta>` declaration, else UTF-8 or Windows-1252.
- **PPTX**: slides in presentation order. Each slide becomes a level-2 heading (its title placeholder, or "Slide N"), then its shapes: paragraphs, list items where a bullet is set on the paragraph or inherited from the layout and master, tables, pictures with their description, and SmartArt text. Slides are separated by page breaks. Speaker notes, comments, charts and slide furniture (footer, date and slide-number placeholders) are left out.
- **PDF**: characters are read one by one with position, size, direction and font style. Lines are built per baseline direction, so rotated text and rotated pages read upright, and slightly skewed lines stay together; right-to-left runs are put back into logical order. Headings come from font size (up to three levels), plus list items, horizontal rules and paragraphs; words hyphenated across lines are joined. Running headers and footers are kept only at their first occurrence and page numbers are dropped; two-column text is read column by column. Pages are separated by page breaks. The title comes from the Info dictionary.
- **DOCX**: headings from outline levels or heading styles, list items from numbering, code blocks and quotes from styles, styled runs and external links. Tracked changes read as accepted. Tables keep their grid (spans padded, merged cells empty); a table nested in a cell is flattened into the cell's paragraphs. Pictures with a description become images; text boxes follow their paragraph. Notes, comments, headers and footers are not extracted.
<!-- fdd-id-content -->

#### Markdown Renderer

- [ ] `p1` - **ID**: `cpt-cf-file-parser-component-markdown-renderer`

**ID**: [ ] `p1` `fdd-file-parser-component-markdown-v1`

<!-- fdd-id-content -->
`src/domain/markdown.rs` — converts any `ParsedDocument` to Markdown, preserving headings, lists, tables, code blocks, quotes, and inline formatting. Both eager (`render`) and streaming (`render_iter`) modes are supported.

Where Markdown has no syntax of its own, the output uses inline HTML or a fixed convention:

- Underlined text is wrapped in `<u>…</u>`.
- Line breaks inside a table cell become `<br/>`; `|` in cells is written as `\|`, inside code too (GFM removes that backslash before it reads the cell). Readers agree on that only when the pipe follows an odd number of backslashes: `\\|` is kept whole by comrak and loses a backslash in pulldown-cmark. So a pipe after an even run of backslashes (none, or escaped backslashes of text or code) gets one more, and a pipe after an odd run is left alone, since a table nested in the cell already escaped it. Backslashes are not doubled otherwise (inside code a doubled one would show doubled), so a `\|` in code in a cell shows as `|`.
- Every line of a list item after the first is indented to the item's content (the marker's width), and a nested item starts at its parent's content, so a code block in an item stays inside it. Leading blank lines of the item's content are dropped, since a blank line right after the marker would end the item. A table, a nested item or a quote in a list item is followed by a blank line, so the next block is not read as a row or as their continuation, and so are a paragraph or image followed by another one, which would otherwise join into one paragraph; such a list becomes loose. A rule or page break in a list item is written as `***`: `- ---` would be a rule in place of the item, and `---` under a paragraph line a heading underline. In a quote, every line gets `> `, also after a lone `\r`.
- A table without a header row gets an empty header row; its first row stays a data row.
- Link and image destinations that contain whitespace, parentheses, angle brackets, backslashes or control characters are written in angle brackets, with `<`, `>` and control characters percent-encoded and `\` written as `&#92;` (before the `&amp;` written for a `&`, a backslash would escape it). `&` is written as `&amp;` in every destination, so a renderer that decodes entity references gets the target back unchanged. Image titles use `&quot;`, `&#92;` and `&amp;` the same way. An image without a source but with a title gets `<>` as its destination; otherwise the title would be read as the destination and the rest of it as Markdown.

Document text (text runs, link text, image alt, the title and the metadata values) is escaped just enough that it cannot form a link, an image, raw HTML or code, with CommonMark backslash escapes, which leave the text readable as it is (the output is mostly read raw, by language models): `[` is written as `\[` and a backtick as `` \` ``, and `<` as `\<` where it could start raw HTML or an autolink: before an ASCII letter or digit, before ASCII punctuation other than `<>()[]\,;:@"` (which covers `/`, `!`, `?` and the characters an email address can start with), and at the end of the text, where the next run follows. `a < b` stays as is.

- `]` is escaped (`\]`) only in link text and image alt, where it would close the brackets early. Elsewhere it has no `[` to close: every `[` of the text is escaped, so text cannot open a link, an image, a reference definition or a footnote. The renderer's own `[` is closed by its own `](`, except that a `]` inside a code span in link text would end the label of a link reference definition (`` [`a]: b`](…) `` at the start of a line would vanish): link text in code style that contains `]` or starts with `^` is written as escaped text, without the code style. A `^` at the start of link text or image alt is escaped too: comrak reads `[^x\]: y` at the start of a line as a footnote definition, ignoring the escape before `]`.
- `\` is doubled only where it would escape or break what follows: before ASCII punctuation (including another backslash and the escapes above, so `\[` in the document becomes `\\\[`), before a line ending (a hard line break) and at the end of the text, where the renderer's own markers follow. `C:\Users` stays as is.
- `~` is escaped (`\~`) in every run of three or more, wherever it is, and in a run at either end of the text, so no line can start with `~~~` and open a code block, even after a list or quote marker or next to the renderer's own `~~` strikethrough markers. The whole run is escaped, not just its first `~`: pulldown-cmark reads `~~\~~x~~` as no strikethrough at all.
- `!` at the end of the text is escaped (`\!`) when a link follows directly, since `![` would make the link an image. Other `!` stay as is.
- `>` at the end of underlined text is escaped (`\>`): followed by the renderer's `</u>` after a line break it would be a quote holding an HTML block, where escapes are not read.
- `&` is written as `&amp;` when it would start a character reference (`&name;`, `&#digits;`, `&#xhex;`) or ends the text, where the next run could complete one (`AT&` then `lt;T`), so `AT&T` stays as is and `&lt;` in the document reads back as `&lt;`. It keeps the entity, the form destinations and image titles use as well.

Spaces and tabs at the start of each line of a paragraph or heading are dropped (not in code): renderers do not show them, and four columns of them at the start of a block or after a blank line would make an indented code block. A run of `#` that ends the first line of a heading or the title, alone or after a space, gets its first `#` escaped (`# Issue \#`); Markdown would take it for the closing sequence and drop it.

Line breaks in link text and image alt are written as spaces: inside the brackets they could end the paragraph or start a block, and the destination and title would then be read as Markdown. Other emphasis characters (`*`, `_`, `#` and so on) are not escaped, so document text can still produce emphasis or, at the start of a line, a block marker; an escaped character is punctuation like the one it stands for, so it does not change where emphasis starts or ends. GFM extended autolinks are not prevented either: `www.…`, `http://…`, `https://…` and email addresses in text still become links, but only to http, https or mailto targets and with the visible text equal to the target.

Code is written unescaped, inside delimiters chosen so that it cannot end early: a code span uses one backtick more than the longest run of backticks in its text, with a space inside the delimiters when the text starts or ends with a backtick, and with line breaks written as spaces; code that directly follows code (an inline code run, or text in code style, with nothing in between) joins the same span, because two spans side by side would touch as one longer run of backticks that closes neither; a code block uses a backtick fence at least three long and longer than any run of backticks in the code. A code block language is written only when it consists of `[A-Za-z0-9_+.#-]`; otherwise the fence has no info string.

The plain-text parser emits code blocks, so a `.md`, `.log` or `.txt` file is rendered as its source inside a fence its content cannot close, with no escaping; its Markdown syntax (links, code fences, HTML) shows as literal characters, not as formatting. Eager and streaming rendering give the same output.

The output is untrusted: a consumer that renders it to HTML must sanitise the result, as it would for any user-supplied Markdown.
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
    "spreadsheet": ["xlsx", "xls", "xlsm", "xlsb"],
    "html": ["html", "htm"],
    "pptx": ["pptx"],
    "pdf": ["pdf"],
    "docx": ["docx"],
    "image": ["png", "jpg", "jpeg", "webp", "gif"],
    "generic_stub": ["doc", "rtf", "odt", "xls", "xlsx", "ppt", "pptx"]
  }
}
```

The map is built from each registered parser's `id()` and `supported_extensions()`; key order is not fixed.

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
| `zip` | 9 (deflate via `flate2` / `zlib-rs`) | MIT | ZIP containers: DOCX, PPTX, XLSX, XLSM, XLSB |
| `quick-xml` | 0.42 (`escape-html`) | MIT | XML parts; HTML5 named character references |
| `cfb` | 0.16 | MIT | OLE compound files (legacy `.xls`) |
| `encoding_rs` | 0.8 | (Apache-2.0 OR MIT) AND BSD-3-Clause | HTML and legacy `.xls` text encodings |
| `pdfium-render` | `=0.9.5` (`pdfium_7881`, `thread_safe`) | MIT OR Apache-2.0 | Bindings to libpdfium |
| `tempfile` | 3 | MIT OR Apache-2.0 | Private directory the embedded libpdfium is written to |
| libpdfium | `chromium/7961`, embedded | BSD-3-Clause, plus bundled third-party components | PDF engine; see `file-parser/THIRD_PARTY_NOTICES.md` |
| `flate2` (build) | 1 | MIT OR Apache-2.0 | Unpacks the pdfium archive |
| `tar` (build) | 0.4 | MIT OR Apache-2.0 | Unpacks the pdfium archive |
| `cf-gears-toolkit-security` (build) | workspace | Apache-2.0 | SHA-256 of the pdfium archive, through the crypto facade |

`build.rs` downloads the pdfium archive with `curl` from `https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/7961/`, verifies it, and extracts only the library file, which is embedded with `include_bytes!`. At runtime the library is written to a private temporary directory and bound once per process. Build inputs: `FILE_PARSER_PDFIUM_ARCHIVE` (local archive, still verified), `FILE_PARSER_PDFIUM_LIB` (prebuilt library, not verified), `FILE_PARSER_PDFIUM_CACHE_DIR` (archive cache), `CARGO_NET_OFFLINE=true` (no download), `DOCS_RS` (nothing bundled). At runtime `FILE_PARSER_PDFIUM_RUNTIME_LIB` loads a library from a path instead of the embedded one. Digests per target:

| Target | Archive | SHA-256 |
|---|---|---|
| Linux x86_64 (gnu) | `pdfium-linux-x64.tgz` | `019665c8877d46fe65f625f80fd714ab07aac68554b0636acf2a2adf9288adb2` |
| Linux aarch64 (gnu) | `pdfium-linux-arm64.tgz` | `974107999784a438149605024475d42d80dd306799d90e1af5f6fa63f976455f` |
| macOS aarch64 | `pdfium-mac-arm64.tgz` | `1193a771e0bd934530afa3df73a0d44551d8f4078442e290054e6dd38ded960f` |
| macOS x86_64 | `pdfium-mac-x64.tgz` | `17f069d7012ab83898ad5eddebd139b240f05d7411c220775d507a0e3e285536` |
| Windows x86_64 | `pdfium-win-x64.tgz` | `88276459349b291c41f10422dad0210f007c04d919c8fa56472b6b7c6406adf4` |
| Windows aarch64 | `pdfium-win-arm64.tgz` | `9d8c50c65f129c3774b972cde09d3bea44478075f72faadfe9fc0c152e65d509` |

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
| Malformed document, or a `ParseLimits` limit exceeded | 400 Bad Request |
| PDF engine busy | 429 Too Many Requests, `Retry-After: 1` |
| PDF engine unavailable; parser panic; I/O failure | 500 Internal Server Error |

## Appendix

### Change Log

| Date | Version | Author | Changes |
|------|---------|--------|---------|
| 2026-02-09 | 0.1.0 | System | Initial DESIGN for cypilot validation |
| 2026-02-17 | 0.2.0 | Security | Removed `/file-parser/v1/parse-url*` endpoints, HTTP client dependency, and `download_timeout_secs` config. Rationale: SSRF risk (issue #525). |
| 2026-02-17 | 0.3.0 | Security | Added path-traversal protections for `parse-local` endpoints: `..` rejection, path canonicalization, `allowed_local_base_dir` enforcement, symlink-escape prevention, `PathTraversalBlocked` error (HTTP 403). Added constraint `fdd-file-parser-constraint-local-path-security-v1`. |
| 2026-04-29 | 0.4.0 | Engineering | Restructured to match cypilot SDLC DESIGN template. Consolidated four format-specific parsers (`HtmlParser`, `PdfParser`, `XlsxParser`, `PptxParser`) into `KreuzbergParser` backed by `kreuzberg =4.9.4` (Elastic-2.0). Retained `DocxParser`, `ImageParser`, `PlainTextParser`, `StubParser` plugins. Documented domain model, component model, API contracts, and interaction sequences. |
| 2026-04-30 | 0.5.0 | Engineering | Rewrote to be accurate to the full gateway+plugin architecture. Corrected domain model (actual `ParsedBlock` variants, `ParsedDocument` fields, `ParsedSource`). Fixed `FileParserBackend` trait signature. Fixed `/info` response key. Added plugin registration order table. Corrected file size limit (100 MB default). Removed kreuzberg-specific wording from gateway-level descriptions. |
| 2026-11-03 | 0.6.0 | Engineering | Bumped `kreuzberg` `=4.9.8` → `=4.10.4` (MIT again; `deny.toml` Elastic-2.0 exception removed). Re-verified the PPTX multi-slide / table known limitations: still present at 4.10.4. Parser tests now require their fixtures instead of silently skipping (issue #5649). |
| 2026-11-22 | 0.7.0 | Engineering | Replaced `KreuzbergParser` and the `docx-rust` DOCX backend with the crate's own spreadsheet, HTML, PPTX and DOCX parsers and a PDF parser on an embedded, SHA-256-verified libpdfium, and documented `ParseLimits`. |
