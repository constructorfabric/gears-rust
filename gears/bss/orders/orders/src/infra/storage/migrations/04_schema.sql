CREATE TABLE bss_orders__policy_election (
    policy_id uuid NOT NULL,
    election text NOT NULL,
    scope text NOT NULL,
    scope_id uuid,
    elected boolean NOT NULL,
    elected_by text NOT NULL,
    elected_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__policy_election PRIMARY KEY (policy_id),
    CONSTRAINT ck_bss_orders__policy_election__0 CHECK (election IN ('tolerate_authorization_failure','acceptance_required')),
    CONSTRAINT ck_bss_orders__policy_election__1 CHECK (scope IN ('platform','seller')),
    CONSTRAINT ck_bss_orders__policy_election__2 CHECK ((scope='platform' AND scope_id IS NULL) OR (scope='seller' AND scope_id IS NOT NULL))
);

CREATE UNIQUE INDEX uq_bss_orders__policy_election__0 ON bss_orders__policy_election (election,scope,scope_id) NULLS NOT DISTINCT;

CREATE TABLE bss_orders__approval_reflection (
    reflection_id uuid NOT NULL,
    order_id uuid NOT NULL,
    version integer NOT NULL,
    verdict_kind text NOT NULL,
    verdict text NOT NULL,
    deciding_authority text NOT NULL,
    denial_reason text,
    correlation_id uuid NOT NULL,
    reflected_at timestamptz NOT NULL,
    CONSTRAINT pk_bss_orders__approval_reflection PRIMARY KEY (reflection_id),
    CONSTRAINT ck_bss_orders__approval_reflection__0 CHECK ((verdict_kind='requirement' AND verdict IN ('required','not_required')) OR (verdict_kind='gate_outcome' AND verdict IN ('granted','denied'))),
    CONSTRAINT ck_bss_orders__approval_reflection__1 CHECK ((verdict='denied') = (denial_reason IS NOT NULL)),
    CONSTRAINT fk_bss_orders__approval_reflection__0 FOREIGN KEY (order_id) REFERENCES bss_orders__order(order_id),
    CONSTRAINT fk_bss_orders__approval_reflection__1 FOREIGN KEY (order_id,version) REFERENCES bss_orders__order_version(order_id,version)
);

CREATE UNIQUE INDEX uq_bss_orders__approval_reflection__0 ON bss_orders__approval_reflection (order_id,version,verdict_kind);
