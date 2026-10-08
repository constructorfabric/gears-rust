#!/usr/bin/env python3
"""Independent frozen vectors: Python stdlib canonical projections, no Rust codec calls.

Review fixture changes; normal verification uses --check and never regenerates expected data.
Pricing public digest.rs defines the projection. Meter/state omissions are intentional upstream
semantics, not omissions from the Orders receipt wire representation.
"""
import copy
import hashlib
import json
import sys
from pathlib import Path
from uuid import UUID

ROOT = Path(__file__).resolve().parents[3]
DEST = ROOT / 'orders-lifecycle-sdk/tests/fixtures/commercial-pin-v2.json'
def uid(n): return str(UUID(int=n))
def digest(domain, payload):
    # All fixture object keys are ASCII; sort_keys equals RFC8785 UTF-16 key ordering here.
    raw=json.dumps({'domain':domain,'payload':payload},sort_keys=True,ensure_ascii=False,separators=(',',':')).encode()
    return hashlib.sha256(raw).hexdigest()
def number(s):
    if s is None: return None
    return s.rstrip('0').rstrip('.') if '.' in s else s

def model(m):
    result=copy.deepcopy(m)
    for key in ['amount','unit_amount','package_size','package_price']:
        if key in result: result[key]=number(result[key])
    for tier in result.get('tiers',[]):
        tier['up_to']=number(tier['up_to']);tier['rate']=number(tier['rate'])
    return result

def policy(p):
    result=copy.deepcopy(p)
    result['fold']='SUM'
    if result['rating_window']['kind']=='billing_cycle':result['rating_window']='billing_cycle'
    else:result['rating_window']['timezone']='UTC'
    return result

def binding(b):
    b=copy.deepcopy(b)
    del b['meter'];del b['price']['state']
    b['price']['model']=model(b['price']['model'])
    b['price']['minimum_fee']=number(b['price']['minimum_fee'])
    b['invoice']['currency_scale']=str(b['invoice']['currency_scale'])
    if b['usage_rating_policy']:b['usage_rating_policy']['content']=policy(b['usage_rating_policy']['content'])
    return b

def terms(t):
    t=copy.deepcopy(t);del t['digest']
    t['schema_version']=str(t['schema_version']);t['timezone']='UTC'
    if t['source']['kind']=='explicit_order':t['source']='explicit_order'
    return t

bs=[]
for n,kind,dimension,m in [
    (1,'recurring',None,{'kind':'flat','amount':'1.2300'}),
    (2,'one_time','default',{'kind':'per_unit','unit_amount':'2.500'}),
    (3,'usage','eu',{'kind':'volume','tiers':[{'up_to':'100.00','rate':'0.0470'},{'up_to':None,'rate':'0.0250'}]})]:
    invoice={'template':'Usage {sku} — café\n','template_source':'sku_version','gl_code':'REVENUE','tax_category':'cloud', 'timing':'arrears' if kind=='usage' else 'advance','currency_scale':2,'rounding':'half_even'}
    invoice['template_digest']=digest('pricing.template.v1',invoice['template'])
    price={'price_id':uid(100+n),'price_book_entry_id':uid(200+n),'currency':'EUR','model':m,'minimum_fee':None,'effective_from':'2026-09-01','ends_on':None,'state':'approved'}
    price['money_digest']=digest('pricing.money.v1',{k:(model(price[k]) if k=='model' else number(price[k]) if k=='minimum_fee' else price[k]) for k in ['currency','model','minimum_fee']})
    p=None
    if kind=='usage':
        content={'rating_window':{'kind':'calendar_hour','timezone':'utc'},'aggregation_scope':'subscription_line','reset':'rating_window_start','partial_window':'actual_quantity_full_thresholds','fold':'sum'}
        p={'policy_id':uid(400),'version':'18446744073709551615','digest':digest('pricing.policy.v1',policy(content)),'content':content}
    bs.append({'item_id':uid(n),'price_book_entry_id':price['price_book_entry_id'],'dimension_key':None if dimension is None else 'region' if dimension=='eu' else 'tier','dimension_value':dimension,
        'sku_id':uid(300+n),'sku_version':'9223372036854775807','sku_code':f'SKU-{n}','sku_name':f'Frozen SKU {n}','unit':None if kind=='recurring' else 'VM·hour','meter':{'usage_type_id':'vm-hours','version':'v1'} if kind=='usage' else None,
        'price':price,'kind':kind,'recurring_period':'month' if kind=='recurring' else None,'via_default':dimension=='default','usage_rating_policy':p,'invoice':invoice})
t={'schema_version':1,'cycle':'month','anchor':'calendar','anchor_at':'2026-10-01T00:00:00.000000000Z','timezone':'utc','source':{'kind':'explicit_order'},'digest':''}
t['digest']=digest('bss.billing-terms.v1',terms(t))
q={'tenant_axes':{'seller_tenant_id':uid(8),'payer_tenant_id':uid(9),'resource_tenant_id':uid(10)},'order_id':uid(11),'order_version':'2','line_id':uid(12),'plan_id':uid(6),'plan_revision_id':uid(7),
   'selections':[{'item_id':b['item_id'],'dimension_value':b['dimension_value']} for b in bs], 'quantity':'123456789.1234567890123456789','market':{'currency':'EUR','region':'eu'},'start_at':'2026-10-01T10:30:00.000000001Z','term':{'kind':'fixed_periods','count':18},'billing_terms':t,'hold_policy_version':'18446744073709551615'}
q['resolved_bindings_digest']=digest('pricing.bindings.v1',{'plan_id':q['plan_id'],'revision_id':q['plan_revision_id'],'bindings':[{'selection':s,'binding':binding(b)} for s,b in zip(q['selections'],bs,strict=True)]})
canonical=copy.deepcopy(q)
canonical['term']['count']=str(canonical['term']['count'])
canonical['billing_terms']=terms(t)|{'digest':t['digest']}
r={'acceptance_id':uid(20),'request_digest':digest('pricing.request.v1',canonical),'terms_digest':digest('pricing.terms.v1',{'query':canonical,'bindings':[binding(b) for b in bs]}),'query':q,'accepted_at':'2026-10-01T10:29:00.123456789Z','hold_until':'2026-10-02T10:29:00.123456789Z','bindings':bs}
pin={'schema_version':2,'assessment_id':uid(30),'assessed_at':'2026-10-01T10:28:00.987654321Z','resolve_date':'2026-10-01','accepted_version_ref':{'order_id':q['order_id'],'order_version':2,'line_id':q['line_id']},'receipt':r,'activation_deadline':r['hold_until']}
rendered=json.dumps(pin,ensure_ascii=False,indent=2)+'\n'
if '--check' in sys.argv:
    if DEST.read_text()!=rendered:raise SystemExit('commercial golden drift: review independent vectors')
    print('Independent schema-2 golden and seven Pricing digest domains verified')
else:DEST.write_text(rendered)
