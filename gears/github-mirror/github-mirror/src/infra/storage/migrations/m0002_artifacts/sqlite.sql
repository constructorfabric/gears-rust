CREATE TABLE IF NOT EXISTS gm_artifacts (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    size_in_bytes INTEGER NOT NULL,
    expired INTEGER NOT NULL,
    workflow_run_id INTEGER,
    head_branch TEXT,
    head_sha TEXT,
    created_at TEXT,
    updated_at TEXT,
    expires_at TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_artifacts_tenant_repo
    ON gm_artifacts (tenant_id, repo_id);
