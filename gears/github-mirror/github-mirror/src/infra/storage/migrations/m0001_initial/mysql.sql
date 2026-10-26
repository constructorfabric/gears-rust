CREATE TABLE IF NOT EXISTS gm_active_syncs (
    tenant_id BINARY(16) NOT NULL,
    repo_full_name VARCHAR(512) NOT NULL,
    session_id BINARY(16) NOT NULL,
    owner_id BINARY(16) NOT NULL,
    scope_json TEXT NOT NULL,
    since VARCHAR(64),
    updated_at VARCHAR(64) NOT NULL,
    PRIMARY KEY (tenant_id, repo_full_name)
);

CREATE TABLE IF NOT EXISTS gm_branches (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    name VARCHAR(512) NOT NULL,
    commit_sha VARCHAR(64) NOT NULL,
    protected BOOLEAN NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, name)
);

CREATE TABLE IF NOT EXISTS gm_check_runs (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    head_sha VARCHAR(64) NOT NULL,
    name VARCHAR(1024) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    started_at VARCHAR(64),
    completed_at VARCHAR(64),
    html_url VARCHAR(1024),
    details_url VARCHAR(1024),
    check_suite_id BIGINT,
    app_slug VARCHAR(255),
    app_name VARCHAR(255),
    output_title VARCHAR(1024),
    output_summary MEDIUMTEXT,
    annotations_count BIGINT NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_check_runs_tenant_repo_sha (tenant_id, repo_id, head_sha)
);

CREATE TABLE IF NOT EXISTS gm_comments (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    issue_number BIGINT NOT NULL,
    author_login VARCHAR(255),
    body MEDIUMTEXT,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    html_url VARCHAR(1024),
    extracted_at DATETIME(6),
    conversation_id BIGINT,
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_comments_tenant_repo_issue (tenant_id, repo_id, issue_number)
);

CREATE TABLE IF NOT EXISTS gm_commit_comments (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    commit_sha VARCHAR(64) NOT NULL,
    path VARCHAR(1024),
    position BIGINT,
    author_login VARCHAR(255),
    body MEDIUMTEXT,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    html_url VARCHAR(1024),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_commit_comments_tenant_repo_sha (tenant_id, repo_id, commit_sha)
);

CREATE TABLE IF NOT EXISTS gm_commit_files (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    commit_sha VARCHAR(64) NOT NULL,
    filename VARCHAR(640) NOT NULL,
    status VARCHAR(32) NOT NULL,
    additions BIGINT NOT NULL,
    deletions BIGINT NOT NULL,
    changes BIGINT NOT NULL,
    previous_filename VARCHAR(1024),
    sha VARCHAR(64),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, commit_sha, filename)
);

CREATE TABLE IF NOT EXISTS gm_commit_statuses (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    commit_sha VARCHAR(64) NOT NULL,
    state VARCHAR(32) NOT NULL,
    context VARCHAR(512) NOT NULL,
    description MEDIUMTEXT,
    target_url VARCHAR(1024),
    creator_login VARCHAR(255),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_commit_statuses_tenant_repo_sha (tenant_id, repo_id, commit_sha)
);

CREATE TABLE IF NOT EXISTS gm_commits (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    sha VARCHAR(64) NOT NULL,
    message MEDIUMTEXT NOT NULL,
    author_login VARCHAR(255),
    committer_login VARCHAR(255),
    authored_at VARCHAR(64),
    committed_at VARCHAR(64),
    additions BIGINT NOT NULL DEFAULT 0,
    deletions BIGINT NOT NULL DEFAULT 0,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, sha),
    KEY idx_gm_commits_tenant_repo_committed (tenant_id, repo_id, committed_at)
);

CREATE TABLE IF NOT EXISTS gm_contributors (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    login VARCHAR(255) NOT NULL,
    account_type VARCHAR(32) NOT NULL,
    avatar_url VARCHAR(1024),
    html_url VARCHAR(1024),
    extracted_at DATETIME(6),
    roles MEDIUMTEXT,
    first_seen_at DATETIME(6),
    last_seen_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, user_id)
);

