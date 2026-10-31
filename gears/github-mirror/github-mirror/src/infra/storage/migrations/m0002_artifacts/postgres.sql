CREATE TABLE IF NOT EXISTS gm_artifacts (
    tenant_id UUID NOT NULL,
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
    extracted_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_artifacts_tenant_repo
    ON gm_artifacts (tenant_id, repo_id);
