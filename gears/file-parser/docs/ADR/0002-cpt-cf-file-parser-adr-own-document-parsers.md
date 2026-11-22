---
status: accepted
date: 2026-11-22
decision-makers: file-parser gear maintainers
---

# Replace kreuzberg and docx-rust with file-parser's own parsers and a directly bound libpdfium

**ID**: `cpt-cf-file-parser-adr-own-document-parsers`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep kreuzberg and docx-rust, wait for upstream](#keep-kreuzberg-and-docx-rust-wait-for-upstream)
  - [Move to xberg 1.4](#move-to-xberg-14)
  - [Use general-purpose format crates directly](#use-general-purpose-format-crates-directly)
  - [Own parsers on low-level crates, libpdfium for PDF](#own-parsers-on-low-level-crates-libpdfium-for-pdf)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

file-parser extracted PDF, HTML, spreadsheets and PPTX through `kreuzberg` 4.10.4 and DOCX through `docx-rust` 0.1.11. Both pulled old crates into the lock file: zip 4.6 (docx-rust) and zip 8.6 with calamine 0.36 (kreuzberg). These carry nine open advisories (RUSTSEC-2026-0367, -0368, -0369, -0429, -0545, -0546, -0547, -0548 for zip below 9; RUSTSEC-2026-0436 for calamine below 0.38), each ignored in `deny.toml`. docx-rust has had no release since 0.1.11 and pins zip 4. Every kreuzberg release caps calamine below 0.38, and kreuzberg does not build with zip 9.

The output also had defects: DOCX never produced headings, PPTX put all slides under one trailing "Slide 1" heading and flattened tables to tab-separated text, HTML and spreadsheets duplicated headings, PDF gave one paragraph per page, and RTL lines changed order between pdfium builds.

Every input is an untrusted upload. How should file-parser extract documents without these dependencies, with explicit resource limits?

## Decision Drivers

* Remove zip below 9 and calamine below 0.38 from `Cargo.lock`, so the nine advisory exceptions can be deleted.
* Stay on Rust 1.98.1. A toolchain bump touches the whole workspace.
* Hostile input must not exhaust memory, stack or CPU: no allocation sized from a value the file declares, no recursion over input structure, no super-linear algorithm on input size.
* Keep the public API of `file-parser-sdk` and the REST contract.
* Fix the known output defects rather than carry them over.
* Keep PDF extraction quality at least at pdfium's level.

## Considered Options

* Keep kreuzberg and docx-rust, wait for upstream
* Move to xberg 1.4 (the kreuzberg fork)
* Use general-purpose format crates directly (calamine 0.38, html5ever/scraper, a maintained DOCX crate, pdfium-render)
* Own parsers on low-level crates (zip 9, quick-xml, cfb, encoding_rs), libpdfium for PDF

## Decision Outcome

Chosen option: "Own parsers on low-level crates, libpdfium for PDF". It is the only option that removes the advisories on Rust 1.98.1 and lets every parser enforce the same limits. The formats we need (OOXML, BIFF8/BIFF12, HTML) are documented and small enough to read for text and structure only.

### Consequences

* New parsers, one per format, ids `spreadsheet`, `html`, `pptx`, `pdf`, `docx`. `KreuzbergParser` and the `kreuzberg_parser`/`ir_convert` modules are removed (a breaking change of the implementation crate's public API; `file-parser-sdk` is unchanged).
* Shared modules: `ooxml` (ZIP package reader with entry-count, size, compression-ratio and path checks; relationship resolution; XML depth guard; no DTD entity expansion), `limits` (`ParseLimits`, text and table budgets), `inlines`.
* libpdfium (bblanchon pdfium-binaries, chromium/7961) is downloaded by `build.rs`, checked against a pinned SHA-256 per target, and embedded. pdfium-render 0.9.5 binds it at runtime. Targets without a pinned archive (musl included) build without it and need `FILE_PARSER_PDFIUM_RUNTIME_LIB`.
* pdfium serves one document at a time under one lock. A single pdfium call cannot be interrupted, so one hostile PDF can hold the engine past `pdf_timeout`. At most 64 documents wait. Only a separate process would bound this; that is left for later.
* Output changes for every format; the e2e goldens are regenerated. Headings, lists, tables, links and code blocks now appear where the source has them.
* We now own the format code: about 18k lines including tests. Format edge cases that kreuzberg handled are ours to handle. The edge-case checklists used for this work are kept with the review notes.
* `deny.toml` loses the nine exceptions. `cargo deny` licenses, bans and sources pass with no new exception.

### Confirmation

* Unit tests per parser, built from small in-memory packages and synthetic PDFs, cover each checklist item and each limit (ZIP bombs, deep nesting, huge spans, long link targets, oversized tables, text budget).
* Integration tests on the fixtures in `testing/e2e/testdata/`; e2e goldens compared byte for byte.
* A local corpus check (22 decks, 60 PDFs, 42 XLS workbooks compared cell by cell with calamine) showed no lost text and no parse errors.
* `cargo deny check` passes without the nine exceptions.

## Pros and Cons of the Options

### Keep kreuzberg and docx-rust, wait for upstream

* Good, because no code to write.
* Bad, because docx-rust is unmaintained and pins zip 4; there is no release to wait for.
* Bad, because the advisory exceptions and output defects stay.

### Move to xberg 1.4

* Good, because it is the closest drop-in.
* Bad, because it needs Rust 1.99; on 1.99 `make clippy` reports 781 new errors in 397 files.
* Bad, because its PDF engine is either a young native parser or text-only.
* Bad, because it keeps the same design: Markdown generated first and parsed back into structure, which lets document text forge structure.

### Use general-purpose format crates directly

* Good, because less own code.
* Bad, because calamine 0.38 allocates dense ranges and buffers from declared sizes (a 2 KB XLSX can request billions of cells), reuses stale buffers on short XLSB records and decodes every formula eagerly.
* Bad, because html5ever runs several algorithms that are super-linear on hostile markup (scope walks on deep nesting and unmatched end tags, formatting reconstruction, attribute de-duplication); a pre-check could not bound them.
* Bad, because no maintained DOCX crate reads structure (styles, numbering, fields) without its own unbounded paths.

### Own parsers on low-level crates, libpdfium for PDF

* Good, because zip 9, quick-xml 0.42, cfb 0.16 and encoding_rs are mature and maintained, and none sizes anything from file input.
* Good, because every parser uses the same limits and streams its XML without recursion.
* Good, because the output can be built directly as IR, with the structure the source has.
* Bad, because the format logic is ours to maintain.
* Bad, because PDF still depends on a native library and its single-threaded engine.

## More Information

Per-phase review verdicts, the edge-case checklists and the corpus results are summarised in the pull request that introduced this change. kreuzberg and xberg sources were read for edge cases only; no code or tests were copied.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-file-parser-fr-formats` — the same formats stay supported, through new parsers
* `cpt-cf-file-parser-fr-extraction` — headings, lists, tables, links and code blocks are extracted where the source has them
* `cpt-cf-file-parser-constraint-parse-limits` — every parser enforces `ParseLimits`
* `cpt-cf-file-parser-constraint-version-pin` — libpdfium is pinned by build and SHA-256 in `build.rs`
* `cpt-cf-file-parser-principle-single-backend` — format logic stays inside plugins; the gateway is unchanged
