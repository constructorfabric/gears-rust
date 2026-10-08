#!/usr/bin/env python3
"""S1-05 specification oracle, NOT a receiver/provider or concurrency test.

Frozen JSON expectations are read independently; this runner never rewrites them.
Tokens for authority/freshness stand for verified owner evidence in real adapters.
"""
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load(name):
    return json.loads((HERE / name).read_text())


class Receiver:
    """Serialized reference model; each step represents one atomic receiver boundary."""

    def __init__(self):
        self.grants = {}
        self.closed = set()
        self.tombstones = set()
        self.drafts = {}
        self.claims = {}
        self.limits = {}
        self.reconcile = set()

    def step(self, s):
        op = s['op']
        g = s.get('generation', 1)
        key = s.get('key', 'k1')
        if op == 'open':
            if not s['verified']:
                return 'denied'
            if g in self.closed:
                return 'closed'
            grant = (s['version'], tuple(s['roster']))
            if g in self.grants and self.grants[g] != grant:
                return 'conflict'
            self.grants[g] = grant
            return 'open'
        if op == 'close':
            self.closed.add(g)
            self.tombstones.update(s['roster'])
            return 'closed'
        if op == 'settle':
            if key in self.drafts:
                return 'draft_found'
            self.tombstones.add(key)
            return 'no_draft_tombstone'
        if op == 'limit':
            self.limits[s['scope']] = s['value']
            return 'updated'
        if op == 'timeout':
            return 'unresolved'
        if op == 'abort':
            if not s['definitive']:
                return 'unresolved'
            self.claims.pop(key, None)
            self.reconcile.discard(key)
            return 'released'
        grant = self.grants.get(g)
        if op in ('create', 'admit', 'confirm'):
            # Confirmation after close records actual external effects for recovery.
            if g in self.closed or key in self.tombstones:
                if op == 'confirm' and s['oss_applied']:
                    self.reconcile.add(key)
                return 'closed'
            if grant is None or s['version'] != grant[0] or key not in grant[1]:
                return 'not_authorized'
        if op == 'create':
            value = (g, s['payload'])
            if key in self.drafts:
                return 'replayed' if self.drafts[key] == value else 'conflict'
            self.drafts[key] = value
            return 'created'
        if op == 'admit':
            if key not in self.drafts or self.drafts[key][0] != g:
                return 'draft_missing'
            if s['writer'] not in WRITERS:
                return 'unknown_writer'
            if not s['provenance_valid']:
                return 'scope_unproven'
            if not s['pricing_fresh']:
                return 'commercial_unproven'
            if key in self.claims:
                return 'replayed'
            scope = s['scope']
            used = sum(c['scope'] == scope for c in self.claims.values())
            if used >= self.limits.get(scope, 1):
                return 'capacity_exhausted'
            self.claims[key] = {'scope': scope, 'state': 'pending', 'generation': g}
            return 'pending'
        if op == 'confirm':
            claim = self.claims.get(key)
            if claim is None or claim['generation'] != g:
                return 'intent_missing'
            if not s['oss_applied']:
                return 'unresolved'
            if not s['terms_compatible']:
                self.reconcile.add(key)
                return 'reconcile_terms'
            if claim['state'] == 'active':
                return 'replayed'
            claim['state'] = 'active'
            return 'active'
        raise AssertionError(f'unknown receiver operation {op}')

    def snapshot(self):
        return {'pending': sum(c['state'] == 'pending' for c in self.claims.values()),
                'active': sum(c['state'] == 'active' for c in self.claims.values()),
                'reconcile': sorted(self.reconcile)}


class Consumer:
    """Durable ledger abstraction: fresh reads per attempt, atomic effect + completion."""

    def __init__(self):
        self.complete = set()
        self.pending = set()
        self.effects = []
        self.reads = 0

    def step(self, s):
        if s['op'] == 'restart':
            return 'recovered'  # Only durable state exists in this abstraction.
        event = s['event_id']
        if event in self.complete:
            return 'duplicate'
        if not s['known_event']:
            self.pending.discard(event)
            self.complete.add(event)
            return 'ignored_unknown'
        if s['rule'] != 'effect_free':
            self.reads += 2  # Scoped get_version(event version), then current applicability.
            if s['version_read'] != 'ok' or s['current_read'] != 'ok':
                self.pending.add(event)
                return 'escalated_pending' if s.get('budget_exhausted') else 'pending'
            if not s['known_state']:
                self.pending.discard(event)
                self.complete.add(event)
                return 'ignored_unknown'
            if s['rule'] == 'current_version':
                if s['current_version'] != s['event_version']:
                    self.complete.add(event)
                    self.pending.discard(event)
                    return 'obsolete'
                if s['state'] == 'on_hold':
                    self.pending.add(event)
                    return 'deferred'
            elif s['rule'] != 'historical_financial':
                raise AssertionError('undeclared applicability rule')
        if s.get('crash_before_commit'):
            self.pending.add(event)
            return 'pending'
        self.effects.append(event)
        self.complete.add(event)
        self.pending.discard(event)
        return 'effect'

    def snapshot(self):
        return {'complete': sorted(self.complete), 'pending': sorted(self.pending),
                'effects': self.effects, 'reads': self.reads}


