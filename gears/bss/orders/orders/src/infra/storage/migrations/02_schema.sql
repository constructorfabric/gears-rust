CREATE TABLE bss_orders__date_policy (
    policy_id uuid NOT NULL,
    resource_tenant_id uuid,
    service_activation_required boolean NOT NULL,
    acceptance_due_required boolean NOT NULL,
    revision bigint NOT NULL,
    updated_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__date_policy PRIMARY KEY (policy_id),
    CONSTRAINT ck_bss_orders__date_policy__0 CHECK (revision>0)
);

CREATE UNIQUE INDEX uq_bss_orders__date_policy__0 ON bss_orders__date_policy ((true)) WHERE resource_tenant_id IS NULL;

CREATE UNIQUE INDEX uq_bss_orders__date_policy__1 ON bss_orders__date_policy (resource_tenant_id) WHERE resource_tenant_id IS NOT NULL;

INSERT INTO bss_orders__date_policy VALUES ('00000000-0000-0000-0000-000000000121',NULL,false,false,1,clock_timestamp());
