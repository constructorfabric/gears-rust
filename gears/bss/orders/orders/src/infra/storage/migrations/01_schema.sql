CREATE TABLE bss_orders__order (
    order_id uuid NOT NULL,
    order_number text NOT NULL,
    category text NOT NULL,
    resource_tenant_id uuid NOT NULL,
    audit_tenant_id uuid NOT NULL,
    payer_tenant_id uuid NOT NULL,
    seller_tenant_id uuid NOT NULL,
    initiating_actor text NOT NULL,
    sales_path text NOT NULL,
    contract_id uuid,
    state text NOT NULL,
    state_entered_at timestamptz NOT NULL,
    current_version integer NOT NULL,
    version_allocation_high_water integer NOT NULL,
    draft_revision bigint NOT NULL,
    pre_hold_state text,
    resume_count integer NOT NULL,
    amendment_count integer NOT NULL,
    fulfillment_control_generation bigint NOT NULL,
    fulfillment_control_pending uuid,
    spawn_signal_at timestamptz,
    authorization_failure_tolerated_at timestamptz,
    compensation_evidence jsonb,
    audit_sequence bigint NOT NULL,
    created_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__order PRIMARY KEY (order_id),
    CONSTRAINT ck_bss_orders__order__0 CHECK (state IN ('draft','submitted','pending_approval','approved','in_fulfillment','on_hold','completed','rejected','cancelled','fulfillment_failed','expired')),
    CONSTRAINT ck_bss_orders__order__1 CHECK (pre_hold_state IS NULL OR pre_hold_state IN ('draft','submitted','pending_approval','approved','in_fulfillment','on_hold','completed','rejected','cancelled','fulfillment_failed','expired')),
    CONSTRAINT ck_bss_orders__order__2 CHECK (pre_hold_state IS NULL OR state = 'on_hold'),
    CONSTRAINT ck_bss_orders__order__3 CHECK (sales_path IN ('self_service','partner_placed')),
    CONSTRAINT ck_bss_orders__order__4 CHECK (current_version >= 1 AND current_version <= version_allocation_high_water),
    CONSTRAINT ck_bss_orders__order__5 CHECK (draft_revision >= 0 AND resume_count >= 0 AND amendment_count >= 0 AND fulfillment_control_generation >= 0 AND audit_sequence >= 0),
    CONSTRAINT ck_bss_orders__order__6 CHECK (length(order_number) BETWEEN 1 AND 128)
);

CREATE UNIQUE INDEX uq_bss_orders__order__0 ON bss_orders__order (seller_tenant_id,order_number);

CREATE INDEX idx_bss_orders__order__0 ON bss_orders__order (resource_tenant_id,state,state_entered_at);

CREATE INDEX idx_bss_orders__order__1 ON bss_orders__order (seller_tenant_id,state,state_entered_at,order_id);

CREATE INDEX idx_bss_orders__order__2 ON bss_orders__order (state,state_entered_at,order_id);

CREATE INDEX idx_bss_orders__order__3 ON bss_orders__order (state,created_at,order_id);

CREATE INDEX idx_bss_orders__order__4 ON bss_orders__order (resource_tenant_id,created_at,order_id);

CREATE INDEX idx_bss_orders__order__5 ON bss_orders__order (seller_tenant_id,created_at,order_id);

CREATE INDEX idx_bss_orders__order__6 ON bss_orders__order (resource_tenant_id,state,created_at,order_id);

CREATE INDEX idx_bss_orders__order__7 ON bss_orders__order (seller_tenant_id,state,created_at,order_id);

CREATE INDEX idx_bss_orders__order__8 ON bss_orders__order (payer_tenant_id,created_at,order_id);

CREATE INDEX idx_bss_orders__order__9 ON bss_orders__order (payer_tenant_id,state,created_at,order_id);

CREATE INDEX idx_bss_orders__order__10 ON bss_orders__order (contract_id,created_at,order_id);

