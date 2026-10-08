#!/usr/bin/env python3
"""Extend S1-04 independent digest oracle with finite/rolling and tenant arrangements."""
import copy
import json
import runpy
import sys
from pathlib import Path
from uuid import UUID

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
DEST=ROOT/'orders-lifecycle-sdk/tests/fixtures/commercial-conformance.json'
args=sys.argv[:]
sys.argv=[str(HERE.parent/'commercial/build_vectors.py'),'--check']
oracle=runpy.run_path(sys.argv[0])
sys.argv=args
pins=[]
for index,(id,cycle,term,payer) in enumerate([
 ('self-service-finite-month','month',{'kind':'fixed_periods','count':18},10),
 ('partner-third-party-finite-year','year',{'kind':'fixed_periods','count':2},9),
 ('rolling-mixed-charge-kinds','month',{'kind':'rolling'},10),
 ('single-recurring-basket-seed','month',{'kind':'fixed_periods','count':12},10),
]):
 pin=copy.deepcopy(oracle['pin']);r=pin['receipt'];q=r['query'];t=q['billing_terms']
 q['tenant_axes']['payer_tenant_id']=str(UUID(int=payer));q['order_version']='7';q['line_id']=str(UUID(int=1000+index))
 pin['accepted_version_ref'].update(order_version=7,line_id=q['line_id'])
 q['term']=term;t['cycle']=cycle;t['digest']=oracle['digest']('bss.billing-terms.v1',oracle['terms'](t))
 if index==3:r['bindings']=r['bindings'][:1];q['selections']=q['selections'][:1]
 q['resolved_bindings_digest']=oracle['digest']('pricing.bindings.v1',{'plan_id':q['plan_id'],'revision_id':q['plan_revision_id'],'bindings':[{'selection':s,'binding':oracle['binding'](b)} for s,b in zip(q['selections'],r['bindings'],strict=True)]})
 canonical=copy.deepcopy(q)
 if 'count' in canonical['term']:canonical['term']['count']=str(canonical['term']['count'])
 else:canonical['term']='rolling'
 canonical['billing_terms']=oracle['terms'](t)|{'digest':t['digest']}
 r['request_digest']=oracle['digest']('pricing.request.v1',canonical)
 r['terms_digest']=oracle['digest']('pricing.terms.v1',{'query':canonical,'bindings':[oracle['binding'](b) for b in r['bindings']]})
 preimages=[]
 for name,domain,payload,expected in [
  ('billing_terms','bss.billing-terms.v1',oracle['terms'](t),t['digest']),
  ('bindings','pricing.bindings.v1',{'plan_id':q['plan_id'],'revision_id':q['plan_revision_id'],'bindings':[{'selection':sel,'binding':oracle['binding'](b)} for sel,b in zip(q['selections'],r['bindings'],strict=True)]},q['resolved_bindings_digest']),
  ('request','pricing.request.v1',canonical,r['request_digest']),
  ('terms','pricing.terms.v1',{'query':canonical,'bindings':[oracle['binding'](b) for b in r['bindings']]},r['terms_digest']),
 ]:
  raw=json.dumps({'domain':domain,'payload':payload},sort_keys=True,ensure_ascii=False,separators=(',',':')).encode()
  preimages.append(dict(id=name,preimage_hex=raw.hex(),sha256=expected))
 pins.append(dict(id=id,pin=pin,digest_preimages=preimages))
rendered=json.dumps(dict(format_version=1,pins=pins),ensure_ascii=False,indent=2)+'\n'
if '--check' in args:
 if DEST.read_text()!=rendered:raise SystemExit('commercial conformance drift: review independent oracle before updating')
 print(f'Verified {len(pins)} independent native commercial receipts')
else:DEST.write_text(rendered)
