# Follow-up review remediation — 2026-09-23

The six findings from the uncommitted-change review are addressed in the design and checker.
This records document remediation, not runtime delivery, Product agreement or deployed integration.

| Finding | Resolution | Authoritative location |
|---------|------------|------------------------|
| CI fails on valid design text | Distinguish contributed/reused reasons, SQL column lists/Rust calls and FK target columns; use stable instruction IDs for guard ordering; bound settlement enumeration parsing. Update stale fixtures and add valid/invalid parser cases. CI also triggers on checker, Makefile and workflow edits. | [Checker](../../../../../scripts/check-design-invariants.py), [fixtures](../../../../../scripts/test-design-invariants.py) |
| Assessment replay has no persisted binding | Settled idempotency responses retain a versioned immutable response and assessment result; assessmentId binds to diagnostic run_id. Replay reauthorizes disclosure and returns the original snapshot. | [Foundation §3.7](../design/01-foundation.md#37-database-schemas-and-tables) |
| Refusal returns before diagnostics persist | The composite gate guard and early input-failure path explicitly write complete diagnostic outcomes before settlement/audit commit. The overlap-conflict branch records its authoritative result. Engine-only and stale-input refusals expose no assessment. | [Foundation §3.6](../design/01-foundation.md#36-interactions-and-sequences), [Gate §3.7](../design/03-gate-and-pin.md#37-database-schemas-and-tables) |
| Direct seller/payer read logging contradicts delegation terminology | One decision table covers delegated/direct cross-tenant reads, own-resource-tenant reads, mixed/empty pages, refusals and logging failures. Proof is required only for delegated paths. | [Read/Authz §4.4](../design/08-read-and-authz.md#44-delegation-proof-normative) |
| Event consumers both require and avoid callback reads | Complete event-time content and current applicability checks are separate. Q-25 explicitly routes the PRD no-callback departure. Consumer read grants, durable pending work, unavailable-read recovery and event/action-specific applicability remain required integration work. | [Foundation §4.4](../design/01-foundation.md#44-events-audit-and-the-outbox-normative), [upstream §2.7](../UPSTREAM_REQS.md#27-event-broker) |
| Pricing readiness claims are stale | The index/register distinguish four implemented predicate families, two missing-input families and missing batched fixed-version predicate/pin-composition SDK operations. Earlier dated reviews remain historical snapshots. | [Build-order index](../design/README.md#slice-map-prd--implementation-phase), [Q-15](../DECISIONS.md#open-questions) |

Validation:

- `make design-check` passed, including all 44 positive/negative cases with no skipped anchors.
- Local Markdown file targets and heading anchors passed a filesystem check; external URLs were not checked. Lychee is not installed in this environment.
- `git diff --check` passed.
- The checker explicitly reports Rating's unrecognized register format as NOT CHECKED; it does not establish coverage of that design set.

No Orders runtime, migration or deployed-policy tests were run. Proposed-value authorization,
Pricing/Subscriptions/Payments/Workflow integration, Event Broker deployment/recovery and
Product-owned PRD reconciliation remain open prerequisites. The new CI job has been verified
locally through its command, not observed in a remote workflow run. Nothing was committed or published.

Fresh semantic-review follow-up:

- Assessment uniqueness and ordering now include component plan and canonical catalog scope key;
  complete per-subject results survive persistence and replay on a single bundle order line.
- Pricing frozen bundle composition and component-conjunction integration are explicitly open
  prerequisites. Bundle assessment fails closed until complete pinned coverage is available;
  SDK publication alone is not sufficient, and component-only SKUs retain the owning exemption.
- The checker recognizes UNIQUE and PRIMARY KEY declarations. Positive and negative fixtures
  cover both, including the exact nonexistent-column regression found by review.

The updated `make design-check` passed all 44 fixtures. All 957 local links/heading anchors
across the current 25 checked documents passed, as did `git diff --check`. These checks do not
implement or verify Pricing's missing bundle capabilities.
