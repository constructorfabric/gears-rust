use github_mirror_sdk::{
    Branch, CheckRun, Comment, Commit, CommitComment, CommitFile, CommitStatus, Contributor,
    Deployment, Issue, IssueEvent, IssueReaction, IssueTimelineEvent, Label, Milestone,
    PullRequest, PullRequestCommit, PullRequestFile, Release, Repo, Review, ReviewComment,
    ReviewThread, Tag, WorkflowJob, WorkflowRun,
};

use super::entity::{
    branches, check_runs, comments, commit_comments, commit_files, commit_statuses, commits,
    contributors, deployments, issue_events, issue_reactions, issue_timeline, issues, labels,
    milestones, pull_request_commits, pull_request_files, pull_requests, releases, repositories,
    review_comments, review_threads, reviews, tags, workflow_jobs, workflow_runs,
};

impl From<repositories::Model> for Repo {
    fn from(m: repositories::Model) -> Self {
        Self {
            id: m.id,
            node_id: m.node_id,
            owner: m.owner,
            name: m.name,
            full_name: m.full_name,
            default_branch: m.default_branch,
            private: m.private,
            pushed_at: m.pushed_at,
            stars: m.stars,
            forks: m.forks,
            description: m.description,
            clone_url: m.clone_url,
        }
    }
}

impl From<issues::Model> for Issue {
    fn from(m: issues::Model) -> Self {
        Self {
            id: m.id,
            node_id: m.node_id,
            repo_id: m.repo_id,
            number: m.number,
            title: m.title,
            body: m.body,
            state: m.state,
            is_pull_request: m.is_pull_request,
            created_at: m.created_at,
            updated_at: m.updated_at,
            closed_at: m.closed_at,
            html_url: m.html_url,
            author_login: m.author_login,
            author_json: m.author_json,
            assignees_json: m.assignees_json,
            labels_json: m.labels_json,
            comments_count: m.comments_count,
            locked: m.locked,
        }
    }
}

impl From<pull_requests::Model> for PullRequest {
    fn from(m: pull_requests::Model) -> Self {
        Self {
            id: m.id,
            node_id: m.node_id,
            repo_id: m.repo_id,
            number: m.number,
            title: m.title,
            body: m.body,
            state: m.state,
            draft: m.draft,
            merged: m.merged,
            head_sha: m.head_sha,
            base_sha: m.base_sha,
            lines_added: m.lines_added,
            lines_removed: m.lines_removed,
            created_at: m.created_at,
            updated_at: m.updated_at,
            closed_at: m.closed_at,
            merged_at: m.merged_at,
            html_url: m.html_url,
            head_ref: m.head_ref,
            base_ref: m.base_ref,
            author_login: m.author_login,
            author_json: m.author_json,
            assignees_json: m.assignees_json,
            labels_json: m.labels_json,
            comments_count: m.comments_count,
            locked: m.locked,
            requested_reviewers_json: m.requested_reviewers_json,
        }
    }
}

impl From<commits::Model> for Commit {
    fn from(m: commits::Model) -> Self {
        Self {
            repo_id: m.repo_id,
            sha: m.sha,
            message: m.message,
            author_login: m.author_login,
            committer_login: m.committer_login,
            authored_at: m.authored_at,
            committed_at: m.committed_at,
            additions: m.additions,
            deletions: m.deletions,
        }
    }
}

impl From<comments::Model> for Comment {
    fn from(m: comments::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            issue_number: m.issue_number,
            author_login: m.author_login,
            body: m.body,
            created_at: m.created_at,
            updated_at: m.updated_at,
            html_url: m.html_url,
        }
    }
}

