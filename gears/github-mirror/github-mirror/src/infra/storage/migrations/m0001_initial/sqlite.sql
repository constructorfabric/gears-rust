CREATE TABLE IF NOT EXISTS gm_active_syncs (
    tenant_id TEXT NOT NULL,
    repo_full_name TEXT NOT NULL,
    session_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    scope_json TEXT NOT NULL,
    since TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tenant_id, repo_full_name)
);

CREATE TABLE IF NOT EXISTS gm_branches (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    protected INTEGER NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, name)
);

CREATE TABLE IF NOT EXISTS gm_check_runs (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    head_sha TEXT NOT NULL,
    name TEXT NOT NULL,
    status TEXT,
    conclusion TEXT,
    started_at TEXT,
    completed_at TEXT,
    html_url TEXT,
    details_url TEXT,
    check_suite_id INTEGER,
    app_slug TEXT,
    app_name TEXT,
    output_title TEXT,
    output_summary TEXT,
    annotations_count INTEGER NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_check_runs_tenant_repo_sha
    ON gm_check_runs (tenant_id, repo_id, head_sha);

CREATE TABLE IF NOT EXISTS gm_comments (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    issue_number INTEGER NOT NULL,
    author_login TEXT,
    body TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    html_url TEXT,
    extracted_at TEXT,
    conversation_id BIGINT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_comments_tenant_repo_issue
    ON gm_comments (tenant_id, repo_id, issue_number);

CREATE TABLE IF NOT EXISTS gm_commit_comments (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    commit_sha TEXT NOT NULL,
    path TEXT,
    position INTEGER,
    author_login TEXT,
    body TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    html_url TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_commit_comments_tenant_repo_sha
    ON gm_commit_comments (tenant_id, repo_id, commit_sha);

CREATE TABLE IF NOT EXISTS gm_commit_files (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    commit_sha TEXT NOT NULL,
    filename TEXT NOT NULL,
    status TEXT NOT NULL,
    additions INTEGER NOT NULL,
    deletions INTEGER NOT NULL,
    changes INTEGER NOT NULL,
    previous_filename TEXT,
    sha TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, commit_sha, filename)
);

CREATE TABLE IF NOT EXISTS gm_commit_statuses (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    commit_sha TEXT NOT NULL,
    state TEXT NOT NULL,
    context TEXT NOT NULL,
    description TEXT,
    target_url TEXT,
    creator_login TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_commit_statuses_tenant_repo_sha
    ON gm_commit_statuses (tenant_id, repo_id, commit_sha);

CREATE TABLE IF NOT EXISTS gm_commits (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    sha TEXT NOT NULL,
    message TEXT NOT NULL,
    author_login TEXT,
    committer_login TEXT,
    authored_at TEXT,
    committed_at TEXT,
    additions INTEGER NOT NULL DEFAULT 0,
    deletions INTEGER NOT NULL DEFAULT 0,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, sha)
);

CREATE INDEX IF NOT EXISTS idx_gm_commits_tenant_repo_committed
    ON gm_commits (tenant_id, repo_id, committed_at);

CREATE TABLE IF NOT EXISTS gm_contributors (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    login TEXT NOT NULL,
    account_type TEXT NOT NULL,
    avatar_url TEXT,
    html_url TEXT,
    extracted_at TEXT,
    roles TEXT,
    first_seen_at TEXT,
    last_seen_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, user_id)
);

CREATE TABLE IF NOT EXISTS gm_deployments (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    git_ref TEXT NOT NULL,
    sha TEXT NOT NULL,
    environment TEXT NOT NULL,
    task TEXT NOT NULL,
    description TEXT,
    creator_login TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_deployments_tenant_repo
    ON gm_deployments (tenant_id, repo_id);

CREATE TABLE IF NOT EXISTS gm_entity_fingerprints (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    family TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    updated_at TEXT,
    node_id TEXT,
    child_counts_hash TEXT,
    last_refined_at TEXT,
    refinement_status TEXT NOT NULL,
    PRIMARY KEY (tenant_id, repo_id, family, entity_id)
);

CREATE TABLE IF NOT EXISTS gm_http_cache (
    tenant_id TEXT NOT NULL,
    cache_key TEXT NOT NULL,
    url TEXT NOT NULL,
    status INTEGER NOT NULL,
    etag TEXT,
    last_modified TEXT,
    next_page TEXT,
    body BLOB NOT NULL,
    compression TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    PRIMARY KEY (tenant_id, cache_key)
);

CREATE TABLE IF NOT EXISTS gm_issue_events (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    issue_number INTEGER NOT NULL,
    event TEXT NOT NULL,
    actor_login TEXT,
    label_name TEXT,
    assignee_login TEXT,
    milestone_title TEXT,
    commit_id TEXT,
    created_at TEXT NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_issue_events_tenant_repo_issue
    ON gm_issue_events (tenant_id, repo_id, issue_number);

CREATE TABLE IF NOT EXISTS gm_issue_reactions (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    issue_number INTEGER NOT NULL,
    content TEXT NOT NULL,
    user_login TEXT,
    created_at TEXT NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_issue_reactions_tenant_repo_issue
    ON gm_issue_reactions (tenant_id, repo_id, issue_number);

CREATE TABLE IF NOT EXISTS gm_issue_timeline (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    issue_number INTEGER NOT NULL,
    position INTEGER NOT NULL,
    event TEXT NOT NULL,
    created_at TEXT,
    actor_login TEXT,
    payload_json TEXT NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, issue_number, position)
);

CREATE TABLE IF NOT EXISTS gm_issues (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    state TEXT NOT NULL,
    is_pull_request INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT,
    html_url TEXT,
    extracted_at TEXT,
    author_json TEXT,
    author_login VARCHAR(255),
    assignees_json TEXT,
    labels_json TEXT,
    comments_count BIGINT,
    locked BOOLEAN,
    node_id VARCHAR(255),
    PRIMARY KEY (tenant_id, id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_issues_tenant_repo_number
    ON gm_issues (tenant_id, repo_id, number);

CREATE INDEX IF NOT EXISTS idx_gm_issues_tenant_repo_state
    ON gm_issues (tenant_id, repo_id, state);

CREATE TABLE IF NOT EXISTS gm_labels (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    color TEXT NOT NULL,
    is_default INTEGER NOT NULL,
    description TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_labels_tenant_repo
    ON gm_labels (tenant_id, repo_id);

CREATE TABLE IF NOT EXISTS gm_logical_conversations (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    conv_type TEXT NOT NULL,
    root_comment_id INTEGER NOT NULL,
    parent_kind TEXT NOT NULL,
    parent_number INTEGER NOT NULL,
    comment_count INTEGER NOT NULL,
    is_resolved BOOLEAN,
    created_at TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, conv_type, root_comment_id)
);

CREATE INDEX IF NOT EXISTS idx_gm_logical_conversations_parent
    ON gm_logical_conversations (tenant_id, repo_id, parent_number);

CREATE TABLE IF NOT EXISTS gm_milestones (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    description TEXT,
    open_issues INTEGER NOT NULL,
    closed_issues INTEGER NOT NULL,
    due_on TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT,
    html_url TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_milestones_tenant_repo
    ON gm_milestones (tenant_id, repo_id);

CREATE TABLE IF NOT EXISTS gm_pull_request_commits (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    sha TEXT NOT NULL,
    message TEXT NOT NULL,
    author_login TEXT,
    committer_login TEXT,
    authored_at TEXT,
    committed_at TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, pull_number, sha)
);

CREATE TABLE IF NOT EXISTS gm_pull_request_files (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    filename TEXT NOT NULL,
    status TEXT NOT NULL,
    additions INTEGER NOT NULL,
    deletions INTEGER NOT NULL,
    changes INTEGER NOT NULL,
    previous_filename TEXT,
    sha TEXT,
    extracted_at TEXT,
    patch TEXT,
    PRIMARY KEY (tenant_id, repo_id, pull_number, filename)
);

CREATE TABLE IF NOT EXISTS gm_pull_requests (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    state TEXT NOT NULL,
    draft INTEGER NOT NULL,
    merged INTEGER NOT NULL,
    head_sha TEXT,
    base_sha TEXT,
    lines_added INTEGER NOT NULL DEFAULT 0,
    lines_removed INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT,
    merged_at TEXT,
    html_url TEXT,
    head_ref TEXT,
    base_ref TEXT,
    extracted_at TEXT,
    author_json TEXT,
    author_login VARCHAR(255),
    assignees_json TEXT,
    labels_json TEXT,
    comments_count BIGINT,
    locked BOOLEAN,
    requested_reviewers_json TEXT,
    node_id VARCHAR(255),
    PRIMARY KEY (tenant_id, id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_pull_requests_tenant_repo_number
    ON gm_pull_requests (tenant_id, repo_id, number);

CREATE INDEX IF NOT EXISTS idx_gm_pull_requests_tenant_repo_state
    ON gm_pull_requests (tenant_id, repo_id, state);

CREATE TABLE IF NOT EXISTS gm_releases (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    tag_name TEXT NOT NULL,
    name TEXT,
    draft INTEGER NOT NULL,
    prerelease INTEGER NOT NULL,
    body TEXT,
    author_login TEXT,
    created_at TEXT NOT NULL,
    published_at TEXT,
    html_url TEXT,
    extracted_at TEXT,
    assets_json TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_releases_tenant_repo
    ON gm_releases (tenant_id, repo_id);

CREATE TABLE IF NOT EXISTS gm_repo_sync_status (
    tenant_id TEXT NOT NULL,
    repo_full_name TEXT NOT NULL,
    repo_id INTEGER,
    status TEXT NOT NULL,
    last_session_id TEXT,
    last_synced_at TEXT,
    PRIMARY KEY (tenant_id, repo_full_name)
);

CREATE INDEX IF NOT EXISTS idx_gm_repo_sync_status_tenant_status
    ON gm_repo_sync_status (tenant_id, status);

CREATE TABLE IF NOT EXISTS gm_repositories (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    full_name TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    private INTEGER NOT NULL,
    pushed_at TEXT,
    stars INTEGER NOT NULL DEFAULT 0,
    forks INTEGER NOT NULL DEFAULT 0,
    description TEXT,
    clone_url TEXT,
    extracted_at TEXT,
    node_id VARCHAR(255),
    PRIMARY KEY (tenant_id, id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_repositories_tenant_full_name
    ON gm_repositories (tenant_id, full_name);

CREATE TABLE IF NOT EXISTS gm_review_comments (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    author_login TEXT,
    body TEXT,
    path TEXT,
    diff_hunk TEXT,
    in_reply_to_id INTEGER,
    commit_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    html_url TEXT,
    position INTEGER,
    original_position INTEGER,
    extracted_at TEXT,
    line BIGINT,
    original_line BIGINT,
    start_line BIGINT,
    original_start_line BIGINT,
    side VARCHAR(32),
    start_side VARCHAR(32),
    subject_type VARCHAR(32),
    pull_request_review_id BIGINT,
    conversation_id BIGINT,
    snippet_before TEXT,
    snippet_after TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_review_comments_tenant_repo_pull
    ON gm_review_comments (tenant_id, repo_id, pull_number);

CREATE TABLE IF NOT EXISTS gm_review_threads (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    is_resolved INTEGER NOT NULL,
    is_outdated INTEGER NOT NULL,
    path TEXT,
    line INTEGER,
    resolved_by TEXT,
    comments_count INTEGER NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_review_threads_tenant_repo_pull
    ON gm_review_threads (tenant_id, repo_id, pull_number);

CREATE TABLE IF NOT EXISTS gm_reviews (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    author_login TEXT,
    state TEXT NOT NULL,
    body TEXT,
    commit_id TEXT,
    submitted_at TEXT,
    html_url TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_reviews_tenant_repo_pull
    ON gm_reviews (tenant_id, repo_id, pull_number);

CREATE TABLE IF NOT EXISTS gm_sync_sessions (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    repo_full_name TEXT NOT NULL,
    repo_id INTEGER,
    status TEXT NOT NULL,
    progress_percent INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    summary_json TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    ended_at TEXT,
    updated_at TEXT,
    telemetry_json TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_sync_sessions_status
    ON gm_sync_sessions (status);

CREATE INDEX IF NOT EXISTS idx_gm_sync_sessions_tenant_created
    ON gm_sync_sessions (tenant_id, created_at);

CREATE TABLE IF NOT EXISTS gm_sync_watermarks (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    family TEXT NOT NULL,
    last_seen_updated_at TEXT,
    page1_etag TEXT,
    last_head_sha TEXT,
    sweep_in_progress BOOLEAN NOT NULL,
    candidate_high_water TEXT,
    stage_etags TEXT,
    PRIMARY KEY (tenant_id, repo_id, family)
);

CREATE TABLE IF NOT EXISTS gm_tags (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, name)
);

CREATE TABLE IF NOT EXISTS gm_workflow_jobs (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    run_id INTEGER NOT NULL,
    run_attempt INTEGER NOT NULL,
    name TEXT NOT NULL,
    status TEXT,
    conclusion TEXT,
    head_sha TEXT NOT NULL,
    runner_name TEXT,
    started_at TEXT,
    completed_at TEXT,
    html_url TEXT,
    steps_json TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_workflow_jobs_tenant_repo_run
    ON gm_workflow_jobs (tenant_id, repo_id, run_id);

CREATE TABLE IF NOT EXISTS gm_workflow_runs (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    workflow_id INTEGER NOT NULL,
    run_number INTEGER NOT NULL,
    run_attempt INTEGER NOT NULL,
    name TEXT,
    event TEXT NOT NULL,
    status TEXT,
    conclusion TEXT,
    head_branch TEXT,
    head_sha TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    html_url TEXT,
    actor_login TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_gm_workflow_runs_tenant_repo
    ON gm_workflow_runs (tenant_id, repo_id);
