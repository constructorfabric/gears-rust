-- No security-definer bypasses: the invoking role and its table privileges remain in force.
CREATE FUNCTION bss_orders__immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'Orders evidence is immutable' USING ERRCODE='42501'; END $$;

CREATE FUNCTION bss_orders__update_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE allowed text[] := string_to_array(TG_ARGV[0], ',');
BEGIN
    IF (to_jsonb(OLD) - allowed) IS DISTINCT FROM (to_jsonb(NEW) - allowed) THEN
        RAISE EXCEPTION 'immutable Orders input' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION bss_orders__aggregate_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.current_version < OLD.current_version OR NEW.version_allocation_high_water < OLD.version_allocation_high_water
       OR NEW.draft_revision < OLD.draft_revision OR NEW.resume_count < OLD.resume_count
       OR NEW.amendment_count < OLD.amendment_count OR NEW.audit_sequence < OLD.audit_sequence
       OR NEW.fulfillment_control_generation < OLD.fulfillment_control_generation THEN
        RAISE EXCEPTION 'Orders counter cannot decrease' USING ERRCODE='23514';
    END IF;
    IF (OLD.spawn_signal_at IS NOT NULL AND NEW.spawn_signal_at IS DISTINCT FROM OLD.spawn_signal_at)
       OR (OLD.authorization_failure_tolerated_at IS NOT NULL AND NEW.authorization_failure_tolerated_at IS DISTINCT FROM OLD.authorization_failure_tolerated_at)
       OR (OLD.state <> 'draft' AND NEW.resource_tenant_id <> OLD.resource_tenant_id) THEN
        RAISE EXCEPTION 'immutable Orders fact' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION bss_orders__claim_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.released_at IS NOT NULL OR NEW.released_at IS NULL THEN
        RAISE EXCEPTION 'claim may only be released once' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION bss_orders__working_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE target uuid; s text;
BEGIN
    IF TG_OP='DELETE' THEN target := OLD.order_id; ELSE target := NEW.order_id; END IF;
    SELECT state INTO STRICT s FROM bss_orders__order WHERE order_id=target FOR UPDATE;
    IF TG_ARGV[0]='draft' THEN
        -- Final submit removes membership in the same transaction after appending the snapshot.
        IF s <> 'draft' AND NOT (TG_OP='DELETE' AND s='submitted') THEN
            RAISE EXCEPTION 'draft membership is closed' USING ERRCODE='23514';
        END IF;
    ELSIF s IN ('completed','rejected','cancelled','fulfillment_failed','expired') THEN
        RAISE EXCEPTION 'administrative content is terminal' USING ERRCODE='23514';
    END IF;
    IF TG_OP='DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION bss_orders__retention_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT pg_has_role(current_user,'bss_orders_retention','MEMBER') THEN
        RAISE EXCEPTION 'Orders retention role required' USING ERRCODE='42501';
    END IF;
    IF TG_TABLE_NAME='bss_orders__transition_audit' THEN
        IF OLD.outcome='refused' AND OLD.created_at < clock_timestamp()-interval '90 days' THEN RETURN OLD; END IF;
    ELSIF TG_TABLE_NAME='bss_orders__gate_outcome' THEN
        IF OLD.order_id IS NULL AND OLD.evaluated_at < clock_timestamp()-interval '7 days' THEN RETURN OLD; END IF;
    ELSIF TG_TABLE_NAME='bss_orders__read_access_log' THEN
        IF OLD.accessed_at < clock_timestamp()-interval '90 days' THEN RETURN OLD; END IF;
    END IF;
    RAISE EXCEPTION 'Orders retention window or evidence class forbids deletion' USING ERRCODE='42501';
END $$;