CREATE TABLE bss_orders__order_version (
    order_id uuid NOT NULL,
    version integer NOT NULL,
    supersedes_version integer,
    market_currency char(3),
    market_region text,
    payer_tenant_id uuid NOT NULL,
    category text NOT NULL,
    contract_id uuid,
    actor text NOT NULL,
    actor_tenant_id uuid NOT NULL,
    reason text NOT NULL,
    amendment_reason text,
    created_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__order_version PRIMARY KEY (order_id,version),
    CONSTRAINT ck_bss_orders__order_version__0 CHECK (version > 0),
    CONSTRAINT ck_bss_orders__order_version__1 CHECK ((version = 1 AND supersedes_version IS NULL AND reason = 'create') OR (version > 1 AND supersedes_version IS NOT NULL AND supersedes_version > 0 AND supersedes_version < version AND reason IN ('submit','amendment'))),
    CONSTRAINT ck_bss_orders__order_version__2 CHECK ((reason = 'amendment' AND amendment_reason IS NOT NULL AND length(amendment_reason) BETWEEN 1 AND 4096) OR (reason <> 'amendment' AND amendment_reason IS NULL)),
    CONSTRAINT fk_bss_orders__order_version__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__order_version__1 FOREIGN KEY (order_id,supersedes_version) REFERENCES bss_orders__order_version(order_id,version)
);

CREATE TABLE bss_orders__order_line_identity (
    order_id uuid NOT NULL,
    line_id uuid NOT NULL,
    created_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__order_line_identity PRIMARY KEY (order_id,line_id),
    CONSTRAINT fk_bss_orders__order_line_identity__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id)
);

CREATE INDEX idx_bss_orders__order_line_identity__0 ON bss_orders__order_line_identity (order_id,created_at,line_id);

CREATE TABLE bss_orders__commercial_attempt (
    attempt_id uuid NOT NULL,
    order_id uuid NOT NULL,
    candidate_version integer NOT NULL,
    previous_committed_version integer NOT NULL,
    idempotency_execution_id uuid NOT NULL,
    operation text NOT NULL,
    principal_scope text NOT NULL,
    request_fingerprint text NOT NULL,
    prepared_draft_revision bigint,
    proposed_arrangement jsonb NOT NULL,
    authorization_fact_fingerprint text NOT NULL,
    original_principal jsonb NOT NULL,
    proof_reference text,
    line_requests jsonb NOT NULL,
    date_policy_basis jsonb NOT NULL,
    commercial_subject_id uuid NOT NULL,
    commercial_subject_type text NOT NULL,
    commercial_subject_tenant_id uuid NOT NULL,
    status text NOT NULL,
    owner_token uuid NOT NULL,
    fencing_generation bigint NOT NULL,
    lease_until timestamptz,
    receipt_results jsonb NOT NULL,
    created_at timestamptz NOT NULL,
    terminal_at timestamptz,
    CONSTRAINT pk_bss_orders__commercial_attempt PRIMARY KEY (attempt_id),
    CONSTRAINT ck_bss_orders__commercial_attempt__0 CHECK (operation IN ('submit','amendment')),
    CONSTRAINT ck_bss_orders__commercial_attempt__1 CHECK (status IN ('prepared','running','committed','refused','abandoned')),
    CONSTRAINT ck_bss_orders__commercial_attempt__2 CHECK (candidate_version > previous_committed_version AND previous_committed_version > 0),
    CONSTRAINT ck_bss_orders__commercial_attempt__3 CHECK (fencing_generation >= 0),
    CONSTRAINT ck_bss_orders__commercial_attempt__4 CHECK (prepared_draft_revision IS NULL OR prepared_draft_revision >= 0),
    CONSTRAINT ck_bss_orders__commercial_attempt__5 CHECK ((operation = 'submit') = (prepared_draft_revision IS NOT NULL)),
    CONSTRAINT ck_bss_orders__commercial_attempt__6 CHECK ((status IN ('prepared','running') AND lease_until IS NOT NULL AND terminal_at IS NULL) OR (status IN ('committed','refused','abandoned') AND lease_until IS NULL AND terminal_at IS NOT NULL)),
    CONSTRAINT ck_bss_orders__commercial_attempt__7 CHECK (jsonb_typeof(line_requests) = 'object' AND jsonb_typeof(receipt_results) = 'object'),
    CONSTRAINT fk_bss_orders__commercial_attempt__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__commercial_attempt__1 FOREIGN KEY (order_id,previous_committed_version) REFERENCES bss_orders__order_version(order_id,version)
);

CREATE UNIQUE INDEX uq_bss_orders__commercial_attempt__0 ON bss_orders__commercial_attempt (order_id,candidate_version);

CREATE UNIQUE INDEX uq_bss_orders__commercial_attempt__1 ON bss_orders__commercial_attempt (idempotency_execution_id);

