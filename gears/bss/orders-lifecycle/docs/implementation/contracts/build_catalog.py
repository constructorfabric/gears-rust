#!/usr/bin/env python3
"""Freeze source-derived contract inventories; --check detects drift without rewriting."""
import argparse, hashlib, json, re
from pathlib import Path
HERE=Path(__file__).resolve().parent
DOCS=HERE.parents[1]
ROOT=DOCS.parents[3]
D=(DOCS/'DESIGN.md').read_text()
F=(DOCS/'features/01-foundation.md').read_text()
MODELS=json.loads((HERE/'models.json').read_text())

def section(anchor):
    marker=f'<a id="{anchor}"></a>'
    start=D.index(marker)
    end=D.find('\n<a id=',start+len(marker))
    return D[start:end if end!=-1 else len(D)].strip()
def block(anchor):
    marker=f'<a id="{anchor}"></a>';start=D.index(marker)
    end=D.find('<!-- /contract -->',start)
    return D[start:end if end!=-1 else len(D)].strip()
def cells(line):
    return [x.strip() for x in re.split(r'(?<!\\)\|',line.strip().strip('|'))]
def table_rows(text):
    return [cells(l) for l in text.splitlines() if l.startswith('|') and not re.match(r'^\|[\s|:\-]+$',l)]
def ref(anchor):return 'DESIGN.md#'+anchor

overview=D.split('**Endpoints Overview**',1)[1].split('The line `PATCH`',1)[0]
routes=re.findall(r'^\| `(GET|POST|PATCH|DELETE)` \| `([^`]+)` \| ([^|]+) \|',overview,re.M)
# Method names not already in the Workflow trait are selected local SDK names for implementation.
ops=[
('create','create', ['create'],'order','create',['partner_admin','direct_customer'],'CreateOrder','OrderView'),
('list','list', [],'order','read',['order_readers'],'ListOrders','OrderPage'),
('get','get', [],'order','read',['order_readers'],'OrderId','OrderView'),
('patch_order','patch_order',['draft-mutate','administrative-edit'],'order','field_class:write|edit',['partner_admin','direct_customer'],'HeaderPatch','TransitionResult'),
('add_line','add_line',['draft-mutate'],'order','write',['partner_admin','direct_customer'],'AddLine','TransitionResult'),
('patch_line','patch_line',['draft-mutate','administrative-edit'],'order','field_class:write|edit',['partner_admin','direct_customer'],'LinePatch','TransitionResult'),
('remove_line','remove_line',['draft-mutate'],'order','write',['partner_admin','direct_customer'],'LineId','TransitionResult'),
('preview','preview',[],'order','preview',['partner_admin','direct_customer'],'PreviewBasket','PreviewResult'),
('submit','submit',['submit'],'order','submit',['partner_admin','direct_customer'],'Submit','TransitionResult'),
('amend','amend',['amendment'],'order','amend',['partner_admin'],'Amendment','TransitionResult'),
('list_versions','list_versions',[],'order','read',['order_readers'],'VersionList','VersionPage'),
('get_version','get_version',[],'order','read',['order_readers'],'VersionRef','OrderVersionView'),
('cancel','cancel',['cancel'],'order','cancel',['partner_admin','direct_customer','seller_operator'],'Cancel','TransitionResult'),
('hold','hold',['hold'],'order','hold',['seller_operator','configured_workflow'],'Hold','TransitionResult'),
('resume','resume',['resume'],'order','resume',['seller_operator','configured_workflow'],'Resume','TransitionResult'),
('force_fail','force_fail',['force-fail-unreconciled'],'order','force-fail-unreconciled',['break_glass_user'],'ForcedFailure','TransitionResult'),
('record_acceptance','record_acceptance',['record-acceptance'],'acceptance','record',['eligible_resource_tenant_user'],'Acceptance','TransitionResult'),
('get_acceptance','get_acceptance',[],'order','read',['order_readers'],'AcceptanceList','AcceptancePage'),
('list_lines','list_lines',[],'order','read',['order_readers'],'LineList','LinePage'),
('read_audit','read_audit',[],'audit','read',['partner_admin','seller_operator'],'AuditList','AuditPage'),
('reflect_approval','reflect_approval',['reflect-approval-required','reflect-approval-not-required','reflect-approval-granted','reflect-approval-denied'],'order','approval-reflection',['configured_workflow'],'ApprovalReflection','TransitionResult'),
('begin_fulfillment','begin_fulfillment',['begin-fulfillment'],'order','begin-fulfillment',['configured_workflow'],'AuthorizationOutcome','TransitionResult'),
('report_spawn_signal','report_spawn_signal',['report-spawn-signal'],'order','spawn-signal',['configured_workflow'],'SpawnContribution','SpawnSignalResult'),
('acknowledge','acknowledge',['acknowledge-completed','acknowledge-failed'],'order','fulfillment-acknowledgement',['configured_workflow'],'FulfillmentAcknowledgement','TransitionResult'),
('workflow_cancel','workflow_cancel',['cancel-workflow-mediated'],'order','workflow-cancel',['configured_workflow'],'WorkflowCancel','TransitionResult')]
assert len(ops)==len(routes)==25
states=['draft','submitted','pending_approval','approved','in_fulfillment','on_hold','completed','rejected','cancelled','fulfillment_failed','expired']
terminal=states[6:]
rows=[]; expanded=[]
for match in re.finditer(r'^(\d+)\. \[ \].*?\*\*FROM\*\* (.*?) \*\*TO\*\* (.*?) \*\*WHEN\*\* `([^`]+)`(.*?) - `(inst-tr-[^`]+)`$',F,re.M):
    n,from_text,to_text,trigger,tail,instruction=match.groups();n=int(n)
    if n==1:from_states=[None]
    elif n==3:from_states=states[:6]
    elif n==25:from_states=states[1:6]
    elif n==24:from_states=['submitted','pending_approval','approved','on_hold']
    else:from_states=re.findall(r'`([^`]+)`',from_text)
    if 'same state' in to_text:target='$same'
    elif 'stored pre-hold' in to_text:target='$pre_hold_state'
    else:target=re.findall(r'`([^`]+)`',to_text)[0]
    event_match=re.search(r'`(Order[A-Z][A-Za-z]+)`',tail)
    row=dict(row=n,from_states=from_states,to=target,trigger=trigger,event=event_match[1] if event_match else None,versioning='append' if n in [1,4,18,19,20] else 'none',instruction=instruction,normative_rule=match[0],source='features/01-foundation.md#contract-01-order-state-machine')
    rows.append(row)
    for state in from_states:expanded.append(dict(from_state=state,trigger=trigger,to=state if target=='$same' else target,row=n))