CREATE TABLE IF NOT EXISTS gm_deployments (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    git_ref VARCHAR(512) NOT NULL,
    sha VARCHAR(64) NOT NULL,
    environment VARCHAR(255) NOT NULL,
    task VARCHAR(255) NOT NULL,
    description MEDIUMTEXT,
    creator_login VARCHAR(255),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_deployments_tenant_repo (tenant_id, repo_id)
);

CREATE TABLE IF NOT EXISTS gm_entity_fingerprints (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    family VARCHAR(64) NOT NULL,
    entity_id VARCHAR(128) NOT NULL,
    fingerprint VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64),
    node_id VARCHAR(128),
    child_counts_hash VARCHAR(64),
    last_refined_at VARCHAR(64),
    refinement_status VARCHAR(16) NOT NULL,
    PRIMARY KEY (tenant_id, repo_id, family, entity_id)
);

CREATE TABLE IF NOT EXISTS gm_http_cache (
    tenant_id BINARY(16) NOT NULL,
    cache_key VARCHAR(64) NOT NULL,
    url TEXT NOT NULL,
    status INT NOT NULL,
    etag VARCHAR(255),
    last_modified VARCHAR(64),
    next_page TEXT,
    body LONGBLOB NOT NULL,
    compression VARCHAR(8) NOT NULL,
    content_hash VARCHAR(64) NOT NULL,
    fetched_at VARCHAR(64) NOT NULL,
    PRIMARY KEY (tenant_id, cache_key)
);

CREATE TABLE IF NOT EXISTS gm_issue_events (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    issue_number BIGINT NOT NULL,
    event VARCHAR(64) NOT NULL,
    actor_login VARCHAR(255),
    label_name VARCHAR(255),
    assignee_login VARCHAR(255),
    milestone_title VARCHAR(1024),
    commit_id VARCHAR(64),
    created_at VARCHAR(64) NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_issue_events_tenant_repo_issue (tenant_id, repo_id, issue_number)
);

CREATE TABLE IF NOT EXISTS gm_issue_reactions (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    issue_number BIGINT NOT NULL,
    content VARCHAR(32) NOT NULL,
    user_login VARCHAR(255),
    created_at VARCHAR(64) NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_issue_reactions_tenant_repo_issue (tenant_id, repo_id, issue_number)
);

CREATE TABLE IF NOT EXISTS gm_issue_timeline (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    issue_number BIGINT NOT NULL,
    position BIGINT NOT NULL,
    event VARCHAR(64) NOT NULL,
    created_at VARCHAR(64),
    actor_login VARCHAR(255),
    payload_json MEDIUMTEXT NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, issue_number, position)
);

CREATE TABLE IF NOT EXISTS gm_issues (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    body MEDIUMTEXT,
    state VARCHAR(32) NOT NULL,
    is_pull_request BOOLEAN NOT NULL,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    html_url VARCHAR(1024),
    extracted_at DATETIME(6),
    author_json MEDIUMTEXT,
    author_login VARCHAR(255),
    assignees_json MEDIUMTEXT,
    labels_json MEDIUMTEXT,
    comments_count BIGINT,
    locked BOOLEAN,
    node_id VARCHAR(255),
    PRIMARY KEY (tenant_id, id),
    UNIQUE KEY idx_gm_issues_tenant_repo_number (tenant_id, repo_id, number),
    KEY idx_gm_issues_tenant_repo_state (tenant_id, repo_id, state)
);

CREATE TABLE IF NOT EXISTS gm_labels (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    name VARCHAR(255) NOT NULL,
    color VARCHAR(16) NOT NULL,
    is_default BOOLEAN NOT NULL,
    description VARCHAR(1024),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_labels_tenant_repo (tenant_id, repo_id)
);

CREATE TABLE IF NOT EXISTS gm_logical_conversations (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    conv_type VARCHAR(16) NOT NULL,
    root_comment_id BIGINT NOT NULL,
    parent_kind VARCHAR(16) NOT NULL,
    parent_number BIGINT NOT NULL,
    comment_count BIGINT NOT NULL,
    is_resolved BOOLEAN,
    created_at VARCHAR(64),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, conv_type, root_comment_id),
    KEY idx_gm_logical_conversations_parent (tenant_id, repo_id, parent_number)
);