CREATE UNIQUE INDEX uq_bss_orders__commercial_attempt__2 ON bss_orders__commercial_attempt (attempt_id,order_id,candidate_version);

CREATE INDEX idx_bss_orders__commercial_attempt__0 ON bss_orders__commercial_attempt (status,lease_until,attempt_id);

CREATE TABLE bss_orders__order_line (
    order_id uuid NOT NULL,
    version integer NOT NULL,
    line_id uuid NOT NULL,
    commercial_attempt_id uuid,
    pricing_acceptance_id uuid,
    pricing_request_digest text,
    pricing_terms_digest text,
    plan_id uuid NOT NULL,
    plan_revision_id uuid NOT NULL,
    selected_items jsonb NOT NULL,
    currency char(3) NOT NULL,
    contract_effective_date date NOT NULL,
    service_activation_date date,
    acceptance_due_date date,
    term_duration interval,
    term_kind text NOT NULL,
    authored_term jsonb NOT NULL,
    billing_cycle text NOT NULL,
    order_pin jsonb,
    overlap_scope_key text,
    date_policy_switch_state jsonb NOT NULL,
    CONSTRAINT pk_bss_orders__order_line PRIMARY KEY (order_id,version,line_id),
    CONSTRAINT ck_bss_orders__order_line__0 CHECK (billing_cycle IN ('month','year')),
    CONSTRAINT ck_bss_orders__order_line__1 CHECK (currency ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_bss_orders__order_line__2 CHECK ((commercial_attempt_id IS NULL AND pricing_acceptance_id IS NULL AND pricing_request_digest IS NULL AND pricing_terms_digest IS NULL AND order_pin IS NULL) OR (commercial_attempt_id IS NOT NULL AND pricing_acceptance_id IS NOT NULL AND pricing_request_digest IS NOT NULL AND pricing_terms_digest IS NOT NULL AND order_pin IS NOT NULL AND overlap_scope_key IS NOT NULL)),
    CONSTRAINT ck_bss_orders__order_line__3 CHECK (term_duration > interval '0'),
    CONSTRAINT fk_bss_orders__order_line__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__order_line__1 FOREIGN KEY (order_id,version) REFERENCES bss_orders__order_version(order_id,version),
    CONSTRAINT fk_bss_orders__order_line__2 FOREIGN KEY (order_id,line_id) REFERENCES bss_orders__order_line_identity(order_id,line_id),
    CONSTRAINT fk_bss_orders__order_line__3 FOREIGN KEY (commercial_attempt_id,order_id,version) REFERENCES bss_orders__commercial_attempt(attempt_id,order_id,candidate_version)
);

CREATE TABLE bss_orders__draft_content (
    order_id uuid NOT NULL,
    line_id uuid NOT NULL,
    plan_id uuid NOT NULL,
    plan_revision_id uuid NOT NULL,
    selected_items jsonb NOT NULL,
    currency char(3) NOT NULL,
    contract_effective_date date,
    service_activation_date date,
    acceptance_due_date date,
    term_duration interval,
    term_kind text NOT NULL,
    authored_term jsonb NOT NULL,
    billing_cycle text,
    CONSTRAINT pk_bss_orders__draft_content PRIMARY KEY (order_id,line_id),
    CONSTRAINT ck_bss_orders__draft_content__0 CHECK (billing_cycle IN ('month','year')),
    CONSTRAINT ck_bss_orders__draft_content__1 CHECK (currency ~ '^[A-Z]{3}$'),
    CONSTRAINT fk_bss_orders__draft_content__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__draft_content__1 FOREIGN KEY (order_id,line_id) REFERENCES bss_orders__order_line_identity(order_id,line_id)
);

CREATE TABLE bss_orders__order_admin (
    order_id uuid NOT NULL,
    external_reference text,
    display_label text,
    internal_notes text,
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__order_admin PRIMARY KEY (order_id),
    CONSTRAINT ck_bss_orders__order_admin__0 CHECK (external_reference IS NULL OR length(external_reference) <= 1024),
    CONSTRAINT ck_bss_orders__order_admin__1 CHECK (display_label IS NULL OR length(display_label) <= 256),
    CONSTRAINT ck_bss_orders__order_admin__2 CHECK (internal_notes IS NULL OR length(internal_notes) <= 4096),
    CONSTRAINT fk_bss_orders__order_admin__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id)
);

