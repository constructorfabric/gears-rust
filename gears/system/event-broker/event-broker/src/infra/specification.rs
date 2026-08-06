//! Real `SpecificationManager`, backed by a local SQLite cache
//! (`event_broker_spec_cache`) that is bulk-loaded from `types-registry`
//! once at process startup (eb-single-process-implementation D1) - replaces
//! the dead `infra/type_provisioning.rs` stub (`eb-gts-type-registration`)
//! and the per-call `types-registry` round-trip this manager used to make on
//! every read.
//!
//! `register_topic`/`register_event_type` still write through to
//! `types-registry` directly (unchanged from before) - dynamic provisioning
//! is not supported in this change (no REST endpoint calls them), so a
//! registration made this way is not reflected in the local cache until the
//! next process restart's bulk load. This is a deliberate, documented
//! limitation, not a bug.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use serde_json::Value as JsonValue;
use toolkit_db::DBProvider;
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_gts::{GtsInstanceId, GtsSchema};
use toolkit_security::AccessScope;
use types_registry_sdk::{InstanceQuery, RegisterResult, TypesRegistryClient};

use crate::domain::error::DomainError;
use crate::domain::model::{EventType, Topic};
use crate::domain::specification::SpecificationManager;
use crate::infra::storage::entity::spec_cache::{
    self, ActiveModel as SpecCacheAM, Column as SpecCacheColumn, Entity as SpecCacheEntity,
    SpecKind,
};

pub struct TypesRegistrySpecificationManager {
    client: Arc<dyn TypesRegistryClient>,
    db: Arc<DBProvider<toolkit_db::DbError>>,
}

impl TypesRegistrySpecificationManager {
    #[must_use]
    pub fn new(client: Arc<dyn TypesRegistryClient>, db: Arc<DBProvider<toolkit_db::DbError>>) -> Self {
        Self { client, db }
    }


    async fn find_row(
        &self,
        kind: SpecKind,
        id: &GtsInstanceId,
    ) -> Result<Option<spec_cache::Model>, DomainError> {
        let conn = self.db.conn()?;
        let row = SpecCacheEntity::find()
            .filter(SpecCacheColumn::GtsId.eq(id.as_ref()))
            .filter(SpecCacheColumn::Kind.eq(kind.as_str()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await?;
        Ok(row)
    }

    async fn list_rows(&self, kind: SpecKind) -> Result<Vec<spec_cache::Model>, DomainError> {
        let conn = self.db.conn()?;
        let rows = SpecCacheEntity::find()
            .filter(SpecCacheColumn::Kind.eq(kind.as_str()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await?;
        Ok(rows)
    }
}

/// Reads every `Topic`/`EventType` instance from `types-registry` and
/// upserts each into the local cache by `gts_id`, minting a fresh surrogate
/// id only for a never-before-seen id (design.md D1). Called once from
/// `module.rs::serve()` - deliberately *not* a method on
/// `TypesRegistrySpecificationManager`, since it only ever touches `client`/
/// `db` directly, never the manager's own `SpecificationManager` trait
/// methods; a free function needing just those two pieces means the caller
/// doesn't have to keep the manager's concrete type around solely to reach
/// this one startup-only step, alongside the `Arc<dyn SpecificationManager>`
/// trait object every other caller actually wants.
///
/// # Errors
/// Returns `DomainError::StorageUnavailable` if the local cache database is
/// unreachable. A `types-registry` read/deserialization failure for one
/// instance is logged and skipped, not propagated (matching `list_all`'s
/// existing per-instance error handling).
pub async fn bulk_load(
    client: &Arc<dyn TypesRegistryClient>,
    db: &Arc<DBProvider<toolkit_db::DbError>>,
) -> Result<(), DomainError> {
    let conn = db.conn()?;
    for topic in list_all::<Topic>(client, event_broker_sdk::gts::TopicV1::TYPE_ID).await {
        upsert_spec_row(&conn, SpecKind::Topic, topic.id.as_ref(), &topic).await?;
    }
    for event_type in list_all::<EventType>(client, event_broker_sdk::gts::EventTypeV1::TYPE_ID).await
    {
        upsert_spec_row(
            &conn,
            SpecKind::EventType,
            event_type.id.as_ref(),
            &event_type,
        )
        .await?;
    }
    Ok(())
}

/// Updates the row's `payload` in place if `(kind, gts_id)` already exists
/// (preserving `id`), else inserts a fresh row. Tries the update first via
/// `update_many().filter(...)` (generic over any primary-key type, unlike
/// `secure_update_with_scope`, which hardcodes `id: uuid::Uuid` and can't
/// address `spec_cache`'s `i64` key) and falls back to insert only if it
/// affected zero rows - avoiding a separate existence-check round trip.
async fn upsert_spec_row(
    conn: &impl DBRunner,
    kind: SpecKind,
    gts_id: &str,
    spec: &impl serde::Serialize,
) -> Result<(), DomainError> {
    let payload = serde_json::to_string(spec)
        .map_err(|e| DomainError::Internal(format!("failed to serialize spec: {e}")))?;

    let update_result = SpecCacheEntity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(SpecCacheColumn::GtsId.eq(gts_id))
                .add(SpecCacheColumn::Kind.eq(kind.as_str())),
        )
        .col_expr(SpecCacheColumn::Payload, Expr::value(payload.clone()))
        .exec(conn)
        .await?;

    if update_result.rows_affected == 0 {
        let am = SpecCacheAM {
            id: sea_orm::ActiveValue::NotSet,
            gts_id: Set(gts_id.to_owned()),
            kind: Set(kind.as_str().to_owned()),
            payload: Set(payload),
        };
        secure_insert::<SpecCacheEntity>(am, &AccessScope::allow_all(), conn).await?;
    }
    Ok(())
}

fn deserialize_row<T: serde::de::DeserializeOwned>(row: &spec_cache::Model) -> Option<T> {
    match serde_json::from_str(&row.payload) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!(
                %err, gts_id = %row.gts_id, kind = %row.kind,
                "failed to deserialize cached spec row"
            );
            None
        }
    }
}

#[async_trait]
impl SpecificationManager for TypesRegistrySpecificationManager {
    async fn register_topic(&self, spec: Topic) -> Result<Topic, DomainError> {
        register_one(&self.client, &spec).await?;
        Ok(spec)
    }