def rule(kind, x):
    if kind == 'failure_mapping':
        return {'confirmed_expiry': 'order-binding-expired',
                'explicit_closure': 'order-binding-expired',
                'temporary_end': 'order-binding-expired',
                'market_changed': 'market-divergence',
                'successor_price': 'not_expiry',
                'unavailable': 'unresolved', 'denied': 'unresolved',
                'mismatch': 'repair_or_compensated_line_failure', 'retired': 'repair_or_compensated_line_failure',
                'not_dispatchable': 'reread_without_ack'}[x['observation']]
    if kind == 'acknowledgement':
        reason = x['failure_reason']
        admitted = {'market-divergence', 'order-binding-expired', 'overlap-collision',
                    'identity-party-unavailable', 'overlap-presence-unevaluable',
                    'line-execution-failed', 'dependency-graph-invalid'}
        if (reason is not None and reason not in admitted) or (x['completed'] and reason is not None):
            return 'request-invalid'
        if not x['completed']:
            if reason is None:
                return 'failure-reason-missing'
            if reason == 'dependency-graph-invalid' and not x['historical_replay']:
                return 'must_not_emit'
            return 'validate_compensation'
        lines = x['lines']
        if (len(lines) != len(x['roster']) or {r['line'] for r in lines} != set(x['roster'])
                or any(r['status'] != 'activated' for r in lines)):
            return 'acknowledgement-lines-incomplete'
        if any(r['subscription'] is None for r in lines):
            return 'acknowledgement-subscription-missing'
        if len({r['subscription'] for r in lines}) != len(lines):
            return 'acknowledgement-subscription-duplicated'
        return 'completed'
    if kind == 'barrier':
        identity = ('control_id', 'version', 'attempt', 'generation', 'roster_digest', 'owner_fence')
        exact = all(x['expected'][k] == x['observed'][k] for k in identity)
        targets = x['observed']['targets']
        exact &= len(targets) == len(set(targets)) and set(targets) == set(x['expected']['targets'])
        exact &= x['durable_fences'] and x['reauthorized'] and x['owns_pending_pointer']
        exact &= not x['terminal'] or x['all_effects_settled']
        return 'finalize' if exact else 'pending_or_refuse'
    if kind == 'replacement':
        valid = (x['current_version'] == x['expected_version'] and x['authorized']
                 and not x['pending_control'] and x['predecessor_closed']
                 and x['all_old_keys_settled'] and x['same_commercial_roster']
                 and x['active_members_fixed'] and x['new_keys_only_rebuilt']
                 and x['state'] == 'in_fulfillment' and x['spawn_recorded']
                 and x['current_predecessor'] and x['version_committed']
                 and x['aggregate_generation'] >= x['previous_generation']
                 and x['generation'] == x['aggregate_generation'] + 1
                 and x['generation'] <= 9223372036854775807 and not x['spawn_reset'])
        return 'successor' if valid else 'refuse'
    if kind == 'forced_freshness':
        valid = (isinstance(x['observed'], dict)
                 and set(x['observed']) == {'audit_sequence', 'state', 'version'}
                 and x['observed'] == x['current']
                 and x['request_chain_sequence'] is None and x['request_prev_hash'] is None and x['requester'] != x['approver']
                 and x['both_authorized'] and 0 <= x['age_seconds'] < 86400
                 and x['request_is_refusal'] and x['revocation_durable']
                 and x['uncertain_claims_retained'])
        return 'forced_unknown' if valid else 'refuse'
    if kind == 'compensation':
        e = x['evidence']
        if e is None:
            return 'compensation-evidence-missing'
        keys = {'drafts_voided', 'activated_rolled_back', 'activation_dispatched',
                'at_sale_facts_emitted', 'no_active_subscription_remains'}
        valid = isinstance(e, dict) and set(e) == keys
        if valid:
            valid = all(isinstance(e[k], list) and all(isinstance(v, str) for v in e[k])
                        for k in ('drafts_voided', 'activated_rolled_back'))
            valid &= all(type(e[k]) is bool for k in keys - {'drafts_voided', 'activated_rolled_back'})
            valid &= e['no_active_subscription_remains'] is True
        return 'ordinary_complete' if valid else 'compensation-evidence-incomplete'
    if kind == 'release':
        reports = x['reports']
        valid = (x['enabled'] and x['pricing_authority'] and x['closure_durable']
                 and set(reports) == set(x['writers']) and x['handoff_durable'])
        valid &= all(r['status'] == 'ok' and r['generation'] == x['generation']
                     and r['closed'] and r['drained'] and r['holders'] == 0
                     and r['uncertain'] == 0 for r in reports.values())
        return 'eligible_for_pricing_release' if valid else 'retain'
    if kind == 'payment':
        if not x['identity_matches'] or not x['basis_explicit']:
            return 'invalid_evidence'
        return {'authorized': 'satisfied', 'failed': 'evaluate_lifecycle_tolerance',
                'pending': 'pending', 'unavailable': 'pending', 'denied': 'pending',
                'missing': 'pending'}[x['outcome']]
    if kind == 'consent':
        if x['live_requirement'] == 'unavailable':
            return 'acceptance-requirement-unevaluable'
        if x['live_requirement'] == 'not_required':
            return 'satisfied'
        return 'satisfied' if (x['version_matches'] and x['authorized_party']
                               and x['customer_consent']) else 'consent_required'
    if kind == 'outcome':
        return ('activated' if x['expected'] == x['observed'] and x['status'] == 'applied'
                else 'not_complete')
    if kind == 'handover':
        return ('admit_linked_successor' if x['predecessor_ended'] and x['same_scope']
                and x['unique_successor'] and x['durable_link'] else 'no_exemption')
    if kind == 'clock':
        if (not x['terms_compatible'] or not x['profile_supported']
                or x['held_activation_at'] != x['requested_activation_at']
                or x['status'] != 'applied' or not x['authoritative_outcome']):
            return 'reconcile_without_rewriting_anchor'
        return {'service_effective_at': x['applied_at'], 'quoted_at': x['quoted_at'],
                'intent_at': x['intent_at'], 'acceptance_at': x['acceptance_at'],
                'held_activation_at': x['held_activation_at']}
    raise AssertionError(f'unknown rule {kind}')