CREATE TABLE bss_orders__order_line_admin (
    order_id uuid NOT NULL,
    line_id uuid NOT NULL,
    external_reference text,
    display_label text,
    internal_notes text,
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__order_line_admin PRIMARY KEY (order_id,line_id),
    CONSTRAINT ck_bss_orders__order_line_admin__0 CHECK (external_reference IS NULL OR length(external_reference) <= 1024),
    CONSTRAINT ck_bss_orders__order_line_admin__1 CHECK (display_label IS NULL OR length(display_label) <= 256),
    CONSTRAINT ck_bss_orders__order_line_admin__2 CHECK (internal_notes IS NULL OR length(internal_notes) <= 4096),
    CONSTRAINT fk_bss_orders__order_line_admin__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__order_line_admin__1 FOREIGN KEY (order_id,line_id) REFERENCES bss_orders__order_line_identity(order_id,line_id)
);

CREATE TABLE bss_orders__resolved_total (
    order_id uuid NOT NULL,
    version integer NOT NULL,
    scope text NOT NULL,
    line_id uuid NOT NULL,
    currency char(3) NOT NULL,
    assessment_id uuid NOT NULL,
    currency_minor_digits integer NOT NULL,
    rounding_policy text NOT NULL,
    exclusions jsonb NOT NULL,
    item_breakdown jsonb NOT NULL,
    recurring_by_cycle jsonb NOT NULL,
    amount_status text NOT NULL,
    amount_basis text NOT NULL,
    period_evidence jsonb NOT NULL,
    gross_minor bigint,
    net_minor bigint,
    discount_minor bigint,
    promotion_ref text,
    charge_kind text NOT NULL,
    tcv_minor bigint,
    tcv_basis text,
    tcv_evidence jsonb,
    CONSTRAINT pk_bss_orders__resolved_total PRIMARY KEY (order_id,version,scope,line_id,charge_kind),
    CONSTRAINT ck_bss_orders__resolved_total__0 CHECK (scope IN ('line','order')),
    CONSTRAINT ck_bss_orders__resolved_total__1 CHECK (charge_kind IN ('recurring','usage','one_time')),
    CONSTRAINT ck_bss_orders__resolved_total__2 CHECK (amount_status IN ('committed','uncommitted_usage')),
    CONSTRAINT ck_bss_orders__resolved_total__3 CHECK ((scope='order') = (line_id='00000000-0000-0000-0000-000000000000')),
    CONSTRAINT ck_bss_orders__resolved_total__4 CHECK (currency_minor_digits BETWEEN 0 AND 9),
    CONSTRAINT ck_bss_orders__resolved_total__5 CHECK ((charge_kind='usage' AND amount_status='uncommitted_usage' AND gross_minor IS NULL AND net_minor IS NULL AND discount_minor IS NULL) OR (charge_kind <> 'usage' AND amount_status='committed' AND gross_minor IS NOT NULL AND net_minor IS NOT NULL AND discount_minor IS NOT NULL)),
    CONSTRAINT ck_bss_orders__resolved_total__6 CHECK ((scope='order' AND charge_kind='recurring' AND tcv_minor IS NOT NULL AND tcv_basis IS NOT NULL AND tcv_basis IN ('finite_term','rolling_annualized','mixed') AND tcv_evidence IS NOT NULL) OR ((scope<>'order' OR charge_kind<>'recurring') AND tcv_minor IS NULL AND tcv_basis IS NULL AND tcv_evidence IS NULL)),
    CONSTRAINT fk_bss_orders__resolved_total__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__resolved_total__1 FOREIGN KEY (order_id,version) REFERENCES bss_orders__order_version(order_id,version)
);

CREATE TABLE bss_orders__inflight_overlap_claim (
    claim_id uuid NOT NULL,
    payer_tenant_id uuid NOT NULL,
    resource_tenant_id uuid NOT NULL,
    overlap_scope_key text NOT NULL,
    order_id uuid NOT NULL,
    version integer NOT NULL,
    claimed_at timestamptz NOT NULL,
    released_at timestamptz,
    CONSTRAINT pk_bss_orders__inflight_overlap_claim PRIMARY KEY (claim_id),
    CONSTRAINT ck_bss_orders__inflight_overlap_claim__0 CHECK (version > 0),
    CONSTRAINT ck_bss_orders__inflight_overlap_claim__1 CHECK (released_at IS NULL OR released_at >= claimed_at),
    CONSTRAINT fk_bss_orders__inflight_overlap_claim__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id)
);