assert len(rows)==29 and len({(e['from_state'],e['trigger']) for e in expanded})==len(expanded)
workflow_triggers={t for o in ops[-5:] for t in o[2]}
operations=[]
for route,op in zip(routes,ops):
    method,path,owner=route
    ident,sdk,triggers,resource,action,actors,request,result=op
    transition=bool(triggers)
    operations.append(dict(id=ident,method=method,path=path,owner=owner.strip(),sdk_method=sdk,success_http_status=201 if ident=='create' else 200,sdk_trait='OrdersLifecycleWorkflowV1' if ident in ['get','get_version','hold','resume','reflect_approval','begin_fulfillment','report_spawn_signal','acknowledge','workflow_cancel'] else 'OrdersLifecycleV1 (selected public application API)',actor_eligibility=actors,pdp_resource=resource,pdp_action=action,request_model=request,result_model=result,expected_version='required-before-authorization' if transition and ident!='create' else 'not-applicable',expected_draft_revision='boundary-required-on-submit' if ident=='submit' else 'optional-at-boundary; required by engine after admissibility on draft-mutate' if 'draft-mutate' in triggers else 'not-applicable',idempotency='required' if transition else 'none',retention_min_seconds=2592000 if set(triggers)&workflow_triggers else 86400 if transition else None,triggers=triggers,state_rows=[r['row'] for r in rows if r['trigger'] in triggers],events=sorted({r['event'] for r in rows if r['trigger'] in triggers and r['event']}),failure_mapping='Orders reason registry plus platform canonical errors; shared precedence and disclosure rules',source='DESIGN.md#33-api-contracts'))
errors=[]
for reason,code,cat,status in re.findall(r'^\| `([a-z0-9-]+)` \| `([A-Z_]+)` \| (\w+) \| (\d+) \|$',D,re.M):
    errors.append(dict(reason=reason,error_domain='orders-lifecycle.v1',error_code=code,canonical_category=cat,http_status=int(status),gts_registry_key='gts.cf.bss.orders.err.v1~cf.bss.orders.'+reason.replace('-','_')+'.v1~'))
