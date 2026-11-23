# File Parser Gear

File parsing gear for Gears / ToolKit.

## Overview

The `cf-gears-file-parser` crate implements the `file-parser` gear and registers REST routes.

Each format has its own parser in `src/infra/parsers/`. Spreadsheets, HTML,
PPTX and DOCX are read by the crate's own code. PDF is read through libpdfium
(via `pdfium-render`), which the build embeds into the binary.

Supported formats, by parser id (the keys of the `/info` response):

| Parser id      | Extension(s)                  | Format                                  |
|----------------|-------------------------------|-----------------------------------------|
| `plain_text`   | `txt`, `log`, `md`            | Plain text                              |
| `spreadsheet`  | `xlsx`, `xls`, `xlsm`, `xlsb` | Excel workbooks                         |
| `html`         | `html`, `htm`                 | HTML                                    |
| `pptx`         | `pptx`                        | PowerPoint presentations                |
| `pdf`          | `pdf`                         | PDF                                     |
| `docx`         | `docx`                        | Word documents                          |
| `image`        | `png`, `jpg`, `jpeg`, `webp`, `gif` | Images, returned as base64 data URIs |
| `generic_stub` | `doc`, `rtf`, `odt`, `ppt`    | Placeholder document with a text or base64 preview |

The stub also lists `xls`, `xlsx` and `pptx`, but those go to the earlier
parsers: the first registered parser that claims an extension wins.

Plain-text files come out verbatim as code blocks of up to about 16 KiB, cut
at blank lines where possible, so Markdown or HTML in them is not interpreted;
`.md` blocks are tagged `markdown`.

## Configuration

```yaml
gears:
  file-parser:
    config:
      max_file_size_mb: 100
      # Required. Only files under this directory are accessible via parse-local.
      # Symlinks that resolve outside this directory are also blocked.
      allowed_local_base_dir: /data/documents
      max_image_size_mb: 50          # largest image read; default 50, at most max_file_size_mb
      # Parse limits (below); every key is optional and defaults as in the table.
      limits:
        max_pdf_pages: 2000
        pdf_timeout: 60s             # humantime, from 1s to 1h
      pdf:
        max_queued: 64               # PDFs that may wait for the engine; more get 429
        check_on_startup: true       # bind libpdfium during init; a failure only warns
      parse_route_timeout: 5m        # humantime, from 1s to 4h; default twice pdf_timeout plus 12s
```

The gear refuses to start on an invalid value (a count of 0, `max_entry_bytes`
above `max_total_uncompressed_bytes`, `pdf_timeout` outside 1 s to 1 h,
`parse_route_timeout` outside 1 s to 4 h, `pdf.max_queued` outside 1 to 4096,
an explicit `max_image_size_mb` above `max_file_size_mb`). The parse routes
declare their own request deadline, `parse_route_timeout`: by default twice
`pdf_timeout` plus 12 s (132 s), so the gateway's default request timeout does
not cut a PDF short. It replaces the gateway's default even when that is
longer, so raise it too when you raise the gateway's;
`defaults.max_request_timeout` caps it.

### Security: Local Path Restrictions

The `parse-local` endpoints validate requested file paths before any filesystem access:

1. Paths containing `..` components are always rejected.
2. The requested path is canonicalized (symlinks resolved) and must fall under `allowed_local_base_dir`.
3. `allowed_local_base_dir` is **required** — the gear will fail to start if it is missing or the path cannot be resolved.

### Parse limits

Every document is untrusted input. The spreadsheet, HTML, PPTX, PDF and DOCX
parsers check the limits in `ParseLimits` (`infra::parsers::limits`), and the
plain-text parser its `max_text_bytes`; a document over a limit fails with an
error that names the limit. The gear reads them from `limits` in its
configuration; a key left out keeps the default below. Code that builds the
parsers itself can pass other values with `with_limits`. `ParseLimits` is `#[non_exhaustive]`: start
from `ParseLimits::default()` and change fields.

