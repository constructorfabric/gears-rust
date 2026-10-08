#!/usr/bin/env python3
"""S1-04 executable proposal oracle, not an Orders/Pricing policy or monetary evaluator."""
import copy
import json
from pathlib import Path

HERE=Path(__file__).resolve().parent
manifest=json.loads((HERE/'profiles.json').read_text())
assert {op['id'] for op in manifest['operations']}=={'pricing-assessment','acceptance-policy','billing-terms','rating-purchase','indicative-tax'}
assert all(op['status']=='proposed' and op['kind']=='SafeRead' and op['open'] for op in manifest['operations'])
# Fixture-only registry names. Real producer profile IDs/registries remain named upstream work.
EXPECTED={
    ('line-a','item-a',True,None,'fixture-required'):'required',
    ('line-a','item-a',True,'default','fixture-required'):'required',
    ('line-a',None,False,None,'fixture-term'):'tcv-only',
    ('line-a',None,False,None,'fixture-policy'):'preview-forecast',
}
IDENTITY={'schema':1,'profile':'fixture-only-v1','assessment':'assessment-a','input_digest':'a'*64,'revision':'revision-a'}
def result_key(row):return tuple(row[k] for k in ['line','item','selected','dimension','predicate'])
def diagnostic(row_id,applicability):
    return dict(zip(['line','item','selected','dimension','predicate'],row_id,strict=True))|{'applicability':applicability,'verdict':'passed','reason':None,'observation':'observation-a','detail':{}}
BASE=IDENTITY|{'results':[diagnostic(k,a) for k,a in EXPECTED.items()]}

def assess(response,operation='preview',missing=()):
    if any(response.get(k)!=v for k,v in IDENTITY.items()):return 'unavailable'
    seen=set();verdicts=[];withheld=set()
    for row in response['results']:
        key=result_key(row)
        if key in seen or key not in EXPECTED or row['applicability']!=EXPECTED[key]:return 'unavailable'
        seen.add(key)
        verdict=row['verdict'];reason=row['reason']
        if verdict not in {'passed','failed','unevaluable'}:return 'unavailable'
        if verdict=='passed' and (reason is not None or row['observation'] is None):return 'unavailable'
        if verdict!='passed' and reason not in {'observed-failure','dependency-unavailable','missing-term-cycle','missing-forecast-policy','policy-invalid'}:return 'unavailable'
        if verdict=='failed' and row['observation'] is None:return 'unavailable'
        if row['detail']:return 'unavailable'  # this fixture profile permits no arbitrary detail
        optional=(operation=='preview' and verdict=='unevaluable' and (
            EXPECTED[key]=='tcv-only' and reason=='missing-term-cycle' and set(missing)&{'term','cycle'} or
            EXPECTED[key]=='preview-forecast' and reason=='missing-forecast-policy' and 'forecast-policy' in missing))
        if optional:withheld.add(EXPECTED[key])
        else:verdicts.append(verdict)
    if seen!=set(EXPECTED):return 'unavailable'
    if 'unevaluable' in verdicts:return 'unavailable'
    if 'failed' in verdicts:return 'refused'
    return 'tcv-withheld' if 'tcv-only' in withheld else 'forecast-withheld' if withheld else 'complete'

cases=0
def check(response,expected,**kwargs):
    global cases
    assert assess(response,**kwargs)==expected,(expected,response)
    cases+=1
check(BASE,'complete')
for field in IDENTITY:
    r=copy.deepcopy(BASE);r[field]='unknown';check(r,'unavailable')
for results in [BASE['results'][:-1],BASE['results']+[BASE['results'][0]]]:
    check(BASE|{'results':results},'unavailable')
r=copy.deepcopy(BASE);r['results'][0]['dimension']='default';check(r,'unavailable')
r=copy.deepcopy(BASE);r['results'][0]['observation']=None;check(r,'unavailable')
r=copy.deepcopy(BASE);r['results'][0]['detail']={'raw_error':'secret'};check(r,'unavailable')
r=copy.deepcopy(BASE);r['results'][0]|={'verdict':'failed','reason':'observed-failure'};check(r,'refused')
r['results'][1]|={'verdict':'unevaluable','reason':'dependency-unavailable','observation':None};check(r,'unavailable')
r=copy.deepcopy(BASE);r['results'][2]|={'verdict':'unevaluable','reason':'missing-term-cycle','observation':None}
check(r,'tcv-withheld',missing=('term',));check(r,'unavailable');check(r,'unavailable',operation='submit',missing=('term',));check(r,'unavailable',operation='amendment',missing=('cycle',))
r['results'][0]|={'verdict':'unevaluable','reason':'missing-term-cycle','observation':None};check(r,'unavailable',missing=('term',))
r=copy.deepcopy(BASE);r['results'][3]|={'verdict':'unevaluable','reason':'missing-forecast-policy','observation':None}
check(r,'forecast-withheld',missing=('forecast-policy',));check(r,'unavailable',operation='submit',missing=('forecast-policy',))
r['results'][3]|={'verdict':'failed','reason':'policy-invalid','observation':'observation-a'};check(r,'refused',missing=('forecast-policy',))

