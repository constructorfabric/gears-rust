DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_runtime') THEN CREATE ROLE bss_orders_runtime NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_business') THEN CREATE ROLE bss_orders_business NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_private') THEN CREATE ROLE bss_orders_private NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_discovery') THEN CREATE ROLE bss_orders_discovery NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_maintenance') THEN CREATE ROLE bss_orders_maintenance NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_verifier') THEN CREATE ROLE bss_orders_verifier NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_checkpoint') THEN CREATE ROLE bss_orders_checkpoint NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_retention') THEN CREATE ROLE bss_orders_retention NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='bss_orders_policy') THEN CREATE ROLE bss_orders_policy NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$;

REVOKE ALL ON bss_orders__order FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__order TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__order TO bss_orders_business;

GRANT UPDATE (category,resource_tenant_id,payer_tenant_id,contract_id,state,state_entered_at,current_version,version_allocation_high_water,draft_revision,pre_hold_state,resume_count,amendment_count,fulfillment_control_generation,fulfillment_control_pending,spawn_signal_at,authorization_failure_tolerated_at,compensation_evidence,audit_sequence) ON bss_orders__order TO bss_orders_runtime, bss_orders_business;

GRANT SELECT ON bss_orders__order TO bss_orders_verifier, bss_orders_checkpoint;

GRANT SELECT ON bss_orders__order TO bss_orders_discovery, bss_orders_maintenance;

REVOKE ALL ON bss_orders__order_version FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__order_version TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__order_version TO bss_orders_business;

REVOKE ALL ON bss_orders__order_line_identity FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__order_line_identity TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__order_line_identity TO bss_orders_business;

REVOKE ALL ON bss_orders__commercial_attempt FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__commercial_attempt TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__commercial_attempt TO bss_orders_private;

GRANT UPDATE (status,owner_token,fencing_generation,lease_until,receipt_results,terminal_at) ON bss_orders__commercial_attempt TO bss_orders_runtime, bss_orders_private;

GRANT SELECT ON bss_orders__commercial_attempt TO bss_orders_discovery, bss_orders_maintenance;

REVOKE ALL ON bss_orders__order_line FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__order_line TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__order_line TO bss_orders_business;

REVOKE ALL ON bss_orders__draft_content FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__draft_content TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__draft_content TO bss_orders_business;

GRANT UPDATE (plan_id,plan_revision_id,selected_items,currency,contract_effective_date,service_activation_date,acceptance_due_date,term_duration,billing_cycle,term_kind,authored_term) ON bss_orders__draft_content TO bss_orders_runtime, bss_orders_business;

GRANT DELETE ON bss_orders__draft_content TO bss_orders_runtime, bss_orders_business;

REVOKE ALL ON bss_orders__order_admin FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__order_admin TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__order_admin TO bss_orders_business;

GRANT UPDATE (external_reference,display_label,internal_notes,updated_by,updated_at) ON bss_orders__order_admin TO bss_orders_runtime, bss_orders_business;

REVOKE ALL ON bss_orders__order_line_admin FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__order_line_admin TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__order_line_admin TO bss_orders_business;

GRANT UPDATE (external_reference,display_label,internal_notes,updated_by,updated_at) ON bss_orders__order_line_admin TO bss_orders_runtime, bss_orders_business;

REVOKE ALL ON bss_orders__resolved_total FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__resolved_total TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__resolved_total TO bss_orders_business;

REVOKE ALL ON bss_orders__inflight_overlap_claim FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__inflight_overlap_claim TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__inflight_overlap_claim TO bss_orders_business;

GRANT UPDATE (released_at) ON bss_orders__inflight_overlap_claim TO bss_orders_runtime, bss_orders_business;

REVOKE ALL ON bss_orders__transition_audit FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__transition_audit TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__transition_audit TO bss_orders_private;

GRANT SELECT, DELETE ON bss_orders__transition_audit TO bss_orders_retention;

GRANT SELECT ON bss_orders__transition_audit TO bss_orders_verifier, bss_orders_checkpoint;

REVOKE ALL ON bss_orders__audit_checkpoint FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__audit_checkpoint TO bss_orders_checkpoint;

GRANT SELECT ON bss_orders__audit_checkpoint TO bss_orders_verifier;

REVOKE ALL ON bss_orders__audit_checkpoint_member FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__audit_checkpoint_member TO bss_orders_checkpoint;

GRANT SELECT ON bss_orders__audit_checkpoint_member TO bss_orders_verifier;

REVOKE ALL ON bss_orders__fulfillment_grant FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__fulfillment_grant TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__fulfillment_grant TO bss_orders_private;

