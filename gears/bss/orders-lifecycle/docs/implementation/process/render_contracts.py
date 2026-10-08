#!/usr/bin/env python3
"""Render the local S5-01 delivery and semantic matrix; --check is read-only."""
import json,sys
from pathlib import Path
HERE=Path(__file__).resolve().parent
x=json.loads((HERE/'contracts.json').read_text())
def esc(s):return s.replace('|',' / ').replace('\n',' ')
lines=['# S5-01 — Receiver contract reconciliation','',
'Local normative contracts and fixtures are reconciled under [D-201](../DECISIONS.md#d-201--executable-receiver-contracts-and-forced-request-integrity-s5-01). Provider SDK registration, migrations and runtime integration remain in their implementation packages. No business operation is enabled, deployment observed or owner sign-off claimed. Local files only; no commit/PR.','',
'## Changes and implementation owners','',
'| Contract correction | Selected behavior | Runtime handoff |','|---|---|---|',
'| Expired draft rebuild | [Internal engine continuation](../DESIGN.md#contract-06-replace-fulfillment-grant): current committed source, fully closed/settled predecessor, fixed commercial/active roster, new draft keys, checked aggregate generation and atomic grant/audit/response. No spawn reset. | S2-02 schema; S5-02 guarded writer/SDK; S5-08 Workflow |',
'| Forced approval freshness | Exact committed audit sequence/state/version observed under lock on the refused request. Existing two-user/time/state guards still apply. Refusal chain sequence stays NULL. | S2-02 evidence column/checks; S2-06 audit writer; S5-15 full guard integration |',
'| Audit integrity | New writers use v3 to cover the closed observation object. All 27 previous vectors remained byte-identical; nine v3 vectors added. S2-06 later corrected the three resolved-refusal inputs to `to_state = from_state` (Foundation §3.7, D-98); see [CONFORMANCE](CONFORMANCE.md). Internal rebuild adds one audit token and three registered refusal reasons, without a public state-machine trigger. | S2-02 constraints/roles; S2-06 encoder/verifier |',
'| Flat fulfillment | One line/one subscription, two-wave date/create barrier, no Catalog DAG/bundle expansion. Graph failure is historical decoding only. Hold blocks pending confirmation while active service continues. | S5-05–09 |',
'| Failure taxonomy | Confirmed expiry/closure/temporary end alone maps to binding expiry. Retirement/mismatch stops dispatch for repair or compensated line failure. Outage/denial remains unresolved. | S5-02 typed errors; S5-07/09 adapters/compensation |',
'| Async clocks | [Separate quoted, held, intent and applied instants](../DESIGN.md#contract-06-activation-clocks). Applied service start comes from authoritative OSS outcome. Preserve BillingTerms/anchor and no-backdating; unsupported geometry fails closed. | S5-02/05/07 Subscriptions + Rating profile conformance |','',
'The existing [Pricing SDK](../../../pricing/pricing-sdk/src/acceptance.rs) explicitly makes activation_at business time and freezes it in the first hold. D-201 preserves that identity; it does not claim Pricing accepts a later timestamp or that a receiver/Rating profile already exists. Ledger dual control and Products unknown-participant recovery remain the existing BSS precedents cited in [force-failure design](../features/07-hold-and-expiry.md#contract-07-3-6).','',
'## Semantic operation matrix','',
'All operations use the manifest\'s tenant/correlation context. Schema revision 2 is a local semantic contract, not a registered GTS or shipped owner API. Exact field meanings and result alternatives are in [contracts.json](process/contracts.json). Owner implementations must bind these fields to concrete wire types, error codes, principal grants and real conformance tests before publication.','',
'| Semantic operation / owner / package | Request → result fields | Version / authority | Failure and retry |','|---|---|---|---|']
for o in x['operations']:
 lines.append(f'| `{o["id"]}`; {o["owner"]}; {o["package"]} | {esc(", ".join(o["request"]))} → {esc(", ".join(o["result"]))} | {esc(o["schema_version"])}. {esc(o["authority"])} | {esc(o["failures"])}. Retry: {esc(o["retry"])} |')
lines+=['','## Historical alias register','','IDs retain their source-document namespace. In particular Subscriptions SUB-O6 differs from Workflow SUB-O6; historical SUB-O13 names both status read and create settlement, which remain distinct operations. Unverified labels are provenance only.','','| Semantic obligation | Subscriptions register | Workflow register | Historical alias |','|---|---|---|---|']
for a in x['aliases']:lines.append('| '+' | '.join(esc(str(a.get(k) or '—')) for k in ['semantic','subscriptions','workflow','historical'])+' |')
lines+=['','## Validation and next assignment','',
'The process corpus exercises sparse versions, successor guards, same-timestamp audit changes, historical graph decoding, exact applied outcome identity and separate clocks. The independent Python authoring oracle and Rust encoder validate v3 bytes; PostgreSQL checks stored preimages with its SHA-256 function. This is local contract/harness evidence, not production receiver concurrency or authenticated owner SDK conformance. See [verification record](RECEIVER_VALIDATION.md).','',
'Next: **S2-02 — full schema inventory and secure repositories**, using the updated audit observation/v3 constraints, grant audit FK and internal-writer contract. S2-01 already exists. S2-02 alone owns all Orders DDL/role migrations; S5-02 supplies later typed contributions and repositories. Business handlers stay unavailable until their engine, policy and real provider prerequisites are delivered.','']
rendered='\n'.join(lines)
p=HERE.parent/'RECEIVER_CONTRACTS.md'
if '--check' in sys.argv:
 assert p.read_text()==rendered,'receiver matrix drift'
 print(f'PASS: {len(x["operations"])} semantic operations / {len(x["aliases"])} historical aliases rendered')
else:p.write_text(rendered)
