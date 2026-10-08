#!/usr/bin/env python3
"""Render the inventory from the canonical migration SQL (no independent DDL owner)."""
import json
import re
import sys
from pathlib import Path
root=Path(__file__).resolve().parent
metadata={
'order':('aggregate','PDP: current resource/seller/payer AND standard order ID','order'),
'order_version':('append','current parent','order-version'),
'order_line_identity':('append','current parent','order-line-identity'),
'order_line':('append','current parent','order-line'),
'draft_content':('draft','current parent','draft-content'),
'order_admin':('admin','current parent','administrative-content'),
'order_line_admin':('admin','current parent','administrative-content'),
'resolved_total':('append','current parent','resolved-total'),
'inflight_overlap_claim':('claim','current parent plus complete authorized proposed arrangement','inflight-overlap-claim'),
'commercial_attempt':('attempt','current parent; private recovery authority','commercial-attempt'),
'fulfillment_grant':('append','current parent; engine-only append','fulfillment-grant'),
'fulfillment_control':('control','current parent; private recovery authority','fulfillment-control'),
'idempotency':('registry','trusted principal_scope + operation/key + execution ID','idempotency'),
'line_fulfillment':('projection','current parent','line-fulfillment'),
'acceptance':('append','current parent','acceptance'),
'date_policy':('date_policy','deployment policy IDs; no business authority','date-policy'),
'policy_election':('election','deployment policy IDs; no business authority','policy-election'),
'state_ttl_policy':('ttl_policy','deployment policy IDs; no business authority','state-ttl-policy'),
'gate_outcome':('diagnostic','trusted subject tenant and run ID; private inspection','gate-outcome'),
'approval_reflection':('append','current parent','approval-reflection'),
'read_access_log':('read_log','trusted actor; private inspection','read-access-log'),
'audit_checkpoint':('checkpoint','immutable audit namespace; checkpoint writer','audit-checkpoint'),
'audit_checkpoint_member':('checkpoint','immutable audit namespace; deliberately no live-order FK','audit-checkpoint-member'),
'transition_audit':('audit','current parent for resolved reads; trusted subject tenant for unresolved; audit namespace for verifier','transition-audit'),
}
owners={'order':'engine','order_version':'versioning','order_line_identity':'capture','order_line':'capture','draft_content':'capture','order_admin':'capture','order_line_admin':'capture','resolved_total':'gate-and-pin','transition_audit':'engine','audit_checkpoint':'audit worker','audit_checkpoint_member':'audit worker','fulfillment_grant':'engine','fulfillment_control':'engine','commercial_attempt':'engine','idempotency':'engine','line_fulfillment':'workflow-seam','inflight_overlap_claim':'gate-and-pin','acceptance':'preconditions','gate_outcome':'gate-and-pin','approval_reflection':'workflow-seam','state_ttl_policy':'hold-and-expiry promotion','date_policy':'capture promotion','policy_election':'preconditions promotion','read_access_log':'read-and-authz'}
files=sorted(root.glob('[0-9][0-9]_*.sql'))
sql='\n'.join(f.read_text() for f in files)
tables=[]
for f in files:
 for m in re.finditer(r'CREATE TABLE (bss_orders__\w+) \(\n(.*?)\n\);',f.read_text(),re.S):
  full,body=m.groups();name=full.removeprefix('bss_orders__');mutation,scope,source=metadata[name]
  t=dict(name=name,physical=full,content_owner=owners[name],source_ids=['cpt-cf-bss-orders-lifecycle-dbtable-'+source],migration=f.name,mutation=mutation,scope=scope,columns=[],pk=[],constraints={},indexes={},triggers=[],grants=[])
  for line in body.splitlines():
   line=line.strip().rstrip(',')
   if line.startswith('CONSTRAINT '):
    _,key,value=line.split(' ',2);t['constraints'][key]=value
    if value.startswith('PRIMARY KEY'):t['pk']=re.search(r'\((.*?)\)',value)[1].split(',')
   else:
    c=re.fullmatch(r'(\w+) (char\(3\)|\w+)( NOT NULL)?(.*)',line)
    assert c,line
    n,ty,required,extra=c.groups();t['columns'].append(dict(name=n,type=ty,nullable=not bool(required),extra=extra.strip()))
  for a in re.finditer(r'ALTER TABLE '+full+r' (ADD CONSTRAINT (\w+) (.*?));',sql,re.S):t['constraints'][a[2]]=' '.join(a[3].split())
  for a in re.finditer(r'ALTER TABLE '+full+r' DROP CONSTRAINT (\w+);',sql):t['constraints'].pop(a[1],None)
  for i in re.finditer(r'CREATE (UNIQUE )?INDEX (\w+) ON '+full+r' (.*?);',sql):t['indexes'][i[2]]=(i[1] or '')+i[3]
  for tr in re.finditer(r'CREATE (?:CONSTRAINT )?TRIGGER (\w+) ([^;]*?) ON '+full+r'\s+(.*?);',sql,re.S):
   # Match a trigger statement only, not across prior SQL statements.
   if ';' not in tr[2]:t['triggers'].append(' '.join(tr[0].split()))
  for g in re.finditer(r'GRANT (.*?) ON (.*?) TO (.*?);',sql):
   if full in [name.strip() for name in g[2].split(',')]:t['grants'].append(g[0])
  tables.append(t)
assert len(tables)==24
functions={m[1]:m[0] for m in re.finditer(r'CREATE FUNCTION (bss_orders__\w+)\(.*?\$\$;',sql,re.S)}
result={'physical_prefix':'bss_orders__','postgres_minimum':15,'schema_owner':'migrations/*.sql; this inventory is generated','tables':tables,'guard_functions':functions,'platform_schema_owners':['event_broker_sdk::producer_registration_migrations','toolkit_db::outbox::outbox_migrations'],'role_binding_migration':'08_roles.sql','compatibility':['PostgreSQL interval select_as=text/save_as=interval; calendar units retained','D-193 term_kind/authored_term distinguish missing, rolling and finite; term_duration nullable only when tagged missing/rolling','D-201 v1/v2 retained; runtime writers require v3','Idempotency audit linkage validated at deferred settlement; no lifetime FK that could prevent refused-audit retention','Date policy permanent default is the scope revision high-water; tenant deletion/recreation cannot reset it']}
rendered=json.dumps(result,indent=2)+'\n'
target=root/'schema-inventory.json'
if '--check' in sys.argv:
 if target.read_text()!=rendered:raise SystemExit('Schema inventory differs: run build_inventory.py')
 print('Schema inventory verified: 24 tables with source IDs, columns, keys, checks, indexes, grants and guard functions')
else:target.write_text(rendered)
