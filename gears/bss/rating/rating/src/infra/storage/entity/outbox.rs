//! Read-only views of the outbox tables `toolkit_db::outbox` creates under the prefix
//! `bss_rating__outbox`, for the depth of `rating.child_work` that backpressure reads (feature 04
//! §3.5). The outbox exposes no depth of its own; these name only the columns the depth needs.
//! Platform-scoped: queue bookkeeping, no tenant data.

/// `bss_rating__outbox_partitions`: one row per queue partition.
pub mod partitions {
    use sea_orm::entity::prelude::*;
    use toolkit_db_macros::Scopable;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "bss_rating__outbox_partitions")]
    #[secure(unrestricted)]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub queue: String,
        /// The highest sequence the sequencer assigned in the partition.
        pub sequence: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// `bss_rating__outbox_incoming`: enqueued messages the sequencer has not numbered yet.
pub mod incoming {
    use sea_orm::entity::prelude::*;
    use toolkit_db_macros::Scopable;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "bss_rating__outbox_incoming")]
    #[secure(unrestricted)]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub partition_id: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// `bss_rating__outbox_processor`: the sequence each partition's worker has processed.
pub mod processor {
    use sea_orm::entity::prelude::*;
    use toolkit_db_macros::Scopable;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "bss_rating__outbox_processor")]
    #[secure(unrestricted)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub partition_id: i64,
        pub processed_seq: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
