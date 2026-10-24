CREATE TABLE bss_orders__state_ttl_policy (
    policy_id uuid NOT NULL,
    scope text NOT NULL,
    seller_tenant_id uuid,
    state text NOT NULL,
    ttl_duration interval,
    provisional boolean NOT NULL,
    policy_revision bigint NOT NULL,
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__state_ttl_policy PRIMARY KEY (policy_id),
    CONSTRAINT ck_bss_orders__state_ttl_policy__0 CHECK (scope IN ('platform','seller')),
    CONSTRAINT ck_bss_orders__state_ttl_policy__1 CHECK (state IN ('draft','submitted','pending_approval','approved','on_hold')),
    CONSTRAINT ck_bss_orders__state_ttl_policy__2 CHECK ((scope='platform' AND seller_tenant_id IS NULL) OR (scope='seller' AND seller_tenant_id IS NOT NULL AND ttl_duration IS NOT NULL AND NOT provisional)),
    CONSTRAINT ck_bss_orders__state_ttl_policy__3 CHECK (ttl_duration IS NULL OR ttl_duration > interval '0'),
    CONSTRAINT ck_bss_orders__state_ttl_policy__4 CHECK (policy_revision>0)
);

CREATE UNIQUE INDEX uq_bss_orders__state_ttl_policy__0 ON bss_orders__state_ttl_policy (scope,seller_tenant_id,state) NULLS NOT DISTINCT;

INSERT INTO bss_orders__state_ttl_policy VALUES ('00000000-0000-0000-0000-000000000181','platform',NULL,'draft',interval '90 days',true,1,'migration',clock_timestamp());
INSERT INTO bss_orders__state_ttl_policy VALUES ('00000000-0000-0000-0000-000000000182','platform',NULL,'submitted',interval '14 days',true,1,'migration',clock_timestamp());
INSERT INTO bss_orders__state_ttl_policy VALUES ('00000000-0000-0000-0000-000000000183','platform',NULL,'pending_approval',interval '14 days',true,1,'migration',clock_timestamp());
INSERT INTO bss_orders__state_ttl_policy VALUES ('00000000-0000-0000-0000-000000000184','platform',NULL,'approved',interval '30 days',true,1,'migration',clock_timestamp());
INSERT INTO bss_orders__state_ttl_policy VALUES ('00000000-0000-0000-0000-000000000185','platform',NULL,'on_hold',interval '30 days',true,1,'migration',clock_timestamp());
