use github_mirror_sdk::{Issue, Repository};

use super::entity::{issues, repositories};

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
