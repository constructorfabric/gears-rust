use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

pub mod repositories {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub repository, tenant-scoped from the start (gears-rust#4536).
    ///
    /// The reference implementation keys this table by the GitHub repository id
    /// alone; the gear keys it by `(tenant_id, id)` so two tenants can mirror
    /// the same repository without collision.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_repositories")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub repository id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub owner: String,
        pub name: String,
        /// `owner/name` slug.
        pub full_name: String,
        pub default_branch: String,
        pub private: bool,
        /// RFC3339 push timestamp, if known (kept as text: engine-agnostic).
        pub pushed_at: Option<String>,
        pub stars: i64,
        pub forks: i64,
        pub description: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod issues {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub issue (pull requests included, flagged by
    /// `is_pull_request`), tenant-scoped like every mirror table.
    ///
    /// Keys: `(tenant_id, id)` primary key mirrors `gm_repositories`; a unique
    /// index on `(tenant_id, repo_id, number)` covers the natural GitHub
    /// lookup path.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_issues")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub issue id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        /// Owning repository's GitHub id.
        pub repo_id: i64,
        pub number: i64,
        pub title: String,
        pub body: Option<String>,
        pub state: String,
        pub is_pull_request: bool,
        /// RFC3339 timestamps kept as text (engine-agnostic), as in the reference.
        pub created_at: String,
        pub updated_at: String,
        pub closed_at: Option<String>,
        pub html_url: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