REVOKE ALL ON bss_orders__fulfillment_control FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__fulfillment_control TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__fulfillment_control TO bss_orders_private;

GRANT UPDATE (status,owner_token,fencing_generation,lease_until,receiver_commands,receiver_evidence,error_classification,terminal_at) ON bss_orders__fulfillment_control TO bss_orders_runtime, bss_orders_private;

GRANT SELECT ON bss_orders__fulfillment_control TO bss_orders_discovery, bss_orders_maintenance;

REVOKE ALL ON bss_orders__idempotency FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__idempotency TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__idempotency TO bss_orders_private;

GRANT UPDATE (order_id,status,attempt_id,fulfillment_control_id,owner_token,fencing_generation,lease_expires_at,outcome,outcome_reason,audit_id,settled_response) ON bss_orders__idempotency TO bss_orders_runtime, bss_orders_private;

GRANT DELETE ON bss_orders__idempotency TO bss_orders_runtime, bss_orders_private;

GRANT SELECT ON bss_orders__idempotency TO bss_orders_discovery, bss_orders_maintenance;

GRANT DELETE ON bss_orders__idempotency TO bss_orders_maintenance;

REVOKE ALL ON bss_orders__line_fulfillment FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__line_fulfillment TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__line_fulfillment TO bss_orders_business;

GRANT UPDATE (version,status,subscription_id,transition_request_ref,updated_at) ON bss_orders__line_fulfillment TO bss_orders_runtime, bss_orders_business;

REVOKE ALL ON bss_orders__acceptance FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__acceptance TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__acceptance TO bss_orders_business;

REVOKE ALL ON bss_orders__date_policy FROM PUBLIC;

GRANT SELECT ON bss_orders__date_policy TO bss_orders_runtime, bss_orders_business, bss_orders_maintenance;

GRANT SELECT, INSERT ON bss_orders__date_policy TO bss_orders_policy;

GRANT UPDATE (service_activation_required,acceptance_due_required,revision,updated_at) ON bss_orders__date_policy TO bss_orders_policy;

REVOKE ALL ON bss_orders__gate_outcome FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__gate_outcome TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__gate_outcome TO bss_orders_private;

GRANT SELECT, DELETE ON bss_orders__gate_outcome TO bss_orders_retention;

REVOKE ALL ON bss_orders__policy_election FROM PUBLIC;

GRANT SELECT ON bss_orders__policy_election TO bss_orders_runtime, bss_orders_business, bss_orders_maintenance;

GRANT SELECT, INSERT ON bss_orders__policy_election TO bss_orders_policy;

GRANT UPDATE (elected,elected_by,elected_at) ON bss_orders__policy_election TO bss_orders_policy;

REVOKE ALL ON bss_orders__approval_reflection FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__approval_reflection TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__approval_reflection TO bss_orders_business;

REVOKE ALL ON bss_orders__state_ttl_policy FROM PUBLIC;

GRANT SELECT ON bss_orders__state_ttl_policy TO bss_orders_runtime, bss_orders_business, bss_orders_maintenance;

GRANT SELECT, INSERT ON bss_orders__state_ttl_policy TO bss_orders_policy;

GRANT UPDATE (ttl_duration,provisional,policy_revision,updated_by,updated_at) ON bss_orders__state_ttl_policy TO bss_orders_policy;

GRANT DELETE ON bss_orders__state_ttl_policy TO bss_orders_policy;

REVOKE ALL ON bss_orders__read_access_log FROM PUBLIC;

GRANT SELECT, INSERT ON bss_orders__read_access_log TO bss_orders_runtime;

GRANT SELECT, INSERT ON bss_orders__read_access_log TO bss_orders_private;

GRANT SELECT, DELETE ON bss_orders__read_access_log TO bss_orders_retention;

GRANT SELECT ON bss_orders__order TO bss_orders_private;

GRANT SELECT ON bss_orders__commercial_attempt, bss_orders__fulfillment_control TO bss_orders_business;

GRANT SELECT, INSERT, UPDATE, DELETE ON event_broker_producer_registrations TO bss_orders_runtime;

DO $$ DECLARE t record; BEGIN FOR t IN SELECT tablename FROM pg_tables WHERE schemaname=current_schema() AND tablename IN ('toolkit_outbox_body','toolkit_outbox_partitions','toolkit_outbox_incoming','toolkit_outbox_outgoing','toolkit_outbox_dead_letters','toolkit_outbox_processor','toolkit_outbox_vacuum_counter','toolkit_outbox_trace') LOOP EXECUTE format('GRANT SELECT,INSERT,UPDATE,DELETE ON %I.%I TO bss_orders_runtime',current_schema(),t.tablename); END LOOP; END $$;