    async fn register_event_type(&self, spec: EventType) -> Result<EventType, DomainError> {
        crate::domain::specification::validate_allowed_subject_types(&spec.allowed_subject_types)?;
        register_one(&self.client, &spec).await?;
        Ok(spec)
    }

    async fn get_topic(&self, id: &GtsInstanceId) -> Option<Topic> {
        let row = match self.find_row(SpecKind::Topic, id).await {
            Ok(row) => row?,
            Err(err) => {
                tracing::warn!(%err, topic_id = %id, "spec cache lookup failed while resolving topic");
                return None;
            }
        };
        deserialize_row(&row)
    }

    async fn get_event_type(&self, id: &GtsInstanceId) -> Option<EventType> {
        let row = match self.find_row(SpecKind::EventType, id).await {
            Ok(row) => row?,
            Err(err) => {
                tracing::warn!(%err, event_type_id = %id, "spec cache lookup failed while resolving event type");
                return None;
            }
        };
        deserialize_row(&row)
    }

    async fn validate_event_data(
        &self,
        event_type: &EventType,
        data: &JsonValue,
    ) -> Result<(), DomainError> {
        crate::domain::specification::validate_against_schema(event_type, data)
    }

    async fn list_topics(&self) -> Vec<Topic> {
        match self.list_rows(SpecKind::Topic).await {
            Ok(rows) => rows.iter().filter_map(deserialize_row).collect(),
            Err(err) => {
                tracing::warn!(%err, "spec cache list failed while listing topics");
                Vec::new()
            }
        }
    }

    async fn list_event_types(&self) -> Vec<EventType> {
        match self.list_rows(SpecKind::EventType).await {
            Ok(rows) => rows.iter().filter_map(deserialize_row).collect(),
            Err(err) => {
                tracing::warn!(%err, "spec cache list failed while listing event types");
                Vec::new()
            }
        }
    }

    async fn resolve_topic_id(&self, id: &GtsInstanceId) -> Result<i64, DomainError> {
        self.find_row(SpecKind::Topic, id)
            .await?
            .map(|row| row.id)
            .ok_or_else(|| DomainError::NotFound {
                code: "TopicNotFound",
                message: format!("topic '{id}' is not registered"),
                resource: id.to_string(),
            })
    }

