#!/usr/bin/env python3
"""Limited executable contract oracle, not the Orders engine or production validator."""
import copy, json, re
from pathlib import Path
HERE=Path(__file__).resolve().parent
CATALOG=json.loads((HERE/'catalog.json').read_text())
MODELS=json.loads((HERE/'models.json').read_text())

def check_registry(catalog):
    def unique(values,label):
        if len(values)!=len(set(values)):raise ValueError('duplicate '+label)
    operations=catalog['operations']; reasons={r['reason'] for r in catalog['errors']}
    unique([(o['method'],o['path']) for o in operations],'operation')
    unique([o['id'] for o in operations],'operation-id')
    unique([e['gts_registry_key'] for e in catalog['errors']],'reason')
    unique([(e['error_domain'],e['error_code']) for e in catalog['errors']],'domain-code')
    keys=catalog['expanded_transition_keys']
    unique([(k['from_state'],k['trigger']) for k in keys],'state-trigger')
    rows={r['row'] for r in catalog['source_transition_rows']}
    for op in operations:
        if not op.get('pdp_action') or not op.get('actor_eligibility'):raise ValueError('operation-without-permission')
        if op['request_model'] not in MODELS['models'] or op['result_model'] not in MODELS['models']:raise ValueError('unknown-model')
        if set(op['state_rows'])-rows:raise ValueError('unknown-row')
        if set(op['triggers'])-set(catalog['triggers']):raise ValueError('unknown-trigger')
    for g in catalog.get('guards',[]):
        if g['row'] not in rows:raise ValueError('unknown-guard-row')
        if g['reason'] not in reasons:raise ValueError('unknown-guard-reason')
    for scope,classes in MODELS['field_classes'].items():
        names=[x for fields in classes.values() for x in fields];unique(names,scope+'-field-class')
    if any(not t['fields'] for t in catalog['tables']):raise ValueError('empty-storage-schema')
    declared={r['row']:r['event'] for r in catalog['source_transition_rows']}
    for event in catalog['events']:
        if sorted(event['rows'])!=sorted(n for n,e in declared.items() if e==event['name']):raise ValueError('event-row-mismatch')
    return 'valid'

def etag(value):
    if not isinstance(value,str) or not re.fullmatch(r'"[0-9]+"',value):return 'expected-version-required'
    n=int(value[1:-1])
    return n if 1<=n<=2147483647 else 'expected-version-required'

def boundary(call):
    op=next(o for o in CATALOG['operations'] if o['id']==call['operation'])
    meta=call.get('meta',{});body=call.get('body',{})
    if op['expected_version']=='required-before-authorization':
        v=meta.get('expected_version')
        if type(v)!=int or not 1<=v<=2147483647:return 'expected-version-required'
    if op['idempotency']=='required':
        key=meta.get('idempotency_key')
        if not isinstance(key,str) or not 1<=len(key)<=255 or any(not 32<=ord(c)<=126 for c in key):return 'request-invalid'
    # Orders REST boundary carrier confirmed by D-202 (S2-03, 2026-10-06): optional
    # opaque x-delegation-proof-ref, once, 1..512 printable non-space ASCII; never validated as proof.
    proofs=call.get('headers',{}).get('x-delegation-proof-ref')
    if proofs is not None and (len(proofs)!=1 or not isinstance(proofs[0],str) or not 1<=len(proofs[0])<=512 or any(not 33<=ord(c)<=126 for c in proofs[0])):return 'request-invalid'
    revision=meta.get('expected_draft_revision')
    if 'expected_draft_revision' in meta and (type(revision)!=int or not 0<=revision<=9223372036854775807):return 'request-invalid'
    if op['expected_draft_revision']=='boundary-required-on-submit' and revision is None:return 'request-invalid'
    model=MODELS['models'][op['request_model']]
    if not isinstance(body,dict) or set(body)-set(model['required_fields']+model['optional_fields']) or set(model['required_fields'])-set(body):return 'request-invalid'
    if 'page_size' in body and (type(body['page_size'])!=int or not 1<=body['page_size']<=200):return 'page-size-exceeded'
    if op['id'] in ['patch_order','patch_line']:
        fields=body['fields'];scope='line' if op['id']=='patch_line' else 'header'
        classes=MODELS['field_classes'][scope];known={f for group in classes.values() for f in group}
        if not isinstance(fields,dict) or not fields or set(fields)-known:return 'request-invalid'
        for field,limit in [('external_reference',256),('display_label',200),('internal_notes',2000)]:
            if field in fields and fields[field] is not None and (not isinstance(fields[field],str) or len(fields[field])>limit or '\0' in fields[field]):return 'request-invalid'
        return 'draft-mutate' if set(fields)&set(classes['commercial']+classes['commercial_frozen']) else 'administrative-edit'
    if op['id']=='reflect_approval':
        if body['verdict'] not in ['required','not_required','granted','denied']:return 'request-invalid'
        if body['verdict']!='denied' and 'denial_reason' in body:return 'request-invalid'
    if op['id']=='acknowledge':
        if body['outcome'] not in ['completed','failed']:return 'request-invalid'
        if body['outcome']=='completed' and {'failure_reason','compensation_evidence'}&set(body):return 'request-invalid'
    if op['id']=='begin_fulfillment' and body['outcome'] not in ['authorized','pending','failed']:return 'request-invalid'
    # Domain/authorization/registry/terms validation is intentionally not simulated here.
    return 'boundary-pass'

