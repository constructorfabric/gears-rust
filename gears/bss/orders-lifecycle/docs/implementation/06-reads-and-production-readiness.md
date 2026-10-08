# Stage 6 — Read surfaces, operational recovery and production evidence

Status: **early S6-01 draft reads and S6-04 read-access logging implemented, independently gap-fix reviewed and locally verified** ([READS ledger](READS.md), D-210; live HTTP E2E in [MILESTONE](MILESTONE.md)); the remaining S6 packages are planned. Read [README](README.md), the full [read feature](../features/08-read-and-authz.md),
[operational architecture](../DESIGN.md#4-additional-context) and the linked upstream contracts.
This stage contains early read packages as well as final release work: it must not delay shared
write authorization or make the first draft impossible to inspect.

## S6-01 — Implement coherent current/historical read projections

**Depends on:** S2-02/03 and the relevant data producer. Draft subset starts with S2-09; complete projection joins S3/S4/S5. **Owner:** Orders.
**Files:** proposed `orders-lifecycle/src/{domain/read,infra/storage/repo/read,api/rest/read}.rs`, SDK read models.
**Sources:** [all read interfaces](../DESIGN.md#contract-08-3-3), [read disclosure](../DESIGN.md#contract-08-4-2), [point flow](../features/08-read-and-authz.md#contract-08-scoped-read).

- Implement order detail, current/versioned lines, version detail/list and acceptance history against the stored aggregate/current-version pointer, never event replay or a chain walk. No replica or stale-cache success.
- Shared prefetch/PDP/re-read wrapper starts a consistent scoped snapshot after authorization; changed relevant properties restart authorization before disclosure. Use current-parent authority even for historical versions; former payer loses its path.
- Compose full schema-2 receipt/query/bindings, exact stored dates/policy, total basis/exclusions, fulfillment projection/linkage, expected fulfillment/deferral, market, overlap key, payer and deadlines. Do not revive legacy `items[].chains[]` mapping or derive evidence from current catalog.
- Return current commercial ETag and draftRevision coherently; sparse committed history is valid and explicit supersedes_version is authoritative. Reserved-only attempt candidates look like ordinary missing versions to callers.
- Keep internal guards, registry, operational attempt/control/grant internals, outbox/dead letters and private diagnostic details out of commercial reads. Exact committed receipt handoff uses finite order-ID scope; history is not activation permission.
- Require S6-04 served access logging before exposing cross-tenant reads. Output exclusions and nonbinding totals must be visible, including usage/unavailable distinctions and per-period money basis.

**Tests/done:** simultaneous amendment/draft edit/payer change cannot produce a mixed snapshot; missing/hidden target has the same denial/outage shape; historical access cannot resurrect old authority; corrupted/unsupported stored codec fails closed. Cover REST and local SDK, empty drafts and completed history.

**Early delivery (2026-10-07, draft subset only):** the composed draft detail, the order list and the per-order line page are implemented, independently gap-fix reviewed and locally verified ([READS](READS.md), [D-210](../DECISIONS.md#d-210--draft-read-wire-bindings-the-cursor-token-and-the-snapshotaccess-log-transactions-early-s6-01s6-04)): prefetch → shared PEP → one read-only snapshot through the decided scope → composition from stored rows, with the §3.6 hidden/missing pattern. Versions, acceptance, audit and every committed-version member remain with this package's completion; a non-draft order answers unavailable on the point and line reads.

## S6-02 — Implement bounded SQL-scoped collections and cursor contracts

**Depends on:** S6-01, S2-03. **Owner:** Orders.
**Files:** cursor/filter codecs, read repositories, indexes, DTO/OpenAPI query contracts.
**Sources:** [pagination contract](../DESIGN.md#contract-08-page-size-is-bounded), [list algorithm](../features/08-read-and-authz.md#contract-08-paginated-list).
**Precedent:** [Approvals versioned cursor](../../../approvals/approvals/src/domain/cursor.rs); reuse the validation pattern, not its composite-source semantics.

- Validate supported filter vocabulary, values, page-size range 1–200/default 50 and versioned cursor binding before access-log writes. Bind principal/tenant, endpoint, parent, normalized filters and sort; preserve exact microsecond precision and binary UUID comparison.
- Authorize each page; SQL applies complete PDP scope, filter and strict keyset boundary before ORDER/LIMIT. Never post-filter an unrestricted page or derive authorization from a cursor.
- Implement exact immutable orders: order creation time/ID; descending commercial version; line identity creation time/ID joined to selected membership; descending accepted version; ascending audit timestamp/ID. Fetch page size + 1 and encode last returned tuple only when needed.
- Cursor continuation survives deletion of the cursor row by allowed retention; no lookup to validate position. Read authorization changes may narrow later pages.
- Implement every design filter/index combination and preserve documented live-pagination limitations. Audit API ordering does not replace audit sequence integrity checks.

**Tests/done:** same-time ties, missing/extra/malformed fields, unsupported versions, principal/parent/filter/sort replay, 0/1/200/201 sizes, removed draft members, sparse versions and retained-row deletion. PostgreSQL EXPLAIN/production-scale fixtures prove scoped index access, not full scans or cross-tenant leakage.

## S6-03 — Implement separately authorized audit retrieval

**Depends on:** S2-06, S6-01/02/04. **Owner:** Orders.
**Source:** [audit retrieval](../features/08-read-and-authz.md#contract-08-audit-retrieval).
**Files:** audit read query/response and distinct action registrations.

- Require current order access plus audit-read permission. Resolved committed/refused entries use current-parent access; unresolved entries require independent subject-tenant-scoped audit-unresolved grant.
- Match requested_order_ref only within that extra scope; ID equality or immutable chain namespace conveys no access. A missing order never gains a readable trail.
- Merge disjoint authorized branches before ordering/limiting. Optional-grant denial omits unresolved rows; provider outage fails the whole response, never a partial trail.
- Return permitted stored actor/reason/correlation/key evidence without identity enrichment, private diagnosis or a promise of exhaustive refusal history. Persist required served logging first.

**Tests/done:** every permission combination, denied optional grant vs outage, unresolved foreign tenant, hidden/missing order, pagination across both branches and logging failures. Preserve Q-20 product status of the audit surface; no automatic new customer grant.

## S6-04 — Implement read-access logging and disclosure failure semantics

**Depends on:** S2-02/03; starts alongside S6-01 before first public read. **Owner:** Orders.
**Sources:** [log schema](../DESIGN.md#contract-08-table-orders_read_access_log), [logging policy](../DESIGN.md#contract-08-4-4).
**Files:** access-log repository, read wrapper, retention integration and metrics.

- Implement served/refused decision table for every REST/SDK read. Supplied proof or foreign resource tenancy causes required served logging; broad/unknown collection scope logs once even for an empty page. Provably own-resource reads without proof do not log.
- Keep current/requested targets distinct, list targets NULL, trusted actor/class, timestamp and reference provenance. Public targeted proof refusal stays order-not-found; its detail remains operational only.
- Commit required served log before returning protected payload; write failure returns read-store-unavailable with no payload. Refused-log failure preserves the original refusal and emits a failure signal.
- Boundary validation and malformed/missing PDP constraints are not access refusals and produce no access-log row. Wire 90-day retention through S2-11, never commercial-history deletion.

**Tests/done:** route census covers every served/refused/empty/mixed collection and child read; log-failure injection, identity removal and denied privilege paths. No false claim of a committed log after a storage failure.

**Delivery (2026-10-07, for the three delivered reads):** implemented, independently gap-fix reviewed and locally verified ([READS](READS.md)). The served/refused decision table, the §3.7 target columns, the D-141 operational detail, the served-before-disclosure and refused-log-failure semantics, the no-row rule for input validation and the S2-11 90-day purge are in place for `get`, `list` and `list_lines`; each later read surface applies the same wrapper when it ships.

## S6-05 — Deliver event consumers and supported operator recovery

**Depends on:** S2-08; relevant Workflow/Subscriptions/Billing consumers may build after S1 contracts, before end-to-end release. **Owner:** Platform + each consumer owner + Orders.
**Sources:** [consumer semantics](../DESIGN.md#contract-01-event-consumer-contract), [D-200](../DESIGN.md#contract-01-event-platform-integration), [event upstream register](../UPSTREAM_REQS.md#27-event-broker).
**Existing code:** [producer processor](../../../../system/event-broker/event-broker-sdk/src/producer/outbox.rs), [toolkit outbox](../../../../../libs/toolkit-db/src/outbox/mod.rs).

- Implement consumer-owned durable inbox/deduplication and atomic effect/idempotency protocol, fresh scoped Orders reads and stale/inapplicable notification handling. Same event identity on retry must not produce duplicate business effects.
- For read outage, persist bounded operational retry/escalation according to the consumer contract; do not acknowledge an unrecoverable lost business obligation or invent order state from payload. Broker root access is not an order-read grant.
- Deliver the missing supported producer dead-letter republication API/adapter preserving original identity/business payload after chain advancement. Implement authenticated shared operator listing/claim/recover/audit/runbook through the platform owner; toolkit claiming and consumer DLQ helpers alone are insufficient.
- Verify root UUID source, actual producer/consumer grants, partition agreement, schema validation, deployed initial-cursor retry revision and empty-cache transient regression.
- Run the shared `orders-events` corpus for Workflow, Subscriptions and Billing: duplicates, gaps, out-of-order/inapplicable versions, terminal events, unknown schema, unavailable reads, rejection/republication, operator concurrency and lost acknowledgements.

**Tests/done:** each consumer has its own evidence and grant record; recovered messages require no new Orders transition and no payload/identity rewrite. Inspectable DLQ and tested operator authorization/recovery exist before event-producing production release.

## S6-06 — Implement observability, capacity and latency evidence

**Depends on:** instrumentation starts in every producing package; final evidence after integrated S2–S5. **Owner:** Orders + Platform + provider owners.
**Sources:** [capacity/cost](../DESIGN.md#41-capacity-and-cost), [shared observability](../DESIGN.md#44-observability) and every feature's architecture §3.8.
**Files:** metrics/traces, dashboards/alert definitions in the repo's established deployment locations, benchmark fixtures and operational runbook.

- Census all eight feature metric/alert sections: transition/guard/replay, capture/date/classification, gate per-port/breaker/bulkhead, amendment, consent/tolerated payment failure, verdict/line topology, overdue/forced failure/TTL, read latency/log denial, plus operational attempt/control backlogs.
- Preserve correlation through audit, events, every owner port and Subscriptions. Sanitize logs and avoid credential/PII exposure or unbounded raw-tenant/order metric labels.
- Measure request-start → commit → broker acknowledgement separately. Governing write-plus-publish p95 remains <1 s, reads p95 <200 ms; the unapproved 30 s event proposal is not a passing criterion. Attribute authorization and external resolution time.
- Exercise 50 committed transitions/s, 250 engine-entering requests/s, 200 refusal audits/s and platform producer capacity 200 events/s as current design baselines; size actual mean/peak storage including indexes. Reconcile stale pre-attempt row-growth estimates using measured schema footprints.
- Measure pending age/backlog as well as completed percentiles, every producer outcome and permanent dead letter. Validate 64-KiB event envelope and 1-MiB immutable version constraints under worst supported shapes.
- Prove retention throughput meets ingress, 24-hour checkpoints/30-day verification, early draft-flow failure alerts, provisional policy/overdue/forced-unknown alerts and breaker recovery. Alert windows/owners and runbook actions are concrete before release.

**Tests/done:** sustained load and fault injection produce the expected alerts, not just metrics. Q-11/Q-16 budget conflicts remain explicit if targets fail; require documented product change before weakening them. No fabricated performance result from timeout configuration.

## S6-07 — Prove security, durability, residency and disaster recovery

**Depends on:** S2-03, S6-01–06; platform deployment support. **Owner:** Platform + Orders.
**Sources:** [deployment](../DESIGN.md#38-deployment-topology), [security](../DESIGN.md#42-security-posture), [data protection](../DESIGN.md#43-data-protection-residency-and-retention).

- Demonstrate synchronous commit quorum with a standby in a second failure domain within the residency boundary; database primary-only reads; backups, continuous WAL archive and a tested promotion/restore path.
- Run a release DR drill for the selected RPO-zero/intra-cell and RTO-60-minute scope, retaining registry/attempt/control/grant/audit/outbox consistency and identity stability. Do not claim recovery from total jurisdictional-cell loss.
- Prove all stores, standby, backups, logs and optional audit anchors satisfy residency and platform encryption/TLS/KMS controls. Runtime owns infrastructure/grants; no unrelated custom cloud stack is introduced.
- Test data minimization, trusted opaque actor identity, current-parent access after relationship changes, proof revocation and profile deletion without historical rewrite. Retention/privacy approval is distinct from a UUID-format unit test.
- If optional independent audit anchoring is selected for a deployment, implement post-commit idempotent export, acknowledgement checking, immutable retention and restore verification under separate credentials. Keep it disabled and make no stronger integrity claim otherwise.

**Tests/done:** attached real environment/grant/restore evidence, failed/disconnected lock-session recovery and security negative cases; known unsafe configuration cannot be waived by passing mocks. Open platform identity lifecycle guarantees stay attributed to their owner.

## S6-08 — Implement safe revision-reference release, or prove it remains disabled

**Depends on:** Stage 3 attempts/snapshots and Stage 5 receiver/handoff; may be deferred with references retained. **Owner:** Pricing coordinator + Orders/Subscriptions usage providers.
**Source:** [D-196](../DESIGN.md#contract-03-revision-reference-protection).
**Existing pattern:** Pricing alone owns the Products reference registry; do not add Orders as a second reference owner.

- Deliver durable seller/revision release generation and admission closure across all possible writers; drain in-flight/ambiguous commercial attempts that could still commit.
- Implement authorized Orders committed-nonterminal/pending-attempt and Subscriptions usage reports bound to the same generation, including gap-free transfer and unresolved receiver claims.
- Require fenced acknowledged zero-holder evidence before Products release. Unknown/stale/missing answers retain protection; a count without admission closure is not proof.
- Test closure against pending acceptance, final commit, initial activation, late handoff/reply, failed participant, restart, stale report and concurrent new revision work. Retire/close behavior still follows fresh Pricing eligibility.

**Done:** either implementation and race tests pass before cleanup is enabled, or deployment evidence confirms cleanup disabled and references retained. The latter permits the new-sale release but does not mark D-196 implementation complete.

## S6-09 — Close every requirement with a release ledger

**Depends on:** applicable S1–S6 packages, except explicitly conditional scope. **Owner:** delivery coordinator and domain owners.
**Files:** package evidence ledger, [coverage inventory](COVERAGE.md), configuration/runbook/deployment evidence in established repository locations.

- Run all four PRD use cases and all §12 acceptance criteria against real providers, across both sales paths, current/new payer and every admitted lifecycle state. Reconcile the selected design exceptions from REVIEW rather than ignoring contradictory acceptance language.
- Map every declared FR/NFR, constraint, flow/algorithm/state/DoD, API, table, upstream ask and ADR to implemented code/test/evidence or an explicit deferred/conditional/open disposition. Coverage routing is a planning aid, not test proof.
- Complete startup/readiness/role/worker checks and all cross-service failure cases. Standard tests/lints are scoped to changed packages; PostgreSQL concurrency, auth and provider checks run in their actual venues.
- Update implementation checkboxes only after relevant evidence passes; record exact commands, versions, environment, limitations and runbook owner. Do not claim producer acceptance means consumer completion.
- Confirm launch scope: net-new acquisition; no change-order, CPQ/independent binding quote, refund/capture or subscription lifecycle ownership silently added. Preserve operational unknown outcomes and product-owned unresolved policy gates.

**Exit:** a reviewer can follow each claimed guarantee to code and evidence, identify every outstanding gate and start/stop/recover the system from its runbook. No blanket “production complete” from authored plans or local doubles.