CREATE UNIQUE INDEX uq_bss_orders__inflight_overlap_claim__0 ON bss_orders__inflight_overlap_claim (payer_tenant_id,resource_tenant_id,overlap_scope_key) WHERE released_at IS NULL;

CREATE INDEX idx_bss_orders__inflight_overlap_claim__0 ON bss_orders__inflight_overlap_claim (order_id) WHERE released_at IS NULL;

CREATE TABLE bss_orders__transition_audit (
    audit_id uuid NOT NULL,
    hash_version smallint NOT NULL,
    audit_tenant_id uuid,
    subject_tenant_id uuid NOT NULL,
    resource_tenant_id uuid,
    order_id uuid,
    requested_order_ref uuid,
    sequence bigint,
    prev_hash bytea,
    entry_hash bytea NOT NULL,
    from_state text,
    to_state text,
    trigger text NOT NULL,
    outcome text NOT NULL,
    actor text NOT NULL,
    actor_class text NOT NULL,
    delegation_proof_ref text,
    reason text NOT NULL,
    caller_reason text,
    force_request_observation jsonb,
    changed_field text,
    prior_value text,
    new_value text,
    idempotency_key text NOT NULL,
    correlation_id uuid,
    version integer,
    created_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__transition_audit PRIMARY KEY (audit_id),
    CONSTRAINT ck_bss_orders__transition_audit__0 CHECK (outcome IN ('committed','refused')),
    CONSTRAINT ck_bss_orders__transition_audit__1 CHECK (actor_class IN ('system','service','user')),
    CONSTRAINT ck_bss_orders__transition_audit__2 CHECK (hash_version IN (1,2,3)),
    CONSTRAINT ck_bss_orders__transition_audit__3 CHECK (octet_length(entry_hash)=32 AND (prev_hash IS NULL OR octet_length(prev_hash)=32)),
    CONSTRAINT ck_bss_orders__transition_audit__4 CHECK (from_state IS NULL OR from_state IN ('draft','submitted','pending_approval','approved','in_fulfillment','on_hold','completed','rejected','cancelled','fulfillment_failed','expired')),
    CONSTRAINT ck_bss_orders__transition_audit__5 CHECK (to_state IS NULL OR to_state IN ('draft','submitted','pending_approval','approved','in_fulfillment','on_hold','completed','rejected','cancelled','fulfillment_failed','expired')),
    CONSTRAINT ck_bss_orders__transition_audit__6 CHECK ((outcome='committed' AND audit_tenant_id IS NOT NULL AND resource_tenant_id IS NOT NULL AND order_id IS NOT NULL AND sequence IS NOT NULL AND prev_hash IS NOT NULL AND to_state IS NOT NULL AND version IS NOT NULL AND version > 0 AND ((trigger='create' AND sequence=1 AND version=1 AND from_state IS NULL AND to_state='draft') OR (trigger<>'create' AND from_state IS NOT NULL AND sequence>1))) OR (outcome='refused' AND sequence IS NULL AND prev_hash IS NULL AND caller_reason IS NULL)),
    CONSTRAINT ck_bss_orders__transition_audit__7 CHECK (order_id IS NOT NULL OR (audit_tenant_id IS NULL AND resource_tenant_id IS NULL AND from_state IS NULL AND to_state IS NULL AND version IS NULL)),
    CONSTRAINT ck_bss_orders__transition_audit__8 CHECK (order_id IS NULL OR requested_order_ref IS NULL OR order_id=requested_order_ref),
    CONSTRAINT ck_bss_orders__transition_audit__9 CHECK (hash_version<>1 OR caller_reason IS NULL),
    CONSTRAINT ck_bss_orders__transition_audit__10 CHECK (hash_version=3 OR force_request_observation IS NULL),
    CONSTRAINT ck_bss_orders__transition_audit__11 CHECK (caller_reason IS NULL OR length(caller_reason) BETWEEN 1 AND 4096),
    CONSTRAINT ck_bss_orders__transition_audit__12 CHECK (outcome <> 'committed' OR reason=trigger),
    CONSTRAINT ck_bss_orders__transition_audit__13 CHECK (outcome <> 'committed' OR trigger <> 'replace-fulfillment-grant' OR (from_state='in_fulfillment' AND to_state='in_fulfillment')),
    CONSTRAINT fk_bss_orders__transition_audit__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id)
);

CREATE UNIQUE INDEX uq_bss_orders__transition_audit__0 ON bss_orders__transition_audit (order_id,sequence);

