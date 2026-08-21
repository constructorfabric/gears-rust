---
status: accepted
date: 2026-08-19
---

# Close the Error-Middleware Catch-All Gap with Wrapper Extractors and a Generic Fallback

**ID**: `cpt-cf-errors-adr-error-middleware-catchall`

## Context and Problem Statement

`canonical_error_middleware` enriches (`instance`, `trace_id`, structured
logging) any response already carrying `Content-Type: application/problem+json`,
and passes everything else through unchanged. This assumes every error a
client can receive was, somewhere upstream, explicitly typed as
`CanonicalError`. That assumption doesn't hold for axum extractor rejections:
`Json<T>`'s `FromRequest` impl resolves its own `JsonRejection` and calls
`.into_response()` on it directly, inside axum's generated `Handler::call`,
before the handler body - and therefore any handler-level `CanonicalError`
conversion - ever runs (gears-rust#4596). The same structural gap applies to
any fallible extractor (`Query<T>`, `Path<T>`, `Form<T>`) and to tower layers
that resolve a response before axum's Router/extractor machinery is entered
at all (e.g. a `Content-Length`-based body-size pre-check).

This was a known, deliberately deferred gap: `PRD.md` §4.2 Out of Scope lists
`"Error middleware catch-all — depends on foundation phase"`. The foundation
- canonical error categories (ADR 0001), GTS error identification (ADR
0002), the RFC 9457 wire format (ADR 0003), the typed-enum implementation
(ADR 0004), SDK canonical projection (ADR 0005), and per-occurrence transport
overrides (gears-rust#4466) - has since shipped. This ADR is that deferred
phase: how should a failure that was never typed as `CanonicalError` render
as a valid error response?

## Decision Drivers

* No regression to axum's existing per-failure-kind HTTP status codes (e.g.
  422 for a `deny_unknown_fields` violation vs. 400 for malformed JSON
  syntax) - a fix must preserve this distinction, not collapse it
* Coverage must extend beyond JSON specifically: any extractor, and any
  tower-layer-level short-circuit that produces a response before axum's
  Router/extractor machinery runs at all
* Avoid guessing a specific canonical category (`invalid_argument` vs.
  `failed_precondition` vs. `out_of_range`, etc.) from information that
  doesn't uniquely determine it - a bare HTTP status code is ambiguous
  across several categories
* Reuse already-shipped primitives (`.with_override`, gears-rust#4466;
  `#[resource_error]`) rather than introducing new machinery
* Zero required changes to any gear - the fix must apply through
  `libs/toolkit` alone, given every gear already receives
  `canonical_error_middleware` as its outermost layer via the OoP bootstrap
  (`runtime/oop_serve.rs`)

## Considered Options

* **Option A**: Precise wrapper extractors (one per axum extractor family,
  e.g. `toolkit::api::rest::extract::Json<T>`) for known failure shapes, plus a
  generic `canonical_error_middleware` fallback for anything a wrapper
  extractor architecturally cannot reach
* **Option B**: Generic middleware fallback only - treat every untyped
  rejection the same way, with no per-extractor precision
* **Option C**: Wrapper extractors only - require every possible rejection
  source to be explicitly wrapped, with no middleware-level fallback

## Decision Outcome

Chosen option: **Option A - wrapper extractors plus a generic fallback**,
because it delivers full precision (specific GTS category, machine-readable
reason code) wherever the failure's shape is known ahead of time - the
common case, and the only case a wrapper extractor is even capable of
reaching - while the generic fallback guarantees every response is at least
a syntactically valid RFC 9457 `Problem` for the cases a wrapper extractor
architecturally cannot reach: a tower `Layer` that resolves `Ok(response)`
directly, before axum's Router is ever entered, produces no `Result`/rejection
for any extractor-shaped wrapper to intercept in the first place.

### Consequences

* `toolkit::api::rest::extract` is a module family - `Json`, `Query`, and
  `Path` in this change, scoped to verified real usage across the workspace
  (`Query<T>`: 54 typed call sites; `Path<T>`: real but fewer; `Form<T>`:
  zero, not added; `Multipart`: real usage but a structurally different
  failure shape - fails field-by-field during handler-body iteration, not
  at extraction time, so this pattern doesn't apply to it) - each member
  delegating to its axum built-in counterpart and mapping the built-in
  `Rejection` to `CanonicalError` via a shared, crate-internal
  `GenericResourceError` marker (`#[resource_error]`) for the "no domain
  resource in scope" case - this is axum's own recommended pattern for
  customizing an extractor's rejection (see More Information), not a
  bespoke mechanism
* `canonical_error_middleware` gains a second branch: any response reaching
  it with a client- or server-error status that is not already
  `application/problem+json`, and is not tagged `ForeignPassthrough` (see
  below), is rewritten into a minimal, valid `Problem`. This wrap is
  category-aware, not a single uniform shape: a foreign 5xx becomes a real
  `internal` `CanonicalError` (DESIGN.md §2.1's fail-safe fallback - a 5xx
  is unambiguously this platform's own fault), a foreign 4xx uses RFC 9457
  §4.2.1's `about:blank` convention instead (`type: "about:blank"`, `title`
  *and* `detail` both the status's standard reason phrase - a bare 4xx
  doesn't determine one canonical category over another). If a
  `CanonicalError` can be recovered from the original response's
  extensions (e.g. a `Problem` body that failed to deserialize but whose
  extension survived), that real category is used directly instead of
  either class-based guess. In every case, the foreign body's own content
  is never placed on the wire, only logged server-side (capped, timeboxed,
  escaped), since nothing has vetted it against this platform's "no
  internal details on the wire" guarantee. A response that claims
  `application/problem+json` but fails to deserialize as a valid `Problem`
  is routed through this same branch rather than passed through malformed -
  the "every error is a valid Problem" guarantee has no exception for a
  response that misrepresented its own `Content-Type`, provided the
  response is an actual error status (4xx/5xx) and not already tagged
  `ForeignPassthrough` - see the next bullet for both of those exceptions.
* A response tagged `ForeignPassthrough` (`toolkit-canonical-errors`) is
  returned completely unchanged by `canonical_error_middleware`, regardless
  of status or `Content-Type` - inserted by a reverse-proxy/passthrough
  layer (`toolkit-gateway::Forwarder`, `oagw`'s proxy data-plane) on a
  response it relays verbatim from a genuine upstream (or an underlying
  data-plane layer that already decided how to represent its own
  failure). Without this escape hatch, the fallback above would buffer and
  replace every such response, destroying the upstream's own error
  identity and forcing full buffering of what may still be a streamed
  body - confirmed by a real, previously-passing `oagw` e2e test
  (`test_upstream_500_passthrough`) that broke the moment the fallback
  shipped and now passes again with this tag in place.
* This closes PRD §4.2's "Error middleware catch-all" item; the line is
  struck from PRD Out of Scope and a new Functional Requirement records it
  under §5
* Handlers opt into per-extractor precision at their own pace
  (`axum::Json<T>` -> `extract::Json<T>`); the fallback ensures RFC-shape
  correctness even for handlers that never migrate - confirmed by test that
  an unmigrated, bare `axum::Json<T>` handler's rejection is *already*
  wrapped into a valid `Problem` by the fallback alone
* No gear-level change is required: `canonical_error_middleware` is applied
  as every gear's outermost layer by `toolkit`'s own OoP bootstrap
  (`runtime/oop_serve.rs`), so the fallback applies platform-wide the moment
  `toolkit` ships it
* Neither the wrapper extractors nor the fallback negotiate response
  content type - both unconditionally render `application/problem+json`,
  inherited from `CanonicalError`'s own `IntoResponse` (this ADR changes
  neither). This matches every existing REST error response in this
  codebase, but means a gear serving HTML directly (server-rendered pages,
  not a JSON API - none does today) would get a JSON error body on any
  failure `canonical_error_middleware` sees, on any route in that gear,
  whether or not that route used one of these extractors. Adding
  `Accept`-based negotiation, if ever needed, is future work on
  `CanonicalError`'s `IntoResponse` and/or this middleware - not something
  the wrapper-extractor pattern itself could provide.

### Confirmation

`libs/toolkit`'s test suite exercises every `JsonRejection` variant through
`extract::Json` (syntax error, `deny_unknown_fields`, invalid enum
variant, missing `Content-Type`, oversized body) asserting the precise
status/category/reason-code triple for each; exercises `extract::Query`
against non-deserializable and missing query fields; exercises
`extract::Path` against a non-deserializable path segment and its
`>= 500` route/type-definition-bug branch; exercises the generic fallback
against synthetic foreign 4xx responses (asserting `about:blank`/status/
detail/instance/trace_id) and foreign 5xx responses (asserting the real
`internal` category, not `about:blank`); exercises the extensions-recovered
path directly, confirming a `CanonicalError` recovered from a malformed
body's extensions is used verbatim instead of either class-based guess;
exercises `ForeignPassthrough`, confirming a tagged response (even a 500
with a non-`Problem` body and a gear-specific header) survives completely
untouched; confirms 2xx/3xx and already-`Problem` responses are unaffected;
and confirms - by routing a bare `axum::Json<T>` handler through
`canonical_error_middleware` with no `extract::Json` involved - that the
fallback alone already produces a valid `Problem` for an unmigrated call
site. `oagw`'s own e2e suite (`testing/e2e/gears/oagw/`) exercises
`ForeignPassthrough` end-to-end against a real proxied upstream failure.

## Pros and Cons of the Options

### Option A: Wrapper Extractors + Generic Fallback

* Good, because precise wherever precision is achievable, honest fallback
  elsewhere - no case is left as plain text
* Good, because reuses established primitives end to end
  (`.with_override`, `#[resource_error]`, RFC 9457's own `about:blank`
  convention - independently already used by `toolkit-gateway::Forwarder`
  for analogous untyped transport failures)
* Good, because the fallback requires zero gear-level changes, given the
  universal OoP bootstrap wiring
* Neutral, because it is two mechanisms rather than one, though each has a
  narrow, non-overlapping job (precise vs. safety-net)
* Bad, because wrapper extractors require ongoing per-extractor-family
  engineering effort - `Form<T>` (unused) and `Multipart` (different
  failure shape) are not covered by this ADR, and any future axum extractor
  family would need its own

### Option B: Generic Fallback Only

* Good, because a single mechanism, applying everywhere immediately with no
  per-extractor engineering
* Bad, because every rejection - including well-understood ones like
  `deny_unknown_fields` - permanently loses its specific GTS category and
  machine-readable reason code, with no compensating benefit, since precise
  interception is available cheaply via axum's own recommended pattern
* Bad, because it forecloses the more useful answer (Option A subsumes
  Option B's coverage while adding precision) for no engineering savings
  large enough to justify it

### Option C: Wrapper Extractors Only

* Good, because always precise, never a generic guess
* Bad, because it cannot cover a genuine architectural gap: a tower `Layer`
  that resolves `Ok(response)` before axum's Router runs (e.g. a
  `Content-Length`-based body-size pre-check, confirmed via
  `tower_http::limit::RequestBodyLimit`'s source) produces no rejection for
  any wrapper to intercept, at any layer of wrapping - this is not a gap
  that "more extractors" can close
* Bad, because it leaves such cases as bare plain-text responses
  indefinitely, with no forcing function to ever notice or fix a newly
  introduced one

## More Information

`JsonRejection` -> `CanonicalError` mapping (`extract::Json`, Option A's
precise half):

| `JsonRejection` variant | status | reason code |
|---|---|---|
| `JsonSyntaxError` | 400 (category default) | `json_syntax_error` |
| `JsonDataError` | 422 (override) | `invalid_json_body` |
| `MissingJsonContentType` | 415 (override) | `missing_json_content_type` |
| `BytesRejection` | axum's own resolved status (override) | `json_body_read_error` |
| any future (`#[non_exhaustive]`) variant | debug/test: panics via `debug_assert!`, forcing an explicit update the moment axum actually introduces one. Release: logs the unhandled variant and degrades to `unclassified_json_rejection` rather than unwinding a request-serving task with no guaranteed panic isolation | (release) `unclassified_json_rejection` |

`QueryRejection` -> `CanonicalError` mapping (`extract::Query`):

| `QueryRejection` variant | status | reason code |
|---|---|---|
| `FailedToDeserializeQueryString` | 400 (category default) | `invalid_query_string` |
| any future (`#[non_exhaustive]`) variant | same status-driven logic as the known variant | `invalid_query_string` - never panics: the known arm was already fully status-driven, not variant-specific, so an unknown variant needs nothing invented |

`PathRejection` -> `CanonicalError` mapping (`extract::Path`) - not
purely a client-fault mapping, unlike `Json`/`Query`:

| `PathRejection` variant | status | maps to |
|---|---|---|
| `FailedToDeserializePathParams` (most `ErrorKind`s) | 400 | `invalid_argument`, code `invalid_path_params` |
| `FailedToDeserializePathParams` (`WrongNumberOfParameters`/`UnsupportedType`) | 500 | `internal` - a route/type definition bug, not the client's |
| `MissingPathParams` | 500 | `internal` - the extractor used outside a matched route |
| any future (`#[non_exhaustive]`) variant | same `>= 500` status-driven classification as `FailedToDeserializePathParams` | `internal` or `invalid_path_params` per the status - never panics: that classification was already status-driven, not variant-specific, so an unknown variant needs nothing invented; logged at `error!` for visibility |

Wrapper-extractor pattern precedent: `tokio-rs/axum`'s own
`examples/customize-path-rejection/src/main.rs` wraps `axum::extract::Path<T>`
identically - delegate to the built-in extractor, pattern-match its
rejection, return a custom error shape.

`about:blank` convention precedent (Option A's generic half, pre-existing in
this codebase, not introduced by this ADR): `libs/toolkit-gateway/src/forward.rs`'s
`problem()` helper already builds gateway-level `Problem`s with
`type: "about:blank"` for untyped transport failures (no-route 404,
proxy-body-limit 413, upstream 502/503/504).

See [DESIGN.md](../DESIGN.md) §3.2 Component Model and §3.7 Interactions &
Sequences for how `canonical_error_middleware` fits the existing error
pipeline.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements:

* `cpt-cf-errors-fr-middleware-catchall` - resolves the deferred "Error
  middleware catch-all" item (PRD §4.2 Out of Scope, now promoted to a
  Functional Requirement under §5)
* `cpt-cf-errors-contract-problem-response` - extends the REST error
  response contract to cover responses that never passed through a
  handler-level `CanonicalError`
