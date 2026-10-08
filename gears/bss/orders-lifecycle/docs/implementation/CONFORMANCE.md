# S1-06 — Shared conformance corpus and deterministic fault seams

Status: **implemented as local fixtures, reference encoders and PostgreSQL harness evidence**. Business release remains disabled. The fixtures now compile and execute in the existing runtime/SDK test targets. Actual engine, provider, broker, receiver, payment and read implementations must consume them in their owning packages. No production handlers, dependencies or migrations were added. Local files; no commit/PR.

The [evidence index](conformance/index.json) maps seven fixture families to source contracts, owner packages, observed local evidence and remaining runtime work. It extends the planning [coverage inventory](COVERAGE.md), not its implementation-completeness claim. S1-07 still owns the full per-provider deployment ledger.

## Delivered corpus

| Family | Files / executable venue | Evidence and limit |
|---|---|---|
| Audit v1/v2, genesis, checkpoints, framing | [Frozen vectors](../../orders-lifecycle/tests/fixtures/conformance-v1.json), [Rust byte suite](../../orders-lifecycle/tests/conformance_vectors.rs) | Independent Python preimages and SHA-256; Rust encoder matches exact bytes. Covers create, later transition/resource change, resolved/unresolved refusal in both versions, v2 after v1, null/present/empty caller reason, empty/two-member roll-ups, null/empty, adjacent fields and Unicode. Runtime writer/verifier remains S2-06/11 |
| Request identity | Same vectors and Rust suite | Proposed fixture format includes exactly operation/trigger/target/three axes/expected version/draft revision/document; verifies every semantic field and key-order stability. S2-05 still owns typed request projection and actual idempotency |
| Cursor bindings | Same vectors and Rust suite | All five collections bind endpoint, parent, authenticated subject tenant/subject, normalized filters, direction and exact position. A one-microsecond position change changes bytes. S6-02 still owns token encoding/validation, SQL pagination and current authorization |
| Native commercial receipts | [Four frozen receipts](../../orders-lifecycle-sdk/tests/fixtures/commercial-conformance.json), [SDK suite](../../orders-lifecycle-sdk/tests/conformance_commercial.rs) | Real public native digest checks/round trips for finite month/year, rolling, payer arrangements, all three charge kinds and native default versus literal `default`; 200 unique-line/acceptance receipts fit the receipt-only fixture budget. Synthetic encoding inputs are not owner-issued acceptances or live eligibility proof |
| Commercial integration scenarios | [13 scenario recipes](../../orders-lifecycle/tests/fixtures/commercial-scenarios.json) | Self-service, partner/delegation, third-party payer; published/scheduled/superseded/closed/retired; finite/rolling/mixed; one-time/usage; 200/201 lines; unavailable owner and dimension identities. Explicit expected facts and owner packages; real flow execution remains pending |
| Crash/recovery boundaries | [16 fault contracts](../../orders-lifecycle/tests/fixtures/fault-contracts.json), [test hooks](../../orders-lifecycle/tests/conformance_support/faults.rs), [PostgreSQL tests](../../orders-lifecycle/tests/conformance_support/pg_faults.rs) | Fail-once selector, one-shot rendezvous and injected clock; real transaction rollback and commit-with-lost-reply probes. Remote seams are defined but not attached to absent runtime providers |
| Receiver/consumer recovery | [S1-05 corpus](PROCESS.md) | Reuses the 151 reference-model cases, including the required 13 consumer cases. Does not duplicate them into another potentially divergent specification or claim real consumer conformance |

There are **27 frozen byte/hash vectors**, four additional native receipt vectors with **16 frozen digest preimages**, 13 integration recipes and 16 before/after fault points. The runtime suite gains six byte-contract tests and four harness tests; the SDK gains two commercial tests. These are distinct from scenario recipe counts.

## Independent bytes and exact scope

[build_vectors.py](conformance/build_vectors.py) uses Python standard-library UUID/network bytes, explicit big-endian framing and `hashlib.sha256`. [The Rust reference encoder](../../orders-lifecycle/tests/conformance_support/encoding.rs) separately transcribes the normative field order and framing; it does not load an expected encoder specification from the generator. Tests read frozen preimages and never regenerate expected values. PostgreSQL independently computes SHA-256 over the stored bytes in a disposable generated-column probe and round-trips microsecond instants, including one microsecond before the Unix epoch. Production audit hashing must still use the approved cryptographic provider.