CREATE UNIQUE INDEX uq_bss_orders__transition_audit__1 ON bss_orders__transition_audit (audit_id,order_id,version,outcome);

CREATE INDEX idx_bss_orders__transition_audit__0 ON bss_orders__transition_audit (order_id,created_at,audit_id);

CREATE INDEX idx_bss_orders__transition_audit__1 ON bss_orders__transition_audit (created_at) WHERE outcome='refused';

CREATE INDEX idx_bss_orders__transition_audit__2 ON bss_orders__transition_audit (subject_tenant_id,requested_order_ref,created_at,audit_id);

CREATE TABLE bss_orders__audit_checkpoint (
    audit_tenant_id uuid NOT NULL,
    checkpoint_sequence bigint NOT NULL,
    format_version smallint NOT NULL,
    captured_at timestamptz NOT NULL,
    member_count bigint NOT NULL,
    prev_checkpoint_hash bytea NOT NULL,
    checkpoint_hash bytea NOT NULL,
    CONSTRAINT pk_bss_orders__audit_checkpoint PRIMARY KEY (audit_tenant_id,checkpoint_sequence),
    CONSTRAINT ck_bss_orders__audit_checkpoint__0 CHECK (checkpoint_sequence>0 AND member_count>=0),
    CONSTRAINT ck_bss_orders__audit_checkpoint__1 CHECK (format_version=1),
    CONSTRAINT ck_bss_orders__audit_checkpoint__2 CHECK (octet_length(prev_checkpoint_hash)=32 AND octet_length(checkpoint_hash)=32)
);

CREATE TABLE bss_orders__audit_checkpoint_member (
    audit_tenant_id uuid NOT NULL,
    checkpoint_sequence bigint NOT NULL,
    order_id uuid NOT NULL,
    audit_sequence bigint NOT NULL,
    entry_hash bytea NOT NULL,
    CONSTRAINT pk_bss_orders__audit_checkpoint_member PRIMARY KEY (audit_tenant_id,checkpoint_sequence,order_id),
    CONSTRAINT ck_bss_orders__audit_checkpoint_member__0 CHECK (audit_sequence>0),
    CONSTRAINT ck_bss_orders__audit_checkpoint_member__1 CHECK (octet_length(entry_hash)=32),
    CONSTRAINT fk_bss_orders__audit_checkpoint_member__0 FOREIGN KEY (audit_tenant_id,checkpoint_sequence) REFERENCES bss_orders__audit_checkpoint(audit_tenant_id,checkpoint_sequence)
);

CREATE TABLE bss_orders__fulfillment_grant (
    grant_id uuid NOT NULL,
    order_id uuid NOT NULL,
    order_version integer NOT NULL,
    fulfillment_attempt_id uuid NOT NULL,
    generation bigint NOT NULL,
    roster_receipt_digest text NOT NULL,
    roster jsonb NOT NULL,
    authority_fact_digest text NOT NULL,
    predecessor_grant_id uuid,
    created_at timestamptz NOT NULL,
    execution_id uuid NOT NULL,
    audit_id uuid NOT NULL,
    audit_outcome text NOT NULL DEFAULT 'committed',
    CONSTRAINT pk_bss_orders__fulfillment_grant PRIMARY KEY (grant_id),
    CONSTRAINT ck_bss_orders__fulfillment_grant__0 CHECK (generation>0),
    CONSTRAINT ck_bss_orders__fulfillment_grant__1 CHECK (audit_outcome='committed'),
    CONSTRAINT ck_bss_orders__fulfillment_grant__2 CHECK (jsonb_typeof(roster)='array'),
    CONSTRAINT ck_bss_orders__fulfillment_grant__3 CHECK (predecessor_grant_id IS NULL OR predecessor_grant_id<>grant_id),
    CONSTRAINT fk_bss_orders__fulfillment_grant__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__fulfillment_grant__1 FOREIGN KEY (order_id,order_version) REFERENCES bss_orders__order_version(order_id,version),
    CONSTRAINT fk_bss_orders__fulfillment_grant__3 FOREIGN KEY (audit_id,order_id,order_version,audit_outcome) REFERENCES bss_orders__transition_audit(audit_id,order_id,version,outcome) DEFERRABLE INITIALLY DEFERRED
);

CREATE UNIQUE INDEX uq_bss_orders__fulfillment_grant__0 ON bss_orders__fulfillment_grant (order_id,generation);

