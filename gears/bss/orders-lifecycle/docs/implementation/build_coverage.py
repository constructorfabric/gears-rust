#!/usr/bin/env python3
"""Build a source-routing inventory. This does not verify implementation semantics."""
import argparse
import hashlib
import json
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
DOCS = HERE.parent
ROOT = DOCS.parents[3]
PLANS = sorted(HERE.glob('[0-9][0-9]-*.md'))
STAGES = {int(p.name[:2]): p.name for p in PLANS}
UP = {}
def assign(stage, packages, names):
    for name in names.split():
        UP[name] = (stage, packages)
assign(3, 'S3-02', 'pricing-read-sdk pricing-catalog-tenant-reads commercial-service-provisioning products-sku-read-grant')
assign(3, 'S3-03/09', 'pricing-purchase-assessment pricing-bundle-sellability')
assign(3, 'S3-05/12', 'pricing-acceptance-policy initial-binding-acceptance')
assign(3, 'S3-04', 'pre-subscription-billing-terms')
assign(3, 'S3-07', 'rating-evaluation pre-subscription-evaluation tcv-with-annualisation')
assign(3, 'S3-08', 'indicative-tax-read')
assign(3, 'S3-12/13; S6-01', 'frozen-commercial-materialization')
assign(4, 'S4-02; S3-06', 'payer-commercial-profile delegation-proof-credential contract-party-eligibility contract-acceptance-declaration')
assign(4, 'S4-09/11', 'authorization-outcome workflow-amendment-verdict workflow-pricebook-contracts')
assign(5, 'S5-06a/b; S3-06', 'overlap-presence-read catalog-subscription-product-key overlap-activation-atomicity')
assign(5, 'S5-05/07/10', 'subscription-start-instant compensation-cancel-reason order-reference-on-create two-phase-pair-preserved correlation-propagation settle-create intent-status-read transition-outcome-echo external-reference-propagation')
assign(5, 'S5-14', 'workflow-overdue-escalation')
assign(6, 'S6-08; S3-13', 'sku-protection')
assign(6, 'S6-05; S2-08', 'event-consumer-conformance event-broker-runtime event-broker-cursor-retry event-broker-dead-letter-recovery event-broker-root-tenancy event-delivery-observability')
assign(2, 'S2-03; S1-03', 'pdp-policy-integration')
assign(2, 'S2-11; S6-07', 'audit-identity-lifecycle')
assign(2, 'S2-12; S6-06', 'gateway-path-param-throttle-key')

sources = [DOCS / n for n in ('PRD.md','DESIGN.md','DECISIONS.md','DECOMPOSITION.md','UPSTREAM_REQS.md')]
sources += sorted((DOCS/'features').glob('*.md')) + sorted((DOCS/'ADR').glob('*.md'))
# Include the directly consulted cross-owner normative documents, not unrelated gear roadmaps.
bss = DOCS.parent.parent
for rel in ('orders-workflow/docs/PRD.md','rating/docs/PRD.md','rating/docs/SEAMS.md','subscriptions/docs/SEAMS.md','subscriptions/docs/design/01-foundation-lifecycle.md','subscriptions/docs/design/03-plan-changes.md'):
    sources.append(bss/rel)

def route(path, context, token):
    if '-upreq-' in token:
        name = token.split('-upreq-',1)[1]
        if name not in UP:
            raise ValueError(f'Unassigned upstream requirement: {name}')
        return UP[name]
    full = str(path)
    if '/orders-workflow/' in full: return 5, 'S5-01/03/08; S4-09/11'
    if '/subscriptions/' in full: return 5, 'S5-01/05/06/07; S3-04'
    if '/rating/' in full: return 3, 'S3-01/07'
    m = re.search(r'contract-(0[1-8])', context+' '+token)
    if path.parent.name == 'features': n = int(path.name[:2])
    elif m: n = int(m.group(1))
    else: n = 0
    if n:
        return {1:(2,'S2-02–08/11'),2:(2,'S2-09/10/12'),3:(3,'S3-01–14'),4:(4,'S4-01–06/12'),5:(4,'S4-07–12'),6:(5,'S5-01–10/15/16'),7:(5,'S5-11–16'),8:(6,'S6-01–07; S2-03')}[n]
    if path.name == 'DECISIONS.md': return 1,'S1-01; REVIEW question/decision disposition'
    if path.name == 'PRD.md': return 1,'S1-02/07; S6-09 acceptance reconciliation'
    if path.parent.name == 'ADR': return 1,'S1-01/02; S6-09 architecture conformance'
    return 1,'S1-01/02/07; S6-09 cross-stage inventory'

