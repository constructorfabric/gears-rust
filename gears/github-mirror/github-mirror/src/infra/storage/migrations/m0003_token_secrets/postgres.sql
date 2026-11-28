CREATE TABLE IF NOT EXISTS gm_token_secrets (
    tenant_id UUID NOT NULL,
    fingerprint VARCHAR(64) NOT NULL,
    secret_key VARCHAR(255) NOT NULL,
    scope_note TEXT,
    PRIMARY KEY (tenant_id, fingerprint),
    UNIQUE (tenant_id, secret_key)
);