CREATE FUNCTION bss_orders__registry_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN
        IF OLD.expires_at > clock_timestamp() OR (OLD.status='in_flight' AND OLD.lease_expires_at > clock_timestamp()) THEN
            RAISE EXCEPTION 'live idempotency generation' USING ERRCODE='23514';
        END IF;
        IF EXISTS(SELECT 1 FROM bss_orders__commercial_attempt WHERE attempt_id=OLD.attempt_id AND status IN ('prepared','running'))
           OR EXISTS(SELECT 1 FROM bss_orders__fulfillment_control WHERE control_id=OLD.fulfillment_control_id AND status IN ('prepared','awaiting','barrier_ready')) THEN
            RAISE EXCEPTION 'unresolved execution must survive retention' USING ERRCODE='23514';
        END IF;
        RETURN OLD;
    END IF;
    IF OLD.status='settled' OR NEW.fencing_generation<OLD.fencing_generation
       OR (OLD.attempt_id IS NOT NULL AND NEW.attempt_id IS DISTINCT FROM OLD.attempt_id)
       OR (OLD.fulfillment_control_id IS NOT NULL AND NEW.fulfillment_control_id IS DISTINCT FROM OLD.fulfillment_control_id)
       OR (OLD.order_id IS NOT NULL AND NEW.order_id IS DISTINCT FROM OLD.order_id)
       OR (NEW.owner_token IS DISTINCT FROM OLD.owner_token AND NEW.fencing_generation<=OLD.fencing_generation) THEN
        RAISE EXCEPTION 'immutable registry settlement or stale fence' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION bss_orders__execution_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_results jsonb; new_results jsonb;
BEGIN
    IF OLD.status IN ('committed','settled','refused','abandoned') OR NEW.fencing_generation<OLD.fencing_generation
       OR (NEW.owner_token<>OLD.owner_token AND NEW.fencing_generation<=OLD.fencing_generation) THEN
        RAISE EXCEPTION 'terminal execution or stale ownership fence' USING ERRCODE='23514';
    END IF;
    IF TG_TABLE_NAME='bss_orders__commercial_attempt' THEN
        old_results := OLD.receipt_results; new_results := NEW.receipt_results;
        IF OLD.status='running' AND NEW.status='prepared' THEN RAISE EXCEPTION 'execution cannot regress' USING ERRCODE='23514'; END IF;
    ELSE
        old_results := OLD.receiver_evidence; new_results := NEW.receiver_evidence;
        IF EXISTS(SELECT 1 FROM jsonb_each(OLD.receiver_commands) e WHERE NEW.receiver_commands->e.key IS DISTINCT FROM e.value) THEN RAISE EXCEPTION 'command identity changed' USING ERRCODE='23514'; END IF;
        IF (OLD.status='awaiting' AND NEW.status='prepared') OR (OLD.status='barrier_ready' AND NEW.status IN ('prepared','awaiting')) THEN RAISE EXCEPTION 'control cannot regress' USING ERRCODE='23514'; END IF;
    END IF;
    -- Top-level first results are immutable, including arrays and nested objects; @> alone
    -- would permit adding fields inside an already recorded receipt.
    IF EXISTS(SELECT 1 FROM jsonb_each(old_results) e WHERE new_results->e.key IS DISTINCT FROM e.value) THEN
        RAISE EXCEPTION 'first result cannot be rewritten' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION bss_orders__observation_valid(j jsonb,s text,v integer) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
    IF j IS NULL THEN RETURN true; END IF;
    IF jsonb_typeof(j)<>'object' OR NOT j ?& ARRAY['audit_sequence','state','version']
       OR j-ARRAY['audit_sequence','state','version'] <> '{}'::jsonb
       OR jsonb_typeof(j->'audit_sequence')<>'number' OR jsonb_typeof(j->'version')<>'number'
       OR jsonb_typeof(j->'state')<>'string' THEN RETURN false; END IF;
    RETURN COALESCE((j->>'audit_sequence') ~ '^[1-9][0-9]*$' AND (j->>'audit_sequence')::numeric <= 9223372036854775807
       AND (j->>'version') ~ '^[1-9][0-9]*$' AND (j->>'version')::numeric<=2147483647
       AND j->>'state'=s AND (j->>'version')::numeric=v, false);
EXCEPTION WHEN OTHERS THEN RETURN false;
END $$;
ALTER TABLE bss_orders__transition_audit ADD CONSTRAINT ck_bss_orders__transition_audit__observation CHECK (
    (force_request_observation IS NULL OR (hash_version=3 AND outcome='refused' AND order_id IS NOT NULL
     AND trigger='force-fail-unreconciled' AND reason='second-approver-required'
     AND from_state IS NOT NULL AND to_state=from_state AND version IS NOT NULL
     AND bss_orders__observation_valid(force_request_observation,from_state,version)))
    AND (hash_version<>3 OR outcome<>'refused' OR trigger<>'force-fail-unreconciled' OR reason<>'second-approver-required' OR force_request_observation IS NOT NULL));