| Field | Default | Bounds |
|---|---|---|
| `max_zip_entries` | 10 000 | Entries in a ZIP container (DOCX, PPTX, XLSX, XLSM, XLSB) |
| `max_entry_bytes` | 64 MiB | Inflated size of one ZIP entry |
| `max_total_uncompressed_bytes` | 500 MiB | Inflated size of all entries of one container, declared and read |
| `max_compression_ratio` | 100 | Inflated / compressed ratio above which an entry is treated as a ZIP bomb |
| `ratio_check_floor_bytes` | 1 MiB | Entries smaller than this skip the ratio check |
| `max_nesting_depth` | 1024 | Element nesting in XML parts (HTML has its own, smaller caps and flattens deeper content) |
| `max_table_cells` | 100 000 | Table cells in the whole document; each table is charged rows x columns |
| `max_table_columns` | 16 384 | Columns in one table |
| `max_sheets` | 1 000 | Sheets in a workbook, or slides in a presentation |
| `max_html_nodes` | 500 000 | Elements and text nodes of one HTML document |
| `max_pdf_pages` | 2 000 | Pages of one PDF |
| `max_pdf_chars_per_page` | 200 000 | Characters pdfium reports on one PDF page |
| `pdf_timeout` | 60 s | Wall-clock time for one PDF once the PDF engine is taken; also the longest wait for the engine |
| `max_text_bytes` | 100 MiB | Text extracted from one document, plus 64 bytes for each block, inline run and PDF line |

pdfium is not thread-safe across documents, so all PDFs are parsed on one
dedicated thread, one at a time. At most `pdf.max_queued` (64) wait for it; further
ones, and documents that wait longer than `pdf_timeout`, are rejected as busy
(429 with `Retry-After`). Callers wait asynchronously, so a queued document
holds no thread. `pdf_timeout` is checked between pdfium calls. A single
pdfium call cannot be interrupted, so a hostile PDF can keep the engine busy
past the timeout. The engine's part of the answer still comes within
`pdf_timeout` of waiting plus `pdf_timeout` and 2 s of reading; arranging the
lines into blocks then runs on the blocking pool, bounded by the page,
character and text limits. The thread is never joined, so it does not delay
shutdown.

## License

This gear is licensed under **Apache-2.0**.

### Bundled libpdfium

The PDF parser uses libpdfium (BSD-3-Clause, with bundled third-party
components under their own licences). `build.rs` downloads the prebuilt
library from [bblanchon/pdfium-binaries `chromium/7961`](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium/7961)
with `curl`, checks the archive against a SHA-256 digest pinned per target,
and extracts only the library file. The file is embedded into the crate. At
runtime it is written to a private temporary directory and loaded once per
process.

| Target | Archive | SHA-256 |
|---|---|---|
| Linux x86_64 (gnu) | `pdfium-linux-x64.tgz` | `019665c8877d46fe65f625f80fd714ab07aac68554b0636acf2a2adf9288adb2` |
| Linux aarch64 (gnu) | `pdfium-linux-arm64.tgz` | `974107999784a438149605024475d42d80dd306799d90e1af5f6fa63f976455f` |
| macOS aarch64 | `pdfium-mac-arm64.tgz` | `1193a771e0bd934530afa3df73a0d44551d8f4078442e290054e6dd38ded960f` |
| macOS x86_64 | `pdfium-mac-x64.tgz` | `17f069d7012ab83898ad5eddebd139b240f05d7411c220775d507a0e3e285536` |
| Windows x86_64 | `pdfium-win-x64.tgz` | `88276459349b291c41f10422dad0210f007c04d919c8fa56472b6b7c6406adf4` |
| Windows aarch64 | `pdfium-win-arm64.tgz` | `9d8c50c65f129c3774b972cde09d3bea44478075f72faadfe9fc0c152e65d509` |

Build-time environment variables:

- `FILE_PARSER_PDFIUM_ARCHIVE`: a downloaded `pdfium-<target>.tgz` to use
  instead of the network. It is still verified.
- `FILE_PARSER_PDFIUM_LIB`: a prebuilt library to embed as is. It is not
  verified; the build prints a warning.
- `FILE_PARSER_PDFIUM_CACHE_DIR`: where downloaded archives are kept (default
  `OUT_DIR`), so worktrees and `cargo clean` can share them.
- `CARGO_NET_OFFLINE=true`: no download; the archive must be cached or given.
  Cargo does not pass `--offline` to build scripts.
- `DOCS_RS`: nothing is bundled.

Targets without a pinned archive (musl included) and docs.rs build without a
bundled library. PDF parsing then needs `FILE_PARSER_PDFIUM_RUNTIME_LIB` at
runtime, pointing at a libpdfium `chromium/7961`. The same variable replaces
the embedded library on any build, for example where the temporary directory
is mounted `noexec`. The gear binds the library at startup and logs a warning
if PDF parsing is unavailable.

To move to another pdfium build, change `PDFIUM_BUILD` and the digest table in
`build.rs`, set the `pdfium-render` `pdfium_NNNN` feature in the workspace
`Cargo.toml` to the highest one not above the build, and regenerate the e2e
goldens with `testing/e2e/suites/file_parser/generate_file_parser_golden.py`.

Binaries built with this crate contain libpdfium and must carry its notices:
see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