The audit suite rejects unsupported hash versions, v1 caller evidence, zero/overflowed sequence, overflowed commercial version, malformed timestamp text and digest lengths. Each covered v2 field is mutated and must change bytes or be rejected. Checkpoint tests reject duplicate/unsorted/missing/miscounted members and unsupported versions. Framing separates null from empty and adjacent text fields; exact UTF-8 is preserved without normalization. These tests do not implement full-chain validation, immutable namespace enforcement, snapshot completeness, tamper recovery or role isolation for the final schema. S2-06/11 must add those implementations and consume these vectors.

The native extension [build_commercial.py](conformance/build_commercial.py) reuses S1-04's independent public Pricing projection in read-only verification mode. Its request, terms, selected-binding and billing-terms canonical preimages are also frozen and SHA-256 checked separately. Its first Rust run found and corrected a generator error: rolling terms hash as the public SDK's string `rolling`, while the receipt wire represents a tagged term object. This is now pinned independently. Existing meter/state omissions from Pricing digests remain covered by S1-04's exact-binding checks; these fixtures do not change that owner contract.

The 200-line test constructs a reproducible basket of individual receipts using the real codec, distinct line IDs and acceptance IDs, and exact public digest recomputation. It is a size/construction fixture, not a claim that the absent whole-version submit serializer enforces 200 lines or 1 MiB. Whole-version overhead, 201-line rejection and unavailable-owner/catalog scenarios remain S3-12/13 integration work.

## Proposed request/cursor byte profiles

Audit bytes are normative. Request and cursor designs specify binding semantics without choosing concrete bytes. To make the S1-06 corpus reviewable, these two families use explicitly **fixture-only v1 profiles**. S2-05 and S6-02 must adopt or deliberately replace them before runtime use; these are not registered public formats.

- Request prefix: ASCII `VHP-BSS-ORDERS-REQUEST-FIXTURE-v1` plus `0x1f`. Cursor prefix: ASCII `VHP-BSS-ORDERS-CURSOR-FIXTURE-v1` plus `0x1f`.
- Append compact UTF-8 JSON, recursively sorting object keys by UTF-16 order; preserve array order and string content. Values are strings, booleans, nulls, arrays and objects. Numeric meanings use exact strings; general JSON numbers are rejected by this restricted oracle. Operation-specific normalization, supported fields, omitted-versus-null semantics and numeric conversion belong to the future typed adapters. The corpus does not pretend to implement arbitrary RFC 8785 numeric canonicalization.
- Request outer nulls are fixed create/not-applicable sentinels; legitimate non-create target/version remain mandatory. Document omission and explicit null stay distinct. Stable principal scope and key identify the registry record separately; correlation, transport, request time and server assignments never enter the fingerprint.
- Cursor identity includes version, endpoint, parent, authenticated subject/tenant, normalized supported filters, sort and tuple. Outer encoding/integrity, malformed-token rejection and SQL continuation are deferred. A binding digest is not an authentication credential and does not preserve access after revocation.

S2-05 adopts the request profile v1 byte-for-byte for the runtime registry fingerprint (stored as `rf1:<sha256 hex>`); `domain::idempotency::tests::runtime_fingerprint_matches_frozen_independent_vectors` checks both frozen preimages and digests ([IDEMPOTENCY](IDEMPOTENCY.md)). The cursor profile remains S6-02's decision.

The byte tests check preimage identity, not runtime request projection or acceptance of untrusted tokens. Those packages must add negative boundary, replay and current-authorization tests against their actual implementations.

## Fault injection and test venues

The fault manifest assigns **before and after** points around remote acceptance, local commit, outbox enqueue, outbox acknowledgement, receiver admission, receiver confirmation, grant revocation and payment response. Each point records durable facts, recovery, forbidden outcomes, owner package and required venue. Every hook is exercised for selection and exactly-once firing. The test clock samples microseconds explicitly; rendezvous uses oneshot channels and no sleeps or polling.

The real PostgreSQL probes reuse S1-03's disposable container, secure ORM and recording PDP fixture:

1. A failure just before commit rolls back both the proposed aggregate change and audit probe row.
2. While a transaction is paused at the test gate, an independent connection sees the old committed version and no new evidence. After release, the worker samples the advanced injected clock, commits both rows, then loses its response. Re-reading finds the new version and exactly one evidence row.
3. The database stores the frozen preimages, independently recomputes every expected SHA-256, and round-trips the chosen signed microsecond instants exactly.