CREATE FUNCTION bss_orders__audit_writer() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT (SELECT rolsuper FROM pg_roles WHERE rolname=current_user) AND (pg_has_role(current_user,'bss_orders_runtime','MEMBER') OR pg_has_role(current_user,'bss_orders_private','MEMBER')) THEN
        IF NEW.hash_version<>3 THEN RAISE EXCEPTION 'new audit writers require v3' USING ERRCODE='23514'; END IF;
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION bss_orders__grant_link() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE a bss_orders__transition_audit; p bss_orders__fulfillment_grant;
BEGIN
    SELECT * INTO STRICT a FROM bss_orders__transition_audit WHERE audit_id=NEW.audit_id;
    IF a.outcome<>'committed' OR a.order_id<>NEW.order_id OR a.version<>NEW.order_version
       OR a.trigger NOT IN ('report-spawn-signal','resume','replace-fulfillment-grant')
       OR a.to_state<>'in_fulfillment' THEN
        RAISE EXCEPTION 'grant requires its originating committed writer audit' USING ERRCODE='23514';
    END IF;
    IF NEW.predecessor_grant_id IS NOT NULL THEN
        SELECT * INTO STRICT p FROM bss_orders__fulfillment_grant WHERE grant_id=NEW.predecessor_grant_id;
        IF p.order_id<>NEW.order_id OR p.generation>=NEW.generation THEN
            RAISE EXCEPTION 'invalid grant predecessor' USING ERRCODE='23514';
        END IF;
        -- D-198/D-201: a successor replaces the current grant only; no fork or skipped head.
        IF EXISTS(SELECT 1 FROM bss_orders__fulfillment_grant g WHERE g.order_id=NEW.order_id
                  AND g.grant_id<>NEW.grant_id AND g.generation>p.generation) THEN
            RAISE EXCEPTION 'grant predecessor is not the current grant' USING ERRCODE='23514';
        END IF;
    ELSIF a.trigger<>'report-spawn-signal' THEN
        RAISE EXCEPTION 'successor grant requires predecessor' USING ERRCODE='23514';
    ELSIF EXISTS(SELECT 1 FROM bss_orders__fulfillment_grant g WHERE g.order_id=NEW.order_id AND g.grant_id<>NEW.grant_id) THEN
        RAISE EXCEPTION 'first dispatch grant already issued' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;
CREATE CONSTRAINT TRIGGER bss_orders__grant_audit AFTER INSERT ON bss_orders__fulfillment_grant
DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__grant_link();

CREATE FUNCTION bss_orders__total_line_link() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.scope='line' AND NOT EXISTS(SELECT 1 FROM bss_orders__order_line_identity WHERE order_id=NEW.order_id AND line_id=NEW.line_id) THEN
        RAISE EXCEPTION 'missing line identity for total' USING ERRCODE='23503';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER bss_orders__total_line BEFORE INSERT ON bss_orders__resolved_total FOR EACH ROW EXECUTE FUNCTION bss_orders__total_line_link();

CREATE FUNCTION bss_orders__policy_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN
        IF TG_TABLE_NAME='bss_orders__date_policy' THEN
            IF OLD.resource_tenant_id IS NULL THEN RAISE EXCEPTION 'platform date policy is permanent' USING ERRCODE='42501'; END IF;
        ELSIF OLD.scope='platform' THEN
            RAISE EXCEPTION 'platform TTL row is permanent' USING ERRCODE='42501';
        END IF;
        RETURN OLD;
    END IF;
    IF TG_TABLE_NAME='bss_orders__date_policy' THEN
        IF NEW.revision<=OLD.revision THEN RAISE EXCEPTION 'date revision must advance' USING ERRCODE='23514'; END IF;
    ELSIF NEW.policy_revision<=OLD.policy_revision THEN
        RAISE EXCEPTION 'TTL revision must advance' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;