# Rating shape validation: no arithmetic, no fabricated money, independently expected coverage.
MONEY={('item','i'),('line','l'),('order',None)}
def rating(rows,missing_term=False,operation='preview',tax_available=True):
    expected={(scope,identity,kind) for scope,identity in MONEY for kind in ['recurring','usage','one_time']}
    seen=set()
    for row in rows:
        key=(row['scope'],row['id'],row['kind'])
        if key not in expected or key in seen:return False
        seen.add(key)
        if row['currency']!='EUR' or row['scale']!=2 or row['rounding']!='half_even':return False
        if row['kind']=='usage':
            if row['status']!='uncommitted' or any(row[k] is not None for k in ['gross','net','discount']):return False
        elif row['status']!='committed' or any(not isinstance(row[k],int) or isinstance(row[k],bool) or abs(row[k])>2**63-1 for k in ['gross','net','discount']):return False
        if row['kind']=='recurring' and (not row['amount_basis'] or not row['period_evidence']):return False
        if key==('order',None,'recurring'):
            if missing_term:
                if operation!='preview' or row['tcv_status']!='withheld' or row['tcv'] is not None:return False
            elif row['tcv_status']!='available' or not isinstance(row['tcv'],int) or row['tcv_basis'] not in ['finite_term','rolling_annualized','mixed']:return False
        elif any(row[k] is not None for k in ['tcv','tcv_status','tcv_basis']):return False
    return seen==expected and (operation!='preview' or tax_available)
rows=[{'scope':scope,'id':identity,'kind':kind,'status':'uncommitted' if kind=='usage' else 'committed','gross':None if kind=='usage' else 0,'net':None if kind=='usage' else 0,'discount':None if kind=='usage' else 0,'currency':'EUR','scale':2,'rounding':'half_even','amount_basis':'finite_term','period_evidence':['declared horizon'],'tcv':0 if (scope,kind)==('order','recurring') else None,'tcv_status':'available' if (scope,kind)==('order','recurring') else None,'tcv_basis':'finite_term' if (scope,kind)==('order','recurring') else None} for scope,identity in sorted(MONEY) for kind in ['recurring','usage','one_time']]
assert rating(rows)
for mutation in ['omit','duplicate','usage-zero','discount-missing','basis-missing','extra-tcv','currency','overflow']:
    r=copy.deepcopy(rows)
    if mutation=='omit':r.pop()
    elif mutation=='duplicate':r.append(r[0])
    elif mutation=='usage-zero':next(x for x in r if x['kind']=='usage')['net']=0
    elif mutation=='discount-missing':r[0]['discount']=None
    elif mutation=='basis-missing':r[0]['amount_basis']=None
    elif mutation=='extra-tcv':r[0]['tcv']=0
    elif mutation=='currency':r[0]['currency']='USD'
    elif mutation=='overflow':r[0]['net']=2**63
    assert not rating(r),mutation
    cases+=1
for basis in ['finite_term','rolling_annualized','mixed']:
    r=copy.deepcopy(rows);next(x for x in r if x['scope']=='order' and x['kind']=='recurring')['tcv_basis']=basis
    assert rating(r);cases+=1
r=copy.deepcopy(rows);next(x for x in r if x['scope']=='order' and x['kind']=='recurring').update(tcv=None,tcv_status='withheld',tcv_basis=None)
assert rating(r,missing_term=True);assert not rating(r,missing_term=True,operation='submit');assert not rating(r,missing_term=True,tax_available=False)
cases+=4
print(f'Verified five proposed owner contracts and {cases} diagnostic/Rating fixture cases; no real-provider conformance claimed')
graph=json.loads((HERE/'call-graph.json').read_text())
nodes={n['id']:n for n in graph['nodes']}
assert len(nodes)==len(graph['nodes'])
visited=set();pending=set()
def visit(name):
    assert name in nodes and name not in pending,('missing/cyclic dependency',name)
    if name in visited:return
    pending.add(name)
    for predecessor in nodes[name]['after']:visit(predecessor)
    pending.remove(name);visited.add(name)
for name in nodes:visit(name)
assert nodes['terms']['after']==['resolve-bindings','contract']
assert nodes['accept-lines']['after']==['reserve-attempt']
assert nodes['assessment']['budget_ms'] is None
assert graph['existing_resolution_ceilings_ms']=={'submit':2250,'preview':2500}
print(f'Verified {len(nodes)} call-graph nodes, acyclic dependencies and explicit unratified budgets')
