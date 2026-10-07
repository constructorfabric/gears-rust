use sea_orm_migration::prelude::*;

pub mod m0001_initial;
pub mod m0002_issues;
pub mod m0003_pull_requests;
pub mod m0004_commits;
pub mod m0005_comments;
pub mod m0006_review_comments;
pub mod m0007_reviews;
pub mod m0008_labels;
pub mod m0009_milestones;
pub mod m0010_releases;
pub mod m0011_branches;
pub mod m0012_contributors;
pub mod m0013_workflow_runs;
pub mod m0014_pull_request_files;
pub mod m0015_tags;
pub mod m0016_commit_files;
pub mod m0017_review_threads;
pub mod m0018_commit_comments;
pub mod m0019_issue_events;
pub mod m0020_deployments;
pub mod m0021_pull_request_commits;
pub mod m0022_commit_statuses;
pub mod m0023_workflow_jobs;
pub mod m0024_issue_reactions;
pub mod m0025_check_runs;
pub mod m0026_issue_timeline;
pub mod m0027_repo_clone_url;
pub mod m0028_pull_requests_refs;
pub mod m0029_review_comments_diff_anchors;
pub mod m0030_sync_sessions;
pub mod m0031_sync_watermarks;
pub mod m0032_entity_fingerprints;
pub mod m0033_repo_sync_status;
pub mod m0034_http_cache;
pub mod m0035_extracted_at;
pub mod m0036_contributor_derivation;
pub mod m0037_release_assets;
pub mod m0038_issue_pull_author;
pub mod m0039_issue_pull_people;
pub mod m0040_node_id;
pub mod m0041_pull_request_file_patch;
pub mod m0042_review_comment_anchors;
pub mod m0043_review_comment_review_id;
pub mod support;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_initial::Migration),
            Box::new(m0002_issues::Migration),
            Box::new(m0003_pull_requests::Migration),
            Box::new(m0004_commits::Migration),
            Box::new(m0005_comments::Migration),
            Box::new(m0006_review_comments::Migration),
            Box::new(m0007_reviews::Migration),
            Box::new(m0008_labels::Migration),
            Box::new(m0009_milestones::Migration),
            Box::new(m0010_releases::Migration),
            Box::new(m0011_branches::Migration),
            Box::new(m0012_contributors::Migration),
            Box::new(m0013_workflow_runs::Migration),
            Box::new(m0014_pull_request_files::Migration),
            Box::new(m0015_tags::Migration),
            Box::new(m0016_commit_files::Migration),
            Box::new(m0017_review_threads::Migration),
            Box::new(m0018_commit_comments::Migration),
            Box::new(m0019_issue_events::Migration),
            Box::new(m0020_deployments::Migration),
            Box::new(m0021_pull_request_commits::Migration),
            Box::new(m0022_commit_statuses::Migration),
            Box::new(m0023_workflow_jobs::Migration),
            Box::new(m0024_issue_reactions::Migration),
            Box::new(m0025_check_runs::Migration),
            Box::new(m0026_issue_timeline::Migration),
            Box::new(m0027_repo_clone_url::Migration),
            Box::new(m0028_pull_requests_refs::Migration),
            Box::new(m0029_review_comments_diff_anchors::Migration),
            Box::new(m0030_sync_sessions::Migration),
            Box::new(m0031_sync_watermarks::Migration),
            Box::new(m0032_entity_fingerprints::Migration),
            Box::new(m0033_repo_sync_status::Migration),
            Box::new(m0034_http_cache::Migration),
            Box::new(m0035_extracted_at::Migration),
            Box::new(m0036_contributor_derivation::Migration),
            Box::new(m0037_release_assets::Migration),
            Box::new(m0038_issue_pull_author::Migration),
            Box::new(m0039_issue_pull_people::Migration),
            Box::new(m0040_node_id::Migration),
            Box::new(m0041_pull_request_file_patch::Migration),
            Box::new(m0042_review_comment_anchors::Migration),
            Box::new(m0043_review_comment_review_id::Migration),
        ]
    }
}