-- The internal continuation is an audit writer, not a public state-machine trigger.
ALTER TABLE bss_orders__transition_audit ADD CONSTRAINT ck_bss_orders__transition_audit__tokens CHECK (
 outcome<>'committed' OR trigger IN ('create','draft-mutate','administrative-edit','submit','cancel','auto-void','reflect-approval-required','reflect-approval-not-required','reflect-approval-granted','reflect-approval-denied','begin-fulfillment','report-spawn-signal','acknowledge-completed','acknowledge-failed','cancel-workflow-mediated','amendment','hold','resume','expire','record-acceptance','force-fail-unreconciled','replace-fulfillment-grant'));
ALTER TABLE bss_orders__transition_audit ADD CONSTRAINT ck_bss_orders__transition_audit__actor CHECK (actor ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$');
ALTER TABLE bss_orders__read_access_log ADD CONSTRAINT ck_bss_orders__read_access_log__actor CHECK (actor ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$');
ALTER TABLE bss_orders__draft_content ADD CONSTRAINT ck_bss_orders__draft_content__term CHECK (term_duration > interval '0');

-- D-193: a tagged authored intent preserves rolling vs missing and calendar units.
CREATE FUNCTION bss_orders__term_valid(k text, d interval, j jsonb, cycle text) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE expected interval;
BEGIN
    IF jsonb_typeof(j)<>'object' THEN RETURN false; END IF;
    IF k IN ('missing','rolling') THEN RETURN d IS NULL AND j=jsonb_build_object('kind',k); END IF;
    -- A draft may leave the cycle unauthored (02 §4.1): a period count then keeps its authored
    -- intent with no interval, which is derived once the cycle is authored.
    IF k='finite' AND cycle IS NULL AND j->>'kind'='periods' THEN
        RETURN d IS NULL AND j-ARRAY['kind','count']='{}'::jsonb AND j ? 'count'
          AND jsonb_typeof(j->'count')='number' AND (j->>'count') ~ '^[1-9][0-9]*$' AND (j->>'count')::numeric<=4294967295;
    END IF;
    IF k<>'finite' OR d IS NULL OR d<=interval '0' THEN RETURN false; END IF;
    IF j->>'kind'='periods' AND j-ARRAY['kind','count']='{}'::jsonb AND j ? 'count'
       AND jsonb_typeof(j->'count')='number' AND (j->>'count') ~ '^[1-9][0-9]*$' AND (j->>'count')::numeric<=4294967295 THEN
        expected := ((j->>'count') || CASE WHEN cycle='month' THEN ' months' ELSE ' years' END)::interval;
    ELSIF j->>'kind'='calendar' AND j ?& ARRAY['kind','years','months','days','microseconds']
       AND j-ARRAY['kind','years','months','days','microseconds']='{}'::jsonb THEN
        IF EXISTS(SELECT 1 FROM jsonb_each(j-'kind') e WHERE jsonb_typeof(e.value)<>'number' OR e.value::text !~ '^[0-9]+$') THEN RETURN false; END IF;
        expected := make_interval(years=>(j->>'years')::integer, months=>(j->>'months')::integer, days=>(j->>'days')::integer)
          + ((j->>'microseconds') || ' microseconds')::interval;
    ELSE RETURN false;
    END IF;
    -- Interval '=' treats 1 month and 30 days as equal. Compare each stored component.
    RETURN extract(year FROM d)=extract(year FROM expected) AND extract(month FROM d)=extract(month FROM expected)
       AND extract(day FROM d)=extract(day FROM expected) AND extract(hour FROM d)=extract(hour FROM expected)
       AND extract(minute FROM d)=extract(minute FROM expected) AND extract(second FROM d)=extract(second FROM expected);
EXCEPTION WHEN OTHERS THEN RETURN false;
END $$;
ALTER TABLE bss_orders__order_line ADD CONSTRAINT ck_bss_orders__order_line__term_intent CHECK (bss_orders__term_valid(term_kind,term_duration,authored_term,billing_cycle));
ALTER TABLE bss_orders__draft_content ADD CONSTRAINT ck_bss_orders__draft_content__term_intent CHECK (bss_orders__term_valid(term_kind,term_duration,authored_term,billing_cycle));

-- 02 §4.2 (D-60, D-121, S2-10): an admitted line stores all three resolved dates and the exact
-- effective-policy snapshot: both switches, the source scope and row identity, and its revision.
CREATE FUNCTION bss_orders__date_snapshot_valid(j jsonb) RETURNS boolean LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
    IF jsonb_typeof(j)<>'object'
       OR NOT j ?& ARRAY['service_activation_required','acceptance_due_required','scope','resource_tenant_id','policy_id','revision']
       OR j-ARRAY['service_activation_required','acceptance_due_required','scope','resource_tenant_id','policy_id','revision']<>'{}'::jsonb
       OR jsonb_typeof(j->'service_activation_required')<>'boolean' OR jsonb_typeof(j->'acceptance_due_required')<>'boolean'
       OR jsonb_typeof(j->'policy_id')<>'string' OR jsonb_typeof(j->'revision')<>'number'
       OR (j->>'revision') !~ '^[1-9][0-9]*$' OR (j->>'revision')::numeric>9223372036854775807 THEN
        RETURN false;
    END IF;
    RETURN COALESCE(CASE j->>'scope'
        WHEN 'platform_default' THEN j->'resource_tenant_id'='null'::jsonb
            AND (j->>'policy_id')::uuid='00000000-0000-0000-0000-000000000121'::uuid
        WHEN 'resource_tenant' THEN jsonb_typeof(j->'resource_tenant_id')='string'
            AND (j->>'resource_tenant_id')::uuid<>'00000000-0000-0000-0000-000000000000'::uuid
            AND (j->>'policy_id')::uuid<>'00000000-0000-0000-0000-000000000121'::uuid
        ELSE false END, false);
EXCEPTION WHEN OTHERS THEN RETURN false;
END $$;
ALTER TABLE bss_orders__order_line ADD CONSTRAINT ck_bss_orders__order_line__dates CHECK (
 service_activation_date IS NOT NULL AND acceptance_due_date IS NOT NULL AND bss_orders__date_snapshot_valid(date_policy_switch_state));

CREATE FUNCTION bss_orders__compensation_valid(j jsonb) RETURNS boolean LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE a jsonb; item jsonb;
BEGIN
    IF j IS NULL THEN RETURN true; END IF;
    IF jsonb_typeof(j)<>'object' OR NOT j ?& ARRAY['drafts_voided','activated_rolled_back','activation_dispatched','at_sale_facts_emitted','no_active_subscription_remains']
       OR jsonb_typeof(j->'drafts_voided')<>'array' OR jsonb_typeof(j->'activated_rolled_back')<>'array'
       OR jsonb_typeof(j->'activation_dispatched')<>'boolean' THEN RETURN false; END IF;
    FOR item IN SELECT value FROM jsonb_array_elements((j->'drafts_voided') || (j->'activated_rolled_back')) LOOP
        IF jsonb_typeof(item)<>'string' OR (item #>> '{}') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN RETURN false; END IF;
    END LOOP;
    IF NOT j ? 'operator_attestation' THEN
        RETURN j-ARRAY['drafts_voided','activated_rolled_back','activation_dispatched','at_sale_facts_emitted','no_active_subscription_remains']='{}'::jsonb
          AND jsonb_typeof(j->'at_sale_facts_emitted')='boolean' AND jsonb_typeof(j->'no_active_subscription_remains')='boolean';
    END IF;
    IF j-ARRAY['drafts_voided','activated_rolled_back','activation_dispatched','at_sale_facts_emitted','no_active_subscription_remains','operator_attestation']<>'{}'::jsonb
       OR j->'activation_dispatched'<>'true'::jsonb OR j->'at_sale_facts_emitted'<>'"unknown"'::jsonb
       OR j->'no_active_subscription_remains'<>'"unknown"'::jsonb THEN RETURN false; END IF;
    a:=j->'operator_attestation';
    IF jsonb_typeof(a)<>'object' OR NOT a ?& ARRAY['requested_by','request_audit_id','requested_at','approved_by']
       OR a-ARRAY['requested_by','request_audit_id','requested_at','approved_by']<>'{}'::jsonb THEN RETURN false; END IF;
    FOR item IN SELECT value FROM jsonb_each(a-'requested_at') LOOP
        IF jsonb_typeof(item)<>'string' OR (item #>> '{}') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN RETURN false; END IF;
    END LOOP;
    RETURN COALESCE(jsonb_typeof(a->'requested_at')='string' AND (a->>'requested_at')::timestamptz IS NOT NULL AND a->>'requested_by'<>a->>'approved_by',false);
EXCEPTION WHEN OTHERS THEN RETURN false;
END $$;
ALTER TABLE bss_orders__order ADD CONSTRAINT ck_bss_orders__order__compensation CHECK (bss_orders__compensation_valid(compensation_evidence) IS TRUE);

-- SQL NULL cannot satisfy any required closed object member.
ALTER TABLE bss_orders__idempotency ADD CONSTRAINT ck_bss_orders__idempotency__response CHECK (
 status<>'settled' OR (jsonb_typeof(settled_response)='object' AND settled_response ? 'formatVersion' AND (settled_response->'formatVersion'='1'::jsonb) IS TRUE));

-- A registry replay snapshot can outlive a refused audit's 90-day retention. Validate
-- its origin at settlement, then retain the immutable reference without a lifetime FK.
ALTER TABLE bss_orders__idempotency DROP CONSTRAINT fk_bss_orders__idempotency__3;
CREATE FUNCTION bss_orders__settlement_link() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE a bss_orders__transition_audit;
BEGIN
    IF NEW.status<>'settled' THEN RETURN NEW; END IF;
    SELECT * INTO a FROM bss_orders__transition_audit WHERE audit_id=NEW.audit_id;
    IF NOT FOUND OR (NEW.outcome='success' AND a.outcome<>'committed') OR (NEW.outcome='refused' AND a.outcome<>'refused')
       OR a.order_id IS DISTINCT FROM NEW.order_id THEN
        RAISE EXCEPTION 'settlement requires its originating audit' USING ERRCODE='23503';
    END IF;
    RETURN NEW;
END $$;
CREATE CONSTRAINT TRIGGER bss_orders__settlement_audit AFTER INSERT OR UPDATE ON bss_orders__idempotency
DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__settlement_link();

ALTER TABLE bss_orders__commercial_attempt ADD CONSTRAINT uq_bss_orders__commercial_attempt__execution UNIQUE(attempt_id,idempotency_execution_id,order_id);
ALTER TABLE bss_orders__fulfillment_control ADD CONSTRAINT uq_bss_orders__fulfillment_control__execution UNIQUE(control_id,idempotency_execution_id,order_id);
ALTER TABLE bss_orders__idempotency ADD CONSTRAINT fk_bss_orders__idempotency__attempt_execution FOREIGN KEY(attempt_id,execution_id,order_id) REFERENCES bss_orders__commercial_attempt(attempt_id,idempotency_execution_id,order_id);
ALTER TABLE bss_orders__idempotency ADD CONSTRAINT fk_bss_orders__idempotency__control_execution FOREIGN KEY(fulfillment_control_id,execution_id,order_id) REFERENCES bss_orders__fulfillment_control(control_id,idempotency_execution_id,order_id);

-- Permanent platform policies serialize namespace changes. Date revision allocation uses
-- that permanent row as a high-water mark, so deleting/recreating an override cannot reset it.
CREATE FUNCTION bss_orders__policy_namespace() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE r bigint; s text; is_default boolean;
BEGIN
    IF TG_TABLE_NAME='bss_orders__date_policy' THEN
        IF TG_OP='DELETE' THEN is_default:=OLD.resource_tenant_id IS NULL; ELSE is_default:=NEW.resource_tenant_id IS NULL; END IF;
        IF is_default THEN IF TG_OP='DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF; END IF;
        SELECT revision INTO STRICT r FROM bss_orders__date_policy WHERE resource_tenant_id IS NULL FOR UPDATE;
        IF TG_OP<>'DELETE' THEN
            IF NEW.revision<=r THEN RAISE EXCEPTION 'date override revision must exceed namespace high water' USING ERRCODE='23514'; END IF;
            r:=NEW.revision;
        END IF;
        UPDATE bss_orders__date_policy SET revision=r+1,updated_at=clock_timestamp() WHERE resource_tenant_id IS NULL;
    ELSE
        IF TG_OP='DELETE' THEN is_default:=OLD.scope='platform';s:=OLD.state; ELSE is_default:=NEW.scope='platform';s:=NEW.state; END IF;
        IF is_default THEN IF TG_OP='DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF; END IF;
        SELECT policy_revision INTO STRICT r FROM bss_orders__state_ttl_policy WHERE scope='platform' AND state=s FOR UPDATE;
        UPDATE bss_orders__state_ttl_policy SET policy_revision=r+1,updated_at=clock_timestamp() WHERE scope='platform' AND state=s;
    END IF;
    IF TG_OP='DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END $$;
CREATE TRIGGER bss_orders__date_namespace BEFORE INSERT OR UPDATE OR DELETE ON bss_orders__date_policy FOR EACH ROW EXECUTE FUNCTION bss_orders__policy_namespace();
CREATE TRIGGER bss_orders__ttl_namespace BEFORE INSERT OR UPDATE OR DELETE ON bss_orders__state_ttl_policy FOR EACH ROW EXECUTE FUNCTION bss_orders__policy_namespace();
ALTER TABLE bss_orders__transition_audit ADD CONSTRAINT ck_bss_orders__transition_audit__admin CHECK (
 (outcome='committed' AND trigger='administrative-edit' AND changed_field IS NOT NULL AND prior_value IS DISTINCT FROM new_value)
 OR ((outcome<>'committed' OR trigger<>'administrative-edit') AND changed_field IS NULL AND prior_value IS NULL AND new_value IS NULL));

CREATE FUNCTION bss_orders__grant_source() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE o bss_orders__order;
BEGIN
 SELECT * INTO STRICT o FROM bss_orders__order WHERE order_id=NEW.order_id;
 IF o.current_version<>NEW.order_version OR o.state<>'in_fulfillment' OR o.spawn_signal_at IS NULL
    OR o.fulfillment_control_pending IS NOT NULL OR o.fulfillment_control_generation<>NEW.generation THEN
    RAISE EXCEPTION 'grant requires current admitted fulfillment source' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE CONSTRAINT TRIGGER bss_orders__grant_source AFTER INSERT ON bss_orders__fulfillment_grant
DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__grant_source();
ALTER TABLE bss_orders__transition_audit ADD CONSTRAINT ck_bss_orders__transition_audit__resolved CHECK (
 (trigger='create' OR requested_order_ref IS NOT NULL) AND
 (outcome<>'refused' OR order_id IS NULL OR (audit_tenant_id IS NOT NULL AND resource_tenant_id IS NOT NULL AND from_state IS NOT NULL AND to_state IS NOT NULL AND from_state=to_state AND version IS NOT NULL AND version>0)));

CREATE FUNCTION bss_orders__checkpoint_complete() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE expected bigint; actual bigint;
BEGIN
 SELECT member_count INTO STRICT expected FROM bss_orders__audit_checkpoint WHERE audit_tenant_id=NEW.audit_tenant_id AND checkpoint_sequence=NEW.checkpoint_sequence;
 SELECT count(*) INTO actual FROM bss_orders__audit_checkpoint_member WHERE audit_tenant_id=NEW.audit_tenant_id AND checkpoint_sequence=NEW.checkpoint_sequence;
 IF expected<>actual THEN RAISE EXCEPTION 'checkpoint header/member cardinality mismatch' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END $$;
CREATE CONSTRAINT TRIGGER bss_orders__checkpoint_complete AFTER INSERT ON bss_orders__audit_checkpoint DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__checkpoint_complete();
CREATE CONSTRAINT TRIGGER bss_orders__checkpoint_member_complete AFTER INSERT ON bss_orders__audit_checkpoint_member DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__checkpoint_complete();

CREATE FUNCTION bss_orders__execution_link() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE marker bss_orders__idempotency; a bss_orders__commercial_attempt; c bss_orders__fulfillment_control; execution uuid;
BEGIN
 IF TG_TABLE_NAME='bss_orders__idempotency' THEN execution:=NEW.execution_id;
 ELSE execution:=NEW.idempotency_execution_id; END IF;
 SELECT * INTO marker FROM bss_orders__idempotency WHERE execution_id=execution;
 IF NOT FOUND THEN RAISE EXCEPTION 'execution requires its owning registry generation' USING ERRCODE='23503'; END IF;
 IF marker.attempt_id IS NOT NULL THEN
   SELECT * INTO STRICT a FROM bss_orders__commercial_attempt WHERE attempt_id=marker.attempt_id;
   IF a.idempotency_execution_id<>marker.execution_id OR a.order_id IS DISTINCT FROM marker.order_id
     OR a.operation<>marker.operation OR a.principal_scope<>marker.principal_scope OR a.request_fingerprint<>marker.request_fingerprint
     OR a.owner_token IS DISTINCT FROM marker.owner_token OR a.fencing_generation<>marker.fencing_generation THEN
      RAISE EXCEPTION 'commercial execution identity/fence mismatch' USING ERRCODE='23514';
   END IF;
   IF (a.status IN ('prepared','running')) IS DISTINCT FROM (marker.status='in_flight') THEN
      RAISE EXCEPTION 'commercial execution settlement mismatch' USING ERRCODE='23514';
   END IF;
   IF a.status='committed' AND (marker.outcome<>'success' OR NOT EXISTS(SELECT 1 FROM bss_orders__order_version WHERE order_id=a.order_id AND version=a.candidate_version)) THEN
      RAISE EXCEPTION 'committed attempt requires selected commercial version' USING ERRCODE='23514';
   END IF;
 ELSIF marker.fulfillment_control_id IS NOT NULL THEN
   SELECT * INTO STRICT c FROM bss_orders__fulfillment_control WHERE control_id=marker.fulfillment_control_id;
   IF c.idempotency_execution_id<>marker.execution_id OR c.order_id IS DISTINCT FROM marker.order_id
      OR c.operation<>marker.operation OR c.request_fingerprint<>marker.request_fingerprint
      OR c.owner_token IS DISTINCT FROM marker.owner_token OR c.fencing_generation<>marker.fencing_generation
      OR (c.status IN ('prepared','awaiting','barrier_ready')) IS DISTINCT FROM (marker.status='in_flight') THEN
       RAISE EXCEPTION 'control execution identity/fence mismatch' USING ERRCODE='23514';
   END IF;
 ELSIF TG_TABLE_NAME<>'bss_orders__idempotency' THEN
   RAISE EXCEPTION 'registry must link its execution' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE CONSTRAINT TRIGGER bss_orders__attempt_execution AFTER INSERT OR UPDATE ON bss_orders__commercial_attempt DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__execution_link();
CREATE CONSTRAINT TRIGGER bss_orders__control_execution AFTER INSERT OR UPDATE ON bss_orders__fulfillment_control DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__execution_link();
CREATE CONSTRAINT TRIGGER bss_orders__registry_execution AFTER INSERT OR UPDATE ON bss_orders__idempotency DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__execution_link();

CREATE FUNCTION bss_orders__line_attempt() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.commercial_attempt_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM bss_orders__commercial_attempt WHERE attempt_id=NEW.commercial_attempt_id AND status='committed') THEN
   RAISE EXCEPTION 'reserved-only receipt cannot become a commercial line' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE CONSTRAINT TRIGGER bss_orders__line_attempt AFTER INSERT ON bss_orders__order_line DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION bss_orders__line_attempt();
ALTER TABLE bss_orders__commercial_attempt ADD CONSTRAINT ck_bss_orders__commercial_attempt__inputs CHECK (jsonb_typeof(proposed_arrangement)='object' AND jsonb_typeof(original_principal)='object' AND jsonb_typeof(date_policy_basis)='object');
ALTER TABLE bss_orders__fulfillment_control ADD CONSTRAINT ck_bss_orders__fulfillment_control__inputs CHECK (jsonb_typeof(original_actor)='object' AND jsonb_typeof(roster)='array');

-- Public trigger vocabulary plus the separate D-201 internal writer identity.
-- Refused attempts also carry a parsed registered trigger, never caller-supplied tokens.
ALTER TABLE bss_orders__transition_audit ADD CONSTRAINT ck_bss_orders__audit_trigger CHECK (trigger IN ('create','draft-mutate','administrative-edit','submit','cancel','auto-void','reflect-approval-required','reflect-approval-not-required','reflect-approval-granted','reflect-approval-denied','begin-fulfillment','report-spawn-signal','acknowledge-completed','acknowledge-failed','cancel-workflow-mediated','amendment','hold','resume','expire','record-acceptance','force-fail-unreconciled','replace-fulfillment-grant'));
