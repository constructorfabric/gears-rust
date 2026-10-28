CREATE TABLE bss_orders__read_access_log (
    access_id uuid NOT NULL,
    order_id uuid,
    requested_order_ref uuid,
    actor text NOT NULL,
    actor_class text NOT NULL,
    operation text NOT NULL,
    outcome text NOT NULL,
    refusal_reason text,
    internal_refusal_detail text,
    delegation_proof_ref text,
    accessed_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__read_access_log PRIMARY KEY (access_id),
    CONSTRAINT ck_bss_orders__read_access_log__0 CHECK (actor_class IN ('system','service','user')),
    CONSTRAINT ck_bss_orders__read_access_log__1 CHECK (outcome IN ('served','refused')),
    CONSTRAINT ck_bss_orders__read_access_log__2 CHECK ((outcome='refused') = (refusal_reason IS NOT NULL)),
    CONSTRAINT ck_bss_orders__read_access_log__3 CHECK (internal_refusal_detail IS NULL OR (outcome='refused' AND refusal_reason='order-not-found' AND internal_refusal_detail IN ('delegation-proof-required','delegation-proof-invalid'))),
    CONSTRAINT ck_bss_orders__read_access_log__4 CHECK (order_id IS NULL OR (requested_order_ref IS NOT NULL AND order_id=requested_order_ref)),
    CONSTRAINT fk_bss_orders__read_access_log__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id)
);

CREATE INDEX idx_bss_orders__read_access_log__0 ON bss_orders__read_access_log (accessed_at);

CREATE INDEX idx_bss_orders__read_access_log__1 ON bss_orders__read_access_log (order_id,accessed_at);

CREATE INDEX idx_bss_orders__read_access_log__2 ON bss_orders__read_access_log (requested_order_ref,accessed_at);
