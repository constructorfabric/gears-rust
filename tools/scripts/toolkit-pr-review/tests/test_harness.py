"""Offline regression tests for the review commands. No LLM or network calls."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import harness as h
import review
import budget
import ghsource


class Run(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='review-harness-test-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.work = self.root / 'work'
        self.sources = self.root / 'fixture'
        for side in ['base', 'head']:
            (self.sources / side).mkdir(parents=True)
            (self.sources / side / 'x.rs').write_text(
                '\n'.join(('base' if side == 'base' else 'head') + str(i) for i in range(1, 11))+'\n', encoding='utf-8')
        self.diff = self.root / 'diff.patch'
        self.diff.write_text('diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,10 +1,10 @@\n'+
                             ''.join('-base'+str(i)+'\n' for i in range(1,11))+
                             ''.join('+head'+str(i)+'\n' for i in range(1,11)),encoding='utf-8')
        self.meta = self.root / 'meta.json'
        h.save(self.meta, {'number':5391,'headRefOid':'a'*40,'baseRefOid':'b'*40})
        with contextlib.redirect_stdout(io.StringIO()):
            code = review.main(['prepare','--from-diff',str(self.diff),'--from-meta',str(self.meta),
                                '--source-root',str(self.sources),'--work-dir',str(self.work)])
        self.assertEqual(code, 0)
        self.ctx = h.context(self.work)

    def claim(self, module='async', number=0, severity='LOW', kind='defect'):
        return dict(candidate_id=f'{module}:{number}', path='x.rs', line=1, side='RIGHT',subject='line',kind=kind,
                    rule_id=self.ctx['rule_ids'][module][0], severity=severity,
                    issue='Scoped context is lost in child work', evidence='Production handoff reads independent task-local state',
                    impact='Conditional downstream diagnostic request loses correlation', fix='Capture and rescope around child work')

    def a(self, side='head', line=1):
        return dict(side=side,path='x.rs',line=line,quote=('head' if side=='head' else 'base')+str(line))

    def envelope(self, module='async', findings=None, obligations=None):
        return dict(schema='toolkit.review-pass.v1',context_sha256=h.digest(self.ctx),head_sha=self.ctx['head_sha'],
                    module=module,status='complete',notes=['Reviewed source and normal/rejecting control'],
                    findings=findings or [],boundary_obligations=obligations or [])

    def closure(self, draft):
        c = copy.deepcopy(draft)
        c['dispositions'] = []
        c['scope_assessments'] = [dict(candidate_id=f['candidate_id'],status='introduced',reason='Compared changed behavior and assertions',
                                     evidence=[self.a('base'),self.a()]) for f in c['findings']]
        c['boundary_audit'] = dict(schema='toolkit.boundary-audit.v2',head_sha=self.ctx['head_sha'],module=c['module'],chains=[],
            severity_checks=[dict(candidate_id=f['candidate_id'],severity=f['severity'],impact_class='observability_only',
                                 consequence=f['impact'],secondary_effect='',evidence=[self.a()]) for f in c['findings']])
        for o in c['boundary_obligations']:
            c['boundary_audit']['chains'].append(dict(id=o['id'],hypothesis=o['hypothesis'],status='supported',
                entry=self.a(),boundary=self.a(line=2),dispatch=self.a(line=3),sink=self.a(line=4),explanation='Trace through concrete dispatch to send',
                branches=[dict(kind=k,status='examined',explanation='Source-backed branch',evidence=[self.a()])
                          for k in ['main_transport','auxiliary_io','configuration']]))
        return c

    def source_complete(self, claims=None):
        claims = claims if claims is not None else [self.claim()]
        for agent in self.ctx['agents']:
            module = agent['name']
            d = self.envelope(module,[c for c in claims if c['candidate_id'].startswith(module+':')])
            h.validate_pass(self.work,module,'draft',d)
            h.validate_pass(self.work,module,'closure',self.closure(d))

    def collected(self, claims=None, merges=None):
        self.source_complete(claims)
        req = h.collect_input(self.work)
        h.validate_collect(self.work,dict(schema='toolkit.collect.v1',context_sha256=h.digest(self.ctx),
            checkpoint_sha256=req['checkpoint_sha256'],merges=merges or [],exclusions=[],notes=[]))
        h.match_input(self.work)  # fixture/local: deterministic no-history matching
        return h.format_input(self.work)

    def finished(self, claims=None):
        fmt = self.collected(claims)
        return h.finalize(self.work,dict(schema='toolkit.format.v1',context_sha256=h.digest(self.ctx),
                         comments=[dict(stable_id=g['stable_id'],comment='On OAuth2 cache miss, child work loses correlation; capture and rescope its context.') for g in fmt['groups']]))

    def invalid(self, fn, text=None):
        return self.assertRaisesRegex((h.Error, KeyError, TypeError, ValueError),text or '.')


class TestStages(Run):
    def test_context_pins_versions_sources_and_rules(self):
        self.assertEqual(self.ctx['schema_version'],5)
        self.assertEqual(self.ctx['policy'],h.POLICY)
        self.assertEqual(sum(map(len,self.ctx['rule_ids'].values())),52)
        self.assertEqual(h.materialize(self.work,self.ctx,'base','x.rs').splitlines()[0],b'base1')
        self.assertEqual(h.materialize(self.work,self.ctx,'head','x.rs').splitlines()[0],b'head1')
        self.assertIn('instructions/docs/toolkit-pr-review/policies/pass-contract.md',h.read(self.work/'manifest.json')['files'])

    def test_legacy_context_is_untouched_and_rejected(self):
        h.save(self.work/'context.json',{'schema_version':4})
        before = (self.work/'context.json').read_bytes()
        with self.assertRaises(h.Error): h.context(self.work)
        self.assertEqual((self.work/'context.json').read_bytes(),before)

    def test_prepare_does_not_overwrite_existing_run(self):
        before = (self.work/'context.json').read_bytes()
        with contextlib.redirect_stderr(io.StringIO()):
            code = review.main(['prepare','--from-diff',str(self.diff),'--work-dir',str(self.work)])
        self.assertEqual(code,1)
        self.assertEqual((self.work/'context.json').read_bytes(),before)

    def test_context_hash_change_is_detected(self):
        ctx=copy.deepcopy(self.ctx);ctx['head_sha']='c'*40;h.save(self.work/'context.json',ctx)
        with self.assertRaises(h.Error): h.context(self.work)

    def test_frozen_policy_change_is_detected(self):
        (self.work/'instructions/docs/toolkit-pr-review/policies/collect.md').write_text('changed',encoding='utf-8')
        with self.assertRaises(h.Error): h.context(self.work)

    def test_cached_source_change_is_detected(self):
        (self.work/'sources/head/x.rs').write_text('changed',encoding='utf-8')
        with self.assertRaises(h.Error): h.anchor(self.work,self.ctx,self.a())

    def test_source_binding_registry_change_is_detected_after_pass(self):
        d=self.envelope();h.validate_pass(self.work,'async','draft',d)
        registry=h.read(self.work/'sources.json');registry['head:x.rs']['revision']='other';h.save(self.work/'sources.json',registry)
        with self.assertRaises(h.Error): h.completed(self.work,'draft/async')

    def test_wrong_quote_or_source_side_fails(self):
        for a in [dict(self.a(),quote='not in file'),dict(self.a(),side='base'),dict(self.a(),line=100)]:
            with self.subTest(a=a), self.assertRaises(h.Error): h.anchor(self.work,self.ctx,a)

    def test_absence_needs_a_missing_tree_entry(self):
        self.assertEqual(h.anchor(self.work,self.ctx,dict(side='base',path='missing.rs',absent=True)),'base')
        with self.assertRaises(h.Error): h.anchor(self.work,self.ctx,dict(side='base',path='x.rs',absent=True))

    def test_path_traversal_is_rejected(self):
        for p in ['../x.rs','/x.rs','C:/x.rs','a/../x.rs','a\\x.rs','.git/config']:
            with self.subTest(p=p),self.assertRaises(h.Error): h.materialize(self.work,self.ctx,'head',p)

    def test_malformed_response_never_becomes_empty_findings(self):
        for raw in ['[]','bad json','{"findings":[]}', '```json\n[]\n```']:
            with self.subTest(raw=raw),self.assertRaises((h.Error,ValueError,KeyError)):
                h.validate_pass(self.work,'async','draft',h.payload(raw))

    def test_wrong_context_or_head_is_rejected(self):
        for key in ['context_sha256','head_sha']:
            d=self.envelope();d[key]='wrong'
            with self.subTest(key=key),self.assertRaises(h.Error): h.validate_pass(self.work,'async','draft',d)

    def test_lost_candidate_id_is_an_error(self):
        d=self.envelope(findings=[self.claim()]);h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d);c['findings']=[];c['scope_assessments']=[];c['boundary_audit']['severity_checks']=[]
        with self.assertRaisesRegex(h.Error,'disappeared'): h.validate_pass(self.work,'async','closure',c)

    def test_retained_anchor_and_rule_are_immutable(self):
        d=self.envelope(findings=[self.claim()]);h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d);c['findings'][0]['line']=2
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)

    def test_closed_empty_pass_is_distinct_from_unrun(self):
        with self.assertRaises(h.Error): h.collect_input(self.work)
        self.source_complete([])
        self.assertEqual(h.collect_input(self.work)['claims'],[])

    def test_unresolved_branches_cannot_be_marked_complete(self):
        d=self.envelope(obligations=[dict(id='async:chain:0',hypothesis='Scoped state might not reach client',next_edge='dispatch')])
        h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d);c['boundary_audit']['chains'][0]['branches'][1]['status']='unresolved'
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)
        c['status']='partial'
        self.assertFalse(h.validate_pass(self.work,'async','closure',c))
        with self.assertRaises(h.Error): h.collect_input(self.work)

    def test_obligation_cannot_disappear_or_change(self):
        d=self.envelope(obligations=[dict(id='async:chain:0',hypothesis='handoff',next_edge='send')]);h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d);c['boundary_audit']['chains']=[]
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)

    def test_partial_and_uncertain_are_stored_but_block_collect(self):
        d=self.envelope(findings=[self.claim()]);h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d);c['findings']=[];c['scope_assessments']=[];c['boundary_audit']['severity_checks']=[]
        c['dispositions']=[dict(candidate_id='async:0',status='uncertain',reason='Missing dependency',evidence=[self.a()])]
        c['status']='partial'
        self.assertFalse(h.validate_pass(self.work,'async','closure',c))
        self.assertEqual(h.completed(self.work,'closure/async')['dispositions'][0]['status'],'uncertain')
        with self.assertRaises(h.Error): h.collect_input(self.work)

    def test_preexisting_requires_both_revisions(self):
        d=self.envelope(findings=[self.claim()]);h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d);c['findings']=[];c['scope_assessments']=[];c['boundary_audit']['severity_checks']=[]
        c['dispositions']=[dict(candidate_id='async:0',status='preexisting',reason='same exposure',evidence=[self.a()])]
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)
        c['dispositions'][0]['evidence'].append(self.a('base'))
        self.assertTrue(h.validate_pass(self.work,'async','closure',c))

    def test_diagnostic_high_rejected_security_high_preserved(self):
        d=self.envelope(findings=[self.claim(severity='HIGH')]);h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d)
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)
        check=c['boundary_audit']['severity_checks'][0]
        check.update(impact_class='critical_breach',secondary_effect='Attacker-controlled input reaches an authorization bypass')
        self.assertTrue(h.validate_pass(self.work,'async','closure',c))

    def test_finding_requires_a_changed_side(self):
        c=self.claim();c['line']=11
        with self.assertRaises(h.Error): h.candidate(self.ctx,'async',c)
        c['line']=1;c['side']='LEFT';h.candidate(self.ctx,'async',c)

    def test_completed_artifacts_are_immutable(self):
        d=self.envelope();h.validate_pass(self.work,'async','draft',d)
        other=copy.deepcopy(d);other['notes']=['changed after completion']
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','draft',other)

    def test_partial_repair_preserves_history(self):
        d=self.envelope();d['status']='partial';h.validate_pass(self.work,'async','draft',d)
        d['status']='complete';h.validate_pass(self.work,'async','draft',d)
        self.assertEqual(len(list((self.work/'history/draft/async').glob('*.json'))),1)

    def test_two_invalid_attempts_record_errors_without_clean_result(self):
        raw=self.root/'response.json'
        for content in ['bad','[]','{}']:
            raw.write_text(content,encoding='utf-8')
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(review.main(['validate-pass','--work-dir',str(self.work),'--module','async','--phase','draft','--input',str(raw)]),1)
        attempts=self.work/'attempts/draft/async'
        self.assertEqual(len(list(attempts.glob('*.txt'))),2)
        self.assertEqual(len(list(attempts.glob('*.error.json'))),2)
        self.assertFalse((self.work/'out/draft/async.json').exists())

    def test_low_survives_all_stages_and_never_payload(self):
        final=self.finished()
        self.assertEqual(final['findings'][0]['severity'],'LOW')
        self.assertEqual(final['publication'],[])
        self.assertIn('LOW',(self.work/'report.md').read_text(encoding='utf-8'))

    def test_duplicate_evidence_reaches_formatter_without_rewriting(self):
        a=self.claim();b=self.claim('design');b['issue']='Complementary conditional auxiliary request';b['fix']='Require opt-in and validate untrusted value'
        fmt=self.collected([a,b],[dict(candidate='design:0',duplicate_of='async:0',reason='Same evidenced boundary flow; retain both requirements')])
        self.assertEqual(fmt['groups'][0]['claim'],a)
        self.assertEqual(fmt['groups'][0]['duplicate_evidence'],[b])
        self.assertEqual(h.completed(self.work,'collect')['original_claims'],[a,b])

    def test_independent_same_line_findings_are_not_deduplicated(self):
        fmt=self.collected([self.claim(),self.claim(number=1)])
        self.assertEqual(len(fmt['groups']),2)

    def test_runtime_and_coverage_cannot_merge(self):
        a,b=self.claim(),self.claim('tests',kind='coverage_gap')
        with self.assertRaises(h.Error): h.derive_groups([a,b],[dict(candidate='tests:0',duplicate_of='async:0',reason='same line')],[])

    def test_weak_representative_and_transitive_group_rejected(self):
        a,b,c=self.claim(severity='MEDIUM'),self.claim(number=1),self.claim(number=2)
        for merges in [[dict(candidate='async:0',duplicate_of='async:1',reason='same')],
                       [dict(candidate='async:2',duplicate_of='async:1',reason='same'),dict(candidate='async:1',duplicate_of='async:0',reason='same')]]:
            with self.subTest(merges=merges),self.assertRaises(h.Error): h.derive_groups([a,b,c],merges,[])

    def test_formatter_cannot_change_metadata_or_drop_ids(self):
        self.collected()
        v=dict(schema='toolkit.format.v1',context_sha256=h.digest(self.ctx),comments=[])
        with self.assertRaises(h.Error): h.finalize(self.work,v)
        v['comments']=[dict(stable_id='async:0',comment='text',severity='HIGH')]
        with self.assertRaises(h.Error): h.finalize(self.work,v)

    def test_formatter_bound_to_collect_and_matching(self):
        self.collected()
        saved=h.read(self.work/'out/collect.json');saved['groups']=[];h.save(self.work/'out/collect.json',saved)
        with self.assertRaises(h.Error): h.finalize(self.work,dict(schema='toolkit.format.v1',context_sha256=h.digest(self.ctx),comments=[]))

    def test_collect_cannot_invent_findings(self):
        self.source_complete();req=h.collect_input(self.work)
        v=dict(schema='toolkit.collect.v1',context_sha256=h.digest(self.ctx),checkpoint_sha256=req['checkpoint_sha256'],
               merges=[],exclusions=[],notes=[],findings=[])
        with self.assertRaises(h.Error): h.validate_collect(self.work,v)

    def test_fixture_cannot_publish(self):
        self.finished()
        with patch.object(h,'gh',side_effect=AssertionError('network forbidden')),self.assertRaises(h.Error): h.publish(self.work)

    def test_publisher_requires_live_same_head_and_does_not_post(self):
        self.finished()
        with patch.object(h,'all_claims',return_value=(dict(self.ctx,offline_fixture=False),[],[])),patch.object(h,'current_head',return_value='changed'),patch.object(h,'gh',side_effect=AssertionError('POST forbidden')):
            with self.assertRaisesRegex(h.Error,'head changed'): h.publish(self.work)

    def test_unknown_post_blocks_retry_and_reconcile_never_posts(self):
        self.finished([self.claim(severity='MEDIUM')])
        real=dict(self.ctx,offline_fixture=False)
        with patch.object(h,'all_claims',return_value=(real,[],[])),patch.object(h,'current_head',return_value=self.ctx['head_sha']):
            with patch.object(h,'gh',side_effect=h.Error('lost response')) as api:
                with self.assertRaises(h.Error): h.publish(self.work)
                self.assertEqual(api.call_count,1)
            journal=h.read(self.work/'publication-journal.json')
            self.assertEqual(next(iter(journal['operations'].values()))['status'],'unknown')
            with patch.object(h,'gh',side_effect=AssertionError('must not blindly repeat')):
                with self.assertRaises(h.Error): h.publish(self.work)
            body=h.completed(self.work,'final')['publication'][0]['comment']
            row=dict(commit_id=self.ctx['head_sha'],path='x.rs',body=body,line=1,side='RIGHT')
            with patch.object(h,'existing_comments',return_value=[row]),patch.object(h,'gh',side_effect=AssertionError('no POST during reconcile')):
                result=h.publish(self.work,reconcile=True)
            self.assertEqual(next(iter(result['operations'].values()))['status'],'posted')

    def test_no_manual_low_payload_can_pass_publisher(self):
        final=self.finished()
        final['publication']=final['findings']
        real=dict(self.ctx,offline_fixture=False)
        original=h.completed
        def complete(work,stage): return final if stage=='final' else original(work,stage)
        with patch.object(h,'all_claims',return_value=(real,[],[])),patch.object(h,'completed',side_effect=complete),patch.object(h,'current_head',return_value=self.ctx['head_sha']),patch.object(h,'gh',side_effect=AssertionError('no POST')):
            with self.assertRaises(h.Error): h.publish(self.work)

    def test_budget_accounts_for_second_pass_and_post(self):
        old=budget.agent_costs(10000,1000);new,post=budget.pipeline_costs(10000,1000)
        self.assertGreater(post,0)
        self.assertTrue(all(new[m]>2*old[m] for m in old))

    def test_source_rename_and_deletion_both_sides(self):
        ctx=copy.deepcopy(self.ctx)
        ctx['files']['renamed.rs']=dict(status='renamed',old_path='old.rs',ranges={'right':[[2,2]],'left':[[2,2]]})
        ctx['files']['gone.rs']=dict(status='deleted',old_path=None,ranges={'right':[],'left':[[1,2]]})
        for path in ['old.rs','gone.rs']:
            (self.sources/'base'/path).write_bytes(b'base one\nbase two\n')
        (self.sources/'head/renamed.rs').write_bytes(b'head one\nhead two\n')
        self.assertEqual(h.materialize(self.work,ctx,'base','old.rs'),b'base one\nbase two\n')
        self.assertEqual(h.materialize(self.work,ctx,'head','renamed.rs'),b'head one\nhead two\n')
        self.assertIsNone(h.materialize(self.work,ctx,'head','gone.rs'))
        c=self.claim();c.update(path='renamed.rs',line=2,side='LEFT');h.candidate(ctx,'async',c)
        c.update(path='gone.rs',line=1);h.candidate(ctx,'async',c)
        c.update(side='RIGHT')
        with self.assertRaises(h.Error): h.candidate(ctx,'async',c)
        c.update(subject='file',line=None,side=None);h.candidate(ctx,'async',c)

    def test_bad_receipt_schema_and_missing_scope_block_collect(self):
        d=self.envelope(findings=[self.claim()]);h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d);c['boundary_audit']['schema']='toolkit.boundary-audit.v1'
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)
        c['boundary_audit']['schema']='toolkit.boundary-audit.v2';c['scope_assessments']=[]
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)

    def test_missing_auxiliary_branch_and_rejected_without_source_fail(self):
        d=self.envelope(obligations=[dict(id='async:chain:0',hypothesis='handoff',next_edge='dispatch')]);h.validate_pass(self.work,'async','draft',d)
        c=self.closure(d);chain=c['boundary_audit']['chains'][0]
        chain['branches']=[b for b in chain['branches'] if b['kind']!='auxiliary_io']
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)
        c=self.closure(d);chain=c['boundary_audit']['chains'][0];chain.update(status='rejected',entry=None,boundary=None,dispatch=None,sink=None)
        with self.assertRaises(h.Error): h.validate_pass(self.work,'async','closure',c)

    def test_corrupt_draft_after_closure_blocks_collect(self):
        self.source_complete()
        (self.work/'out/draft/async.json').write_text('[]',encoding='utf-8')
        with self.assertRaises(h.Error): h.collect_input(self.work)

    def test_unknown_merge_id_is_not_ignored(self):
        with self.assertRaises(h.Error): h.derive_groups([self.claim()],[dict(candidate='wrong:0',duplicate_of='async:0',reason='same')],[])

    def test_publish_uses_comment_event_and_idempotent_journal(self):
        self.finished([self.claim(severity='MEDIUM')])
        real=dict(self.ctx,offline_fixture=False)
        with patch.object(h,'all_claims',return_value=(real,[],[])),patch.object(h,'current_head',return_value=self.ctx['head_sha']),patch.object(h,'gh',return_value={'id':101}) as api:
            h.publish(self.work);h.publish(self.work)
            self.assertEqual(api.call_count,1)
            self.assertEqual(api.call_args.args[1]['event'],'COMMENT')
            self.assertEqual(api.call_args.args[1]['comments'][0]['side'],'RIGHT')

    def test_missing_post_id_is_unknown_not_success(self):
        self.finished([self.claim(severity='MEDIUM')])
        with patch.object(h,'all_claims',return_value=(dict(self.ctx,offline_fixture=False),[],[])),patch.object(h,'current_head',return_value=self.ctx['head_sha']),patch.object(h,'gh',return_value={}):
            with self.assertRaises(h.Error): h.publish(self.work)
        self.assertEqual(next(iter(h.read(self.work/'publication-journal.json')['operations'].values()))['status'],'unknown')

    def test_matching_is_separate_and_uses_full_historical_comments(self):
        self.collected()
        # Use a new directory with the same frozen sources and fresh source results.
        # Remove no artifacts: build a second run from prepare.
        first=self.work;self.work=self.root/'other'
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(review.main(['prepare','--from-diff',str(self.diff),'--from-meta',str(self.meta),'--source-root',str(self.sources),'--work-dir',str(self.work)]),0)
        self.ctx=h.context(self.work);self.source_complete();req=h.collect_input(self.work)
        self.assertNotIn('existing_comments',req)
        h.validate_collect(self.work,dict(schema='toolkit.collect.v1',context_sha256=h.digest(self.ctx),checkpoint_sha256=req['checkpoint_sha256'],merges=[],exclusions=[],notes=[]))
        with patch.object(h,'all_claims',return_value=(dict(self.ctx,offline_fixture=False),[],[])),patch.object(h,'existing_comments',return_value=[{'id':7,'body':'Full historic explanation and condition','path':'x.rs','line':1}]):
            matching=h.match_input(self.work)
        self.assertEqual(matching['existing_comments'][0]['body'],'Full historic explanation and condition')
        verdict=dict(schema='toolkit.match.v1',context_sha256=h.digest(self.ctx),collect_sha256=matching['collect_sha256'],decisions=[dict(stable_id='async:0',verdict='duplicate',prior_comment_id=999,reason='same')])
        with self.assertRaises(h.Error): h.validate_match(self.work,verdict)
        verdict['decisions'][0]['prior_comment_id']=7;h.validate_match(self.work,verdict)
        self.assertEqual(h.format_input(self.work)['groups'],[])

    def test_matching_uncertain_is_not_a_suppressed_duplicate(self):
        self.source_complete();req=h.collect_input(self.work)
        h.validate_collect(self.work,dict(schema='toolkit.collect.v1',context_sha256=h.digest(self.ctx),checkpoint_sha256=req['checkpoint_sha256'],merges=[],exclusions=[],notes=[]))
        with patch.object(h,'all_claims',return_value=(dict(self.ctx,offline_fixture=False),[],[])),patch.object(h,'existing_comments',return_value=[{'id':7,'body':'Old comment'}]):
            req=h.match_input(self.work)
        h.validate_match(self.work,dict(schema='toolkit.match.v1',context_sha256=h.digest(self.ctx),collect_sha256=req['collect_sha256'],decisions=[dict(stable_id='async:0',verdict='uncertain',prior_comment_id=None,reason='Cannot establish equivalent consequence')]))
        with self.assertRaises(h.Error): h.format_input(self.work)

    def test_non_json_constants_and_duplicate_keys_rejected(self):
        for raw in ['{"status":"complete","status":"partial"}','{"notes":NaN}']:
            with self.subTest(raw=raw),self.assertRaises(h.Error): h.payload(raw)


class TestPinnedGit(unittest.TestCase):
    def setUp(self):
        isolated_env = patch.dict(os.environ, ghsource.git_environment(), clear=True)
        isolated_env.start()
        self.addCleanup(isolated_env.stop)

    def test_local_and_pr_prepare_use_actual_merge_base_and_immutable_diff(self):
        with tempfile.TemporaryDirectory(prefix='review-git-') as temp:
            repo=Path(temp);work=repo/'work'
            def git(*args):
                return subprocess.run(['git',*args],cwd=repo,capture_output=True,text=True,check=True).stdout.strip()
            def commit(msg):
                git('add','*.rs');git('-c','user.name=test','-c','user.email=test@test','-c','commit.gpgsign=false','commit','--no-verify','-qm',msg)
                return git('rev-parse','HEAD')
            git('init','-q','-b','main')
            (repo/'x.rs').write_text('base\n',encoding='utf-8');base=commit('base')
            git('switch','-qc','feature')
            (repo/'odd.txt').write_bytes(b'needle valid\nneedle invalid \xff\n')
            git('add','odd.txt')
            (repo/'x.rs').write_text('head\n',encoding='utf-8');head=commit('feature')
            git('switch','-q','main');(repo/'trunk.rs').write_text('trunk\n',encoding='utf-8');trunk=commit('trunk')
            git('switch','-q','feature')
            previous=Path.cwd()
            try:
                os.chdir(repo)
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(review.main(['prepare','--local','--branch','feature','--base','main','--repo','offline/repo','--work-dir',str(work)]),0)
                ctx=h.context(work)
                self.assertEqual(ctx['base_sha'],base);self.assertEqual(ctx['head_sha'],head)
                self.assertNotIn('trunk.rs',ctx['files'])
                (repo/'x.rs').write_text('later\n',encoding='utf-8');commit('later')
                self.assertNotEqual(h.current_head(ctx),head)
                self.assertEqual(h.materialize(work,ctx,'head','x.rs'),b'head\n')
                self.assertEqual(h.materialize(work,ctx,'base','x.rs'),b'base\n')
                output=io.StringIO()
                with contextlib.redirect_stdout(output):
                    self.assertEqual(review.main(['source','--work-dir',str(work),'--side','head','--search','head','--directory','x.rs']),0)
                receipt=json.loads(output.getvalue())
                self.assertEqual(receipt['revision'],head)
                self.assertTrue(receipt['matches']);self.assertFalse(receipt['truncated'])
                self.assertEqual(len(list((work/'searches').glob('*.json'))),1)
                output=io.StringIO()
                with contextlib.redirect_stdout(output):
                    self.assertEqual(review.main(['source','--work-dir',str(work),'--side','head','--search','needle']),0)
                matches=json.loads(output.getvalue())['matches']
                self.assertEqual(len(matches),2)
                self.assertIn('needle valid',matches[0])
                self.assertIn('invalid \ufffd',matches[1])
                with patch.object(review.ghsource,'fetch_pr_meta',return_value=dict(headRefOid=head,baseRefOid=trunk)),patch.object(review.ghsource,'pr_diff',side_effect=AssertionError('mutable diff forbidden')),contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(review.main(['prepare','--pr','1','--repo','offline/repo','--work-dir',str(repo/'prwork')]),0)
                self.assertEqual(h.context(repo/'prwork')['base_sha'],base)
                # Alter both registry hash and cache: Git object comparison still rejects.
                registry=h.read(work/'sources.json');(work/'sources/head/x.rs').write_text('fake\n',encoding='utf-8')
                registry['head:x.rs']['sha256']=h.file_hash(work/'sources/head/x.rs');h.save(work/'sources.json',registry)
                with self.assertRaises(h.Error): h.materialize(work,ctx,'head','x.rs')
            finally:
                os.chdir(previous)


class TestRunLock(unittest.TestCase):
    def test_contending_stage_waits_until_owner_releases(self):
        with tempfile.TemporaryDirectory(prefix='review-lock-') as temp:
            work=Path(temp);started=threading.Event();entered=threading.Event();errors=[]
            def contender():
                started.set()
                try:
                    with h.lock(work,timeout=2,poll_interval=0.01): entered.set()
                except Exception as exc:
                    errors.append(exc)
            with h.lock(work):
                worker=threading.Thread(target=contender)
                worker.start()
                self.assertTrue(started.wait(1))
                self.assertFalse(entered.wait(0.05))
            worker.join(3)
            self.assertFalse(worker.is_alive());self.assertEqual(errors,[])
            self.assertTrue(entered.is_set());self.assertFalse((work/'.harness.lock').exists())

    def test_timeout_does_not_steal_or_remove_owned_lock(self):
        with tempfile.TemporaryDirectory(prefix='review-lock-') as temp:
            work=Path(temp)
            with h.lock(work):
                before=(work/'.harness.lock').read_bytes()
                with self.assertRaisesRegex(h.Error,'timed out'):
                    with h.lock(work,timeout=0): self.fail('lock was stolen')
                self.assertEqual((work/'.harness.lock').read_bytes(),before)
            self.assertFalse((work/'.harness.lock').exists())

    def test_exception_releases_owned_lock(self):
        with tempfile.TemporaryDirectory(prefix='review-lock-') as temp:
            work=Path(temp)
            with self.assertRaisesRegex(RuntimeError,'failure'):
                with h.lock(work): raise RuntimeError('failure')
            with h.lock(work,timeout=0): self.assertTrue((work/'.harness.lock').is_file())


class TestGitEnvironment(unittest.TestCase):
    def test_only_git_repository_overrides_are_removed_without_mutating_caller(self):
        poison={key:'foreign' for key in ghsource.GIT_LOCATION_VARS}
        poison.update(GIT_CONFIG_KEY_0='core.worktree',GIT_CONFIG_VALUE_0='foreign',GIT_SSH_COMMAND='ssh -v')
        with patch.dict(os.environ,poison):
            clean=ghsource.git_environment()
            self.assertTrue(ghsource.GIT_LOCATION_VARS.isdisjoint(clean))
            self.assertNotIn('GIT_CONFIG_KEY_0',clean);self.assertNotIn('GIT_CONFIG_VALUE_0',clean)
            self.assertEqual(clean['GIT_SSH_COMMAND'],'ssh -v')
            self.assertEqual(os.environ['GIT_DIR'],'foreign')

    def test_production_git_readers_ignore_foreign_hook_environment(self):
        with tempfile.TemporaryDirectory(prefix='review-git-env-') as temp:
            parent=Path(temp);wanted=parent/'wanted';foreign=parent/'foreign'
            def git(repo,*args):
                return subprocess.run(['git',*args],cwd=repo,env=ghsource.git_environment(),capture_output=True,check=True).stdout
            for repo,marker in [(wanted,b'wanted\n'),(foreign,b'foreign\n')]:
                repo.mkdir();git(repo,'init','-q','-b','main');(repo/'marker.txt').write_bytes(marker)
                git(repo,'add','marker.txt');git(repo,'-c','user.name=test','-c','user.email=test@test','-c','commit.gpgsign=false','commit','--no-verify','-qm','fixture')
            before_head=git(foreign,'rev-parse','HEAD');before_index=(foreign/'.git/index').read_bytes()
            wanted_sha=git(wanted,'rev-parse','HEAD').decode().strip()
            poison={'GIT_DIR':str(foreign/'.git'),'GIT_WORK_TREE':str(foreign),'GIT_INDEX_FILE':str(foreign/'.git/index'),
                    'GIT_OBJECT_DIRECTORY':str(foreign/'.git/objects'),'GIT_COMMON_DIR':str(foreign/'.git'),
                    'GIT_CONFIG_COUNT':'1','GIT_CONFIG_KEY_0':'core.worktree','GIT_CONFIG_VALUE_0':str(foreign)}
            previous=Path.cwd()
            try:
                with patch.dict(os.environ,poison):
                    self.assertEqual(Path(h.git('rev-parse','--show-toplevel',root=wanted).stdout.decode().strip()).resolve(),wanted.resolve())
                    os.chdir(wanted)
                    self.assertEqual(Path(ghsource.run(['git','rev-parse','--show-toplevel']).strip()).resolve(),wanted.resolve())
                    self.assertTrue(ghsource.ensure_objects('offline/fixture',None,[wanted_sha]))
                    self.assertEqual(ghsource.read_blob_git('HEAD','marker.txt'),b'wanted\n')
                    self.assertEqual(ghsource.blob_size_git('HEAD','marker.txt'),7)
                    self.assertEqual(ghsource.local_base('main','main'),wanted_sha)
            finally:
                os.chdir(previous)
            self.assertEqual(git(foreign,'rev-parse','HEAD'),before_head)
            self.assertEqual((foreign/'.git/index').read_bytes(),before_index)


class TestRuleContracts(unittest.TestCase):
    def test_file_level_guidance_matches_required_null_fields(self):
        docs=h.DOCS
        for path in [docs/'agents/architecture.md',docs/'rules/security.md']:
            text=path.read_text(encoding='utf-8')
            self.assertNotIn('without `line`',text)
            self.assertNotIn('**no** `line` field',text)
            self.assertIn('`subject=file`, `line=null` and `side=null`',text)


if __name__=='__main__': unittest.main()
