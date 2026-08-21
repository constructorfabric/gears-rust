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

pub mod pull_requests {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub pull request, tenant-scoped like every mirror table.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_pull_requests")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub pull-request id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        /// Owning repository's GitHub id.
        pub repo_id: i64,
        pub number: i64,
        pub title: String,
        pub body: Option<String>,
        pub state: String,
        pub draft: bool,
        pub merged: bool,
        pub head_sha: Option<String>,
        pub base_sha: Option<String>,
        pub lines_added: i64,
        pub lines_removed: i64,
        /// RFC3339 timestamps kept as text (engine-agnostic), as in the reference.
        pub created_at: String,
        pub updated_at: String,
        pub closed_at: Option<String>,
        pub merged_at: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod commits {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub commit, tenant-scoped like every mirror table.
    ///
    /// Commits have no numeric GitHub id, so the key is
    /// `(tenant_id, repo_id, sha)` — the reference keys by `(repo_id, sha)`.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_commits")]
    #[secure(tenant_col = "tenant_id", resource_col = "sha", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// Owning repository's GitHub id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub repo_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub sha: String,
        pub message: String,
        pub author_login: Option<String>,
        pub committer_login: Option<String>,
        /// RFC3339 timestamps kept as text (engine-agnostic), as in the reference.
        pub authored_at: Option<String>,
        pub committed_at: Option<String>,
        pub additions: i64,
        pub deletions: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod comments {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub issue/PR comment, tenant-scoped like every mirror table.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_comments")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub comment id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        /// Owning repository's GitHub id.
        pub repo_id: i64,
        /// Owning issue/PR number.
        pub issue_number: i64,
        pub author_login: Option<String>,
        pub body: Option<String>,
        /// RFC3339 timestamps kept as text (engine-agnostic), as in the reference.
        pub created_at: String,
        pub updated_at: String,
        pub html_url: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod review_comments {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub PR review comment (inline diff comment), tenant-scoped.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_review_comments")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub review-comment id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        /// Owning repository's GitHub id.
        pub repo_id: i64,
        /// Owning pull-request number.
        pub pull_number: i64,
        pub author_login: Option<String>,
        pub body: Option<String>,
        pub path: Option<String>,
        pub diff_hunk: Option<String>,
        pub in_reply_to_id: Option<i64>,
        pub commit_id: Option<String>,
        /// RFC3339 timestamps kept as text (engine-agnostic), as in the reference.
        pub created_at: String,
        pub updated_at: String,
        pub html_url: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod reviews {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub PR review (the verdict object), tenant-scoped.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_reviews")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub review id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        /// Owning repository's GitHub id.
        pub repo_id: i64,
        /// Owning pull-request number.
        pub pull_number: i64,
        pub author_login: Option<String>,
        /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED`, or `PENDING`.
        pub state: String,
        pub body: Option<String>,
        pub commit_id: Option<String>,
        /// RFC3339 timestamp kept as text; absent for PENDING reviews.
        pub submitted_at: Option<String>,
        pub html_url: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod labels {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub issue/PR label, tenant-scoped.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_labels")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub label id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        /// Owning repository's GitHub id.
        pub repo_id: i64,
        pub name: String,
        /// Hex color without the leading `#`.
        pub color: String,
        /// True for GitHub's default label set.
        pub is_default: bool,
        pub description: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod milestones {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub milestone, tenant-scoped.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_milestones")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub milestone id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        /// Owning repository's GitHub id.
        pub repo_id: i64,
        /// Milestone number within the repository.
        pub number: i64,
        pub title: String,
        /// open or closed.
        pub state: String,
        pub description: Option<String>,
        pub open_issues: i64,
        pub closed_issues: i64,
        /// RFC3339 timestamps kept as text (engine-agnostic), as in the reference.
        pub due_on: Option<String>,
        pub created_at: String,
        pub updated_at: String,
        pub closed_at: Option<String>,
        pub html_url: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod releases {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub release, tenant-scoped.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_releases")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// GitHub release id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        /// Owning repository's GitHub id.
        pub repo_id: i64,
        pub tag_name: String,
        pub name: Option<String>,
        pub draft: bool,
        pub prerelease: bool,
        pub body: Option<String>,
        pub author_login: Option<String>,
        /// RFC3339 timestamps kept as text (engine-agnostic), as in the reference.
        pub created_at: String,
        /// Absent for drafts.
        pub published_at: Option<String>,
        pub html_url: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod branches {
    use super::{DeriveEntityModel, DerivePrimaryKey, DeriveRelation, EnumIter, Scopable, Uuid};
    use sea_orm::entity::prelude::*;

    /// Mirrored GitHub branch head, tenant-scoped.
    ///
    /// Branches have no numeric GitHub id, so the key is
    /// `(tenant_id, repo_id, name)` — like `gm_commits` keys by sha.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "gm_branches")]
    #[secure(tenant_col = "tenant_id", resource_col = "name", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        /// Owning repository's GitHub id.
        #[sea_orm(primary_key, auto_increment = false)]
        pub repo_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub name: String,
        /// SHA the branch currently points at.
        pub commit_sha: String,
        pub protected: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
