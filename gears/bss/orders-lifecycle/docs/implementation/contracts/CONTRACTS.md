# S1-02 — operation and storage contracts

Status: **verified as a design catalog and executable boundary-fixture baseline**, 2026-10-05. No Orders Rust crate, HTTP handler, migration, runtime registry or provider conformance is delivered by this package. S2-01 translates these contracts into the SDK/runtime scaffold; S1-03 proves authorization and database capabilities against it.

Start with the [readable catalog](CATALOG.md). It covers **25 routes, 11 states, 29 source transition rows expanded to 46 state/trigger keys, 21 triggers, 11 events, 24 tables and 86 refusal reasons**. The [machine catalog](catalog.json) includes operation metadata, table fields, complete normative schema sections, later schema supplements and source hashes. [Model declarations](models.json) record selected request/result fields, field classification, operational schemas, internal controls and bounds. [Fixtures](boundary-fixtures.json) contain independent expected results; [the checker](contract_checks.py) deliberately implements only the documented boundary subset.

## Authority and local binding choices

The [design API inventory](../../DESIGN.md#33-api-contracts), [Workflow SDK](../../DESIGN.md#orders-lifecycle-workflow-sdk), [state rows](../../features/01-foundation.md#contract-01-order-state-machine) and [field classification](../../DESIGN.md#contract-02-4-3) remain authoritative for business semantics. Existing D-187–D-200 choices are preserved. The following are concrete implementation selections where those contracts left adapter spelling or routine limits open. They do not add a public operation or change a commercial policy.

| Concern | Selected binding | Existing implementation consulted |
|---|---|---|
| SDK context | Trusted `SecurityContext`, typed path identifiers and call metadata separate from the authored contribution | [Products SDK](../../../../products/products-sdk/src/api.rs) |
| Request names | `snake_case` authoring DTOs; PATCH body `{ "fields": { ... } }`; normalized fixture `meta` is an oracle representation, not an HTTP body | [Pricing authoring DTOs](../../../../pricing/pricing/src/api/rest/authoring/dto.rs) |
| Version precondition | One strong `If-Match: "N"`; SDK `expected_version` is a positive signed SQL integer, at most 2147483647; reject weak/list/wildcard forms | [Pricing preconditions](../../../../pricing/pricing/src/api/rest/preconditions.rs); Orders uses its own narrower version range |
| Idempotency | `Idempotency-Key`, 1–255 printable ASCII bytes including space; preserve exact bytes, no trim/normalization | Same Pricing preconditions |
| Administrative text | Nullable `external_reference` ≤256 Unicode scalar values, `display_label` ≤200, `internal_notes` ≤2000; reject NUL; null explicitly clears a field, omission leaves it unchanged | [Pricing caps](../../../../pricing/pricing/src/domain/caps.rs) supplies character counting and 200/2000 name/note precedents; reference 256 is an Orders selection |
| Success status | Create order 201; other operations 200 with their typed result, including line removal | Selected Orders binding; no silent 202/204 protocol |
| Nullable policy scope | `policy_id` UUID primary key; semantic election/scope/scope-id key with NULL-aware uniqueness | [Pricing receipt migration](../../../../pricing/pricing/src/infra/storage/migrations/m20260930_000019_commercial_receipts.rs) uses surrogate identity separate from business uniqueness |

`expected_draft_revision` is an independent nonnegative signed bigint. REST extracts that body member into SDK metadata before validating the operation-specific contribution. Submit requires it at the boundary (`request-invalid` when absent/malformed); draft commercial edits allow omission through boundary validation, then compare it **after** engine admissibility. Zero is valid for the initial draft. Administrative edits do not acquire a new revision precondition. Required commercial `expected_version` validation still runs first and returns HTTP 428 without authorization, audit or idempotency activity.

Path order/line/version IDs cannot be overridden by the body. Actor identity, delegation authority, timestamps, grant identity/generation and correlation propagation come from trusted context or server state. SDK field names in `models.json` are normalized. D-206 (S2-09) settles one `snake_case` casing for every REST/SDK body and Problem `context.data` member, so the former `draftRevision` annotation is `draft_revision`; the Preview `tcvWithheld`/`lineIds` annotation is reconciled when Preview ships (S3). Event data retains its declared camelCase. Native Pricing receipt/query/BillingTerms encodings are not renamed or flattened by a generic case converter.

Read request declarations are query arguments, never GET bodies. Selected order-list names are `state`, `created_from` (inclusive), `created_to` (exclusive), `state_entered_before` (inclusive: continuously in the current state since that instant or earlier), and `contract_id`. They bind existing design filters. Every collection has `page_size` (default 50, range 1–200) and an opaque `cursor`; clients cannot select a different sort. Preserve the [five collection orders and cursor authority rules](../../DESIGN.md#contract-08-page-size-is-bounded). Current-parent authorization precedes child disclosure: missing/undiscoverable parent is `order-not-found`; a missing committed version of an authorized parent is `version-not-found`. Reserved/burned candidates never become readable history.

## Model and storage boundaries

`models.json` is a field-and-semantics catalog, **not JSON Schema or a production serializer**. The catalog's complete normative schema blocks retain types, nullable exceptions, foreign keys, immutable/mutable distinctions, uniqueness, retention and indexes. Grouped source columns are expanded individually; draft content contains only authored working fields, with dates/term/cycle allowed to be incomplete. Read views compose the declared schemas under current authority rather than exposing database rows directly.

| Model family | Schema source / implementation rule |
|---|---|
| Aggregate, version, line and administrative projection | [Foundation tables](../../DESIGN.md#contract-01-3-7); order-scoped identity and version-scoped membership are different; administrative values are mutable and outside accepted-binding verification |
| Authored selections and pin | [Gate pin schema](../../DESIGN.md#contract-03-4-3) and [D-192 snapshot](../../DESIGN.md#contract-03-frozen-commercial-snapshot); exact item IDs, decimal quantities, dimension values, full native receipt/query/BillingTerms and selected binding identities; schema 2 and explicit unsupported-profile refusal |
| Resolved totals | [Total schema](../../DESIGN.md#contract-01-table-orders_resolved_total) and [Rating contract](../../DESIGN.md#contract-03-rating-purchase-evaluation); integer minor units, currency/rounding evidence, charge/cycle breakdown, exclusions and withheld/unknown amounts; never infer zero from absence |
| Diagnostics | [D-195 mapping](../../DESIGN.md#contract-03-diagnostic-mapping); complete expected coverage, source identities and passed/failed/unevaluable results; absent selection, selected NULL scope and the literal string `default` remain distinct |
| Read-through contract terms | [Read projection](../../DESIGN.md#contract-08-4-2); renewal election, term windows and notice ladder are displayed under authority, never authored or treated as an Orders policy |
| Commercial attempt | [D-188](../../DESIGN.md#contract-01-commercial-attempt); immutable proposed inputs and receipt identities, mutable fenced execution status; no public projection |
| Fulfillment grant and control | [D-198](../../DESIGN.md#contract-06-activation-admission); immutable grant versus operational barrier ledger; no public aggregate projection of owner tokens, leases or pending execution details |

UUID identifiers stay UUID typed; calendar dates stay dates, event/operational times use UTC instants with retained microseconds, monetary values remain integer minor units, and quantities remain exact decimals. Capture interval intent must use the exact BillingTerms conversion contract, never day-based approximation. S1-04 owns compiling golden native codecs and owner-specific interfaces. This catalog does not fabricate those missing provider APIs. S1-05/S5-01 must settle the executable receiver binding before final S2-02 grant/control migrations.

Every authored field belongs to exactly one class in `models.json`. Any commercial or commercial-frozen PATCH field selects `draft-mutate`/`order:write`; administrative-only fields select `administrative-edit`/`order:edit`. The classifier does not read current state. Mixed fields select one trigger and then reach the existing refusal ordering, never two commits. `resource_tenant_id` is editable in draft but frozen after submit; `seller_tenant_id` is frozen from creation. Unknown/read-through fields are boundary-invalid. Field caps do not replace the independent serialized 1 MiB version and 64 KiB event limits.

## Typed fulfillment contribution and internal control

The selected normalized `SpawnContribution` contains `fulfillment_attempt_id: UUID` and `receivers`, each carrying the stable receiver identity and exact line roster. Each line contribution carries `line_id`, selected `acceptance_id`, producer `terms_digest`, and existing `create_key`/`activate_key`. D-198's server verification recomputes roster agreement against the committed current version and selected receipts, verifies caller authority and derives the immutable digest; callers cannot submit `grant_id` or `generation` to obtain authority. Concrete cross-owner identity types and codecs are frozen in S1-05 against the receiver SDK rather than assumed to exist today.

`SpawnSignalResult` contains `transition`, `spawn_signal_at`, server-issued `grant_id` and `generation`. They become visible only after the atomic engine commit; replay returns the same identities. Commercial version 7 and dispatch generation 3 in the golden result intentionally differ.

The `internal_operations` declarations specify preparation/finalization of existing receiver controls. Preparation records exact original authority, execution/attempt/generation, immutable roster and a fenced owner; it leaves public business state unchanged. Receiver work runs outside the SQL transaction. Finalization checks the same pending pointer, fence, current authority/version and complete receiver-bound evidence before committing the existing transition. Unknown outcomes cannot satisfy the ordinary no-active-service barrier; the explicit D-182 forced-failure exception retains uncertain claims. Expiry and draft auto-void also have finite, target-bound maintenance inputs and no REST route. Successor-grant, freshness and actual-start technical corrections belong to S5-01, not an invented public control API.

## Error and event mapping

Every Orders reason has one `orders-lifecycle.v1` code, reason GTS key and canonical Problem category/type/title/status in `catalog.json`. Guard-specific missing data remains a guard refusal where the design requires precedence: missing approval authority, cancellation reason and compensation evidence must not be prematurely converted into generic schema failures. The oracle covers selected closed-union shape checks, not those guards. Unknown categories/profiles, registry failures and permission failures must fail closed.

The current [contract-error macro](../../../../../../libs/toolkit-contract-macros/src/contract_error.rs) builds [ProblemCategory](../../../../../../libs/toolkit-canonical-errors/src/problem.rs) Problems. Its default status for `FailedPrecondition` is 400, and it does not offer a per-variant HTTP override. The REST adapter must map `EXPECTED_VERSION_REQUIRED` to 428 without changing domain/code/type; fixtures use the actual `ProblemCategory` titles (for example, `Failed precondition`). The macro also serializes variant payload fields into `context.data`; use explicit allowed diagnostic fields and sanitized detail, particularly for authorization-context races. Do not serialize raw upstream failures, tenant facts, credentials or private attempt/control data. Preserve an unknown downstream Problem for SDK fallback rather than inventing an Orders business reason.

The event catalog preserves all eleven declarations and their source row mapping. Events are bounded references to authoritative committed versions, not expanded receipt/total snapshots. Sparse amendment examples carry `supersedesVersion`; completion includes per-line subscription linkage. Envelope tenant is the configured Event Broker ROOT tenant, not an order party. Golden UUIDs, ROOT tenant and digest values are **synthetic fixtures**, not deployment settings or native receipt proof.

## Verification and next agent assignment

Run from the repository root:

```sh
python3 gears/bss/orders-lifecycle/docs/implementation/contracts/build_catalog.py --check
python3 gears/bss/orders-lifecycle/docs/implementation/contracts/contract_checks.py
python3 gears/bss/orders-lifecycle/docs/implementation/validate_docs.py
python3 gears/bss/orders-lifecycle/docs/implementation/build_coverage.py --check
```

The fixture oracle checks the original 73 cases and eight golden envelopes: expected-version/ETag and draft-revision omissions, idempotency bounds, unsupported fields, PATCH classification, selected union constraints, page limits, state-key lookup, error mapping, registry mutation rejection and lossless JSON representation. Golden REST metadata/body extraction, result fields, Problem mappings and selected event invariants are checked. This is not complete request type validation, native receipt codec validation, authorization enforcement, PostgreSQL constraint proof, crash recovery or a 200-line payload load test. S1-06/S2 and later packages own those runtime suites.

Required runtime startup failures are cataloged separately: duplicate state/trigger or reason keys, unknown guard row/reason, incomplete operation permission/model declarations, unclassified authored fields, event mismatch and unsupported schema/profile registration. The Python negative registry cases show expected rejection behavior; they do not prove an Orders runtime boots or refuses to boot.

Next assignment: implement **S2-01 SDK/runtime scaffold** from this catalog, then run **S1-03 capability proofs** with the real toolkit and PostgreSQL. Compile Rust boundary and serialization tests from these independent expectations, preserving native types and the adapter exceptions above. Continue S1-04/05 provider interface work alongside the scaffold; keep launch/profile readiness gated on actual owner implementations. Do not claim the rest of S1 or any runtime milestone complete from these files alone.

**S2-03 boundary addition (confirmed by D-202, [08 §4.4](../../DESIGN.md#contract-08-4-4)).** Optional request header `X-Delegation-Proof-Ref` (wire `x-delegation-proof-ref`) carries the opaque delegation proof reference for every operation, including reads: present at most once, 1–512 printable non-space ASCII, otherwise `request-invalid` before authorization, idempotency probe or any read. It is forwarded unvalidated to PDP and stored only as a *supplied* evidence reference. Fixtures B44–B52 cover absent, valid, empty, spaced, oversized, maximum, duplicate and non-ASCII values; the checker now validates 82 cases.
