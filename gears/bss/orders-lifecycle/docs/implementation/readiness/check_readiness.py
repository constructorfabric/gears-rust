#!/usr/bin/env python3
"""Check S1-07 coverage, source drift, Cargo targets, dispatch DAG and evidence boundaries.

This validates the local ledger, not deployed systems. Never obtains credentials or probes prod.
"""
import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path

HERE=Path(__file__).resolve().parent
BASE=HERE.parent
ROOT=BASE.parents[4]
PREFIX='cpt-cf-bss-orders-lifecycle-upreq-'
def load(name):return json.loads((HERE/name).read_text())
def unique(rows):
 ids=[r['id'] for r in rows];assert len(ids)==len(set(ids)), 'duplicate IDs'
 return set(ids)

def check(metadata):
 ledger=load('ledger.json');queue=load('queue.json');snapshot=load('cargo-snapshot.json')
 assert ledger['production_ready'] is False and ledger['deployment_observed'] is False
 assert queue['production_ready'] is False
 upstream=(BASE.parent/'UPSTREAM_REQS.md').read_text()
 declared=list(re.finditer(r'^- \[[ x]\].*?\*\*ID\*\*: `([^`]+)`',upstream,re.M))
 ids=unique(ledger['requirements'])
 assert ids=={m[1] for m in declared},'upstream requirement missing/extra'
 packages=unique(queue['packages']);assert len(packages)==70
 actual_plan={}
 for path in sorted(BASE.glob('0[1-6]-*.md')):
  text=path.read_text();heads=list(re.finditer(r'^#{2,3} (S\d-\d\d) — (.+)$',text,re.M))
  for i,h in enumerate(heads):
   body=text[h.end():heads[i+1].start() if i+1<len(heads) else len(text)]
   dep=next((l for l in body.splitlines() if l.startswith(('**Depends on:**','**Dependencies:**'))),'See package entry conditions and source contracts.')
   actual_plan[h[1]]=(path.name,h[2],dep)
 assert set(actual_plan)==packages
 for package in queue['packages']:
  assert (package['plan'],package['title'],package['dependency_prose'])==actual_plan[package['id']],package['id']+' plan drift'
  if package['status']=='local_verified':assert (BASE/package['evidence']).is_file(),package['id']
 aliases={alias for names in queue['split_packages'].values() for alias in names}
 allowed=packages|aliases
 for row in ledger['requirements']:
  assert row['provider_profile'] in ledger['provider_profiles']
  assert set(row['implementation_packages'])<=allowed,row['id']
  assert all(row[k] for k in ['remaining_work','affected_paths','gate','source_capability'])
  assert row['release_ready'] is False,'local snapshot cannot assert owner production conformance'
  assert row['conformance']['owner_run'] is None
  assert row['conformance']['owner_command'] is None,'publish actual seam target only when delivered'
  assert row['conformance']['required_scenario']
  assert set(row['conformance']['local_evidence'])<=set(ledger['evidence'])
  i=next(i for i,m in enumerate(declared) if m[1]==row['id']);m=declared[i]
  body=upstream[m.start():declared[i+1].start() if i+1<len(declared) else len(upstream)]
  assert row['source']['sha256']==hashlib.sha256(body.encode()).hexdigest(),row['id']+' source drift'
  assert row['source']['line']==upstream[:m.start()].count('\n')+1
 for profile in ledger['provider_profiles'].values():
  for path in profile['source_paths']:assert (ROOT/path).is_file(),path
  for path,digest in profile['source_sha256'].items():assert hashlib.sha256((ROOT/path).read_bytes()).hexdigest()==digest,path+' inspected source drift'
  assert profile['configured_provider'] is None and profile['conformance_run'] is None
  assert profile['authenticated_principal'] is None and profile['grants'] is None
  assert profile['authority_required'] and profile['contract'] and profile['supported_scope']
 for e in ledger['evidence'].values():assert (BASE/e['report']).is_file(),e['report']
 for s in ledger['scope_dispositions']:assert s['enabled'] is False and s['package'] in packages
 units={u['id']:u for u in queue['dispatch_units']};assert len(units)==len(queue['dispatch_units'])
 visited=set();active=set()
 def visit(id):
  assert id in units,id
  assert id not in active,'dispatch dependency cycle: '+id
  if id in visited:return
  active.add(id)
  unit=units[id];assert set(unit['packages'])<=allowed,id
  for dep in unit['start_after']+unit['completion_after']:visit(dep)
  active.remove(id);visited.add(id)
 for id in units:visit(id)
 done={id for id,u in units.items() if u['status']=='local_verified'}
 for id in done:
  unit=units[id]
  assert (BASE/unit['evidence']).is_file(),id
  assert all(p in queue['local_baseline'] for p in unit['packages']),id
 assert set(queue['ready_to_start'])=={id for id,u in units.items() if id not in done and set(u['start_after'])<=done}
 assert queue['next_assignment'] in {p for id in queue['ready_to_start'] for p in units[id]['packages']}
 covered={p for u in units.values() for p in u['packages']}
 for p in packages-set(queue['local_baseline']):
  assert p in covered or (p in queue['split_packages'] and set(queue['split_packages'][p])<=covered),p+' missing dispatch'
 cargo={p['name']:p for p in metadata['packages']}
 for p in snapshot['packages']:
  live=cargo[p['name']]
  assert p['version']==live['version'],p['name']+' version drift'
  assert p['manifest_path']==str(Path(live['manifest_path']).relative_to(ROOT))
  targets=[dict(name=t['name'],kind=t['kind'],required_features=t.get('required-features',[])) for t in live['targets']]
  assert p['targets']==targets,p['name']+' target drift'
 for profile in ledger['provider_profiles'].values():assert set(profile['sdk_packages'])<={p['name'] for p in snapshot['packages']}
 # Every listed runnable Cargo command must target actual metadata. No invented owner test names.
 commands=list(ledger['evidence'].values())+[test for row in ledger['requirements'] for test in row['conformance']['existing_owner_tests']]
 for e in commands:
  command=e['command']
  if not command.startswith('cargo '):
   assert (ROOT/command.split()[1]).is_file();continue
  package=re.search(r' -p (\S+)',command)[1]
  assert package in cargo
  tests={t['name'] for t in cargo[package]['targets'] if 'test' in t['kind']}
  assert set(re.findall(r'--test (\S+)',command))<=tests
  features=set(re.search(r'--features (\S+)',command)[1].split(',')) if '--features ' in command else set()
  for target in re.findall(r'--test (\S+)',command):
   item=next(t for t in cargo[package]['targets'] if t['name']==target and 'test' in t['kind'])
   assert set(item.get('required-features',[]))<=features,command+' missing required features'
 print(f'PASS: {len(ids)} upstream requirements; {len(packages)} packages; {len(units)} acyclic dispatch units; {len(snapshot["packages"])} real Cargo packages; all production gaps explicit')

if __name__=='__main__':
 parser=argparse.ArgumentParser();parser.add_argument('--metadata',type=Path);args=parser.parse_args()
 metadata=json.loads(args.metadata.read_text()) if args.metadata else json.loads(subprocess.check_output(['cargo','+1.98.1','metadata','--no-deps','--format-version','1'],cwd=ROOT))
 check(metadata)