CREATE TRIGGER bss_orders__order_fixed BEFORE UPDATE ON bss_orders__order FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('category,resource_tenant_id,payer_tenant_id,contract_id,state,state_entered_at,current_version,version_allocation_high_water,draft_revision,pre_hold_state,resume_count,amendment_count,fulfillment_control_generation,fulfillment_control_pending,spawn_signal_at,authorization_failure_tolerated_at,compensation_evidence,audit_sequence');

CREATE TRIGGER bss_orders__order_delete BEFORE DELETE ON bss_orders__order FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__order_validate BEFORE UPDATE ON bss_orders__order FOR EACH ROW EXECUTE FUNCTION bss_orders__aggregate_guard();

CREATE TRIGGER bss_orders__order_version_immutable BEFORE UPDATE ON bss_orders__order_version FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__order_version_delete BEFORE DELETE ON bss_orders__order_version FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__order_line_identity_immutable BEFORE UPDATE ON bss_orders__order_line_identity FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__order_line_identity_delete BEFORE DELETE ON bss_orders__order_line_identity FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__commercial_attempt_fixed BEFORE UPDATE ON bss_orders__commercial_attempt FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('status,owner_token,fencing_generation,lease_until,receipt_results,terminal_at');

CREATE TRIGGER bss_orders__commercial_attempt_delete BEFORE DELETE ON bss_orders__commercial_attempt FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__commercial_attempt_validate BEFORE UPDATE ON bss_orders__commercial_attempt FOR EACH ROW EXECUTE FUNCTION bss_orders__execution_guard();

CREATE TRIGGER bss_orders__order_line_immutable BEFORE UPDATE ON bss_orders__order_line FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__order_line_delete BEFORE DELETE ON bss_orders__order_line FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__draft_content_fixed BEFORE UPDATE ON bss_orders__draft_content FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('plan_id,plan_revision_id,selected_items,currency,contract_effective_date,service_activation_date,acceptance_due_date,term_duration,billing_cycle,term_kind,authored_term');

CREATE TRIGGER bss_orders__draft_content_delete BEFORE DELETE ON bss_orders__draft_content FOR EACH ROW EXECUTE FUNCTION bss_orders__working_guard('draft');

CREATE TRIGGER bss_orders__draft_content_state BEFORE INSERT OR UPDATE ON bss_orders__draft_content FOR EACH ROW EXECUTE FUNCTION bss_orders__working_guard('draft');

CREATE TRIGGER bss_orders__order_admin_fixed BEFORE UPDATE ON bss_orders__order_admin FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('external_reference,display_label,internal_notes,updated_by,updated_at');

CREATE TRIGGER bss_orders__order_admin_delete BEFORE DELETE ON bss_orders__order_admin FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__order_admin_state BEFORE INSERT OR UPDATE ON bss_orders__order_admin FOR EACH ROW EXECUTE FUNCTION bss_orders__working_guard('admin');

CREATE TRIGGER bss_orders__order_line_admin_fixed BEFORE UPDATE ON bss_orders__order_line_admin FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('external_reference,display_label,internal_notes,updated_by,updated_at');

CREATE TRIGGER bss_orders__order_line_admin_delete BEFORE DELETE ON bss_orders__order_line_admin FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__order_line_admin_state BEFORE INSERT OR UPDATE ON bss_orders__order_line_admin FOR EACH ROW EXECUTE FUNCTION bss_orders__working_guard('admin');

CREATE TRIGGER bss_orders__resolved_total_immutable BEFORE UPDATE ON bss_orders__resolved_total FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__resolved_total_delete BEFORE DELETE ON bss_orders__resolved_total FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__inflight_overlap_claim_fixed BEFORE UPDATE ON bss_orders__inflight_overlap_claim FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('released_at');

CREATE TRIGGER bss_orders__inflight_overlap_claim_delete BEFORE DELETE ON bss_orders__inflight_overlap_claim FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__inflight_overlap_claim_validate BEFORE UPDATE ON bss_orders__inflight_overlap_claim FOR EACH ROW EXECUTE FUNCTION bss_orders__claim_guard();

CREATE TRIGGER bss_orders__transition_audit_immutable BEFORE UPDATE ON bss_orders__transition_audit FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__transition_audit_delete BEFORE DELETE ON bss_orders__transition_audit FOR EACH ROW EXECUTE FUNCTION bss_orders__retention_guard();