CREATE UNIQUE INDEX uq_bss_orders__fulfillment_grant__1 ON bss_orders__fulfillment_grant (grant_id,order_id);

CREATE TABLE bss_orders__fulfillment_control (
    control_id uuid NOT NULL,
    order_id uuid NOT NULL,
    idempotency_execution_id uuid NOT NULL,
    operation text NOT NULL,
    request_fingerprint text NOT NULL,
    expected_version integer NOT NULL,
    fulfillment_attempt_id uuid NOT NULL,
    generation bigint NOT NULL,
    original_actor jsonb NOT NULL,
    proof_reference text,
    authorization_fact_fingerprint text NOT NULL,
    roster jsonb NOT NULL,
    roster_digest text NOT NULL,
    created_at timestamptz NOT NULL,
    status text NOT NULL,
    owner_token uuid NOT NULL,
    fencing_generation bigint NOT NULL,
    lease_until timestamptz,
    receiver_commands jsonb NOT NULL,
    receiver_evidence jsonb NOT NULL,
    error_classification text,
    terminal_at timestamptz,
    CONSTRAINT pk_bss_orders__fulfillment_control PRIMARY KEY (control_id),
    CONSTRAINT ck_bss_orders__fulfillment_control__0 CHECK (operation IN ('hold','cancel-workflow-mediated','acknowledge-failed','force-fail-unreconciled')),
    CONSTRAINT ck_bss_orders__fulfillment_control__1 CHECK (status IN ('prepared','awaiting','barrier_ready','settled','refused','abandoned')),
    CONSTRAINT ck_bss_orders__fulfillment_control__2 CHECK (generation>0 AND fencing_generation>=0),
    CONSTRAINT ck_bss_orders__fulfillment_control__3 CHECK ((status IN ('prepared','awaiting','barrier_ready') AND terminal_at IS NULL AND lease_until IS NOT NULL) OR (status IN ('settled','refused','abandoned') AND terminal_at IS NOT NULL AND lease_until IS NULL)),
    CONSTRAINT ck_bss_orders__fulfillment_control__4 CHECK (jsonb_typeof(receiver_commands)='object' AND jsonb_typeof(receiver_evidence)='object'),
    CONSTRAINT fk_bss_orders__fulfillment_control__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__fulfillment_control__1 FOREIGN KEY (order_id,expected_version) REFERENCES bss_orders__order_version(order_id,version)
);

CREATE UNIQUE INDEX uq_bss_orders__fulfillment_control__0 ON bss_orders__fulfillment_control (idempotency_execution_id);

CREATE UNIQUE INDEX uq_bss_orders__fulfillment_control__1 ON bss_orders__fulfillment_control (control_id,order_id);

CREATE UNIQUE INDEX uq_bss_orders__fulfillment_control__2 ON bss_orders__fulfillment_control (order_id) WHERE status IN ('prepared','awaiting','barrier_ready');

CREATE INDEX idx_bss_orders__fulfillment_control__0 ON bss_orders__fulfillment_control (status,lease_until,control_id);

CREATE TABLE bss_orders__idempotency (
    operation text NOT NULL,
    principal_scope text NOT NULL,
    idempotency_key text NOT NULL,
    order_id uuid,
    request_fingerprint text NOT NULL,
    status text NOT NULL,
    execution_id uuid NOT NULL,
    attempt_id uuid,
    fulfillment_control_id uuid,
    owner_token uuid,
    fencing_generation bigint NOT NULL,
    lease_expires_at timestamptz,
    outcome text,
    outcome_reason text,
    audit_id uuid,
    settled_response jsonb,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__idempotency PRIMARY KEY (operation,principal_scope,idempotency_key),
    CONSTRAINT ck_bss_orders__idempotency__0 CHECK (status IN ('in_flight','settled')),
    CONSTRAINT ck_bss_orders__idempotency__1 CHECK (outcome IS NULL OR outcome IN ('success','refused')),
    CONSTRAINT ck_bss_orders__idempotency__2 CHECK (fencing_generation>=0),
    CONSTRAINT ck_bss_orders__idempotency__3 CHECK (attempt_id IS NULL OR fulfillment_control_id IS NULL),
    CONSTRAINT ck_bss_orders__idempotency__4 CHECK (operation='create' OR order_id IS NOT NULL),
    CONSTRAINT ck_bss_orders__idempotency__5 CHECK (operation<>'create' OR ((status='settled' AND outcome='success' AND order_id IS NOT NULL) OR ((status='in_flight' OR outcome='refused') AND order_id IS NULL))),
    CONSTRAINT ck_bss_orders__idempotency__6 CHECK ((status='in_flight' AND lease_expires_at IS NOT NULL AND outcome IS NULL AND outcome_reason IS NULL AND audit_id IS NULL AND settled_response IS NULL) OR (status='settled' AND lease_expires_at IS NULL AND outcome IS NOT NULL AND settled_response IS NOT NULL AND settled_response->'formatVersion'='1'::jsonb)),
    CONSTRAINT ck_bss_orders__idempotency__7 CHECK (expires_at>created_at),
    CONSTRAINT ck_bss_orders__idempotency__8 CHECK (outcome<>'refused' OR outcome_reason IS NOT NULL),
    CONSTRAINT fk_bss_orders__idempotency__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__idempotency__1 FOREIGN KEY (attempt_id) REFERENCES bss_orders__commercial_attempt(attempt_id),
    CONSTRAINT fk_bss_orders__idempotency__2 FOREIGN KEY (fulfillment_control_id) REFERENCES bss_orders__fulfillment_control(control_id),
    CONSTRAINT fk_bss_orders__idempotency__3 FOREIGN KEY (audit_id) REFERENCES bss_orders__transition_audit(audit_id)
);

