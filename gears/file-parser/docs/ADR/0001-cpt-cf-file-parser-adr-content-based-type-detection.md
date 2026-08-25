---
status: proposed
date: 2026-08-13
decision-makers: file-parser gear maintainers
---

# Add feature-gated, Magika-based content sniffing to resolve file type when filename/Content-Type are absent or wrong

**ID**: `cpt-cf-file-parser-adr-content-based-type-detection`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Do nothing — keep extension/`Content-Type`-only detection](#do-nothing--keep-extensioncontent-type-only-detection)
  - [Pure-Rust magic-byte sniffing (`infer` or `file-format`)](#pure-rust-magic-byte-sniffing-infer-or-file-format)
  - [Delegate to Kreuzberg's own MIME detection](#delegate-to-kreuzbergs-own-mime-detection)
  - [Magika (ONNX ML content classifier), feature-gated behind `magika`](#magika-onnx-ml-content-classifier-feature-gated-behind-magika)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

`FileParserService` (`domain/service.rs`) currently routes every request to a parser backend using only the filename extension, falling back to a client-supplied `Content-Type` header. It never inspects file bytes. This means `parse_local` hard-rejects extensionless files, `parse_bytes` rejects uploads that have neither a filename nor a recognized `Content-Type` (a case the SDK's own `ParseBytesRequest` doc comment already anticipates), a wrong extension silently routes to the wrong parser, and `ImageParser` trusts an `image/*` `Content-Type` header without checking the bytes — a trust boundary problem, since a caller-supplied header is never a substitute for verifying what the bytes actually are. `DESIGN.md:181-187` already assigns format detection to the gateway, not the plugins, so the gateway is where this must be fixed. How should the gateway determine file type when the filename/`Content-Type` hints are missing or wrong, without imposing a new mandatory runtime dependency on every consumer of the gear? Note that this ADR only *mitigates* the `ImageParser` trust-boundary problem rather than eliminating it — see the "Consequences" section below.

## Decision Drivers

* Callers cannot always be trusted to supply a correct filename or `Content-Type`
* The three existing extension→MIME tables have already drifted and must collapse to one
* Default build behavior and error messages must stay byte-for-byte identical for gear consumers who never opt in, with one intentional, narrowly-scoped exception from collapsing the three drifted tables into one canonical table (see "Consequences"): feature-off `Content-Type`-only routing now also resolves `xlsx`/`xls`/`xlsm`/`xlsb`/`pptx`. The canonical table is used only to fill in a *missing* `Content-Type` — an explicitly supplied `Content-Type` always passes through to the backend unchanged, regardless of whether the resolved extension has a canonical-table entry (an explicit header is a stronger signal than a filename extension, so the table must never silently override it). Both halves are pinned by tests in `detection_precedence_tests.rs`.
* Callers that already know the exact type they're passing (in particular, in-process callers going through `ClientHub`/the SDK, e.g. `FileParserLocalClient::parse_bytes`) can set `ParseBytesRequest::detection` to `Detection::Skip` to bypass content-based detection entirely and route purely off `filename`/`content_type`, even when a detector is registered — detection there buys nothing but latency and a chance of misrouting a type the caller is certain of. The public REST upload endpoints do not expose this — every request through them is always detected, per the "callers are not trusted" driver above.
* Detection must add bounded, predictable latency — parsing runs in a request path, not a batch job
* Whatever mechanism is chosen must resolve the actual formats this gear supports today (PDF, HTML, DOCX, XLSX, PPTX, PNG/JPEG/WEBP/GIF, incl. OOXML formats that share a ZIP container)
* No new dependency, binary size, or native-library requirement may land on the default build path
* The gear has no ADRs yet; this sets the precedent for how model/runtime dependencies get introduced

## Considered Options

* Do nothing — keep extension/`Content-Type`-only detection
* Pure-Rust magic-byte sniffing (e.g. `infer` or `file-format` crates)
* Delegate to Kreuzberg's own MIME/format detection
* Magika (ONNX ML content classifier), feature-gated behind `magika`

## Decision Outcome

Chosen option: "Magika (ONNX ML content classifier), feature-gated behind `magika`", because it is the only option that reliably distinguishes the OOXML-family formats this gear must support (DOCX/XLSX/PPTX are all ZIP containers that magic-byte sniffing cannot tell apart without deep-parsing the archive), and feature-gating confines its runtime/binary cost to deployments that actually need it, per `DESIGN.md:85`'s assignment of detection to the gateway.

### Consequences

* `infra/` gains a `MagikaDetector` implementing a new `domain`-defined `ContentTypeDetector` trait, injected into `FileParserService` as `Option<Arc<dyn ContentTypeDetector>>`; with the `magika` feature off, this is always `None` and every code path is identical to today's behavior, including error message text.
* `EXTENSION_MIME_MAPPINGS` (`domain/service.rs:14-27`), the Kreuzberg-parser table (`infra/parsers/kreuzberg_parser.rs:44-57`), and the image-parser table (`infra/parsers/image_parser.rs:47-61`) collapse into one canonical extension↔MIME table in `domain/`; all three call sites (and the new Magika label→extension map) read from it. This is the union of the three previous tables, so it also changes feature-off routing: a `Content-Type`-only request (no filename) for `xlsx`, `xls`, `xlsm`, `xlsb`, or `pptx` now resolves to a MIME type and routes to `KreuzbergParser` even when the `magika` feature is not compiled in, whereas previously only `pdf`/`html`/`htm` resolved this way at the gateway. This is intentional: these formats were already routable via filename extension and via Kreuzberg's own table; the canonical table just makes `Content-Type`-only requests consistent with that.
* Detection precedence becomes: (1) if the `magika` feature is compiled in and the detector loaded successfully, run content detection on every request, not only when the extension/`Content-Type` are missing; (2) if the detected label maps to a supported extension with confidence ≥ 0.90, that extension wins routing — this is what lets a wrong extension resolve to the correct parser; (3) below 0.90 confidence, or when the label maps to no supported extension, fall back to the extension/`Content-Type`-derived value exactly as today; (4) with the feature off, or with it on but the extension/`Content-Type` agreeing with a confident detection, behavior and routing are unchanged from today.
* This ADR mitigates, but does not eliminate, the `ImageParser` trust-boundary problem: a caller-supplied `image/*` `Content-Type` hint still selects `ImageParser` without byte validation whenever Magika is not compiled in, doesn't run, or scores below 0.90 confidence on the actual bytes. Fully closing this gap (e.g. mandatory content validation before `ImageParser` selection regardless of detector confidence) is deferred to a future ADR.
* Every disagreement between the detected type and the caller-supplied extension/`Content-Type` at ≥ 0.90 confidence is logged at `WARN` with both values, so operators can see when clients are sending an incorrect type hint without the gateway hard-failing the request. A metric on this signal is deferred to a future iteration.
* `parse_bytes` runs detection on the full in-memory buffer after the existing `max_file_size_bytes` check, never before — the size check is cheap and must reject oversized uploads before any CPU is spent on inference.
* `parse_local` runs detection whenever a detector is registered — not only when the extension is absent — mirroring `parse_bytes`'s precedence, so a present-but-wrong extension is corrected the same way for local files. Detection does **not** buffer the file: it goes through `ContentTypeDetector::detect_path`, backed by `magika::Session::identify_file_sync`, which seeks to the head/tail blocks the model needs instead of reading the whole file (and avoids duplicating the read the backend then performs itself).
* The `max_file_size_bytes` limit is enforced in `parse_local` unconditionally, before a backend is selected and regardless of whether a detector is registered — a feature flag must not change the size-limit contract. That check reads `metadata().len()`, so it is not atomic against a file growing afterwards; it exists to reject oversized files early with a message naming the actual size. The actual memory bound is each backend's own bounded read (`infra::parsers::bounded_read::read_bounded`, a `Read::take` against a single open handle). `KreuzbergParser` is the exception: it hands the path to `kreuzberg`, which reads the file itself, so only the metadata check applies there.
* Because `ort`'s inference call is synchronous and CPU-bound, `MagikaDetector` executes it inside `spawn_blocking`; the `magika::Session` is constructed once, eagerly, at gear startup (not lazily on first request), so cold-load latency and any missing-runtime/model failure surface at boot, not on a user request.
* With the `magika` feature compiled in, failure to load the ONNX Runtime shared library or the embedded model is a **startup failure**, not a silent fallback to extension-only routing — a gear that silently disabled detection would reintroduce exactly the spoofable-`Content-Type` risk this ADR exists to close. Operators who need best-effort behavior must build without the feature.
* `ort` 2.0.0-rc.12 does not fail cleanly on its own: when the runtime it loads is missing, the wrong architecture, or older than the ONNX Runtime C API level (`api-24`, i.e. the API surface shipped starting with ONNX Runtime 1.24.x — CI provisions 1.29.0) the `ort` build was compiled against, the call **hangs forever instead of returning an error**. Reproduced directly against a nonexistent `ORT_DYLIB_PATH`: `magika::Session::new()` returns neither `Ok` nor `Err`, and does not panic, within 45s. We deliberately do *not* claim a mechanism: an earlier version of this ADR attributed it to `ort`'s error-reporting path recursively re-entering the `OnceLock` behind `ort::api()`, but that explanation is contradicted by `ort::init_from(path)` — which never touches that lock — hanging on the same input, also verified at 45s. Treat it as an observed property of an unloadable `ORT_DYLIB_PATH`.
* There is consequently **no pre-flight validation available**. `ort::init_from(path)` is the obvious candidate (it takes a dylib path and returns `Result`) and does not work; every `ort` entry point funnels through the same lazy dylib initialization. So the timeout-and-abandon workaround cannot be replaced by a cheap "check the runtime first" call, and anyone attempting that simplification should re-run the two 45s probes before concluding otherwise. No upstream issue has been filed as of 2026-08-18; the `init_from` reproduction is the useful thing to report against `pykeio/ort`.
* `MagikaDetector`'s constructor (`with_config`) is therefore never called directly from `gear.rs`; it runs on a detached `std::thread` (not `tokio::task::spawn_blocking`, whose threads Tokio joins on runtime shutdown — a wedged one would hang shutdown too), with the result awaited through a `oneshot` channel under a 30s `tokio::time::timeout`. A broken runtime surfaces as a startup error after 30s instead of an indefinite hang, at the cost of leaking one OS thread for the life of the process in that failure case. That leak is only bounded because gear-init failure terminates the host; `MagikaInitError::leaked_init_thread()` encodes which failures carry the obligation, so a future non-fatal caller has to confront it in code rather than in a comment.
* Exclusive access per inference is forced by `magika`'s API, not by ONNX Runtime. `ort::session::Session` is `Send + Sync` and ONNX Runtime is designed for one session shared across threads (the `&mut self` on `ort`'s `run` is a borrow-lifetime device for the returned outputs), but `magika::Session` keeps its inner session private and exposes only `&mut self` methods, so none of that is reachable. A mutex is unavoidable while going through `magika`; if detection throughput ever matters, the fix is upstream, not more sessions. The detector therefore holds **one** session, serialized behind a fair FIFO mutex, and exposes ONNX Runtime's intra-op thread count (`magika_intra_op_threads`) as the CPU knob instead of a session count. Measured: ~3.5 ms per detection and ~6 extra OS threads at ONNX Runtime's default, versus ~9.3 ms pinned to a single thread — so one session sustains ~285 detections/second against document extraction costing tens to hundreds of milliseconds on the same request.
* `magika`'s async API (`identify_content_async` / `identify_file_async`) must **not** be used, despite being the obvious way to avoid `spawn_blocking`. `ort`'s `InferenceFut::drop` calls `RunOptions::terminate()`, which sets a sticky flag that only `unterminate()` clears, and `magika` passes a single process-wide `static OnceLock<RunOptions>` to every async run without ever unterminating. The first dropped async inference future would therefore disable async inference for the entire remaining process lifetime — in a request path, one client disconnect would silently kill detection. This is an upstream `magika` bug; until it is fixed, the `_sync` methods on `spawn_blocking` are mandatory rather than merely conventional.
* `magika` 1.1.0's own manifest already pins `ort = "=2.0.0-rc.12"` with `default-features = false` and only `features = ["ndarray", "std"]` — `ort`'s `download-binaries` default is already off for every consumer of `magika`, with no action needed on our side to keep the network fetch out of `cargo build`. That resolved dependency set enables **neither** `download-binaries` **nor** `load-dynamic`, so by default `ort` falls back to build-time static linking against a system ONNX Runtime install, located via `ORT_LIB_LOCATION` (or `pkg-config`) — meaning the ONNX Runtime dev headers/libs would need to be present in *every* environment that compiles `file-parser` with `--features magika`, not just wherever it eventually runs.
* We add an explicit `[workspace.dependencies]` entry, `ort = { version = "=2.0.0-rc.12", default-features = false, features = ["load-dynamic", "api-24"] }`, layered on top of Magika's own feature set — `ndarray` and `std` are not declared directly here because `magika` already contributes them transitively, while `api-24` **must** be declared (without it `ort`'s unconditionally-compiled `ep/vitis.rs` references an `OrtApi` field that only exists at that ABI level). Copy the entry as written; dropping `api-24` breaks the build. `load-dynamic` makes `ort` `dlopen` the ONNX Runtime shared library at process start (path supplied via the `ORT_DYLIB_PATH` env var, or a well-known default search path) instead of linking against it at compile time. This decouples "who builds the gear" from "who has the ONNX Runtime installed": CI here runs on plain GitHub-hosted `ubuntu-latest`/macOS runners with no per-gear container or custom base image (`.github/workflows/ci.yml`), so requiring onnxruntime dev headers at *build* time on every such runner is a heavier, more fragile ask than requiring the shared library only where the `magika`-enabled binary actually runs.
* `ort/download-binaries` was considered and rejected for CI, not just for production: it always links **statically** (verified by reading `ort-sys`'s build script), which is incompatible with `load-dynamic` — already active for every `magika` build via the workspace `ort` entry — and reliably reproduces the deadlock above (a `dlopen` attempt against a static `.a` fails, triggering the same recursive-`OnceLock` hang). This isn't a macOS-only artifact of local testing; `ort-sys` links statically via `download-binaries` on every platform, so the originally-proposed CI job would have hung on Linux runners too.
* CI instead downloads the official ONNX Runtime shared-library release directly (`onnxruntime-linux-x64-<version>.tgz` from `microsoft/onnxruntime`'s GitHub releases, pinned to 1.29.0, which satisfies the `api-24` C API level `ort` requires) and points `ORT_DYLIB_PATH` at the extracted `.so` — the same `load-dynamic` mechanism production uses, just with CI choosing to fetch the library itself rather than assuming it's preinstalled. This was verified end-to-end locally (macOS build, official `onnxruntime-osx-arm64` release): real Magika inference against a PDF byte stream succeeds with high confidence. The `cargo test` step also carries a `timeout-minutes` job-level backstop, since a future regression reintroducing the hang above would otherwise consume the runner indefinitely. Provisioning the shared library for actual deployments (which pinned release, fetched/installed how, on which runtime images) remains a separate, deploy-time decision out of scope for this ADR.
* `deny.toml` needs no new `ignore` entries for Magika itself (Apache-2.0, already allow-listed via the existing Kreuzberg precedent), but `cargo deny check` must be run with `--features magika` in CI to catch any advisory or license issue in the `ort`/`ndarray` transitive tree; any suppression found gets a justification comment in the same style as the existing `kreuzberg` entries in `deny.toml:41-43`.
* `file-parser-sdk`'s public types are unaffected, and so is rendered error text: an unsupported detected label never reaches routing at all. `MagikaDetector` intersects Magika's labels with the registered `supported_extensions()` and yields `None` for anything outside that set, and `FileParserService::reconcile_extension` independently requires a registered parser for the detected extension before letting it win — so a confident `"zip"` detection falls back to the caller's hint rather than producing an error naming `"zip"`. Golden error-message tests are therefore unchanged in **both** feature states.
Surfacing detection confidence/label in the SDK response (`ParsedText`) is explicitly deferred to a future iteration/ADR — this decision does not change the plugin trait (`FileParserBackend`) or the SDK response shape.
* Magika's ~200 output labels map through a single new `magika_label -> extension` table to only the extensions this gear's registered plugins already declare via `supported_extensions()`; a confidently-detected label with no entry in that table (e.g. `"zip"`, `"mp4"`) is treated as "detection did not help": it is discarded and routing falls back to the caller's extension / `Content-Type` hint, so such a request behaves exactly as it does today with the feature off, rather than producing a new error class.
* `DESIGN.md:181-187` (the gateway routing algorithm) must be updated to describe the detector as an optional step 0 ahead of today's steps 1–3.

### Confirmation

* Unit tests for `MagikaDetector` (label→extension mapping, confidence threshold behavior) and for `FileParserService` precedence logic, run twice in CI — once with the `magika` feature off (detector absent, behavior identical to pre-ADR baseline) and once with it on.
* Integration tests covering the three acceptance scenarios from the tracking issue: an extensionless local file parses; a byte upload with no filename and `Content-Type: application/octet-stream` parses; a file with a wrong extension (e.g. a DOCX saved with a `.pdf` name) routes to the DOCX parser and succeeds.
* `cargo deny check --features magika` passes in CI with no new unjustified advisories.
* Code review confirms the default-feature build has no new entries in `cargo tree` and that golden error-message tests (already covering `unsupported_file_type`/`no_parser_available`) pass unmodified **for default-feature (`magika` off) runs**; with the feature on, an unsupported *detected* type populates `extension` with the detected label rather than the literal `"no extension"`, per "Consequences" — that is an expected, feature-on-only change in rendered error text, not a regression.
* **What would validate the 0.90 threshold, and what does not exist yet.** Nothing in this iteration produces the signal needed to tune it: the per-request `WARN` on a detected/hinted disagreement cannot be aggregated or alerted on, and the `file_parser_type_mismatch_total` counter this ADR originally committed to is explicitly deferred (see "Consequences"). Until a metric lands, the threshold is set by judgement, not evidence, and the honest position is that we do not know its false-override rate in production. What *does* exist is a `file_parser.detect` tracing span around every detection and a `magika.inference` span inside the detector, so detection's latency contribution is attributable in a trace without enabling `debug` logging — spans were chosen over a metric because this gear emits no metrics at all and adding that infrastructure is out of scope here. Revisiting the threshold should begin by adding the mismatch counter.

## Pros and Cons of the Options

### Do nothing — keep extension/`Content-Type`-only detection

Leave `parse_local`/`parse_bytes` as they are; only consolidate the three MIME tables.

* Good, because zero new dependencies, zero new runtime/CI/container work
* Good, because zero risk of a new failure mode (model/runtime load failure)
* Neutral, because the table consolidation (already useful on its own) still happens
* Bad, because it does not fix any of the four concrete failure cases in the tracking issue — extensionless files, `octet-stream`-with-no-filename uploads, wrong-extension misrouting, and unchecked image `Content-Type` all remain unresolved
* Bad, because the unresolved image-`Content-Type` trust gap leaves this gear returning parsed content for whatever type the caller *claims* the bytes are, not what they actually are — the primary reason this issue was filed

### Pure-Rust magic-byte sniffing (`infer` or `file-format`)

Sniff a small byte prefix against a table of known magic-number signatures; no ML model, no native runtime, pure Rust.

* Good, because it is a pure-Rust dependency: no `ort`, no native shared library, no embedded model weights, trivially usable as a default (non-feature-gated) dependency
* Good, because inference is a table lookup — sub-microsecond, no `spawn_blocking` needed
* Good, because it fixes the extensionless-file and `octet-stream`-upload cases for formats with a distinct magic number (PDF, PNG, JPEG, GIF, WEBP)
* Bad, because DOCX/XLSX/PPTX are all `PK\x03\x04` ZIP containers at the byte level; magic-byte sniffing alone cannot tell them apart, and these three formats are core to this gear's supported set — a signature crate would need bespoke ZIP-central-directory parsing to distinguish them, which is materially the same integration cost as adopting a purpose-built classifier
* Bad, because plain-text-ish formats (HTML) have no reliable magic number and would still fall back to extension/`Content-Type` heuristics
* Bad, because it does not close the `ImageParser` trust gap any better than "good enough for images" — it *would* fix that specific case, but leaves the office-document cases (the majority of this gear's traffic per `DESIGN.md`) unresolved

### Delegate to Kreuzberg's own MIME detection

Kreuzberg (already a dependency, MIT/Elastic-2.0-licensed, used for PDF/DOCX/etc.) reportedly does some internal format sniffing; call into that instead of adding a new dependency.

* Good, because no new dependency if Kreuzberg's internal detection is reachable and licensable for this purpose
* Bad, because Kreuzberg's detection is invoked *after* a parser is already selected — using it to *select* the parser would require restructuring Kreuzberg's own entry points or duplicating its internal logic outside the crate boundary, which is not something this gear controls (Kreuzberg is an upstream Elastic-2.0 dependency, not code we can extend)
* Bad, because it does nothing for the `ImageParser`/non-Kreuzberg formats (PNG/JPEG/WEBP/GIF), which are handled by a separate, unrelated backend
* Bad, because it couples this gear's routing logic to an upstream library's internal (and undocumented, for this purpose) behavior rather than a purpose-built, versioned API

### Magika (ONNX ML content classifier), feature-gated behind `magika`

Google's small ONNX model, ~200 content-type labels, wrapped by the `magika` crate (Apache-2.0). Chosen option — added as an optional dependency behind a new `magika` Cargo feature, off by default.

* Good, because it distinguishes OOXML formats (DOCX/XLSX/PPTX) and the image/PDF/HTML formats this gear supports from content alone, addressing every case in the tracking issue including — per the confidence threshold and fallback behavior discussed in "Consequences" — mitigating (not eliminating) the `ImageParser` `Content-Type`-trust gap
* Good, because feature-gating means the default build — and every consumer who does not opt in — pays zero cost: no `ort`, no `ndarray`, no embedded model bytes, and behavior/error messages identical to before this ADR (with the one exception noted in "Decision Drivers"/"Consequences": the canonical MIME table also widens feature-off `Content-Type`-only routing for `xlsx`/`xls`/`xlsm`/`xlsb`/`pptx`)
* Good, because the model ships embedded in the crate (`include_bytes!`); no network fetch of weights at build or run time
* Neutral, because it is the first ONNX/`ort` dependency in this workspace, setting precedent for how such dependencies are vetted, built, and shipped (this ADR is that precedent)
* Bad, because it requires the ONNX Runtime shared library to be present wherever the `magika`-enabled binary runs (via `ort/load-dynamic`, resolved at process start) — a new ops dependency for the runtime/container image that `infer`/`file-format` would not need
* Bad, because it adds ~2.8 MB of embedded model weight and pulls in `ort` + `ndarray` for any consumer who does enable the feature
* Bad, because inference is CPU-bound and synchronous, requiring `spawn_blocking` discipline in the gateway that a pure lookup-table approach would not need
* Bad, because confidence-threshold and mismatch-handling policy (this ADR's Consequences section) is new decision surface that a magic-byte or do-nothing approach would not introduce

## More Information

The 0.90 confidence threshold and the "log, don't hard-fail" mismatch policy are initial values, not empirically tuned; `TEST-ADR-002`-style validation (real-world mismatch-rate telemetry, were a metric added in a future iteration per "Consequences") may justify revisiting them without requiring a new ADR — see Confirmation. Because the threshold is expected to move, it is a configuration key (`detection_confidence_threshold`, default `0.90`) rather than a constant, so tuning it per deployment does not require a recompile; `Gear::init` rejects values outside `[0.0, 1.0]` at startup rather than clamping a value an operator plainly did not mean.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-file-parser-fr-formats` — content-based detection lets uploads reach the correct backend for a supported format even when the filename/`Content-Type` hint is absent or wrong, which is this requirement's actual intent
* `cpt-cf-file-parser-fr-local-path-security` — detection for extensionless local paths runs only after the existing `..`-rejection/canonicalization/base-dir checks in `parse_local`; this decision does not relax that ordering
* `cpt-cf-file-parser-fr-plugin-extensibility` — the `magika_label -> extension` map is validated against each registered plugin's `supported_extensions()`, not a hardcoded parser list, so new plugins are picked up without touching the detector
* `cpt-cf-file-parser-nfr-response-time` — the eager, once-at-startup `Session` load and `spawn_blocking`-wrapped, per-request inference are chosen specifically to keep steady-state request latency bounded and predictable
* `cpt-cf-file-parser-component-parser-service` (`fdd-file-parser-component-parser-v1`) — `FileParserService`'s routing algorithm gains an optional detection step; `DESIGN.md:181-187` must be updated to reflect it
* `cpt-cf-file-parser-component-parser-backend` (`fdd-file-parser-component-backend-v1`) — type *resolution* stays entirely in the gateway, consistent with `DESIGN.md:85`, but the trait does change: `parse_local_path` gains a `resolved_content_type: Option<&str>` parameter so the gateway can hand a backend the type it resolved instead of the backend re-deriving one from a possibly-wrong on-disk extension. Implementations must accept the updated local-path signature; those that route purely by filename may ignore the argument