assert len({e['reason'] for e in errors})==len(errors)
canonical_source=ROOT/'libs/toolkit-canonical-errors/src/problem.rs'
canon=canonical_source.read_text()
fragment_text=canon.split('pub fn gts_fragment',1)[1].split('pub fn http_status',1)[0]
status_text=canon.split('pub fn http_status',1)[1].split('pub fn title',1)[0]
title_text=canon.split('pub fn title',1)[1].split('\n    }',1)[0]
fragments=dict(re.findall(r'Self::(\w+) =>\s*\{?\s*gts_id!\("([^"]+)"\)',fragment_text))
statuses={x:int(y) for x,y in re.findall(r'Self::(\w+) => (\d+)',status_text)}
titles=dict(re.findall(r'Self::(\w+) => "([^"]+)"',title_text))
for e in errors:
    c=e['canonical_category'];e.update(type='gts://gts.'+fragments[c],title=titles[c])
    assert e['http_status']==statuses[c] or (e['reason']=='expected-version-required' and e['http_status']==428)

tables=[]
for line in D.splitlines():
    if re.match(r'^\| `orders_[^`]+` \| `bss_orders__',line):
        name,physical,source,owner,mutability=cells(line)
        name=name.strip('`');anchor='contract-01-table-'+name
        if f'<a id="{anchor}">' not in D:
            # Slice tables use their schema contract heading.
            anchor={'orders_order_admin':'contract-01-table-orders_order_admin--orders_order_line_admin','orders_order_line_admin':'contract-01-table-orders_order_admin--orders_order_line_admin','orders_gate_outcome':'contract-03-3-7','orders_approval_reflection':'contract-06-3-7','orders_state_ttl_policy':'contract-07-3-7','orders_date_policy':'contract-02-3-7','orders_policy_election':'contract-05-table-orders_policy_election','orders_read_access_log':'contract-08-table-orders_read_access_log','orders_commercial_attempt':'contract-01-commercial-attempt','orders_fulfillment_grant':'contract-06-activation-admission','orders_fulfillment_control':'contract-06-activation-admission'}[name]
        text=section(anchor) if anchor.startswith('contract-01-table') else block(anchor)
        if name in ['orders_commercial_attempt','orders_fulfillment_grant','orders_fulfillment_control']:
            # Later decision sections are not bounded by old contract-end markers.
            start=D.index(f'<a id="{anchor}">');end=D.find('\n<a id=',start+20);text=D[start:end if end!=-1 else len(D)].strip()
        fields=[]
        for r in table_rows(text):
            if len(r)==3 and r[0] not in ['Column','Field','Setting'] and not r[0].startswith('**'):
                names=[n.strip().strip('`') for n in r[0].split(',')]
                types=[t.strip() for t in r[1].split(',')]
                # Multiple base types correspond positionally; qualifiers apply to all.
                base={'uuid','integer','text','jsonb','timestamptz'}
                positional=len(names)>1 and len(types)>=len(names) and all(t in base for t in types[:len(names)])
                for i,n in enumerate(names):
                    typ=', '.join([types[i]]+types[len(names):]) if positional else r[1]
                    fields.append(dict(name=n,type=typ,semantics=r[2]))
        if name in MODELS['prose_table_fields']:
            fields=[dict(name=n,type=t,semantics='See full normative_schema and model contract; immutable versus operational fields are distinct') for n,t in MODELS['prose_table_fields'][name].items()]
        if name=='orders_draft_content':
            authored=set(MODELS['field_classes']['line']['commercial'])|{'order_id','line_id'}
            fields=[f.copy() for t in tables if t['name']=='orders_order_line' for f in t['fields'] if f['name'] in authored]
        if name=='orders_draft_content':
            for f in fields:
                if f['name'] in ['contract_effective_date','service_activation_date','acceptance_due_date','term_duration','billing_cycle']:
                    f['type'] += ', nullable in draft'
        tables.append(dict(name=name,physical_name=physical.strip('`'),owner=owner,mutability=mutability,source=ref(anchor),fields=fields,normative_schema=text))
assert len(tables)==24
# Full schema supplements retain later overrides instead of silently choosing old tables.
anchors=['contract-02-4-3','contract-03-frozen-commercial-snapshot','contract-03-rating-purchase-evaluation','contract-03-diagnostic-mapping','contract-01-commercial-attempt','contract-06-activation-admission','contract-08-4-3','contract-01-4-7']
supplements=[]
for a in anchors:
    start=D.index(f'<a id="{a}">')
    end=D.find('\n<a id=',start+20)
    # Field classification and receipt/decision blocks are self-contained through next anchor.
    supplements.append(dict(source=ref(a),contract=D[start:end if end!=-1 else len(D)].strip()))