def evaluate(case):
    kind=case['kind']
    if kind=='boundary':return boundary(case['call'])
    if kind=='etag':return etag(case['input'])
    if kind=='transition':
        match=[k for k in CATALOG['expanded_transition_keys'] if k['from_state']==case['from'] and k['trigger']==case['trigger']]
        if not match:return None
        k=match[0];r=next(x for x in CATALOG['source_transition_rows'] if x['row']==k['row'])
        return {'row':r['row'],'to':k['to'],'event':r['event'],'versioning':r['versioning']}
    if kind=='problem':
        r=next(x for x in CATALOG['errors'] if x['reason']==case['reason'])
        return {k:r[k] for k in ['type','title','http_status','error_domain','error_code']}
    if kind=='registry':
        c=copy.deepcopy(CATALOG);mutation=case['mutation']
        if mutation=='duplicate-operation':c['operations'].append(copy.deepcopy(c['operations'][0]))
        elif mutation=='missing-permission':c['operations'][0]['pdp_action']=None
        elif mutation=='unknown-trigger':c['operations'][0]['triggers']=['invented']
        elif mutation=='unknown-guard-row':c['guards']=[{'row':999,'reason':'request-invalid'}]
        elif mutation=='unknown-guard-reason':c['guards']=[{'row':1,'reason':'invented'}]
        elif mutation=='duplicate-reason':c['errors'].append(copy.deepcopy(c['errors'][0]))
        elif mutation=='duplicate-state-trigger':c['expanded_transition_keys'].append(copy.deepcopy(c['expanded_transition_keys'][0]))
        try:return check_registry(c)
        except ValueError as e:return str(e)
    raise ValueError('unknown fixture kind '+kind)

def main():
    check_registry(CATALOG)
    fixtures=json.loads((HERE/'boundary-fixtures.json').read_text())
    for case in fixtures['cases']:
        actual=evaluate(case)
        if actual!=case['expected']:raise SystemExit(f'{case["id"]}: expected {case["expected"]!r}, got {actual!r}')
    for example in fixtures['golden_envelopes']:
        value=example['value']
        if json.loads(json.dumps(value,ensure_ascii=False,separators=(',',':')))!=value:raise SystemExit('lossy JSON round trip '+example['id'])
        if example['kind']=='request':
            body=copy.deepcopy(value['body'])
            meta={'expected_version':etag(value['headers']['If-Match']),'idempotency_key':value['headers']['Idempotency-Key']}
            if 'expected_draft_revision' in body:meta['expected_draft_revision']=body.pop('expected_draft_revision')
            if boundary({'operation':example['operation'],'meta':meta,'body':body})!=example['expected_boundary']:raise SystemExit('golden request binding mismatch')
        elif example['kind']=='problem':
            reason=next(r for r in CATALOG['errors'] if r['error_code']==value['error_code'])
            for k in ['type','title','error_domain','error_code']:
                if value[k]!=reason[k]:raise SystemExit('golden Problem mismatch '+example['id'])
            if value['status']!=reason['http_status']:raise SystemExit('golden Problem status mismatch')
        elif example['kind']=='result':
            model=MODELS['models'][example['model']]
            if set(model['required_fields'])-set(value) or set(value)-set(model['required_fields']+model['optional_fields']):raise SystemExit('golden result fields mismatch')
            if example['model']=='SpawnSignalResult':
                transition=MODELS['models']['TransitionResult']
                if set(transition['required_fields'])-set(value['transition']):raise SystemExit('missing transition result fields')
        elif example['kind']=='event':
            data=value['data'];name=example['event_name']
            if name not in {e['name'] for e in CATALOG['events']}:raise SystemExit('unknown golden event')
            if any(k in data for k in ['orderPin','attempt_id','owner_token','tax','resolvedTotal']):raise SystemExit('private/expanded event data')
            if len(json.dumps(value,separators=(',',':')).encode())>65536:raise SystemExit('oversized golden event')
            if value['subject']!=data['orderId']:raise SystemExit('event subject mismatch')
    print(f'Validated {len(fixtures["cases"])} contract cases and {len(fixtures["golden_envelopes"])} golden JSON envelopes; no runtime/provider conformance claimed')

if __name__=='__main__':main()