WRITERS = set(load('contracts.json')['receiver_writer_inventory'])


def main():
    manifest = load('contracts.json')
    upstream = (HERE.parent.parent / 'UPSTREAM_REQS.md').read_text()
    ids = [o['id'] for o in manifest['operations']]
    assert len(ids) == len(set(ids)) == 19
    assert manifest['production_release'] is False
    for o in manifest['operations']:
        assert all(o[k] for k in ('owner', 'package', 'status', 'kind', 'request', 'result', 'authority', 'retry', 'failures', 'source', 'schema_version'))
        if o['id'] == 'replace-fulfillment-grant':
            path, anchor = o['source'].split('#')
            assert f'id="{anchor}"' in (HERE.parent.parent / path).read_text()
        else:
            assert o['source'] in upstream
    assert manifest['legal_transitions']['receiver_ledger']['settled'] == []
    count = 0
    for filename, cls in [('receiver-traces.json', Receiver), ('orders-events.json', Consumer)]:
        cases = load(filename)['cases']
        assert len({c['id'] for c in cases}) == len(cases)
        for case in cases:
            model = cls()
            for index, step in enumerate(case['steps']):
                actual = model.step(step)
                assert actual == step['expect'], (case['id'], index, actual, step['expect'])
            assert model.snapshot() == case['expected'], (case['id'], model.snapshot(), case['expected'])
            count += 1
    event_ids = {c['id'] for c in load('orders-events.json')['cases']}
    required = {'duplicate-delivery', 'duplicate-republished', 'gap', 'out-of-order',
                'stale-version', 'hold-defers', 'unknown-state', 'unknown-event-type',
                'read-unavailable', 'read-denied', 'recovered-dead-letter-after-newer-state',
                'restart-while-pending', 'effect-free-on-payload'}
    assert required <= event_ids
    cases = load('boundary-cases.json')['cases']
    assert len({c['id'] for c in cases}) == len(cases)
    for case in cases:
        actual = rule(case['rule'], case['input'])
        assert actual == case['expected'], (case['id'], actual, case['expected'])
        count += 1
    for writer in WRITERS:
        assert any(c['id'] == f'{writer}-shares-capacity' for c in load('receiver-traces.json')['cases'])
    print(f'PASS: {len(ids)} proposed operations; {count} reference-model cases; required 13-event corpus present.')
    print('Not provider, database race, authenticated SDK or production conformance evidence.')


if __name__ == '__main__':
    main()