event_section=section('contract-01-the-event-set')
events=[]
for name,nums,payload in re.findall(r'^\| `(Order[A-Za-z]+)` \| ([\d, ]+) \| (.+) \|$',event_section,re.M):
    events.append(dict(name=name,rows=[int(n) for n in nums.split(',')],payload_contract=payload,source=ref('contract-01-the-event-set')))
source_paths=[DOCS/'DESIGN.md',DOCS/'features/01-foundation.md',canonical_source,HERE/'models.json']
catalog=dict(format_version=1,status='design contract catalog; not runtime implementation',operations=operations,states=states,terminal_states=terminal,source_transition_rows=rows,expanded_transition_keys=expanded,triggers=list(dict.fromkeys(r['trigger'] for r in rows)),workflow_triggers=sorted(workflow_triggers),events=events,errors=errors,tables=tables,schema_supplements=supplements,sources={str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in source_paths},startup_invariants=['unique operation method/path and SDK identity','every operation has a complete permission declaration','every authored field is classified once','every guard references an existing row and registered refusal reason','unique expanded state/trigger key','unique reason GTS key and domain/code; complete canonical mapping','every declared event matches exactly its source rows','unsupported schema/profile fails closed'],category=dict(type='gts.cf.bss.orders.category.v1~',admitted=['gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1'],registered_not_admitted=['gts.cf.bss.orders.category.v1~cf.bss.orders.change.v1'],storage='registry-resolved text, not a database enum'))
parser=argparse.ArgumentParser();parser.add_argument('--check',action='store_true');args=parser.parse_args()
def md_cell(value):
    return str(value).replace('|', '&#124;').replace('\n', ' ')
def md_table(headers, entries):
    return '\n'.join(['| '+' | '.join(headers)+' |', '| '+' | '.join(['---']*len(headers))+' |']+['| '+' | '.join(md_cell(v) for v in row)+' |' for row in entries])
readable='# S1-02 generated catalog\n\nGenerated by `build_catalog.py`; edit the source contracts or explicit selections in `models.json`, then regenerate. Read [CONTRACTS](CONTRACTS.md) for binding and evidence limits. Full fields, nullability, constraints, permissions and source hashes are in [catalog.json](catalog.json).\n\n'
readable+='## Operations\n\n'+md_table(['SDK method','HTTP route','Permission resource / action','Request → result','Triggers / rows'], [[o['sdk_method'],o['method']+' '+o['path'],o['pdp_resource']+' / '+o['pdp_action'],o['request_model']+' → '+o['result_model'],', '.join(o['triggers'])+' / '+str(o['state_rows'])] for o in operations])+'\n\n'
readable+='## Storage inventory\n\nThe full normative schema includes relational constraints and later overrides; these field names alone are not migration specifications.\n\n'+md_table(['Logical table','Physical table','Fields'],[[t['name'],t['physical_name'],', '.join(f['name'] for f in t['fields'])] for t in tables])+'\n\n'
readable+='## Transition rows\n\nMulti-state rows expand to '+str(len(expanded))+' unique keys. `$pre_hold_state` requires the stored-state and policy guards.\n\n'+md_table(['Row','From','Trigger','To','Event','Version'],[[r['row'],', '.join(str(x) for x in r['from_states']),r['trigger'],r['to'],r['event'] or 'none',r['versioning']] for r in rows])+'\n\n'
readable+='## Event declarations\n\n'+md_table(['Event','Rows','Payload contract'],[[e['name'],e['rows'],e['payload_contract'].replace('(DESIGN.md#','(../../DESIGN.md#').replace('(features/','(../../features/').replace('](#','](../../DESIGN.md#')] for e in events])+'\n\n'
readable+='## Error registry\n\nDomain: `orders-lifecycle.v1`. Exact canonical type URLs and reason GTS identifiers are in the JSON. Platform transport/infrastructure errors also remain possible.\n\n'+md_table(['Reason','Code','Category','HTTP','Canonical title'],[[e['reason'],e['error_code'],e['canonical_category'],e['http_status'],e['title']] for e in errors])+'\n'
outputs={'catalog.json':json.dumps(catalog,indent=2,ensure_ascii=False)+'\n','CATALOG.md':readable}
for filename,content in outputs.items():
    target=HERE/filename
    if args.check:
        if not target.exists() or target.read_text()!=content:raise SystemExit(filename+' is stale; reconcile and regenerate')
    else:target.write_text(content)
print(f'Catalog {"verified" if args.check else "generated"}: {len(operations)} routes, {len(rows)} rows / {len(expanded)} expanded keys, {len(errors)} errors, {len(tables)} tables, {len(events)} events')
