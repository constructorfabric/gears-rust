use github_mirror_sdk::Repository;

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
