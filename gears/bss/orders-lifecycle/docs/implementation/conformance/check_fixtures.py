#!/usr/bin/env python3
"""Read-only inventory, source, independent hash and deferred-evidence checks."""
import hashlib
import json
from pathlib import Path

HERE=Path(__file__).resolve().parent
GEAR=HERE.parents[2]
DOCS=HERE.parents[1]
FIXTURES=GEAR/'orders-lifecycle/tests/fixtures'
def load(p):return json.loads(p.read_text())
def unique(rows):assert len({r['id'] for r in rows})==len(rows)
index=load(HERE/'index.json')
assert index['production_release'] is False
unique(index['families'])
for family in index['families']:
 path,anchor=family['source'].split('#')
 assert f'id="{anchor}"' in (DOCS/path).read_text(),family['source']
 assert (GEAR/family['fixtures']).is_file(),family['fixtures']
 assert family['packages'] and family['local_evidence'] and family['remaining']
vectors=load(FIXTURES/'conformance-v1.json')['vectors'];unique(vectors)
for v in vectors:
 assert hashlib.sha256(bytes.fromhex(v['preimage_hex'])).hexdigest()==v['sha256'],v['id']
assert {'audit','genesis','checkpoint','checkpoint_genesis','request','cursor','framing'}=={v['kind'] for v in vectors}
native=load(GEAR/'orders-lifecycle-sdk/tests/fixtures/commercial-conformance.json')['pins']
for pin in native:
 assert {p['id'] for p in pin['digest_preimages']}=={'billing_terms','bindings','request','terms'}
 for p in pin['digest_preimages']:
  assert hashlib.sha256(bytes.fromhex(p['preimage_hex'])).hexdigest()==p['sha256'],(pin['id'],p['id'])
 receipt=pin['pin']['receipt'];query=receipt['query']
 expected={'billing_terms':query['billing_terms']['digest'],'bindings':query['resolved_bindings_digest'],'request':receipt['request_digest'],'terms':receipt['terms_digest']}
 assert all(expected[p['id']]==p['sha256'] for p in pin['digest_preimages'])
scenarios=load(FIXTURES/'commercial-scenarios.json')['scenarios'];unique(scenarios)
assert {'published','scheduled','superseded','closed','retired'} <= {s['catalog'] for s in scenarios}
assert {'self_service','partner_placed'}=={s['sales_path'] for s in scenarios}
assert {200,201}<={s['line_count'] for s in scenarios}
assert {'finite','rolling'}=={term for s in scenarios for term in s['terms']}
assert {'recurring','one_time','usage'}=={kind for s in scenarios for kind in s['charge_kinds']}
assert any(s['owner']=='unavailable' for s in scenarios)
for s in scenarios:
 assert s['dimension_values']==[None,'default']
 assert all(s[k] for k in ['requirements','expected','package','venue'])
 assert s['runtime_evidence']=='pending'
faults=load(FIXTURES/'fault-contracts.json')['faults'];unique(faults)
assert len(faults)==16
assert {f['id'] for f in faults}=={f'{when}_{point}' for when in ['before','after'] for point in ['remote_acceptance','local_commit','outbox_enqueue','outbox_ack','receiver_admission','receiver_confirmation','grant_revocation','payment_response']}
for f in faults:
 assert all(f[k] for k in ['owner_package','durable_facts','recovery','forbidden','venue'])
 assert f['runtime_evidence'].startswith('pending')
print(f'PASS: {len(vectors)} frozen hashes + 16 native preimages, {len(scenarios)} scenario recipes, {len(faults)} fault points, {len(index["families"])} evidence families; runtime gaps explicit')