CREATE TRIGGER bss_orders__audit_checkpoint_immutable BEFORE UPDATE ON bss_orders__audit_checkpoint FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__audit_checkpoint_delete BEFORE DELETE ON bss_orders__audit_checkpoint FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__audit_checkpoint_member_immutable BEFORE UPDATE ON bss_orders__audit_checkpoint_member FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__audit_checkpoint_member_delete BEFORE DELETE ON bss_orders__audit_checkpoint_member FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__fulfillment_grant_immutable BEFORE UPDATE ON bss_orders__fulfillment_grant FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__fulfillment_grant_delete BEFORE DELETE ON bss_orders__fulfillment_grant FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__fulfillment_control_fixed BEFORE UPDATE ON bss_orders__fulfillment_control FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('status,owner_token,fencing_generation,lease_until,receiver_commands,receiver_evidence,error_classification,terminal_at');

CREATE TRIGGER bss_orders__fulfillment_control_delete BEFORE DELETE ON bss_orders__fulfillment_control FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__fulfillment_control_validate BEFORE UPDATE ON bss_orders__fulfillment_control FOR EACH ROW EXECUTE FUNCTION bss_orders__execution_guard();

CREATE TRIGGER bss_orders__idempotency_fixed BEFORE UPDATE ON bss_orders__idempotency FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('order_id,status,attempt_id,fulfillment_control_id,owner_token,fencing_generation,lease_expires_at,outcome,outcome_reason,audit_id,settled_response');

CREATE TRIGGER bss_orders__idempotency_delete BEFORE DELETE ON bss_orders__idempotency FOR EACH ROW EXECUTE FUNCTION bss_orders__registry_guard();

CREATE TRIGGER bss_orders__idempotency_validate BEFORE UPDATE ON bss_orders__idempotency FOR EACH ROW EXECUTE FUNCTION bss_orders__registry_guard();

CREATE TRIGGER bss_orders__line_fulfillment_fixed BEFORE UPDATE ON bss_orders__line_fulfillment FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('version,status,subscription_id,transition_request_ref,updated_at');

CREATE TRIGGER bss_orders__line_fulfillment_delete BEFORE DELETE ON bss_orders__line_fulfillment FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__acceptance_immutable BEFORE UPDATE ON bss_orders__acceptance FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__acceptance_delete BEFORE DELETE ON bss_orders__acceptance FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__date_policy_fixed BEFORE UPDATE ON bss_orders__date_policy FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('service_activation_required,acceptance_due_required,revision,updated_at');

CREATE TRIGGER bss_orders__date_policy_delete BEFORE DELETE ON bss_orders__date_policy FOR EACH ROW EXECUTE FUNCTION bss_orders__policy_guard();

CREATE TRIGGER bss_orders__date_policy_validate BEFORE UPDATE ON bss_orders__date_policy FOR EACH ROW EXECUTE FUNCTION bss_orders__policy_guard();

CREATE TRIGGER bss_orders__gate_outcome_immutable BEFORE UPDATE ON bss_orders__gate_outcome FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__gate_outcome_delete BEFORE DELETE ON bss_orders__gate_outcome FOR EACH ROW EXECUTE FUNCTION bss_orders__retention_guard();

CREATE TRIGGER bss_orders__policy_election_fixed BEFORE UPDATE ON bss_orders__policy_election FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('elected,elected_by,elected_at');

CREATE TRIGGER bss_orders__policy_election_delete BEFORE DELETE ON bss_orders__policy_election FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__approval_reflection_immutable BEFORE UPDATE ON bss_orders__approval_reflection FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__approval_reflection_delete BEFORE DELETE ON bss_orders__approval_reflection FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__state_ttl_policy_fixed BEFORE UPDATE ON bss_orders__state_ttl_policy FOR EACH ROW EXECUTE FUNCTION bss_orders__update_guard('ttl_duration,provisional,policy_revision,updated_by,updated_at');

CREATE TRIGGER bss_orders__state_ttl_policy_delete BEFORE DELETE ON bss_orders__state_ttl_policy FOR EACH ROW EXECUTE FUNCTION bss_orders__policy_guard();

CREATE TRIGGER bss_orders__state_ttl_policy_validate BEFORE UPDATE ON bss_orders__state_ttl_policy FOR EACH ROW EXECUTE FUNCTION bss_orders__policy_guard();

CREATE TRIGGER bss_orders__read_access_log_immutable BEFORE UPDATE ON bss_orders__read_access_log FOR EACH ROW EXECUTE FUNCTION bss_orders__immutable();

CREATE TRIGGER bss_orders__read_access_log_delete BEFORE DELETE ON bss_orders__read_access_log FOR EACH ROW EXECUTE FUNCTION bss_orders__retention_guard();

CREATE TRIGGER bss_orders__audit_v3 BEFORE INSERT ON bss_orders__transition_audit FOR EACH ROW EXECUTE FUNCTION bss_orders__audit_writer();

GRANT DELETE ON bss_orders__date_policy TO bss_orders_policy;