    async fn resolve_event_type_id(&self, id: &GtsInstanceId) -> Result<i64, DomainError> {
        self.find_row(SpecKind::EventType, id)
            .await?
            .map(|row| row.id)
            .ok_or_else(|| DomainError::NotFound {
                code: "EventTypeNotFound",
                message: format!("event type '{id}' is not registered"),
                resource: id.to_string(),
            })
    }
}

async fn register_one(
    client: &Arc<dyn TypesRegistryClient>,
    spec: &(impl serde::Serialize + Sync),
) -> Result<(), DomainError> {
    let payload = serde_json::to_value(spec)
        .map_err(|e| DomainError::Internal(format!("failed to serialize spec: {e}")))?;
    let results = client
        .register_instances(vec![payload])
        .await
        .map_err(|e| {
            DomainError::Internal(format!("types-registry register_instances failed: {e}"))
        })?;
    match results.into_iter().next() {
        Some(RegisterResult::Ok { .. }) => Ok(()),
        Some(RegisterResult::Err { error, .. }) => Err(DomainError::Validation {
            code: "InvalidSpec",
            message: error.to_string(),
        }),
        None => Err(DomainError::Internal(
            "types-registry returned no result for register_instances".to_owned(),
        )),
    }
}

async fn list_all<T: serde::de::DeserializeOwned>(
    client: &Arc<dyn TypesRegistryClient>,
    type_id: &str,
) -> Vec<T> {
    let query = InstanceQuery::new().with_pattern(format!("{type_id}*"));
    let instances = match client.list_instances(query).await {
        Ok(instances) => instances,
        Err(err) => {
            tracing::warn!(%err, type_id, "list_instances failed");
            return Vec::new();
        }
    };
    instances
        .into_iter()
        .filter_map(|instance| match serde_json::from_value(instance.object) {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!(%err, instance_id = %instance.id, type_id, "failed to deserialize instance");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{ConnectOpts, DBProvider, connect_db};
    use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
    use uuid::Uuid;

    use super::{Arc, DomainError, EventType, GtsInstanceId, SpecificationManager};
    use crate::infra::storage::migrations::Migrator;
    use sea_orm_migration::MigratorTrait;

    const TOPIC_ID: &str = "gts.cf.core.events.topic.v1~example.eb.t1.topic.v1";
    const EVENT_TYPE_ID: &str = "gts.cf.core.events.event_type.v1~example.eb.t1.foo.v1";

    fn topic_payload(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            "description": null,
            "partitions": 4,
            "streaming": null,
            "retention": null,
            "created_at": "2026-01-01T00:00:00Z",
        })
    }

    /// Only the properties `docs/schemas/topic.v1.schema.json` marks
    /// `required` (`id`, `partitions`) - `description`/`streaming`/
    /// `retention` are omitted entirely, not present-as-`null`.
    fn minimal_topic_payload(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            "partitions": 4,
            "created_at": "2026-01-01T00:00:00Z",
        })
    }

    /// Only the properties `docs/schemas/event_type.v1.schema.json` marks
    /// `required` (`id`, `topic`, `data_schema`, `allowed_subject_types`) -
    /// `description` is omitted entirely.
    fn minimal_event_type_payload(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            // `EventType.topic_id` has no `#[serde(rename)]` - the domain
            // struct's own (de)serialization uses `topic_id`, not the wire
            // schema's `topic` (that rename is REST-DTO-level only,
            // `EventTypeDto::from`). Matches how `register_event_type`/
            // `get_event_type` actually round-trip today - noted as a
            // separate, not-yet-addressed naming mismatch against
            // `docs/schemas/event_type.v1.schema.json`.
            "topic_id": TOPIC_ID,
            "data_schema": {},
            "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            "created_at": "2026-01-01T00:00:00Z",
        })
    }

    /// A fresh temp-file SQLite DB with this crate's migrations applied -
    /// `bulk_load` needs a real, migrated `event_broker_spec_cache` table to
    /// upsert into, unlike the old per-call-`types-registry` implementation
    /// these tests were originally written against.
    async fn test_db() -> Arc<DBProvider<toolkit_db::DbError>> {
        let mut path = std::env::temp_dir();
        path.push(format!("cf-eb-spec-cache-test-{}.db", Uuid::now_v7().simple()));
        let mut file = path.to_string_lossy().replace('\\', "/");
        if !file.starts_with('/') {
            file.insert(0, '/');
        }
        let dsn = format!("sqlite://{file}?mode=rwc");
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db(&dsn, opts).await.expect("connect sqlite");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        Arc::new(DBProvider::new(db))
    }

    /// Builds a manager and runs `bulk_load()` against the given client's
    /// pre-populated instances, so `get_topic`/`list_topics`/etc. (which now
    /// read the local cache, not `types-registry` directly) have something
    /// to find - mirrors what `module.rs::serve()` does in production.
    async fn manager_with_bulk_load(
        client: Arc<dyn types_registry_sdk::TypesRegistryClient>,
    ) -> super::TypesRegistrySpecificationManager {
        let db = test_db().await;
        super::bulk_load(&client, &db).await.expect("bulk_load");
        super::TypesRegistrySpecificationManager::new(client, db)
    }

    #[tokio::test]
    async fn get_topic_resolves_a_pre_registered_instance() {
        let instance = make_test_instance(TOPIC_ID, topic_payload(TOPIC_ID));
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new().with_instances(vec![instance]));
        let manager = manager_with_bulk_load(client).await;

        let topic = manager
            .get_topic(&GtsInstanceId::try_new(TOPIC_ID).unwrap())
            .await
            .expect("pre-registered topic must resolve");
        assert_eq!(topic.id.as_ref(), TOPIC_ID);
        assert_eq!(topic.partitions, 4);
    }

    #[tokio::test]
    async fn get_topic_returns_none_for_an_unregistered_id() {
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new());
        let manager = manager_with_bulk_load(client).await;

        let missing =
            GtsInstanceId::try_new("gts.cf.core.events.topic.v1~example.eb.missing.topic.v1")
                .unwrap();
        assert!(manager.get_topic(&missing).await.is_none());
    }

    /// `SpecificationManager` is responsible for GTS pattern validity
    /// (design.md D2a) - a malformed `allowed_subject_types` entry (here, a
    /// non-terminal wildcard) is rejected at registration, before ever
    /// reaching `client.register_instances` (which would panic on
    /// `MockTypesRegistryClient` - confirming the validation short-circuits
    /// first, not incidentally passing because the mock never got called
    /// for some other reason).
    #[tokio::test]
    async fn register_event_type_rejects_a_malformed_allowed_subject_types_pattern() {
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new());
        let manager = manager_with_bulk_load(client).await;

        let spec = EventType {
            id: GtsInstanceId::try_new(EVENT_TYPE_ID).unwrap(),
            topic_id: GtsInstanceId::try_new(TOPIC_ID).unwrap(),
            description: None,
            // Wildcard not at the end of the pattern - malformed per
            // `gts::GtsIdPattern::try_new`.
            allowed_subject_types: vec!["gts.x.eb.t1.subject.v1~*.more".to_owned()],
            data_schema: json!({}),
            created_at: chrono::Utc::now(),
        };

        let err = manager
            .register_event_type(spec)
            .await
            .expect_err("a malformed allowed_subject_types pattern must reject registration");
        assert!(
            err.to_string().starts_with("InvalidSubjectTypePattern:"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn list_topics_returns_the_registered_topic_instance() {
        let instance = make_test_instance(TOPIC_ID, topic_payload(TOPIC_ID));
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new().with_instances(vec![instance]));
        let manager = manager_with_bulk_load(client).await;

        let topics = manager.list_topics().await;
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].id.as_ref(), TOPIC_ID);
        assert_eq!(topics[0].partitions, 4);
    }

    /// Proves `docs/schemas/topic.v1.schema.json`'s own optionality
    /// (`required: ["id", "partitions"]`) is real, not just documented -
    /// `description`/`streaming`/`retention` all resolve to `None` when the
    /// stored record omits them entirely (`eb-event-type-enforcement` D7/D9).
    #[tokio::test]
    async fn get_topic_resolves_a_minimal_stored_instance() {
        let instance = make_test_instance(TOPIC_ID, minimal_topic_payload(TOPIC_ID));
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new().with_instances(vec![instance]));
        let manager = manager_with_bulk_load(client).await;

        let topic = manager
            .get_topic(&GtsInstanceId::try_new(TOPIC_ID).unwrap())
            .await
            .expect("a minimal (id+partitions+created_at only) record must still deserialize");
        assert_eq!(topic.description, None);
        assert_eq!(topic.streaming, None);
        assert_eq!(topic.retention, None);
    }

    /// Same proof for `EventType` (`docs/schemas/event_type.v1.schema.json`'s
    /// `required: ["id", "topic", "data_schema", "allowed_subject_types"]`) -
    /// `description` resolves to `None` when omitted.
    #[tokio::test]
    async fn get_event_type_resolves_a_minimal_stored_instance() {
        let instance = make_test_instance(EVENT_TYPE_ID, minimal_event_type_payload(EVENT_TYPE_ID));
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new().with_instances(vec![instance]));
        let manager = manager_with_bulk_load(client).await;

        let event_type = manager
            .get_event_type(&GtsInstanceId::try_new(EVENT_TYPE_ID).unwrap())
            .await
            .expect("a minimal (no description) record must still deserialize");
        assert_eq!(event_type.description, None);
    }

    /// `allowed_subject_types` deliberately has no `#[serde(default)]`
    /// (design.md D8) - a stored record omitting it entirely must fail to
    /// deserialize, not silently resolve to `[]`. `get_event_type` swallows
    /// the deserialize error, surfacing as `None` rather than a
    /// distinguishable error, so this test also asserts
    /// `serde_json::from_value::<EventType>` directly for the same payload,
    /// to confirm the underlying cause is really a deserialize failure and
    /// not e.g. a lookup miss.
    #[tokio::test]
    async fn get_event_type_missing_allowed_subject_types_fails_to_deserialize() {
        let mut payload = minimal_event_type_payload(EVENT_TYPE_ID);
        payload
            .as_object_mut()
            .unwrap()
            .remove("allowed_subject_types");
        assert!(
            serde_json::from_value::<super::EventType>(payload.clone()).is_err(),
            "a payload missing 'allowed_subject_types' must fail to deserialize into EventType"
        );

        let instance = make_test_instance(EVENT_TYPE_ID, payload);
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new().with_instances(vec![instance]));
        let manager = manager_with_bulk_load(client).await;

        assert!(
            manager
                .get_event_type(&GtsInstanceId::try_new(EVENT_TYPE_ID).unwrap())
                .await
                .is_none(),
            "get_event_type swallows the deserialize error, surfacing as None"
        );
    }

    /// eb-single-process-implementation D1: a restart's bulk load must
    /// preserve an already-assigned surrogate id, not renumber it.
    #[tokio::test]
    async fn resolve_topic_id_is_stable_across_a_second_bulk_load() {
        let instance = make_test_instance(TOPIC_ID, topic_payload(TOPIC_ID));
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new().with_instances(vec![instance]));
        let db = test_db().await;

        super::bulk_load(&client, &db).await.expect("first bulk_load");
        let manager =
            super::TypesRegistrySpecificationManager::new(Arc::clone(&client), Arc::clone(&db));
        let topic_gts_id = GtsInstanceId::try_new(TOPIC_ID).unwrap();
        let first_id = manager
            .resolve_topic_id(&topic_gts_id)
            .await
            .expect("resolves after first bulk_load");

        super::bulk_load(&client, &db).await.expect("second bulk_load");
        let second_id = manager
            .resolve_topic_id(&topic_gts_id)
            .await
            .expect("resolves after second bulk_load");

        assert_eq!(first_id, second_id, "surrogate id must not be renumbered");
    }

    #[tokio::test]
    async fn resolve_topic_id_returns_not_found_for_an_unregistered_id() {
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new());
        let manager = manager_with_bulk_load(client).await;

        let missing =
            GtsInstanceId::try_new("gts.cf.core.events.topic.v1~example.eb.missing.topic.v1")
                .unwrap();
        let err = manager
            .resolve_topic_id(&missing)
            .await
            .expect_err("an unregistered topic must not resolve");
        assert!(matches!(err, DomainError::NotFound { .. }));
    }
}
