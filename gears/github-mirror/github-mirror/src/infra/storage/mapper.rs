use github_mirror_sdk::{
    Branch, Comment, Commit, Contributor, Issue, Label, Milestone, PullRequest, Release,
    Repository, Review, ReviewComment,
};

use super::entity::{
    branches, comments, commits, contributors, issues, labels, milestones, pull_requests, releases,
    repositories, review_comments, reviews,
};

impl From<repositories::Model> for Repository {
    fn from(m: repositories::Model) -> Self {
        Self {
            id: m.id,
            owner: m.owner,
            name: m.name,
            full_name: m.full_name,
            private: m.private,
            description: m.description,
        }
    }
}

impl From<issues::Model> for Issue {
    fn from(m: issues::Model) -> Self {
        Self {
            id: m.id,
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
        }
    }
}

impl From<pull_requests::Model> for PullRequest {
    fn from(m: pull_requests::Model) -> Self {
        Self {
            id: m.id,
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
            login: m.login,
            contributions: m.contributions,
            user_type: m.user_type,
            avatar_url: m.avatar_url,
            html_url: m.html_url,
        }
    }
}
