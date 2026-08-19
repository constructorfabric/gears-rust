use github_mirror_sdk::Repository;

use super::entity::repositories;

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