impl From<review_comments::Model> for ReviewComment {
    fn from(m: review_comments::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            pull_number: m.pull_number,
            author_login: m.author_login,
            body: m.body,
            path: m.path,
            diff_hunk: m.diff_hunk,
            in_reply_to_id: m.in_reply_to_id,
            commit_id: m.commit_id,
            created_at: m.created_at,
            updated_at: m.updated_at,
            html_url: m.html_url,
            position: m.position,
            original_position: m.original_position,
            line: m.line,
            original_line: m.original_line,
            start_line: m.start_line,
            original_start_line: m.original_start_line,
            side: m.side,
            start_side: m.start_side,
            subject_type: m.subject_type,
            pull_request_review_id: m.pull_request_review_id,
        }
    }
}

impl From<reviews::Model> for Review {
    fn from(m: reviews::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            pull_number: m.pull_number,
            author_login: m.author_login,
            state: m.state,
            body: m.body,
            commit_id: m.commit_id,
            submitted_at: m.submitted_at,
            html_url: m.html_url,
        }
    }
}

impl From<labels::Model> for Label {
    fn from(m: labels::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            name: m.name,
            color: m.color,
            is_default: m.is_default,
            description: m.description,
        }
    }
}

impl From<milestones::Model> for Milestone {
    fn from(m: milestones::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            number: m.number,
            title: m.title,
            state: m.state,
            description: m.description,
            open_issues: m.open_issues,
            closed_issues: m.closed_issues,
            due_on: m.due_on,
            created_at: m.created_at,
            updated_at: m.updated_at,
            closed_at: m.closed_at,
            html_url: m.html_url,
        }
    }
}

impl From<releases::Model> for Release {
    fn from(m: releases::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            tag_name: m.tag_name,
            name: m.name,
            draft: m.draft,
            prerelease: m.prerelease,
            body: m.body,
            author_login: m.author_login,
            created_at: m.created_at,
            published_at: m.published_at,
            html_url: m.html_url,
            assets_json: m.assets_json,
        }
    }
}

impl From<branches::Model> for Branch {
    fn from(m: branches::Model) -> Self {
        Self {
            repo_id: m.repo_id,
            name: m.name,
            commit_sha: m.commit_sha,
            protected: m.protected,
        }
    }
}

impl From<contributors::Model> for Contributor {
    fn from(m: contributors::Model) -> Self {
        Self {
            repo_id: m.repo_id,
            user_id: m.user_id,
            // '' is the NOT NULL column's stand-in for "no login".
            login: (!m.login.is_empty()).then_some(m.login),
            roles: m
                .roles
                .as_deref()
                .map(|raw| {
                    raw.split(',')
                        .filter(|role| !role.is_empty())
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            first_seen_at: m.first_seen_at,
            last_seen_at: m.last_seen_at,
            account_type: m.account_type,
            avatar_url: m.avatar_url,
            html_url: m.html_url,
        }
    }
}

impl From<workflow_runs::Model> for WorkflowRun {
    fn from(m: workflow_runs::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            workflow_id: m.workflow_id,
            run_number: m.run_number,
            run_attempt: m.run_attempt,
            name: m.name,
            event: m.event,
            status: m.status,
            conclusion: m.conclusion,
            head_branch: m.head_branch,
            head_sha: m.head_sha,
            created_at: m.created_at,
            updated_at: m.updated_at,
            html_url: m.html_url,
            actor_login: m.actor_login,
        }
    }
}

impl From<pull_request_files::Model> for PullRequestFile {
    fn from(m: pull_request_files::Model) -> Self {
        Self {
            repo_id: m.repo_id,
            pull_number: m.pull_number,
            filename: m.filename,
            status: m.status,
            additions: m.additions,
            deletions: m.deletions,
            changes: m.changes,
            previous_filename: m.previous_filename,
            patch: m.patch,
            sha: m.sha,
        }
    }
}

impl From<tags::Model> for Tag {
    fn from(m: tags::Model) -> Self {
        Self {
            repo_id: m.repo_id,
            name: m.name,
            commit_sha: m.commit_sha,
        }
    }
}

impl From<commit_files::Model> for CommitFile {
    fn from(m: commit_files::Model) -> Self {
        Self {
            repo_id: m.repo_id,
            commit_sha: m.commit_sha,
            filename: m.filename,
            status: m.status,
            additions: m.additions,
            deletions: m.deletions,
            changes: m.changes,
            previous_filename: m.previous_filename,
            sha: m.sha,
        }
    }
}

impl From<review_threads::Model> for ReviewThread {
    fn from(m: review_threads::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            pull_number: m.pull_number,
            is_resolved: m.is_resolved,
            is_outdated: m.is_outdated,
            path: m.path,
            line: m.line,
            resolved_by: m.resolved_by,
            comments_count: m.comments_count,
        }
    }
}

