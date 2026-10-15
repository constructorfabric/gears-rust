CREATE TABLE bss_orders__gate_outcome (
    outcome_id uuid NOT NULL,
    run_id uuid NOT NULL,
    subject_tenant_id uuid NOT NULL,
    subject_id text NOT NULL,
    resource_tenant_id uuid NOT NULL,
    seller_tenant_id uuid NOT NULL,
    payer_tenant_id uuid NOT NULL,
    correlation_id uuid,
    order_id uuid,
    version integer,
    line_id uuid,
    predicate text NOT NULL,
    item_id uuid,
    has_catalog_selection boolean NOT NULL,
    catalog_scope_key text,
    verdict text NOT NULL,
    reason text,
    mapping_version text NOT NULL,
    applicability text NOT NULL,
    producer_results jsonb NOT NULL,
    upstream_detail jsonb,
    evaluated_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__gate_outcome PRIMARY KEY (outcome_id),
    CONSTRAINT ck_bss_orders__gate_outcome__0 CHECK (verdict IN ('passed','failed','unevaluable')),
    CONSTRAINT ck_bss_orders__gate_outcome__1 CHECK (applicability IN ('required','tcv-only','preview-forecast')),
    CONSTRAINT ck_bss_orders__gate_outcome__2 CHECK (verdict='passed' OR reason IS NOT NULL),
    CONSTRAINT ck_bss_orders__gate_outcome__3 CHECK (has_catalog_selection OR catalog_scope_key IS NULL),
    CONSTRAINT ck_bss_orders__gate_outcome__4 CHECK (line_id IS NOT NULL OR (item_id IS NULL AND NOT has_catalog_selection AND catalog_scope_key IS NULL)),
    CONSTRAINT ck_bss_orders__gate_outcome__5 CHECK (NOT has_catalog_selection OR item_id IS NOT NULL),
    CONSTRAINT ck_bss_orders__gate_outcome__6 CHECK (order_id IS NOT NULL OR version IS NULL),
    CONSTRAINT fk_bss_orders__gate_outcome__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__gate_outcome__1 FOREIGN KEY (order_id,version) REFERENCES bss_orders__order_version(order_id,version)
);

CREATE UNIQUE INDEX uq_bss_orders__gate_outcome__0 ON bss_orders__gate_outcome (run_id,line_id,predicate,item_id,has_catalog_selection,catalog_scope_key) NULLS NOT DISTINCT;

CREATE INDEX idx_bss_orders__gate_outcome__0 ON bss_orders__gate_outcome (evaluated_at) WHERE order_id IS NULL;

CREATE INDEX idx_bss_orders__gate_outcome__1 ON bss_orders__gate_outcome (order_id);
