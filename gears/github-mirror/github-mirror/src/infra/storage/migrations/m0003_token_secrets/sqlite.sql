CREATE TABLE IF NOT EXISTS gm_token_secrets (
    tenant_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    secret_key TEXT NOT NULL,
    scope_note TEXT,
    PRIMARY KEY (tenant_id, fingerprint),
    UNIQUE (tenant_id, secret_key)
);
