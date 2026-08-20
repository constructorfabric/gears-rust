use github_mirror_sdk::{Issue, Repository};

use crate::domain::service::MirrorStatus;

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GithubMirrorHealthDto {
    pub gear: String,
    pub version: String,
    pub api_base_url: String,
}

impl From<MirrorStatus> for GithubMirrorHealthDto {
    fn from(status: MirrorStatus) -> Self {
        Self {
            gear: status.gear,
            version: status.version,
            api_base_url: status.api_base_url,
        }
    }
}

/// A mirrored GitHub repository as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct RepositoryDto {
    pub id: i64,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub description: Option<String>,
}

impl From<Repository> for RepositoryDto {
    fn from(repo: Repository) -> Self {
        Self {
            id: repo.id,
            owner: repo.owner,
            name: repo.name,
            full_name: repo.full_name,
            private: repo.private,
            description: repo.description,
        }
    }
}

/// A mirrored GitHub issue as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct IssueDto {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub is_pull_request: bool,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub html_url: Option<String>,
}

impl From<Issue> for IssueDto {
    fn from(issue: Issue) -> Self {
        Self {
            id: issue.id,
            repo_id: issue.repo_id,
            number: issue.number,
            title: issue.title,
            body: issue.body,
            state: issue.state,
            is_pull_request: issue.is_pull_request,
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            closed_at: issue.closed_at,
            html_url: issue.html_url,
        }
    }
}
