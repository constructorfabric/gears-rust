CREATE TABLE IF NOT EXISTS gm_artifacts (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    name VARCHAR(1024) NOT NULL,
    size_in_bytes BIGINT NOT NULL,
    expired BOOLEAN NOT NULL,
    workflow_run_id BIGINT,
    head_branch VARCHAR(512),
    head_sha VARCHAR(64),
    created_at VARCHAR(64),
    updated_at VARCHAR(64),
    expires_at VARCHAR(64),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_artifacts_tenant_repo (tenant_id, repo_id)
);