CREATE TABLE IF NOT EXISTS gm_milestones (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    state VARCHAR(32) NOT NULL,
    description MEDIUMTEXT,
    open_issues BIGINT NOT NULL,
    closed_issues BIGINT NOT NULL,
    due_on VARCHAR(64),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    html_url VARCHAR(1024),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_milestones_tenant_repo (tenant_id, repo_id)
);

CREATE TABLE IF NOT EXISTS gm_pull_request_commits (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    sha VARCHAR(64) NOT NULL,
    message MEDIUMTEXT NOT NULL,
    author_login VARCHAR(255),
    committer_login VARCHAR(255),
    authored_at VARCHAR(64),
    committed_at VARCHAR(64),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, pull_number, sha)
);

CREATE TABLE IF NOT EXISTS gm_pull_request_files (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    filename VARCHAR(728) NOT NULL,
    status VARCHAR(32) NOT NULL,
    additions BIGINT NOT NULL,
    deletions BIGINT NOT NULL,
    changes BIGINT NOT NULL,
    previous_filename VARCHAR(1024),
    sha VARCHAR(64),
    extracted_at DATETIME(6),
    patch MEDIUMTEXT,
    PRIMARY KEY (tenant_id, repo_id, pull_number, filename)
);

CREATE TABLE IF NOT EXISTS gm_pull_requests (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    body MEDIUMTEXT,
    state VARCHAR(32) NOT NULL,
    draft BOOLEAN NOT NULL,
    merged BOOLEAN NOT NULL,
    head_sha VARCHAR(64),
    base_sha VARCHAR(64),
    lines_added BIGINT NOT NULL DEFAULT 0,
    lines_removed BIGINT NOT NULL DEFAULT 0,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    merged_at VARCHAR(64),
    html_url VARCHAR(1024),
    head_ref VARCHAR(1024),
    base_ref VARCHAR(1024),
    extracted_at DATETIME(6),
    author_json MEDIUMTEXT,
    author_login VARCHAR(255),
    assignees_json MEDIUMTEXT,
    labels_json MEDIUMTEXT,
    comments_count BIGINT,
    locked BOOLEAN,
    requested_reviewers_json MEDIUMTEXT,
    node_id VARCHAR(255),
    PRIMARY KEY (tenant_id, id),
    UNIQUE KEY idx_gm_pull_requests_tenant_repo_number (tenant_id, repo_id, number),
    KEY idx_gm_pull_requests_tenant_repo_state (tenant_id, repo_id, state)
);

CREATE TABLE IF NOT EXISTS gm_releases (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    tag_name VARCHAR(255) NOT NULL,
    name VARCHAR(1024),
    draft BOOLEAN NOT NULL,
    prerelease BOOLEAN NOT NULL,
    body MEDIUMTEXT,
    author_login VARCHAR(255),
    created_at VARCHAR(64) NOT NULL,
    published_at VARCHAR(64),
    html_url VARCHAR(1024),
    extracted_at DATETIME(6),
    assets_json TEXT,
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_releases_tenant_repo (tenant_id, repo_id)
);

CREATE TABLE IF NOT EXISTS gm_repo_sync_status (
    tenant_id BINARY(16) NOT NULL,
    repo_full_name VARCHAR(512) NOT NULL,
    repo_id BIGINT,
    status VARCHAR(32) NOT NULL,
    last_session_id BINARY(16),
    last_synced_at VARCHAR(64),
    PRIMARY KEY (tenant_id, repo_full_name),
    KEY idx_gm_repo_sync_status_tenant_status (tenant_id, status)
);

CREATE TABLE IF NOT EXISTS gm_repositories (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    owner VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    full_name VARCHAR(191) NOT NULL,
    default_branch VARCHAR(255) NOT NULL,
    private BOOLEAN NOT NULL,
    pushed_at VARCHAR(64),
    stars BIGINT NOT NULL DEFAULT 0,
    forks BIGINT NOT NULL DEFAULT 0,
    description MEDIUMTEXT,
    clone_url VARCHAR(1024),
    extracted_at DATETIME(6),
    node_id VARCHAR(255),
    PRIMARY KEY (tenant_id, id),
    UNIQUE KEY idx_gm_repositories_tenant_full_name (tenant_id, full_name)
);

