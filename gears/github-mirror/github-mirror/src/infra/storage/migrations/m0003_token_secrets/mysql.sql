CREATE TABLE IF NOT EXISTS gm_token_secrets (
    tenant_id BINARY(16) NOT NULL,
    fingerprint VARCHAR(64) NOT NULL,
    secret_key VARCHAR(255) NOT NULL,
    scope_note TEXT NULL,
    PRIMARY KEY (tenant_id, fingerprint),
    UNIQUE KEY uq_gm_token_secrets_key (tenant_id, secret_key)
);