rows=[]; hashes={}; updecl=set()
for path in sources:
    rel=path.relative_to(ROOT).as_posix(); raw=path.read_text(); hashes[rel]=hashlib.sha256(raw.encode()).hexdigest()
    context=''; fenced=False
    for num,line in enumerate(raw.splitlines(),1):
        if line.startswith('```'): fenced=not fenced
        heading = re.match(r'^#{1,6}\s+(.+)',line) if not fenced else None
        if heading: context=heading.group(1)
        anchors=re.findall(r'<a\s+(?:id|name)=["\']([^"\']+)',line)
        declared = re.findall(r'`(cpt-[^`]+)`',line) if re.search(r'\*\*ID\*\*|\*\*ID:|^- \[[ x]\]',line) else []
        q=re.match(r'^\| (Q-\d+) \|',line)
        units=([('section',context)] if heading else []) + [('anchor',a) for a in anchors] + [('requirement',i) for i in declared] + ([('question',q.group(1))] if q else [])
        for kind,token in units:
            stage,packages=route(path,context,token)
            if '-upreq-' in token: updecl.add(token.split('-upreq-',1)[1])
            rows.append(dict(source=rel,line=num,kind=kind,unit=token,stage=stage,packages=packages))
if set(UP) != updecl:
    raise ValueError(f'Upstream coverage mismatch: {set(UP)^updecl}')
data={'purpose':'Planning routing only; not implementation or test evidence. Section rows include their full body until the next heading. Historical decisions require supersession review, not literal implementation.', 'sources':hashes, 'units':rows}
manifest=json.dumps(data,indent=2,ensure_ascii=False)+'\n'
out=['# Design coverage inventory','', 'Generated by `build_coverage.py`. Every heading, explicit contract anchor, declared requirement and question in the listed sources is inventoried. A section assignment includes its prose, tables, unnumbered acceptance criteria and examples. It routes review work; it does **not** prove semantic completeness or test passage. Implementers must read the complete assigned section and record requirement-level evidence in S6-09. Historical and out-of-scope sibling content must receive an explicit applicability/supersession disposition, not automatic implementation.','',f'Baseline: **{len(sources)} source documents**, **{len(rows)} source units**, **{len(updecl)} upstream requirements**. Full line-specific routing and source SHA-256 fingerprints: [coverage-manifest.json](coverage-manifest.json).','', 'The manifest retains duplicate anchor occurrences by source line. Source edits invalidate `--check`; regenerate only after reviewing changed requirements and updating their plan owners. Source-wide/global requirements are assigned to S1 contract reconciliation and S6 release review, while stage plans contain the detailed implementation and test tasks.','', '## Document coverage','', '| Source | Units | Primary delivery / review |','|---|---:|---|']
import os
for path in sources:
    rel=path.relative_to(ROOT).as_posix(); subset=[r for r in rows if r['source']==rel]; stages=sorted({r['stage'] for r in subset})
    out.append(f'| [{path.parent.name}/{path.name}]({os.path.relpath(path,HERE)}) | {len(subset)} | '+', '.join(f'[S{s}]({STAGES[s]})' for s in stages)+' |')
out+=['','## Every upstream delivery','', '| Requirement | Delivery packages |','|---|---|']
for name,(stage,packages) in sorted(UP.items()):out.append(f'| `{name}` | [{packages}]({STAGES[stage]}) |')
out+=['','## Contract sections and explicit anchors','', 'These are source locations, not generated GitHub heading slugs. The source-line column disambiguates duplicate explicit anchors with the S1-01 anchor migration recorded separately. Use the JSON inventory for all requirement IDs and PRD/ADR/question rows.','', '| Source location | Contract / section | Packages |','|---|---|---|']
for r in rows:
    if r['kind']=='anchor' and r['source'].endswith('/DESIGN.md'):
        label=r['unit'].replace('|','\\|')
        out.append(f'| [DESIGN:{r["line"]}](../DESIGN.md) | `{label}` | [{r["packages"]}]({STAGES[r["stage"]]}) |')
markdown='\n'.join(out)+'\n'
parser=argparse.ArgumentParser();parser.add_argument('--check',action='store_true');args=parser.parse_args()
for name,content in [('coverage-manifest.json',manifest),('COVERAGE.md',markdown)]:
    target=HERE/name
    if args.check:
        if not target.exists() or target.read_text()!=content: raise SystemExit(f'Stale coverage: {name}; regenerate and review source routing')
    else:target.write_text(content)
print(f'Coverage {"verified" if args.check else "generated"}: {len(sources)} sources, {len(rows)} units, {len(updecl)} upstream requirements')
