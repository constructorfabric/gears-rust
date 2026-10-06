//! Exercise timestamp binding, persisted reload and generation replacement on SQL.

use super::*;
use crate::producer::types::{MissingProducerRegistration, UnknownProducerRegistration};

async fn round_trip(url: &str) {
    let db = toolkit_db::connect_db(
        url,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::producer_registration_migrations(),
    )
    .await
    .unwrap();
    let store = ProducerRegistrationStore::new(db.clone());
    let managed = ManagedDeduplication {
        key: "timestamp-regression".to_owned(),
        mode: ProducerMode::Chained,
        on_missing: MissingProducerRegistration::RegisterNew,
        on_unknown: UnknownProducerRegistration::Fail,
    };
    let original = store
        .insert_new(&managed, ProducerId(uuid::Uuid::new_v4()), "regression/1")
        .await
        .unwrap();
    let loaded = store.load(&managed.key).await.unwrap().unwrap();
    assert_eq!(loaded.producer_id, original.producer_id);
    let replacement = ProducerId(uuid::Uuid::new_v4());
    store
        .replace_with_new_generation(&loaded, replacement)
        .await
        .unwrap();
    let loaded = store.load(&managed.key).await.unwrap().unwrap();
    assert_eq!(loaded.producer_id, replacement);
    assert_eq!(loaded.generation, 2);
    let row = Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(row.updated_at >= row.created_at);
}

#[tokio::test]
async fn sqlite_managed_registration_timestamp_round_trip() {
    round_trip("sqlite::memory:").await;
}

#[tokio::test]
#[ignore = "requires fresh EVENT_SDK_POSTGRES_URL database"]
async fn postgres_managed_registration_timestamp_round_trip() {
    round_trip(&std::env::var("EVENT_SDK_POSTGRES_URL").unwrap()).await;
}

/// Rows written before the `DateTimeUtc` binding still load on SQLite, where
/// timestamps are TEXT: the previous code stored `Utc::now().to_rfc3339()`, and
/// the table default stores `datetime('now')` (`YYYY-MM-DD HH:MM:SS`, no zone).
#[tokio::test]
async fn sqlite_registration_rows_written_as_legacy_text_still_load() {
    let db = toolkit_db::connect_db(
        "sqlite::memory:",
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::producer_registration_migrations(),
    )
    .await
    .unwrap();
    let conn = db.conn().unwrap();
    for (key, stamp) in [
        ("legacy-rfc3339", "2026-09-30T08:15:42.123456789+00:00"),
        ("legacy-sqlite-default", "2026-09-30 08:15:42"),
    ] {
        secure_insert::<legacy::Entity>(
            legacy::ActiveModel {
                registration_key: ActiveValue::Set(key.to_owned()),
                producer_id: ActiveValue::Set(uuid::Uuid::new_v4()),
                mode: ActiveValue::Set("chained".to_owned()),
                client_agent: ActiveValue::Set("legacy/1".to_owned()),
                generation: ActiveValue::Set(3),
                created_at: ActiveValue::Set(stamp.to_owned()),
                updated_at: ActiveValue::Set(stamp.to_owned()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await
        .unwrap();
    }

    let store = ProducerRegistrationStore::new(db);
    for key in ["legacy-rfc3339", "legacy-sqlite-default"] {
        let loaded = store
            .load(key)
            .await
            .unwrap_or_else(|error| panic!("legacy row {key} failed to load: {error}"))
            .unwrap_or_else(|| panic!("legacy row {key} missing"));
        // Generation and identity survive the binding change unchanged.
        assert_eq!(loaded.generation, 3);
        assert_eq!(loaded.mode, ProducerMode::Chained);
    }
}

/// The registration table as the previous release wrote it: TEXT timestamps.
mod legacy {
    use sea_orm::entity::prelude::*;
    use toolkit_db::secure::ScopableEntity;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "event_broker_producer_registrations")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub registration_key: String,
        pub producer_id: uuid::Uuid,
        pub mode: String,
        pub client_agent: String,
        pub generation: i64,
        pub created_at: String,
        pub updated_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl ScopableEntity for Entity {
        const IS_UNRESTRICTED: bool = true;

        fn tenant_col() -> Option<Self::Column> {
            None
        }

        fn resource_col() -> Option<Self::Column> {
            None
        }

        fn owner_col() -> Option<Self::Column> {
            None
        }

        fn type_col() -> Option<Self::Column> {
            None
        }

        fn resolve_property(_property: &str) -> Option<Self::Column> {
            None
        }

        fn scope_columns() -> Vec<Self::Column> {
            Vec::new()
        }
    }
}