CREATE UNIQUE INDEX uq_bss_orders__idempotency__0 ON bss_orders__idempotency (execution_id);

CREATE INDEX idx_bss_orders__idempotency__0 ON bss_orders__idempotency (expires_at);

CREATE TABLE bss_orders__line_fulfillment (
    order_id uuid NOT NULL,
    line_id uuid NOT NULL,
    version integer NOT NULL,
    status text NOT NULL,
    subscription_id uuid,
    transition_request_ref text,
    updated_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__line_fulfillment PRIMARY KEY (order_id,line_id),
    CONSTRAINT ck_bss_orders__line_fulfillment__0 CHECK (status IN ('created','activated','failed')),
    CONSTRAINT fk_bss_orders__line_fulfillment__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__line_fulfillment__1 FOREIGN KEY (order_id,line_id) REFERENCES bss_orders__order_line_identity(order_id,line_id),
    CONSTRAINT fk_bss_orders__line_fulfillment__2 FOREIGN KEY (order_id,version) REFERENCES bss_orders__order_version(order_id,version),
    CONSTRAINT fk_bss_orders__line_fulfillment__3 FOREIGN KEY (order_id,version,line_id) REFERENCES bss_orders__order_line(order_id,version,line_id)
);

CREATE UNIQUE INDEX uq_bss_orders__line_fulfillment__0 ON bss_orders__line_fulfillment (order_id,subscription_id) WHERE subscription_id IS NOT NULL;

CREATE TABLE bss_orders__acceptance (
    order_id uuid NOT NULL,
    accepted_version integer NOT NULL,
    accepted_at timestamptz NOT NULL,
    recorded_by text NOT NULL,
    recording_path text NOT NULL,
    requirement_source text NOT NULL,
    CONSTRAINT pk_bss_orders__acceptance PRIMARY KEY (order_id,accepted_version),
    CONSTRAINT ck_bss_orders__acceptance__0 CHECK (recording_path IN ('self_service','partner_placed')),
    CONSTRAINT ck_bss_orders__acceptance__1 CHECK (requirement_source IN ('contract','seller','platform_default','volunteered')),
    CONSTRAINT ck_bss_orders__acceptance__2 CHECK (accepted_version>1),
    CONSTRAINT fk_bss_orders__acceptance__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__acceptance__1 FOREIGN KEY (order_id,accepted_version) REFERENCES bss_orders__order_version(order_id,version)
);

ALTER TABLE bss_orders__order ADD CONSTRAINT fk_bss_orders__order__current FOREIGN KEY (order_id,current_version) REFERENCES bss_orders__order_version(order_id,version) DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE bss_orders__order ADD CONSTRAINT fk_bss_orders__order__control FOREIGN KEY (fulfillment_control_pending,order_id) REFERENCES bss_orders__fulfillment_control(control_id,order_id) DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE bss_orders__fulfillment_grant ADD CONSTRAINT fk_bss_orders__fulfillment_grant__2 FOREIGN KEY (predecessor_grant_id,order_id) REFERENCES bss_orders__fulfillment_grant(grant_id,order_id);
