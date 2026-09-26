#![allow(clippy::unwrap_used, clippy::expect_used)]

use github_mirror::infra::storage::migrations::Migrator;
use sea_orm_migration::sea_orm::Database;
use sea_orm_migration::{MigratorTrait, SchemaManager};

#[tokio::test]
async fn migrations_apply_and_roll_back_on_a_clean_database() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory database must connect");

    Migrator::up(&conn, None).await.expect("up must succeed");

    let manager = SchemaManager::new(&conn);
    for table in [
        "gm_repositories",
        "gm_issues",
        "gm_pull_requests",
        "gm_commits",
        "gm_comments",
        "gm_review_comments",
        "gm_reviews",
        "gm_labels",
        "gm_milestones",
        "gm_releases",
        "gm_branches",
        "gm_contributors",
        "gm_workflow_runs",
        "gm_pull_request_files",
        "gm_tags",
        "gm_commit_files",
        "gm_review_threads",
        "gm_commit_comments",
        "gm_issue_events",
        "gm_deployments",
        "gm_pull_request_commits",
        "gm_commit_statuses",
        "gm_workflow_jobs",
        "gm_issue_reactions",
        "gm_check_runs",
        "gm_issue_timeline",
        "gm_sync_sessions",
        "gm_sync_watermarks",
        "gm_entity_fingerprints",
        "gm_repo_sync_status",
        "gm_http_cache",
    ] {
        assert!(
            manager.has_table(table).await.unwrap(),
            "{table} must exist after up()"
        );
    }

    // The two additive-column migrations that predate this test, plus the
    // one added alongside review-comment diff anchoring: `up`/`down` for
    // these never touched a whole table, so the table-existence loop above
    // cannot tell a working migration from a no-op one — only checking the
    // column itself can.
    for (table, column) in [
        ("gm_repositories", "clone_url"),
        ("gm_pull_requests", "html_url"),
        ("gm_pull_requests", "head_ref"),
        ("gm_pull_requests", "base_ref"),
        ("gm_review_comments", "position"),
        ("gm_review_comments", "original_position"),
    ] {
        assert!(
            manager.has_column(table, column).await.unwrap(),
            "{table}.{column} must exist after up()"
        );
    }

    // The 26-table extracted_at migration is column-additive: only checking
    // the column itself proves its up() did anything.
    for table in ["gm_issues", "gm_pull_requests", "gm_commits", "gm_labels"] {
        assert!(
            manager.has_column(table, "extracted_at").await.unwrap(),
            "{table}.extracted_at must exist after up()"
        );
    }

    assert!(
        manager
            .has_column("gm_releases", "assets_json")
            .await
            .unwrap(),
        "gm_releases.assets_json must exist after up()"
    );

    // The sync engine's own tables: `has_table` cannot tell a table that
    // carries what the repositories read from one that was created with a
    // column missing or misspelled, so every column production code names is
    // asserted here.
    for (table, column) in [
        ("gm_sync_sessions", "id"),
        ("gm_sync_sessions", "repo_full_name"),
        ("gm_sync_sessions", "repo_id"),
        ("gm_sync_sessions", "status"),
        ("gm_sync_sessions", "progress_percent"),
        ("gm_sync_sessions", "error"),
        ("gm_sync_sessions", "summary_json"),
        ("gm_sync_sessions", "created_at"),
        ("gm_sync_sessions", "started_at"),
        ("gm_sync_sessions", "ended_at"),
        ("gm_sync_watermarks", "repo_id"),
        ("gm_sync_watermarks", "family"),
        ("gm_sync_watermarks", "last_seen_updated_at"),
        ("gm_sync_watermarks", "page1_etag"),
        ("gm_sync_watermarks", "sweep_in_progress"),
        ("gm_sync_watermarks", "candidate_high_water"),
        ("gm_entity_fingerprints", "repo_id"),
        ("gm_entity_fingerprints", "family"),
        ("gm_entity_fingerprints", "entity_id"),
        ("gm_entity_fingerprints", "fingerprint"),
        ("gm_entity_fingerprints", "updated_at"),
        ("gm_entity_fingerprints", "node_id"),
        ("gm_entity_fingerprints", "child_counts_hash"),
        ("gm_entity_fingerprints", "last_refined_at"),
        ("gm_entity_fingerprints", "refinement_status"),
        ("gm_repo_sync_status", "repo_full_name"),
        ("gm_repo_sync_status", "repo_id"),
        ("gm_repo_sync_status", "status"),
        ("gm_repo_sync_status", "last_session_id"),
        ("gm_repo_sync_status", "last_synced_at"),
        ("gm_http_cache", "cache_key"),
        ("gm_http_cache", "url"),
        ("gm_http_cache", "status"),
        ("gm_http_cache", "etag"),
        ("gm_http_cache", "last_modified"),
        ("gm_http_cache", "next_page"),
        ("gm_http_cache", "body"),
        ("gm_http_cache", "compression"),
        ("gm_http_cache", "content_hash"),
        ("gm_http_cache", "fetched_at"),
    ] {
        assert!(
            manager.has_column(table, column).await.unwrap(),
            "{table}.{column} must exist after up()"
        );
    }

    for table in [
        "gm_sync_sessions",
        "gm_sync_watermarks",
        "gm_entity_fingerprints",
        "gm_repo_sync_status",
        "gm_http_cache",
    ] {
        assert!(
            manager.has_column(table, "tenant_id").await.unwrap(),
            "{table}.tenant_id must exist after up(): every read is scoped by it"
        );
    }

    for (table, column) in [
        ("gm_review_comments", "pull_request_review_id"),
        ("gm_review_comments", "line"),
        ("gm_review_comments", "side"),
        ("gm_review_comments", "subject_type"),
        ("gm_pull_request_files", "patch"),
        ("gm_repositories", "node_id"),
        ("gm_issues", "node_id"),
        ("gm_pull_requests", "node_id"),
        ("gm_issues", "author_login"),
        ("gm_issues", "author_json"),
        ("gm_pull_requests", "author_json"),
        ("gm_issues", "labels_json"),
        ("gm_pull_requests", "requested_reviewers_json"),
        ("gm_sync_sessions", "updated_at"),
    ] {
        assert!(
            manager.has_column(table, column).await.unwrap(),
            "{table}.{column} must exist after up()"
        );
    }

    for column in ["roles", "first_seen_at", "last_seen_at"] {
        assert!(
            manager.has_column("gm_contributors", column).await.unwrap(),
            "gm_contributors.{column} must exist after up()"
        );
    }

    for migration in Migrator::migrations().iter().rev() {
        migration.down(&manager).await.expect("down must succeed");
    }

    for table in [
        "gm_repositories",
        "gm_issues",
        "gm_pull_requests",
        "gm_commits",
        "gm_comments",
        "gm_review_comments",
        "gm_reviews",
        "gm_labels",
        "gm_milestones",
        "gm_releases",
        "gm_branches",
        "gm_contributors",
        "gm_workflow_runs",
        "gm_pull_request_files",
        "gm_tags",
        "gm_commit_files",
        "gm_review_threads",
        "gm_commit_comments",
        "gm_issue_events",
        "gm_deployments",
        "gm_pull_request_commits",
        "gm_commit_statuses",
        "gm_workflow_jobs",
        "gm_issue_reactions",
        "gm_check_runs",
        "gm_issue_timeline",
        "gm_sync_sessions",
        "gm_sync_watermarks",
        "gm_entity_fingerprints",
        "gm_repo_sync_status",
        "gm_http_cache",
    ] {
        assert!(
            !manager.has_table(table).await.unwrap(),
            "{table} must be gone after down()"
        );
    }
    // The additive migrations roll back to no table at all (down() for the
    // 26 CREATE TABLE migrations already dropped these tables by this point),
    // so there is nothing further to assert for the individual columns.
}

