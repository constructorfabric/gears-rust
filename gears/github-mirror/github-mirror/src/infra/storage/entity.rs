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