CREATE TABLE IF NOT EXISTS gm_review_comments (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    author_login VARCHAR(255),
    body MEDIUMTEXT,
    path VARCHAR(1024),
    diff_hunk MEDIUMTEXT,
    in_reply_to_id BIGINT,
    commit_id VARCHAR(64),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    html_url VARCHAR(1024),
    position BIGINT,
    original_position BIGINT,
    extracted_at DATETIME(6),
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
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_review_comments_tenant_repo_pull (tenant_id, repo_id, pull_number)
);

CREATE TABLE IF NOT EXISTS gm_review_threads (
    tenant_id BINARY(16) NOT NULL,
    id VARCHAR(128) NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    is_resolved BOOLEAN NOT NULL,
    is_outdated BOOLEAN NOT NULL,
    path VARCHAR(1024),
    line BIGINT,
    resolved_by VARCHAR(255),
    comments_count BIGINT NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_review_threads_tenant_repo_pull (tenant_id, repo_id, pull_number)
);

CREATE TABLE IF NOT EXISTS gm_reviews (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    author_login VARCHAR(255),
    state VARCHAR(32) NOT NULL,
    body MEDIUMTEXT,
    commit_id VARCHAR(64),
    submitted_at VARCHAR(64),
    html_url VARCHAR(1024),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_reviews_tenant_repo_pull (tenant_id, repo_id, pull_number)
);

CREATE TABLE IF NOT EXISTS gm_sync_sessions (
    tenant_id BINARY(16) NOT NULL,
    id BINARY(16) NOT NULL,
    repo_full_name VARCHAR(512) NOT NULL,
    repo_id BIGINT,
    status VARCHAR(32) NOT NULL,
    progress_percent INT NOT NULL DEFAULT 0,
    error TEXT,
    summary_json TEXT,
    created_at VARCHAR(64) NOT NULL,
    started_at VARCHAR(64),
    ended_at VARCHAR(64),
    updated_at VARCHAR(64),
    telemetry_json TEXT,
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_sync_sessions_tenant_created (tenant_id, created_at),
    KEY idx_gm_sync_sessions_status (status)
);

CREATE TABLE IF NOT EXISTS gm_sync_watermarks (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    family VARCHAR(64) NOT NULL,
    last_seen_updated_at VARCHAR(64),
    page1_etag VARCHAR(255),
    last_head_sha VARCHAR(64),
    sweep_in_progress BOOLEAN NOT NULL,
    candidate_high_water VARCHAR(64),
    stage_etags TEXT,
    PRIMARY KEY (tenant_id, repo_id, family)
);

CREATE TABLE IF NOT EXISTS gm_tags (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    name VARCHAR(512) NOT NULL,
    commit_sha VARCHAR(64) NOT NULL,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, name)
);

CREATE TABLE IF NOT EXISTS gm_workflow_jobs (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    run_id BIGINT NOT NULL,
    run_attempt BIGINT NOT NULL,
    name VARCHAR(1024) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    head_sha VARCHAR(64) NOT NULL,
    runner_name VARCHAR(255),
    started_at VARCHAR(64),
    completed_at VARCHAR(64),
    html_url VARCHAR(1024),
    steps_json MEDIUMTEXT,
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_workflow_jobs_tenant_repo_run (tenant_id, repo_id, run_id)
);

CREATE TABLE IF NOT EXISTS gm_workflow_runs (
    tenant_id BINARY(16) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    workflow_id BIGINT NOT NULL,
    run_number BIGINT NOT NULL,
    run_attempt BIGINT NOT NULL,
    name VARCHAR(1024),
    event VARCHAR(64) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    head_branch VARCHAR(512),
    head_sha VARCHAR(64) NOT NULL,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    html_url VARCHAR(1024),
    actor_login VARCHAR(255),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_workflow_runs_tenant_repo (tenant_id, repo_id)
);