These are probes of the harness/database transaction behavior, not actual Orders transition/sequence/outbox/idempotency recovery. The generated-column table exists only in the isolated test migration. It does not introduce a runtime SQL hashing dependency or complete S2-02. Likewise, the injected clock is not a substitute for sampling fresh database time in the future lease/guard implementation.

| Required venue | What it establishes | Remaining owner |
|---|---|---|
| Pure Rust / public native SDK | Byte framing, lossless values, digest linkage, fixture construction | Runtime domain implementations must reuse the same expectations |
| Secure repository + actual PostgreSQL | Current local probes; later full FK/uniqueness/locks, owner takeover and transactional invariants | S2-02–08, S5-06b/09 |
| Authenticated REST/SDK + actual providers/broker | Real grants, live policy, remote command ambiguity, dedup effects, receiver barriers | S3-13/14, S4-12, S5-16, S6-05 |
| Load, operational recovery and DR | Latency/capacity, replay limits, restoration and retained evidence | S6-06/07/09 |

SQL fixture success does not establish real provider conformance. The remote hooks will gain runtime evidence only when inserted into actual owner implementations, including crash after external effect but before local completion, stale worker publication, and receiver close/confirmation interleavings. S5-01 amendments remain prerequisites for the corrected receiver paths.

## Verification

Run from the repository root; Docker is required for PostgreSQL suites.

```sh
python3 gears/bss/orders-lifecycle/docs/implementation/conformance/build_vectors.py --check
python3 gears/bss/orders-lifecycle/docs/implementation/conformance/build_commercial.py --check
python3 gears/bss/orders-lifecycle/docs/implementation/conformance/check_fixtures.py
python3 gears/bss/orders-lifecycle/docs/implementation/process/check_contracts.py
python3 gears/bss/orders-lifecycle/docs/implementation/contracts/contract_checks.py
python3 gears/bss/orders-lifecycle/docs/implementation/commercial/check_contracts.py
cargo +1.98.1 test -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk
cargo +1.98.1 clippy -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk --all-targets -- -D warnings
cargo +1.98.1 fmt -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk --check
cargo gears lint --dylint -P cf-gears-bss-orders-lifecycle-sdk -P cf-gears-bss-orders-lifecycle
python3 gears/bss/orders-lifecycle/docs/implementation/validate_docs.py
python3 gears/bss/orders-lifecycle/docs/implementation/build_coverage.py --check
```

Observed on 2026-10-06: all **38 Rust tests passed** (12 added in this package), including real PostgreSQL and the existing compile-fail capability test. The final changed byte/native fixture suites also passed after review fixes. Clippy with warnings denied, formatting, targeted architecture lint and new-file syntax/whitespace checks passed. Independent vector checks, all 151 process cases, 73 boundary cases/eight golden envelopes, and 36 commercial cases passed. Documentation validation checked 4298 local links; unchanged coverage verified 27 source documents, 2557 units and 44 upstream requirements. `git diff --check` passed.

S1-07 local inventory is now recorded in [READINESS](READINESS.md). Next: **S2-02 — schema and secure repositories**, using the reconciled [S5-01 contracts](RECEIVER_CONTRACTS.md). Maintain per-upstream provider/identity/grant evidence and preserve the seven evidence families and all S1-03–06 provider gaps. Public business readiness remains false.

D-201/S5-01 extends the corpus with nine audit-v3 vectors; all 27 S1-06 vectors remain unchanged.

**S2-06 gap-review correction (2026-10-06).** The `v1-`, `v2-` and `v3-resolved-refusal` inputs carried `to_state = NULL`, which contradicts Foundation §3.7: `to_state` is NULL only when the aggregate was not resolved, and a resolved refusal keeps its observed state and version (D-98). The S2-02 row CHECK and the `v3-force-request` vector already used `to_state = from_state`. The independent generator now authors those three inputs with `to_state = from_state` (`draft`). Only those three preimages and digests changed; every other input, preimage and digest, including the other nine v3 vectors, is byte-identical. Encodings and tags are unchanged, and no stored evidence existed. A PostgreSQL test stores every refusal vector as a row under the S2-02 CHECKs and verifies it with the runtime verifier, and it shows the former NULL shape is refused ([AUDIT](AUDIT.md)). Current results and handoff are in [RECEIVER_CONTRACTS](RECEIVER_CONTRACTS.md). New writers use v3; the original results above remain historical evidence.