impl From<commit_comments::Model> for CommitComment {
    fn from(m: commit_comments::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            commit_sha: m.commit_sha,
            path: m.path,
            position: m.position,
            author_login: m.author_login,
            body: m.body,
            created_at: m.created_at,
            updated_at: m.updated_at,
            html_url: m.html_url,
        }
    }
}

impl From<issue_events::Model> for IssueEvent {
    fn from(m: issue_events::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            issue_number: m.issue_number,
            event: m.event,
            actor_login: m.actor_login,
            label_name: m.label_name,
            assignee_login: m.assignee_login,
            milestone_title: m.milestone_title,
            commit_id: m.commit_id,
            created_at: m.created_at,
        }
    }
}

impl From<deployments::Model> for Deployment {
    fn from(m: deployments::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            git_ref: m.git_ref,
            sha: m.sha,
            environment: m.environment,
            task: m.task,
            description: m.description,
            creator_login: m.creator_login,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<pull_request_commits::Model> for PullRequestCommit {
    fn from(m: pull_request_commits::Model) -> Self {
        Self {
            repo_id: m.repo_id,
            pull_number: m.pull_number,
            sha: m.sha,
            message: m.message,
            author_login: m.author_login,
            committer_login: m.committer_login,
            authored_at: m.authored_at,
            committed_at: m.committed_at,
        }
    }
}

impl From<commit_statuses::Model> for CommitStatus {
    fn from(m: commit_statuses::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            commit_sha: m.commit_sha,
            state: m.state,
            context: m.context,
            description: m.description,
            target_url: m.target_url,
            creator_login: m.creator_login,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<workflow_jobs::Model> for WorkflowJob {
    fn from(m: workflow_jobs::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            run_id: m.run_id,
            run_attempt: m.run_attempt,
            name: m.name,
            status: m.status,
            conclusion: m.conclusion,
            head_sha: m.head_sha,
            runner_name: m.runner_name,
            started_at: m.started_at,
            completed_at: m.completed_at,
            html_url: m.html_url,
            steps_json: m.steps_json,
        }
    }
}

impl From<issue_reactions::Model> for IssueReaction {
    fn from(m: issue_reactions::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            issue_number: m.issue_number,
            content: m.content,
            user_login: m.user_login,
            created_at: m.created_at,
        }
    }
}

impl From<check_runs::Model> for CheckRun {
    fn from(m: check_runs::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            head_sha: m.head_sha,
            name: m.name,
            status: m.status,
            conclusion: m.conclusion,
            started_at: m.started_at,
            completed_at: m.completed_at,
            html_url: m.html_url,
            details_url: m.details_url,
            check_suite_id: m.check_suite_id,
            app_slug: m.app_slug,
            app_name: m.app_name,
            output_title: m.output_title,
            output_summary: m.output_summary,
            annotations_count: m.annotations_count,
        }
    }
}

impl From<issue_timeline::Model> for IssueTimelineEvent {
    fn from(m: issue_timeline::Model) -> Self {
        Self {
            repo_id: m.repo_id,
            issue_number: m.issue_number,
            position: m.position,
            event: m.event,
            created_at: m.created_at,
            actor_login: m.actor_login,
            payload_json: m.payload_json,
        }
    }
}