/// The last ten migrations are the cache table and the nine that only add
/// columns, so they are the first ten to roll back. Undoing exactly those leaves every table in place, which
/// is what makes their `down()` bodies observable: replace one with `Ok(())`
/// and its column survives here.
#[tokio::test]
async fn the_additive_migrations_drop_their_columns_on_rollback() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory database must connect");

    Migrator::up(&conn, None).await.expect("up must succeed");
    Migrator::down(&conn, Some(10))
        .await
        .expect("rolling back the additive migrations must succeed");

    let manager = SchemaManager::new(&conn);
    for (table, column) in [
        ("gm_repositories", "extracted_at"),
        ("gm_issues", "extracted_at"),
        ("gm_contributors", "roles"),
        ("gm_contributors", "first_seen_at"),
        ("gm_contributors", "last_seen_at"),
        ("gm_releases", "assets_json"),
        ("gm_review_comments", "pull_request_review_id"),
        ("gm_review_comments", "line"),
        ("gm_review_comments", "subject_type"),
        ("gm_repositories", "node_id"),
        ("gm_issues", "node_id"),
        ("gm_pull_request_files", "patch"),
        ("gm_issues", "author_login"),
        ("gm_issues", "labels_json"),
        ("gm_issues", "author_json"),
        ("gm_pull_requests", "requested_reviewers_json"),
    ] {
        assert!(
            manager.has_table(table).await.unwrap(),
            "{table} must still exist: only the additive migrations were rolled back"
        );
        assert!(
            !manager.has_column(table, column).await.unwrap(),
            "{table}.{column} must be gone after its migration's down()"
        );
    }
}
