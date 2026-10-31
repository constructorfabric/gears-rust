# Orders / Subscriptions proposed contract (D-242)

Status: Orders semantic adoption of Subscriptions PR 5516 at commit
`56f7c6ca17e21be8c27db36bc902298b0d2c7886`, reviewed 2026-10-27.
This is an Orders proposal for producer/consumer implementation and ratification. It does not
assert bilateral agreement, delivered SDK methods, runtime providers or production readiness.
Orders-local collection and protected-observation validation are implemented in `a81090675`
and `cd82261cf`, with malformed-observation handling corrected in `39648a630`. These commits deliver the proposed local profiles below; they do not deliver
a Subscriptions reservation provider or financial capture/cancellation protocol.

## Sources and precedence

Pinned owner sources are [consumer contract §3.7](https://github.com/constructorfabric/gears-rust/blob/56f7c6ca17e21be8c27db36bc902298b0d2c7886/gears/bss/subscriptions/docs/features/09-consumer-contracts.md#37-consume-product-identity-and-occupancy-policy),
[occupancy design](https://github.com/constructorfabric/gears-rust/blob/56f7c6ca17e21be8c27db36bc902298b0d2c7886/gears/bss/subscriptions/docs/DESIGN.md#accepted-acquisition-evidence),
[acquisition recovery](https://github.com/constructorfabric/gears-rust/blob/56f7c6ca17e21be8c27db36bc902298b0d2c7886/gears/bss/subscriptions/docs/features/01-foundation.md#acquisition-recovery),
and [S-U01/S-U07/S-U10 owner requirements](https://github.com/constructorfabric/gears-rust/blob/56f7c6ca17e21be8c27db36bc902298b0d2c7886/gears/bss/subscriptions/docs/UPSTREAM_REQS.md).
Their accepted S-Q02 semantic direction is adopted here; exact interfaces remain open.
For the protected target, this amendment supersedes D-180 active-only/release-on-leaving-active
wording and D-198's reservation-start-at-activation-intent minimum, two-count observation and
proposed linked predecessor exemption. Confirmation reserves before capture; linked replacement
is deferred. Historical decisions and verification results remain evidence of their original scope.

## Ownership and protected acquisition

| Owner | Required responsibility |
| --- | --- |
| Products | Versioned SKU identity and agreed overlap classification; explicit source evidence |
| Pricing | Accepted plan/revision/item selections and native bindings, frozen commercial evidence |
| Subscriptions | Authoritative occupancy/policy evidence and atomic multi-key reservation, conversion, movement and applied release; exact order/version/line association |
| Orders Lifecycle | Immutable order/version/line evidence, local in-flight exclusion and durable Orders-origin capture/cancellation arbitration |
| Workflow | Carry exact acquisition/attempt identities, recover unknown outcomes, preserve fences and deliver acknowledgements |
| Billing / Payments / financial admission owners | Authoritative capture recovery/suppression, accepted future commercial commitment and financial-remedy/release ordering |

The full capacity tuple is `(payerTenantId, resourceTenantId, overlapScopeKey)` including any
owner-declared canonical dimensions. Each observation names explicit limit, source/reference,
key and policy versions. Missing evidence is unknown: never default to one or infer unlimited.
Active subscriptions, retained suspended places and explicit confirmed-order reservations are
three literal counts sharing capacity; one logical right occurs in exactly one category.
Active 0 plus suspended 1 or reserved 1 at limit 1 blocks a new right.

Confirmation durably reserves every required key before capture. A plain draft, availability read,
Pricing receipt/hold or Orders in-flight claim creates no Subscriptions reservation or permission
to debit. Future bookings count from confirmation, without date-sliced pools. Activation consumes
its own reservation atomically; unchanged-key resume converts the retained right, without another
slot. A lower limit preserves all existing rights, even above the new limit, while blocking excess
new demand. Other activation, payment, term, market, Policy and readiness guards still apply.

Paid reservations survive future waits, holds, ordinary TTL and technical fulfillment failure.
Unknown capture retains protection and recovers the original durable financial outcome; it is
neither definitive failure nor permission to retry collection under a new identity. Cancellation
must fence late capture and activation, record the required recoverable remedy and obtain the
agreed release acknowledgements. Cancellation receipt, pending/failed cancellation and draft void
alone cannot release a protected right. Active/suspended cancellation releases only at application.
A confirmed unpaid prepay reservation follows its accepted versioned payment deadline and cancel
route; unpaid draft cleanup cannot steal paid or uncertain rights. No automatic-refund amount,
timing or universal wait-for-posted-refund rule is invented. Resource retention after applied
cancellation does not itself retain ordinary capacity. Unsupported key/policy movement preserves
the source claim; supported movement needs explicit destination/version mapping and atomic move.

## Proposed Orders item-key collection

The proposed `item_keys_v1` record retains complete line/revision, requested selection, accepted
SKU UUID and i64 version, independent product/scope strings and classification provenance
`{source, reference, version}`. Keys and each provenance component are nonempty, NUL-free and
bounded to 1024 UTF-8 bytes. Match requested dimension identity even when the resolved binding
is a fallback/default. Bound collection counts by requested selections/lines and include the
aggregate payload in persisted-size accounting.

Request every distinct selected paid recurring binding, retaining line, plan revision,
Pricing selection `(item_id, dimension_value)`, SKU identity/version and supplied provenance.
Different items sharing a SKU remain distinct. Repeated identical selections must not silently
multiply rights: reject duplicate selection identities; one item with distinct selected dimensions
requires distinct mapping coverage. Unselected, usage-only and one-time bindings add no key.
The implemented local request/response spelling is recorded below; owner ratification remains open.

A proposed response echoes exact requested selection context with independent catalog product
and overlap-scope identities. Require exact coverage: reject omitted, extra, duplicate (including
identical), mismatched or conflicting answers before occupancy reads. Never assign answers by
array position or choose the first recurring item. Preserve every item-to-product/scope relation;
dedupe scopes only for queries and transactional claims. Product/scope cardinality is an open
owner decision, not an inferred bijection.

The proposed local unit `line_scope_v1` preserves current distinct-line counting per scope: two selected items in one line sharing a scope
contribute one proposed unit; two lines contribute two. This is the local development convention,
not an agreed subscription-right unit. Positive quantity does not multiply local demand under `line_scope_v1`; non-unit quantities
remain supported locally. This cannot prove provider quantity/subscription-right semantics.
Reject incompatible or ambiguous capacity-unit mappings; lots or multiple rights per item need
an explicit owner contract, not an inferred mapping. All unique scopes enter the existing all-or-nothing Orders
claim transaction; a conflict maps to every affected line without partial version/pin/event writes.

The runtime target freezes the complete canonical collection before acceptance and preserves it
through recovery, committed storage and recheck. Legacy scalar evidence remains legacy; absent
item/product/provenance is never reconstructed from today's catalog. Multi-item scalar evidence
cannot prove full coverage and requires fail-closed recovery or an explicit agreed migration.
Frozen/persisted forms must be explicitly legacy scalar or `item_keys_v1`; reject mixed forms.
New collection rows keep scalar columns NULL; NULL collection preserves legacy scalars, including
absent product. Legacy commit requires proven zero recurring selections with no key or exactly one
with a key. Multiple recurring selections fail through the existing Integration recovery outcome
without mutating frozen evidence or inferring release. New collections require exact verified
receipt coverage including SKU version; `[]` is keyless only with that proof. No implicit backfill.
Legacy migration and linked replacement are outside this proposal.

## Proposed versioned protected observation

Introduce the proposed `protected_full_tuple_v1` profile, distinct from the legacy active-only
development profile. These are semantic fields, not an agreed wire schema: exact query tuple; all three
nonnegative counts; explicit finite/unlimited limit; source/reference and key/policy versions;
observation identity/revision and completeness; stable provider claim identity/state with exact
order/version/line or subscription binding. Counts, policy and own bindings form one coherent observation with a nonempty revision.
Require complete scoped own-membership for the exact requested order/version/line set and tuple,
with explicit `complete=true`; do not expose or accept arbitrary foreign-order identity lists.
Explicit complete empty evidence differs from omitted/partial evidence. Validate against trusted
stored identities; new submit and Preview have no reusable context and require no own membership
assertions, never a guessed candidate/version exemption. Same order ID alone is no exemption.
Claim IDs must be unique across the observation, line/scope bindings unique and represented
category counts no greater than global category counts; permit at most one right per line/scope.
Every own right binds to the observation revision and policy/key evidence.

Protected demand is provider occupancy plus only proven unrepresented requested rights, using
checked arithmetic and refusing overflow. Only an exact reserved right qualifies for the initial
Orders activation exemption; active/retained-suspended evidence defers without a separately agreed
idempotent activation/resume mapping. Zero new demand permits protected reservation conversion
above a lower limit; positive new demand must satisfy total plus new demand <= current limit.
Unlimited policy still requires every identity, scope and completeness validation. Unknown or
malformed protected profiles never fall back to legacy. Wrong tuple/profile/version, duplicate claim,
conflicting state, absent association, inconsistent totals or ambiguous successor mapping makes
the answer unevaluable; legacy data cannot be upgraded by filling missing counts with zero.

The initial proposed protected profile accepts full-tuple evidence only: do not apply the legacy
per-payer cross-resource addend. Predicate 9 and the engine's same-tuple in-flight exclusion remain
separate. Generalized reconciliation needs exact represented identities and a coherent handover
fence/snapshot; aggregate subtraction of reserved count is unsafe. Any future per-payer protected
profile needs independent agreement and proof before use. Recheck remains advisory: only the
Subscriptions transaction can reserve/consume capacity. Public diagnostics expose no foreign
order/claim identities.

## Delivery gates and required joint conformance

Orders now preserves every selected recurring item's complete key evidence through assessment,
frozen recovery, committed JSONB storage, all-key Orders claims and multi-scope recheck. Forward
migration 11 adds nullable `order_line.overlap_item_keys`; complete collection rows keep scalar
columns NULL, while legacy rows keep collection NULL. JSONB membership counts a line once per
scope. The current implementation does not reinterpret or backfill legacy evidence.

The strict protected profile validates the entire batch and trusted tuple before counting literal
active, retained-suspended and reserved occupancy. Exact reserved membership allows zero-new-demand
conversion above a reduced limit; a new line still needs capacity. Active-only development evidence
remains separately decoded. Production occupancy/key adapters still return Missing, and local
in-flight claims remain distinct from protected commercial reservations. Existing provider checkboxes
in [UPSTREAM_REQS](UPSTREAM_REQS.md) remain unchecked.

### Implemented local DTO spelling (proposed owner contract)

These are Orders-owned types under `orders/src/domain/gate/`, not a delivered Subscriptions SDK.

| Type / serialized member | Concrete local representation |
| --- | --- |
| `answers::OverlapKeyLine` | `line_id`, `plan_revision_id`, `item_id`, `dimension_value: Option<String>`, `paid_recurring_sku` UUIDs and `sku_version: i64`; dimension preserves the requested selection |
| `item_keys::ItemOverlapKey` | `profile: "item_keys_v1"`, `selection: OverlapKeyLine`, `catalog_subscription_product_key`, `overlap_scope_key`, `classification: ClassificationProvenance { source, reference, version }` |
| `answers::OverlapKey` | Scalar line/product/scope wrapper with optional `item_key`; absent echo is explicitly legacy and is never manufactured |
| Frozen collection | Exactly `{key, query, request_digest, item_keys_v1: [...]}`; `commercial::{LineRequest,FrozenLine}.overlap_item_keys` is `Option<Vec<ItemOverlapKey>>` internally |
| `protected::OccupancyObservation` | `LegacyActiveV1(Occupancy)` or `ProtectedFullTupleV1(Box<ProtectedOccupancy>)`; custom decoding selects by explicit `profile` and never falls back on malformed protected evidence |
| `ProtectedOccupancy` | Required `profile: "protected_full_tuple_v1"`, `demand_unit: "line_scope_v1"`, `observation_revision`, `scope`, `policy`, literal `active_count`, `retained_suspended_count`, `order_reserved_count: u32`, `max_concurrent_active` and `own_membership` |
| `ProtectedScope` / `OccupancyPolicyEvidence` | Exact payer/resource UUIDs and scope key; policy `{source: LimitProvenance, reference, version}`. All rights inherit the row's scope/policy/revision |
| `OwnMembership` / `OwnContext` | `{complete, context, rights}`; context has exact `order_id`, nonzero `version: u32`, payer/resource UUIDs and distinct `line_ids` |
| `OwnRight` | `{claim_id, line_id, state}`; state literals `active`, `retained_suspended`, `order_reserved`, with only `order_reserved` accepted for initial activation conversion |
| `answers::OccupancyQuery` | Payer/resource/key plus optional `own_context`; Submit/Preview supply none. Protected recheck builds exact context from trusted stored order/version/resource/payer and every relevant line |

The protected `max_concurrent_active` member is required: number is finite and explicit `null`
is the proposed Unlimited encoding. `own_membership` is also required: explicit `null` exactly
for no-context Submit/Preview, or a complete matching object for recheck. Omission differs from
null. Unknown struct members/discriminators refuse; the legacy four-member shape also rejects
unknown fields. The exact representation binds key evidence through full tuple/policy/revision;
an additional independently versioned key-policy mapping remains an owner ratification gate.

### Current local verification checkpoint

At source `39648a630`, the fresh verification run completed 33 commands successfully: 34
summaries recorded 583 tests passed, zero failures and 224 ignored PostgreSQL tests. The expanded
HTTP run on the preceding source recorded 46 passes and two failures: malformed protected evidence
returned 500 instead of canonical 503 unavailable. `39648a630` corrects that unavailable-input
mapping; the fix passed independent review. Final verification on the corrected source passed all 224
PostgreSQL tests (zero failed/ignored, 112.00 seconds), rebuilt the HTTP host successfully and
passed all 48 HTTP-suite cases (43 HTTP scenarios and five sidecar helpers, 85.34 seconds),
including both former failures. Historical Task 2's 43-case HTTP pass remains an earlier checkpoint. These are Orders development results, not provider conformance
or bilateral agreement.

- [ ] Ratify operation/schema/profile versions, Products/Subscriptions mapping ownership, SKU/time
  basis, product/scope relationships, repeated selections and quantity/right units.
- [ ] Deliver exact binding, complete claim/exclusion observation, policy serialization and
  multi-key reserve/consume/release with immutable association and handover recovery.
- [ ] Agree future commercial commitment, capture/cancel arbitration, financial recovery/remedy
  and release acknowledgements; provision finite owner grants and Workflow conformance.
- [ ] Prove active 0/suspended 1/limit 1 and reserved 1/limit 1 refuse new demand; limit N > 1
  concurrent final-slot races have one winner without overbooking.
- [ ] Prove own conversion at capacity and above a lower limit, retained suspend/resume, unrelated
  claim non-exemption, protected-plus-new demand and wrong order/version/line/resource/key refusal.
- [ ] Prove all selected item coverage, same-SKU items, distinct dimensions, shared/multiple scopes,
  repeated selection refusal, fallback-dimension and SKU-version checks, malformed/duplicate
  answers, non-unit quantity under `line_scope_v1` and incompatible capacity-unit refusal.
- [ ] Prove partial multi-key failure rolls back all effects; exact retry, lost response, cancellation
  versus late capture/activation, unknown payment, future start, TTL and technical failure preserve
  the same protected right until authoritative conversion or coordinated release.

Legacy recheck fixture v1 remains legacy; proposed protected vectors use v2 without implying
Workflow adoption. The checklist remains joint producer/consumer acceptance work. Local cases already covered by
Orders tests do not establish owner runtime conformance. S-U01/S-U07/S-U10,
Subscriptions/Workflow implementation, exact SDK mapping and bilateral ratification remain release
gates. Orders local tests cannot enable prepayment or establish owner agreement.
