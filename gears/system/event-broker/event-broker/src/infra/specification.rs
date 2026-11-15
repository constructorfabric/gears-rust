//! Real `SpecificationManager`, backed by a cache table in the gear database
//! (`event_broker_spec_cache`; `SQLite`, `PostgreSQL` or `MySQL`/`MariaDB`)
//! that is loaded from `types-registry` and holds what the most recent read
//! found.
//!
//! Each entity is read as the GTS kind it actually is. A topic is an
//! **instance** of the topic base type, so it comes back from
//! `list_instances`. An event type is a **derived type schema** of the abstract
//! event base type, so it comes back from `list_type_schemas`; that listing
//! includes the base itself, because a base identifier behaves as the implicit
//! envelope `...~*`, and the base is excluded by identity.
//!
//! What lands in the cache is the projection rather than the registered
//! document, joined for a topic with the settings this deployment resolved for
//! it. Nothing on a request path parses a schema: the payload contract ingest
//! validates against is already part of the projection, and re-deriving it per
//! call would walk the inheritance chain to recompute a value that cannot
//! change between reads.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use event_broker_sdk::models::EventType;
use gts::{GtsIdPattern, GtsTypeId};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use serde_json::Value as JsonValue;
use toolkit_db::DBProvider;
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_gts::{GtsInstanceId, GtsSchema};
use toolkit_security::{AccessScope, PlatformSecurityContext};
use types_registry_sdk::{
    ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};

mod provisioning;

use crate::config::{EventBrokerConfig, checks};
use crate::domain::backend::BackendResolver;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::Topic;
use crate::domain::resolution::{Declaration, resolve};
use crate::domain::specification::{
    EventTypeCandidate, EventTypeCandidates, ProvisioningLimits, SpecificationManager, TopicState,
};
use crate::domain::{event_type as event_type_traits, projection};
use crate::infra::storage::entity::spec_cache::{
    self, ActiveModel as SpecCacheAM, Column as SpecCacheColumn, Entity as SpecCacheEntity,
    SpecKind,
};

/// A compiled JSON-Schema validator kept alongside a fingerprint of the
/// `data_schema` it was compiled from, so a re-registered event type
/// self-invalidates: a changed schema hashes differently and forces a recompile.
struct CachedValidator {
    fingerprint: u64,
    validator: Arc<jsonschema::Validator>,
}

/// A stable hash of a `data_schema`, used only to notice when a re-registered
/// event type's schema has changed. `serde_json::Value` serializes its object
/// keys in a deterministic order, so the same schema always hashes identically.
fn schema_fingerprint(schema: &JsonValue) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(schema)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// Serves what the last load put in the cache.
///
/// Holds no `types-registry` client: every read is a local lookup, and the load
/// that fills the cache is a free function with its own inputs. A client here
/// would be a second, unused route to the registry.
pub struct TypesRegistrySpecificationManager {
    db: Arc<DBProvider<toolkit_db::DbError>>,
    /// Compiled validators, keyed by event-type id. `EventType` carries no
    /// version, so the key is paired with a schema fingerprint rather than
    /// trusting the id alone. Compilation is the expensive part of schema
    /// validation and a batch publishes many events of the same type, so this
    /// turns per-event recompilation into one compile per (type, schema).
    validators: Mutex<HashMap<GtsTypeId, CachedValidator>>,
}

impl TypesRegistrySpecificationManager {
    #[must_use]
    pub fn new(db: Arc<DBProvider<toolkit_db::DbError>>) -> Self {
        Self {
            db,
            validators: Mutex::new(HashMap::new()),
        }
    }

    /// Returns a compiled validator for `event_type`, reusing the cached one
    /// when the schema fingerprint still matches and recompiling otherwise.
    fn cached_validator(
        &self,
        event_type: &EventType,
    ) -> Result<Arc<jsonschema::Validator>, DomainError> {
        let fingerprint = schema_fingerprint(&event_type.data_schema);
        // A poisoned lock means a prior compile panicked; the map itself stays
        // consistent, so recover the guard rather than propagate the panic.
        let mut cache = self
            .validators
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = cache.get(&event_type.id)
            && entry.fingerprint == fingerprint
        {
            return Ok(Arc::clone(&entry.validator));
        }
        let validator = Arc::new(crate::domain::specification::compile_schema(event_type)?);
        cache.insert(
            event_type.id.clone(),
            CachedValidator {
                fingerprint,
                validator: Arc::clone(&validator),
            },
        );
        Ok(validator)
    }

    async fn find_row(
        &self,
        kind: SpecKind,
        id: &str,
    ) -> Result<Option<spec_cache::Model>, DomainError> {
        let conn = self.db.conn()?;
        let row = SpecCacheEntity::find()
            .filter(SpecCacheColumn::GtsId.eq(id))
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

    /// `topic`'s event-type rows, ordered by `gts_id`: at most one more than
    /// [`EventTypeCandidates::CAP`], which is enough to tell the cap was passed.
    async fn event_type_candidate_rows(
        &self,
        topic: &GtsInstanceId,
    ) -> Result<Vec<CandidateRow>, DomainError> {
        let conn = self.db.conn()?;
        let rows = SpecCacheEntity::find()
            // Topic rows hold no topic, so `topic` alone selects the same rows;
            // `kind` is here to match the `(kind, topic)` index.
            .filter(SpecCacheColumn::Kind.eq(SpecKind::EventType.as_str()))
            .filter(SpecCacheColumn::Topic.eq(topic.as_ref()))
            .order_by_asc(SpecCacheColumn::GtsId)
            .limit(CANDIDATE_ROWS)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .project_all(&conn, |select| {
                select
                    .select_only()
                    .column(SpecCacheColumn::GtsId)
                    .column(SpecCacheColumn::AllowedSubjectTypes)
                    .into_model::<CandidateRow>()
            })
            .await?;
        Ok(rows)
    }
}

/// The two columns [`TypesRegistrySpecificationManager::event_type_candidate_rows`]
/// reads, leaving `payload` and its `data_schema` behind.
#[derive(sea_orm::FromQueryResult)]
struct CandidateRow {
    gts_id: String,
    allowed_subject_types: String,
}

/// The one column the registered-type lookup reads.
#[derive(sea_orm::FromQueryResult)]
struct GtsIdRow {
    gts_id: String,
}

/// [`EventTypeCandidates::CAP`] plus the one row that shows it was passed.
const CANDIDATE_ROWS: u64 = EventTypeCandidates::CAP as u64 + 1;

/// Reads every topic and event type `types-registry` holds, projects each one,
/// resolves a topic's settings against configuration, and upserts the result
/// into the local cache by `gts_id` - minting a fresh surrogate id only for a
/// never-before-seen identifier, so an id already assigned survives the load.
///
/// An entity that fails the broker's own validation on the way in is excluded
/// with a warning rather than failing the load: one gear's malformed event type
/// is not a reason to leave every other topic on the instance unserved. A
/// publish naming an excluded type is answered as unregistered.
///
/// A free function rather than a method: it needs the client, the database and
/// the configuration, and none of the trait's own methods, so a caller does not
/// have to keep the concrete manager type around to reach it.
///
/// Two warnings ride the topic pass (feature 0005 §2.2). A fully qualified
/// `backend_routing` key that names no registered topic is reported on every
/// pass, since a mistyped key would otherwise leave its topic on the
/// instance-less route for good; a pass whose listing failed reports none, as
/// it saw no topics to compare against. And a topic whose route differs from
/// the one cached for it is reported once, when it changes: its log was
/// expected on the old entry.
///
/// # Errors
///
/// Returns `DomainError::StorageUnavailable` if the local cache database is
/// unreachable, and `DomainError::Internal` if `config` breaks a routing
/// rule, which the startup checks prevent, or if a base type's listing pattern
/// does not parse, which its constant identifier rules out. A per-entity
/// failure is logged and skipped.
#[allow(clippy::cognitive_complexity)]
pub async fn bulk_load(
    client: &Arc<dyn PlatformTypesRegistryApi>,
    db: &Arc<DBProvider<toolkit_db::DbError>>,
    config: &EventBrokerConfig,
) -> Result<(), DomainError> {
    let conn = db.conn()?;
    let routing = checks::routing(config).map_err(|err| DomainError::Internal(err.to_string()))?;
    let cached_routes = cached_routes(&conn).await?;
    let ctx = PlatformSecurityContext::outbound_marker();

    let topic_base = event_broker_sdk::gts::TopicV1::TYPE_ID;
    let listed = client
        .list_instances(
            &ctx,
            ListEntitiesRequest::matching(listing_pattern(topic_base)?),
        )
        .await;
    let listing_failed = listed.is_err();
    let instances = listed.unwrap_or_else(|err| {
        tracing::warn!(%err, type_id = topic_base, "list_instances failed");
        Vec::new()
    });
    let mut registered = std::collections::BTreeSet::new();
    for instance in &instances {
        let projected = match projection::topic(instance) {
            Ok(projected) => projected,
            Err(err) => {
                tracing::warn!(%err, id = %instance.id, "topic excluded: it does not project");
                continue;
            }
        };
        let declared = Declaration {
            retention: projected
                .retention
                .as_ref()
                .map(|retention| std::time::Duration::from(*retention)),
        };
        registered.insert(projected.id.as_ref().to_owned());
        let settings = match resolve(config, &routing, &projected.id, &declared) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(%err, id = %instance.id, "topic excluded: its settings do not resolve");
                continue;
            }
        };
        if let Some(previous) = cached_routes.get(projected.id.as_ref())
            && previous != settings.backend().value()
        {
            tracing::warn!(
                topic = %projected.id,
                from = %previous,
                to = %settings.backend().value(),
                "topic re-routed to another backend registry entry; its log was expected on the previous one"
            );
        }
        let topic = Topic {
            id: projected.id.clone(),
            uuid: instance.uuid,
            description: projected.description,
            retention: projected.retention,
            settings,
        };
        upsert_spec_row(&conn, topic.id.as_ref(), &topic, SpecColumns::Topic).await?;
    }
    if !listing_failed {
        for key in routing
            .topic_keys()
            .filter(|key| !registered.contains(*key))
        {
            tracing::warn!(
                key,
                "backend_routing key names no registered topic; the topic it was meant for takes the instance-less route"
            );
        }
    }

    let event_base = event_broker_sdk::gts::EventV1::<()>::TYPE_ID;
    let schemas = match client
        .list_type_schemas(
            &ctx,
            ListEntitiesRequest::matching(listing_pattern(event_base)?),
        )
        .await
    {
        Ok(schemas) => schemas,
        Err(err) => {
            tracing::warn!(%err, type_id = event_base, "list_type_schemas failed");
            Vec::new()
        }
    };
    for schema in &schemas {
        // The pattern is an envelope, so the base comes back inside it. It is
        // abstract: no producer can publish it, and nothing may resolve it as a
        // type of its own.
        if schema.type_id.as_ref() == event_base {
            continue;
        }
        // Validated here because this is the only moment the broker holds the
        // resolved schema. A pointer naming no declared member would fail
        // identically on every publish of the type, so admitting it would turn
        // one gear's mistake into an open-ended runtime failure.
        if let Err(err) = event_type_traits::validate_partition_key(schema) {
            tracing::warn!(%err, id = %schema.type_id, "event type excluded: its partition key does not validate");
            continue;
        }
        let projected = match projection::event_type(schema) {
            Ok(projected) => projected,
            Err(err) => {
                tracing::warn!(%err, id = %schema.type_id, "event type excluded: it does not project");
                continue;
            }
        };
        upsert_spec_row(
            &conn,
            projected.id.as_ref(),
            &EventTypeRecord::from(&projected),
            SpecColumns::EventType {
                topic: &projected.topic,
                allowed_subject_types: &projected.allowed_subject_types,
            },
        )
        .await?;
    }
    Ok(())
}

/// The pattern that lists `base` and every entity derived from it.
fn listing_pattern(base: &str) -> Result<GtsIdPattern, DomainError> {
    GtsIdPattern::try_new(&format!("{base}*"))
        .map_err(|err| DomainError::Internal(format!("`{base}*` is not a GTS pattern: {err}")))
}

/// The backend registry entry every cached topic was last resolved to, keyed
/// by topic identifier. A row that no longer deserializes as a topic is left
/// out: the pass rewrites it anyway.
async fn cached_routes(
    conn: &impl DBRunner,
) -> Result<HashMap<String, crate::domain::backend::Name>, DomainError> {
    let rows = SpecCacheEntity::find()
        .filter(SpecCacheColumn::Kind.eq(SpecKind::Topic.as_str()))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(conn)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let topic: Topic = serde_json::from_str(&row.payload).ok()?;
            Some((row.gts_id, topic.settings.backend().value().clone()))
        })
        .collect())
}

/// The columns a row stores beside its `payload`, and its `kind`.
enum SpecColumns<'a> {
    /// `topic` is `NULL` and `allowed_subject_types` is `[]`.
    Topic,
    EventType {
        topic: &'a GtsInstanceId,
        allowed_subject_types: &'a [GtsIdPattern],
    },
}

impl SpecColumns<'_> {
    fn kind(&self) -> SpecKind {
        match self {
            Self::Topic => SpecKind::Topic,
            Self::EventType { .. } => SpecKind::EventType,
        }
    }

    fn topic(&self) -> Option<String> {
        match self {
            Self::Topic => None,
            Self::EventType { topic, .. } => Some(topic.as_ref().to_owned()),
        }
    }

    fn allowed_subject_types(&self) -> Result<String, DomainError> {
        let patterns = match self {
            Self::Topic => &[][..],
            Self::EventType {
                allowed_subject_types,
                ..
            } => allowed_subject_types,
        };
        serde_json::to_string(&SubjectTypesColumn(patterns))
            .map_err(|e| DomainError::Internal(format!("failed to serialize subject types: {e}")))
    }
}

/// The `allowed_subject_types` column: a JSON array in the form
/// `EventTypeRecord` writes the same list in `payload`.
#[derive(serde::Serialize)]
#[serde(transparent)]
struct SubjectTypesColumn<'a>(
    #[serde(serialize_with = "crate::domain::gts_pattern_serde::serialize")] &'a [GtsIdPattern],
);

/// [`SubjectTypesColumn`] as read back.
#[derive(serde::Deserialize)]
#[serde(transparent)]
struct StoredSubjectTypes(
    #[serde(deserialize_with = "crate::domain::gts_pattern_serde::deserialize")] Vec<GtsIdPattern>,
);

/// Updates the row's `payload` and `columns` in place if `(kind, gts_id)`
/// already exists (preserving `id`), else inserts a fresh row. Tries the
/// update first via `update_many().filter(...)` (generic over any primary-key
/// type, unlike `secure_update_with_scope`, which hardcodes `id: uuid::Uuid`
/// and can't address `spec_cache`'s `i64` key) and falls back to insert only
/// if it affected zero rows - avoiding a separate existence-check round trip.
async fn upsert_spec_row(
    conn: &impl DBRunner,
    gts_id: &str,
    spec: &impl serde::Serialize,
    columns: SpecColumns<'_>,
) -> Result<(), DomainError> {
    let kind = columns.kind();
    let payload = serde_json::to_string(spec)
        .map_err(|e| DomainError::Internal(format!("failed to serialize spec: {e}")))?;
    let topic = columns.topic();
    let allowed_subject_types = columns.allowed_subject_types()?;

    let update_result = SpecCacheEntity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(SpecCacheColumn::GtsId.eq(gts_id))
                .add(SpecCacheColumn::Kind.eq(kind.as_str())),
        )
        .col_expr(SpecCacheColumn::Payload, Expr::value(payload.clone()))
        .col_expr(SpecCacheColumn::Topic, Expr::value(topic.clone()))
        .col_expr(
            SpecCacheColumn::AllowedSubjectTypes,
            Expr::value(allowed_subject_types.clone()),
        )
        .exec(conn)
        .await?;

    if update_result.rows_affected == 0 {
        let am = SpecCacheAM {
            id: sea_orm::ActiveValue::NotSet,
            gts_id: Set(gts_id.to_owned()),
            kind: Set(kind.as_str().to_owned()),
            payload: Set(payload),
            topic: Set(topic),
            allowed_subject_types: Set(allowed_subject_types),
        };
        secure_insert::<SpecCacheEntity>(am, &AccessScope::allow_all(), conn).await?;
    }
    Ok(())
}

/// Cache-persistence shape for a projected event type.
///
/// The spec cache stores specs as JSON. The SDK's public
/// [`EventType`](event_broker_sdk::models::EventType) carries no serde -
/// persistence and transport stay off the public SDK surface - so this private
/// mirror owns the serialized form and converts at the cache boundary. Its
/// field names are the persisted contract.
#[derive(serde::Serialize, serde::Deserialize)]
struct EventTypeRecord {
    id: GtsTypeId,
    topic: GtsInstanceId,
    description: Option<String>,
    #[serde(with = "crate::domain::gts_pattern_serde")]
    allowed_subject_types: Vec<GtsIdPattern>,
    partition_key: String,
    data_schema: JsonValue,
}

impl From<&EventType> for EventTypeRecord {
    fn from(e: &EventType) -> Self {
        Self {
            id: e.id.clone(),
            topic: e.topic.clone(),
            description: e.description.clone(),
            allowed_subject_types: e.allowed_subject_types.clone(),
            partition_key: e.partition_key.clone(),
            data_schema: e.data_schema.clone(),
        }
    }
}

impl From<EventTypeRecord> for EventType {
    fn from(r: EventTypeRecord) -> Self {
        Self {
            id: r.id,
            topic: r.topic,
            description: r.description,
            allowed_subject_types: r.allowed_subject_types,
            partition_key: r.partition_key,
            data_schema: r.data_schema,
        }
    }
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
    async fn get_topic(&self, id: &GtsInstanceId) -> Result<Option<Topic>, DomainError> {
        let Some(row) = self.find_row(SpecKind::Topic, id.as_ref()).await? else {
            return Ok(None);
        };
        let Some(topic) = deserialize_row::<Topic>(&row) else {
            return Ok(None);
        };
        Ok(Some(match self.read_topic_state(&topic).await? {
            Some(TopicState::Provisioned { partitions, .. }) => {
                with_backend_partitions(topic, i64::from(partitions))
            }
            _ => topic,
        }))
    }

    async fn get_event_type(&self, id: &GtsTypeId) -> Result<Option<EventType>, DomainError> {
        let Some(row) = self.find_row(SpecKind::EventType, id.as_ref()).await? else {
            return Ok(None);
        };
        Ok(deserialize_row::<EventTypeRecord>(&row).map(EventType::from))
    }

    async fn get_event_type_candidates(
        &self,
        topic: &GtsInstanceId,
    ) -> Result<EventTypeCandidates, DomainError> {
        let rows = self.event_type_candidate_rows(topic).await?;
        if rows.len() > EventTypeCandidates::CAP {
            return Ok(EventTypeCandidates::OverCap);
        }
        rows.into_iter()
            .map(|row| {
                // Only `upsert_spec_row` writes these columns, so a value that
                // does not parse is a broken cache. The parse messages quote
                // the stored value, so the errors leave them out.
                let id = GtsTypeId::try_new(&row.gts_id).map_err(|_| {
                    DomainError::Internal("a cached event type id does not parse".to_owned())
                })?;
                let StoredSubjectTypes(allowed_subject_types) =
                    serde_json::from_str(&row.allowed_subject_types).map_err(|_| {
                        DomainError::Internal(
                            "a cached allowed_subject_types list does not decode".to_owned(),
                        )
                    })?;
                Ok(EventTypeCandidate {
                    id,
                    allowed_subject_types,
                })
            })
            .collect::<Result<_, _>>()
            .map(EventTypeCandidates::Listed)
    }

    async fn get_registered_event_types(
        &self,
        topic: &GtsInstanceId,
        ids: &[GtsTypeId],
    ) -> Result<Vec<GtsTypeId>, DomainError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.conn()?;
        let rows = SpecCacheEntity::find()
            .filter(SpecCacheColumn::Kind.eq(SpecKind::EventType.as_str()))
            .filter(SpecCacheColumn::Topic.eq(topic.as_ref()))
            .filter(SpecCacheColumn::GtsId.is_in(ids.iter().map(AsRef::<str>::as_ref)))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .project_all(&conn, |select| {
                select
                    .select_only()
                    .column(SpecCacheColumn::GtsId)
                    .into_model::<GtsIdRow>()
            })
            .await?;
        rows.iter()
            .map(|row| {
                // Only `upsert_spec_row` writes this column; the parse message
                // quotes the stored value, so the error leaves it out.
                GtsTypeId::try_new(&row.gts_id).map_err(|_| {
                    DomainError::Internal("a cached event type id does not parse".to_owned())
                })
            })
            .collect()
    }

    async fn validate_event_data(
        &self,
        event_type: &EventType,
        data: &JsonValue,
    ) -> Result<(), DomainError> {
        let validator = self.cached_validator(event_type)?;
        crate::domain::specification::check_against_schema(&validator, event_type, data)
    }

    async fn list_topics(&self) -> Result<Vec<Topic>, DomainError> {
        let topics = self.resolved_topics().await?;
        let provisioned = self.provisioned_partitions().await?;
        Ok(topics
            .into_iter()
            .map(|topic| match provisioned.get(topic.id.as_ref()) {
                Some(partitions) => with_backend_partitions(topic, *partitions),
                None => topic,
            })
            .collect())
    }

    async fn list_event_types(&self) -> Result<Vec<EventType>, DomainError> {
        Ok(self
            .list_rows(SpecKind::EventType)
            .await?
            .iter()
            .filter_map(deserialize_row::<EventTypeRecord>)
            .map(EventType::from)
            .collect())
    }

    async fn resolve_topic_id(&self, id: &GtsInstanceId) -> Result<i64, DomainError> {
        self.find_row(SpecKind::Topic, id.as_ref())
            .await?
            .map(|row| row.id)
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::TopicNotFound,
                message: format!("topic '{id}' is not registered"),
                resource: id.to_string(),
            })
    }

    async fn resolve_event_type_id(&self, id: &GtsTypeId) -> Result<i64, DomainError> {
        self.find_row(SpecKind::EventType, id.as_ref())
            .await?
            .map(|row| row.id)
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::EventTypeNotFound,
                message: format!("event type '{}' is not registered", id.as_ref()),
                resource: id.as_ref().to_owned(),
            })
    }

    async fn topic_state(&self, topic: &Topic) -> Result<Option<TopicState>, DomainError> {
        self.read_topic_state(topic).await
    }

    async fn provision_topics(
        &self,
        backends: &dyn BackendResolver,
        limits: ProvisioningLimits,
    ) -> usize {
        self.provision_all(backends, limits).await
    }
}

/// `topic` with the partition count its backend holds, which is in force once
/// the topic is provisioned, whatever configuration resolved.
fn with_backend_partitions(mut topic: Topic, partitions: i64) -> Topic {
    if let Ok(partitions) = i32::try_from(partitions)
        && *topic.settings.partitions().value() != partitions
    {
        topic.settings = topic.settings.with_partitions_held_by_backend(partitions);
    }
    topic
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use toolkit_canonical_errors::CanonicalError;
    use toolkit_db::DBProvider;
    use types_registry_sdk::PlatformTypesRegistryApi;
    use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

    use super::{
        Arc, DomainError, GtsIdPattern, GtsInstanceId, GtsSchema, GtsTypeId, SpecificationManager,
    };
    use crate::config::EventBrokerConfig;
    use crate::domain::resolution::Source;
    use crate::domain::specification::{EventTypeCandidate, EventTypeCandidates};
    use crate::test_support::gear_db::ScratchSqlite;
    use crate::test_support::type_registry::{StaticTypesRegistry, registry_holding};
    use event_broker_sdk::gts::{EventV1, EventV1Traits};

    const TOPIC_ID: &str = toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.t1.topic.v1");
    const EVENT_TYPE_ID: &str = <FooV1 as GtsSchema>::TYPE_ID;
    const SUBJECT_TYPE: &str = toolkit_gts::gts_id!("example.eb.t1.subject.v1~");
    const SQLITE_BACKEND: &str =
        toolkit_gts::gts_id!("cf.core.events.backend.v1~cf.core.backend.sqlite.v1~");
    /// The instance-less routing key every `backend_routing` requires.
    const BASE_ROUTE: &str = toolkit_gts::gts_id!("cf.core.events.topic.v1~");

    #[toolkit_gts::gts_type_schema(
        dir_path = "schemas",
        base = EventV1,
        type_id = toolkit_gts::gts_id!("cf.core.events.event.v1~example.eb.t1.foo.v1~"),
        description = "An event declaring its partition pointer.",
        properties = "",
        traits = EventV1Traits::builder()
            .with_topic(TOPIC_ID)
            .with_allowed_subject_types([SUBJECT_TYPE])
            .build(),
    )]
    struct FooV1;

    // Omit the partition key deliberately to exercise GTS default inheritance.
    #[toolkit_gts::gts_type_schema(
        dir_path = "schemas",
        base = EventV1,
        type_id = toolkit_gts::gts_id!("cf.core.events.event.v1~example.eb.t1.foo.v1~"),
        description = "An event inheriting its partition pointer.",
        properties = "",
        traits = json!({ "topic": TOPIC_ID, "allowed_subject_types": [SUBJECT_TYPE] }),
    )]
    struct SilentFooV1;

    // Names `/tenant_id`, a member the base does not declare.
    #[toolkit_gts::gts_type_schema(
        dir_path = "schemas",
        base = EventV1,
        type_id = toolkit_gts::gts_id!("cf.core.events.event.v1~example.eb.t1.old_pointer.v1~"),
        description = "An event partitioned by the old tenant pointer.",
        properties = "",
        traits = json!({
            "topic": TOPIC_ID,
            "allowed_subject_types": [SUBJECT_TYPE],
            "partition_key": "/tenant_id",
        }),
    )]
    struct OldPointerV1;

    /// A topic instance carrying exactly what its base type declares - the
    /// partition count is not among them, and a document that named one would
    /// be refused at registration.
    fn topic_document(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            "description": "events this test publishes",
            "retention": "PT168H",
        })
    }

    fn config(topics: &serde_json::Value) -> EventBrokerConfig {
        serde_json::from_value(json!({
            "mode": "standalone",
            "backends": { "main": { "type": SQLITE_BACKEND } },
            "backend_routing": { BASE_ROUTE: "main" },
            "topics": topics.clone(),
        }))
        .expect("test configuration deserializes")
    }

    async fn test_db() -> ScratchSqlite {
        ScratchSqlite::open("cf-eb-spec-cache-test").await
    }

    /// Loads the given documents through the real load path, so every test here
    /// is evidence about what a booting process does; the manager comes back
    /// with the database it reads.
    async fn manager_with(
        instances: Vec<serde_json::Value>,
        schemas: Vec<serde_json::Value>,
        cfg: &EventBrokerConfig,
    ) -> (super::TypesRegistrySpecificationManager, ScratchSqlite) {
        let client: Arc<dyn PlatformTypesRegistryApi> = registry_holding(&instances, &schemas);
        let scratch = test_db().await;
        super::bulk_load(&client, scratch.db(), cfg)
            .await
            .expect("bulk_load");
        (
            super::TypesRegistrySpecificationManager::new(Arc::clone(scratch.db())),
            scratch,
        )
    }

    #[tokio::test]
    async fn a_topic_instance_resolves_with_the_settings_configuration_gives_it() {
        let cfg = config(
            &json!({ toolkit_gts::gts_id!("cf.core.events.topic.v1~"): { "partitions": 4 } }),
        );
        let (manager, _scratch) =
            manager_with(vec![topic_document(TOPIC_ID)], Vec::new(), &cfg).await;

        let topic = manager
            .get_topic(&GtsInstanceId::try_new(TOPIC_ID).unwrap())
            .await
            .expect("the cache reads")
            .expect("a registered topic resolves");
        assert_eq!(topic.id.as_ref(), TOPIC_ID);
        assert_eq!(topic.description, "events this test publishes");
        assert_eq!(*topic.settings.partitions().value(), 4);
        assert_eq!(topic.settings.partitions().source(), Source::TypeEntry);
    }

    /// The declared tier of the ladder, end to end: the instance says a week,
    /// configuration says nothing about retention, so a week is enforced.
    #[tokio::test]
    async fn a_declared_retention_survives_a_deployment_that_states_none() {
        let cfg = config(&json!({}));
        let (manager, _scratch) =
            manager_with(vec![topic_document(TOPIC_ID)], Vec::new(), &cfg).await;

        let topic = manager
            .get_topic(&GtsInstanceId::try_new(TOPIC_ID).unwrap())
            .await
            .expect("the cache reads")
            .expect("a registered topic resolves");
        assert_eq!(
            topic.settings.retention().value().duration,
            std::time::Duration::from_hours(168)
        );
        assert_eq!(
            topic.settings.retention().source(),
            Source::Specification,
            "the built-in must not displace what the topic declared"
        );
    }

    #[tokio::test]
    async fn an_event_type_resolves_from_its_derived_schema() {
        let cfg = config(&json!({}));
        let (manager, _scratch) = manager_with(
            vec![topic_document(TOPIC_ID)],
            vec![FooV1::gts_schema_with_refs()],
            &cfg,
        )
        .await;

        let event_type = manager
            .get_event_type(&GtsTypeId::try_new(EVENT_TYPE_ID).unwrap())
            .await
            .expect("the cache reads")
            .expect("a derived event-type schema resolves");
        assert_eq!(event_type.id.as_ref(), EVENT_TYPE_ID);
        assert_eq!(event_type.topic.as_ref(), TOPIC_ID);
        assert_eq!(
            event_type
                .allowed_subject_types
                .iter()
                .map(gts::GtsIdPattern::pattern)
                .collect::<Vec<_>>(),
            vec![SUBJECT_TYPE]
        );
        assert_eq!(
            event_type.partition_key, "/subject_tenant",
            "the pointer this type declares for itself"
        );
    }

    /// A type that declares no pointer of its own still resolves one, from the
    /// default on the base's trait schema. This is the mechanism the whole
    /// partition contract rests on - without it, a type that says nothing about
    /// partitioning would resolve no key at all and every publish of it would
    /// fail - so it is asserted against a schema that genuinely omits the
    /// trait, not one built by a helper that fills it in.
    #[tokio::test]
    async fn a_type_declaring_no_pointer_resolves_the_bases_default() {
        let cfg = config(&json!({}));
        let silent = SilentFooV1::gts_schema_with_refs();
        let (manager, _scratch) =
            manager_with(vec![topic_document(TOPIC_ID)], vec![silent], &cfg).await;

        let event_type = manager
            .get_event_type(&GtsTypeId::try_new(EVENT_TYPE_ID).unwrap())
            .await
            .expect("the cache reads")
            .expect("it resolves");
        assert_eq!(
            event_type.partition_key, "/subject_tenant",
            "the default on the base's trait schema reaches a type that states none"
        );
    }

    /// A type naming `/tenant_id`, which the base does not declare, is left out at
    /// boot with a warning; its publishes find no event type.
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_type_naming_the_old_tenant_pointer_is_not_loaded_and_is_warned_about() {
        let cfg = config(&json!({}));
        let (manager, _scratch) = manager_with(
            vec![topic_document(TOPIC_ID)],
            vec![OldPointerV1::gts_schema_with_refs()],
            &cfg,
        )
        .await;

        let event_type = manager
            .get_event_type(&GtsTypeId::try_new(<OldPointerV1 as GtsSchema>::TYPE_ID).unwrap())
            .await
            .expect("the cache reads");
        assert!(event_type.is_none(), "{event_type:?}");
        logs_assert(|lines: &[&str]| {
            let warned: Vec<&&str> = lines
                .iter()
                .filter(|line| {
                    line.contains("event type excluded: its partition key does not validate")
                })
                .collect();
            match warned.as_slice() {
                [line]
                    if line.contains(<OldPointerV1 as GtsSchema>::TYPE_ID)
                        && line.contains("names no declared member `tenant_id`") =>
                {
                    Ok(())
                }
                _ => Err(format!(
                    "expected one warning naming the type, got {warned:?}"
                )),
            }
        });
    }

    /// The listing of a base type's derived schemas includes the base, because
    /// a base identifier behaves as the implicit envelope `...~*`. It is
    /// abstract, so nothing may resolve it as an event type of its own.
    #[tokio::test]
    async fn the_abstract_base_is_not_materialized_as_an_event_type() {
        let cfg = config(&json!({}));
        let (manager, _scratch) = manager_with(
            vec![topic_document(TOPIC_ID)],
            vec![FooV1::gts_schema_with_refs()],
            &cfg,
        )
        .await;

        let listed: Vec<String> = manager
            .list_event_types()
            .await
            .expect("the cache reads")
            .into_iter()
            .map(|event_type| event_type.id.as_ref().to_owned())
            .collect();
        assert_eq!(listed, vec![EVENT_TYPE_ID.to_owned()]);
    }

    /// The delivery side of the role split: an instance that holds no topic
    /// configuration serves the settings ingest already resolved, out of the
    /// database they share. Two instances with drifted configuration cannot
    /// therefore serve one topic under two partition counts.
    #[tokio::test]
    async fn a_manager_serves_what_a_load_resolved_under_configuration_it_never_saw() {
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        let client: Arc<dyn PlatformTypesRegistryApi> =
            registry_holding(&[topic_document(TOPIC_ID)], &[]);
        let ingest_config = config(
            &json!({ toolkit_gts::gts_id!("cf.core.events.topic.v1~"): { "partitions": 4 } }),
        );
        super::bulk_load(&client, &db, &ingest_config)
            .await
            .expect("the ingest role loads");

        // Built with no configuration of its own at all - it only reads.
        let delivery = super::TypesRegistrySpecificationManager::new(Arc::clone(&db));
        let topic = delivery
            .get_topic(&GtsInstanceId::try_new(TOPIC_ID).unwrap())
            .await
            .expect("the cache reads")
            .expect("the stored record resolves");
        assert_eq!(*topic.settings.partitions().value(), 4);
        assert_eq!(topic.settings.partitions().source(), Source::TypeEntry);
    }

    #[tokio::test]
    async fn get_topic_returns_none_for_an_unregistered_id() {
        let cfg = config(&json!({}));
        let (manager, _scratch) = manager_with(Vec::new(), Vec::new(), &cfg).await;

        let missing = GtsInstanceId::try_new(toolkit_gts::gts_id!(
            "cf.core.events.topic.v1~example.eb.missing.topic.v1"
        ))
        .unwrap();
        let found = manager.get_topic(&missing).await;
        assert!(matches!(found, Ok(None)), "{found:?}");
    }

    /// A cache that cannot be read says nothing about what is registered.
    #[tokio::test]
    async fn a_spec_cache_read_failure_is_an_error_not_not_found() {
        let scratch = test_db().await;
        let (db, dsn) = (Arc::clone(scratch.db()), scratch.dsn().to_owned());
        super::bulk_load(&client_with(&[TOPIC_ID]), &db, &config(&json!({})))
            .await
            .expect("bulk_load");
        let manager = super::TypesRegistrySpecificationManager::new(Arc::clone(&db));
        crate::test_support::tables::drop_table(&dsn, "event_broker_spec_cache").await;

        let topic = manager
            .get_topic(&GtsInstanceId::try_new(TOPIC_ID).unwrap())
            .await;
        let Err(err) = topic else {
            panic!("an unreadable cache must not answer: {topic:?}");
        };
        assert!(
            matches!(&err, DomainError::StorageUnavailable { reason, source: Some(_) } if reason == "database scope resolution failed"),
            "{err:?}"
        );
        let listed = manager.list_topics().await;
        assert!(
            matches!(&listed, Err(DomainError::StorageUnavailable { .. })),
            "{listed:?}"
        );
    }

    /// An unreadable cache says nothing about which event types are registered.
    #[tokio::test]
    async fn an_event_type_read_failure_is_an_error_not_not_found() {
        let scratch = test_db().await;
        let (db, dsn) = (Arc::clone(scratch.db()), scratch.dsn().to_owned());
        let client: Arc<dyn PlatformTypesRegistryApi> = registry_holding(
            &[topic_document(TOPIC_ID)],
            &[FooV1::gts_schema_with_refs()],
        );
        super::bulk_load(&client, &db, &config(&json!({})))
            .await
            .expect("bulk_load");
        let manager = super::TypesRegistrySpecificationManager::new(Arc::clone(&db));
        crate::test_support::tables::drop_table(&dsn, "event_broker_spec_cache").await;

        let event_type = manager
            .get_event_type(&GtsTypeId::try_new(EVENT_TYPE_ID).unwrap())
            .await;
        assert!(
            matches!(&event_type, Err(DomainError::StorageUnavailable { reason, source: Some(_) }) if reason == "database scope resolution failed"),
            "{event_type:?}"
        );
        let listed = manager.list_event_types().await;
        assert!(
            matches!(&listed, Err(DomainError::StorageUnavailable { .. })),
            "{listed:?}"
        );
    }

    /// A load must preserve an already-assigned surrogate id, not renumber it.
    #[tokio::test]
    async fn resolve_topic_id_is_stable_across_a_second_load() {
        let cfg = config(&json!({}));
        let client: Arc<dyn PlatformTypesRegistryApi> =
            registry_holding(&[topic_document(TOPIC_ID)], &[]);
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());

        super::bulk_load(&client, &db, &cfg).await.expect("first");
        let manager = super::TypesRegistrySpecificationManager::new(Arc::clone(&db));
        let topic_gts_id = GtsInstanceId::try_new(TOPIC_ID).unwrap();
        let first = manager
            .resolve_topic_id(&topic_gts_id)
            .await
            .expect("first");

        super::bulk_load(&client, &db, &cfg).await.expect("second");
        let second = manager
            .resolve_topic_id(&topic_gts_id)
            .await
            .expect("second");

        assert_eq!(first, second, "surrogate id must not be renumbered");
    }

    #[tokio::test]
    async fn resolve_topic_id_returns_not_found_for_an_unregistered_id() {
        let cfg = config(&json!({}));
        let (manager, _scratch) = manager_with(Vec::new(), Vec::new(), &cfg).await;

        let missing = GtsInstanceId::try_new(toolkit_gts::gts_id!(
            "cf.core.events.topic.v1~example.eb.missing.topic.v1"
        ))
        .unwrap();
        let err = manager
            .resolve_topic_id(&missing)
            .await
            .expect_err("an unregistered topic must not resolve");
        assert!(matches!(err, DomainError::NotFound { .. }));
    }

    // ── Backend routing (feature 0005 §2.1, §2.2) ───────────────────────────

    const OTHER_TOPIC_ID: &str =
        toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.t2.topic.v1");
    const MISTYPED_KEY: &str =
        toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.tl.topic.v1");

    /// Two registry entries, the second carrying a settings value that must
    /// never reach the cache, routed by `backend_routing`.
    fn routed_config(backend_routing: &serde_json::Value) -> EventBrokerConfig {
        serde_json::from_value(json!({
            "mode": "standalone",
            "backends": {
                "main": { "type": SQLITE_BACKEND },
                "audit": { "type": SQLITE_BACKEND, "path": "/var/lib/eb/hunter2.db" },
            },
            "backend_routing": backend_routing.clone(),
        }))
        .expect("test configuration deserializes")
    }

    fn client_with(ids: &[&str]) -> Arc<dyn PlatformTypesRegistryApi> {
        let topics: Vec<serde_json::Value> = ids.iter().map(|id| topic_document(id)).collect();
        registry_holding(&topics, &[])
    }

    /// The Registry Reference `client` answers for the topic `id`.
    async fn registry_reference(
        client: &Arc<dyn PlatformTypesRegistryApi>,
        id: &str,
    ) -> uuid::Uuid {
        use super::PlatformTypesRegistryApiExt;

        client
            .resolve_instance_uuid(
                &super::PlatformSecurityContext::outbound_marker(),
                &GtsInstanceId::try_new(id).unwrap(),
            )
            .await
            .expect("the registry holds the topic")
    }

    async fn cached_topic_payload(
        db: &Arc<DBProvider<toolkit_db::DbError>>,
        id: &str,
    ) -> serde_json::Value {
        let row = super::TypesRegistrySpecificationManager::new(Arc::clone(db))
            .find_row(super::SpecKind::Topic, id)
            .await
            .expect("the cache answers")
            .expect("the topic is cached");
        serde_json::from_str(&row.payload).expect("a cached payload is JSON")
    }

    /// The cached record names the registry entry and nothing of its type or
    /// settings: nothing the broker persists carries an address or a setting.
    #[tokio::test]
    async fn a_cached_topic_carries_its_route_and_no_backend_settings() {
        let cfg = routed_config(&json!({
            BASE_ROUTE: "main",
            TOPIC_ID: "audit",
        }));
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        let client = client_with(&[TOPIC_ID]);
        super::bulk_load(&client, &db, &cfg)
            .await
            .expect("bulk_load");
        let reference = registry_reference(&client, TOPIC_ID).await;

        assert_eq!(
            cached_topic_payload(&db, TOPIC_ID).await,
            json!({
                "id": TOPIC_ID,
                "uuid": reference.to_string(),
                "description": "events this test publishes",
                "retention": "PT168H",
                "settings": {
                    "partitions": { "value": 8, "source": "built_in" },
                    "retention": {
                        "value": { "duration": { "secs": 604_800, "nanos": 0 }, "size_bytes": null },
                        "source": "specification",
                    },
                    "backend": { "value": "audit", "source": "topic_entry" },
                },
            })
        );
    }

    /// A fully qualified key that names no registered topic is most likely
    /// mistyped, and would leave its topic on the instance-less route for good.
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_routing_key_naming_no_registered_topic_is_warned_about() {
        let cfg = routed_config(&json!({
            BASE_ROUTE: "main",
            MISTYPED_KEY: "audit",
        }));
        super::bulk_load(&client_with(&[TOPIC_ID]), test_db().await.db(), &cfg)
            .await
            .expect("bulk_load");

        logs_assert(|lines: &[&str]| {
            let warned: Vec<&&str> = lines
                .iter()
                .filter(|line| {
                    line.contains(
                        "backend_routing key names no registered topic; the topic it was meant for takes the instance-less route",
                    )
                })
                .collect();
            match warned.as_slice() {
                [line] if line.contains(MISTYPED_KEY) => Ok(()),
                _ => Err(format!(
                    "expected one warning naming the key, got {warned:?}"
                )),
            }
        });
    }

    /// A key naming a registered topic, and a key naming a type rather than
    /// one topic, are not warned about.
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_routing_key_naming_a_registered_topic_or_a_type_is_not_warned_about() {
        const SPECIAL_TOPIC_TYPE: &str =
            toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.special.topic.v1~");
        let cfg = routed_config(&json!({
            BASE_ROUTE: "main",
            SPECIAL_TOPIC_TYPE: "audit",
            TOPIC_ID: "audit",
        }));
        super::bulk_load(&client_with(&[TOPIC_ID]), test_db().await.db(), &cfg)
            .await
            .expect("bulk_load");

        assert!(!logs_contain(
            "backend_routing key names no registered topic"
        ));
    }

    /// A pass whose listing failed saw no topics, so every key would look
    /// unmatched; it warns about none.
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_failed_listing_warns_about_no_routing_key() {
        let cfg = routed_config(&json!({
            BASE_ROUTE: "main",
            TOPIC_ID: "audit",
        }));
        let registry = MockTypesRegistry::new();
        registry
            .inject(Fault::on(Call::List).fail(CanonicalError::internal("registry down").create()));
        let client: Arc<dyn PlatformTypesRegistryApi> = Arc::new(registry);
        super::bulk_load(&client, test_db().await.db(), &cfg)
            .await
            .expect("a failed listing is not a failed load");

        assert!(logs_contain("list_instances failed"));
        assert!(!logs_contain(
            "backend_routing key names no registered topic"
        ));
    }

    /// Moving a provisioned topic to another entry leaves its log behind on
    /// the old one, so the change is reported, naming both entries.
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_topic_moved_to_another_entry_is_warned_about_once() {
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        let client = client_with(&[TOPIC_ID, OTHER_TOPIC_ID]);
        let before = routed_config(&json!({ BASE_ROUTE: "main", OTHER_TOPIC_ID: "audit" }));
        let after = routed_config(&json!({ BASE_ROUTE: "main", TOPIC_ID: "audit" }));

        super::bulk_load(&client, &db, &before)
            .await
            .expect("first");
        assert!(!logs_contain("topic re-routed"));
        super::bulk_load(&client, &db, &after)
            .await
            .expect("second");

        logs_assert(|lines: &[&str]| {
            let moved: Vec<&&str> = lines
                .iter()
                .filter(|line| line.contains("topic re-routed to another backend registry entry"))
                .collect();
            match moved.as_slice() {
                [first, second] => {
                    let mentions = |line: &str, topic: &str, from: &str, to: &str| {
                        line.contains(topic)
                            && line.contains(&format!("from={from}"))
                            && line.contains(&format!("to={to}"))
                    };
                    let both = [first, second];
                    if both
                        .iter()
                        .any(|line| mentions(line, TOPIC_ID, "main", "audit"))
                        && both
                            .iter()
                            .any(|line| mentions(line, OTHER_TOPIC_ID, "audit", "main"))
                    {
                        Ok(())
                    } else {
                        Err(format!("unexpected re-route lines: {moved:?}"))
                    }
                }
                _ => Err(format!("expected two re-route lines, got {moved:?}")),
            }
        });
        super::bulk_load(&client, &db, &after)
            .await
            .expect("third, with the route unchanged");
        logs_assert(|lines: &[&str]| {
            let moved = lines
                .iter()
                .filter(|line| line.contains("topic re-routed to another backend registry entry"))
                .count();
            if moved == 2 {
                Ok(())
            } else {
                Err(format!(
                    "an unchanged route is not reported again; got {moved} lines"
                ))
            }
        });
        let reference = registry_reference(&client, TOPIC_ID).await;
        assert_eq!(
            cached_topic_payload(&db, TOPIC_ID).await,
            json!({
                "id": TOPIC_ID,
                "uuid": reference.to_string(),
                "description": "events this test publishes",
                "retention": "PT168H",
                "settings": {
                    "partitions": { "value": 8, "source": "built_in" },
                    "retention": {
                        "value": { "duration": { "secs": 604_800, "nanos": 0 }, "size_bytes": null },
                        "source": "specification",
                    },
                    "backend": { "value": "audit", "source": "topic_entry" },
                },
            })
        );
    }

    // ── Event-type candidates ───────────────────────────────────────────────

    const TOPIC_A: &str = toolkit_gts::gts_id!("cf.core.events.topic.v1~x.eb.cand.a.v1");
    const TOPIC_B: &str = toolkit_gts::gts_id!("cf.core.events.topic.v1~x.eb.cand.b.v1");
    const TOPIC_NO_TYPES: &str = toolkit_gts::gts_id!("cf.core.events.topic.v1~x.eb.cand.none.v1");
    const TOPIC_AT_CAP: &str = toolkit_gts::gts_id!("cf.core.events.topic.v1~x.eb.cand.at_cap.v1");
    const TOPIC_OVER_CAP: &str =
        toolkit_gts::gts_id!("cf.core.events.topic.v1~x.eb.cand.over_cap.v1");
    const TYPE_A1: &str = toolkit_gts::gts_id!("cf.core.events.event.v1~x.eb.cand.a1.v1~");
    const TYPE_A2: &str = toolkit_gts::gts_id!("cf.core.events.event.v1~x.eb.cand.a2.v1~");
    const TYPE_B1: &str = toolkit_gts::gts_id!("cf.core.events.event.v1~x.eb.cand.b1.v1~");
    const SUBJECT_S1: &str = toolkit_gts::gts_id!("x.eb.cand.s1.v1~");
    const SUBJECT_S2_ANY: &str = toolkit_gts::gts_id!("x.eb.cand.s2.v1~acme.*");

    /// Loads `topics` and `event_types` (`(id, topic, allowed_subject_types)`)
    /// into `db` through [`super::bulk_load`], the cache's only writer.
    async fn load_candidates(
        db: &Arc<DBProvider<toolkit_db::DbError>>,
        topics: &[&str],
        event_types: &[(String, &str, Vec<&str>)],
    ) {
        let mut spec: Vec<serde_json::Value> = topics
            .iter()
            .map(|id| json!({ "id": id, "partitions": 1 }))
            .collect();
        spec.extend(event_types.iter().map(|(id, topic, allowed)| {
            json!({ "id": id, "topic": topic, "allowed_subject_types": allowed })
        }));
        let fixtures = StaticTypesRegistry::of(serde_json::Value::Array(spec));
        let topic_documents: Vec<serde_json::Value> =
            topics.iter().map(|id| topic_document(id)).collect();
        let client: Arc<dyn PlatformTypesRegistryApi> =
            registry_holding(&topic_documents, fixtures.event_type_schemas());
        super::bulk_load(&client, db, &config(&json!({})))
            .await
            .expect("bulk_load");
    }

    async fn candidates_of(
        manager: &super::TypesRegistrySpecificationManager,
        topic: &str,
    ) -> EventTypeCandidates {
        manager
            .get_event_type_candidates(&GtsInstanceId::try_new(topic).unwrap())
            .await
            .expect("the cache answers")
    }

    fn candidate(id: &str, allowed_subject_types: &[&str]) -> EventTypeCandidate {
        EventTypeCandidate {
            id: GtsTypeId::try_new(id).unwrap(),
            allowed_subject_types: allowed_subject_types
                .iter()
                .map(|pattern| GtsIdPattern::try_new(pattern).unwrap())
                .collect(),
        }
    }

    #[tokio::test]
    async fn the_spec_cache_returns_a_topics_event_type_ids_on_sqlite() {
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        load_candidates(
            &db,
            &[TOPIC_A, TOPIC_B, TOPIC_NO_TYPES],
            &[
                (
                    TYPE_A2.to_owned(),
                    TOPIC_A,
                    vec![SUBJECT_S1, SUBJECT_S2_ANY],
                ),
                (TYPE_B1.to_owned(), TOPIC_B, vec![SUBJECT_S2_ANY]),
                (TYPE_A1.to_owned(), TOPIC_A, vec![SUBJECT_S1]),
            ],
        )
        .await;
        let manager = super::TypesRegistrySpecificationManager::new(db);

        assert_eq!(
            candidates_of(&manager, TOPIC_A).await,
            EventTypeCandidates::Listed(vec![
                candidate(TYPE_A1, &[SUBJECT_S1]),
                candidate(TYPE_A2, &[SUBJECT_S1, SUBJECT_S2_ANY]),
            ])
        );
        assert_eq!(
            candidates_of(&manager, TOPIC_B).await,
            EventTypeCandidates::Listed(vec![candidate(TYPE_B1, &[SUBJECT_S2_ANY])])
        );
        assert_eq!(
            candidates_of(&manager, TOPIC_NO_TYPES).await,
            EventTypeCandidates::Listed(Vec::new())
        );
    }

    #[tokio::test]
    async fn a_topic_with_256_event_types_lists_them_and_one_with_257_is_over_the_cap() {
        let type_id = |name: &str, n: usize| {
            format!("gts.cf.core.events.event.v1~x.eb.cand.{name}_{n:03}.v1~")
        };
        let mut event_types: Vec<(String, &str, Vec<&str>)> = (0..256)
            .map(|n| (type_id("at_cap", n), TOPIC_AT_CAP, vec![SUBJECT_S1]))
            .collect();
        event_types.extend((0..257).map(|n| (type_id("over_cap", n), TOPIC_OVER_CAP, vec![])));
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        load_candidates(&db, &[TOPIC_AT_CAP, TOPIC_OVER_CAP], &event_types).await;
        let manager = super::TypesRegistrySpecificationManager::new(db);

        let EventTypeCandidates::Listed(listed) = candidates_of(&manager, TOPIC_AT_CAP).await
        else {
            panic!("256 event types are within the cap");
        };
        assert_eq!(listed.len(), 256);
        let (Some(first), Some(last)) = (listed.first(), listed.last()) else {
            panic!("the list holds 256 entries");
        };
        assert_eq!(first, &candidate(&type_id("at_cap", 0), &[SUBJECT_S1]));
        assert_eq!(last, &candidate(&type_id("at_cap", 255), &[SUBJECT_S1]));
        assert_eq!(
            candidates_of(&manager, TOPIC_OVER_CAP).await,
            EventTypeCandidates::OverCap
        );
    }

    fn type_ids(ids: &[&str]) -> Vec<GtsTypeId> {
        ids.iter()
            .map(|id| GtsTypeId::try_new(id).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn the_spec_cache_returns_which_of_the_given_ids_are_registered_on_the_topic() {
        const TYPE_X: &str = toolkit_gts::gts_id!("cf.core.events.event.v1~x.eb.cand.x.v1~");
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        load_candidates(
            &db,
            &[TOPIC_A, TOPIC_B],
            &[
                (TYPE_A1.to_owned(), TOPIC_A, vec![SUBJECT_S1]),
                (TYPE_A2.to_owned(), TOPIC_A, vec![SUBJECT_S1]),
                (TYPE_B1.to_owned(), TOPIC_B, vec![SUBJECT_S1]),
            ],
        )
        .await;
        let manager = super::TypesRegistrySpecificationManager::new(db);

        let registered = manager
            .get_registered_event_types(
                &GtsInstanceId::try_new(TOPIC_A).unwrap(),
                &type_ids(&[TYPE_A1, TYPE_B1, TYPE_X]),
            )
            .await
            .expect("the cache answers");

        assert_eq!(registered, type_ids(&[TYPE_A1]));
    }

    /// The table is gone, so any statement fails: an empty ask that answers
    /// sent none.
    #[tokio::test]
    async fn asking_for_no_ids_makes_no_query() {
        let scratch = test_db().await;
        let (db, dsn) = (Arc::clone(scratch.db()), scratch.dsn().to_owned());
        load_candidates(
            &db,
            &[TOPIC_A],
            &[(TYPE_A1.to_owned(), TOPIC_A, vec![SUBJECT_S1])],
        )
        .await;
        crate::test_support::tables::drop_table(&dsn, "event_broker_spec_cache").await;
        let manager = super::TypesRegistrySpecificationManager::new(db);

        let registered = manager
            .get_registered_event_types(&GtsInstanceId::try_new(TOPIC_A).unwrap(), &[])
            .await
            .expect("an empty ask reads nothing");

        assert_eq!(registered, Vec::<GtsTypeId>::new());
    }

    #[tokio::test]
    async fn a_topic_row_stores_no_topic_and_an_empty_subject_type_list() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use toolkit_db::secure::SecureEntityExt;

        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        load_candidates(&db, &[TOPIC_A], &[]).await;

        let row = super::SpecCacheEntity::find()
            .filter(super::SpecCacheColumn::GtsId.eq(TOPIC_A))
            .secure()
            .scope_with(&super::AccessScope::allow_all())
            .one(&db.conn().expect("a connection"))
            .await
            .expect("the cache answers")
            .expect("the topic is cached");
        assert_eq!(
            (
                row.gts_id.as_str(),
                row.kind.as_str(),
                row.topic,
                row.allowed_subject_types.as_str()
            ),
            (TOPIC_A, "topic", None, "[]")
        );
    }

    #[tokio::test]
    async fn reloading_an_event_type_rewrites_its_topic_and_subject_types() {
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        load_candidates(
            &db,
            &[TOPIC_A, TOPIC_B],
            &[(TYPE_A1.to_owned(), TOPIC_A, vec![SUBJECT_S1])],
        )
        .await;
        load_candidates(
            &db,
            &[TOPIC_A, TOPIC_B],
            &[(TYPE_A1.to_owned(), TOPIC_B, vec![SUBJECT_S2_ANY])],
        )
        .await;
        let manager = super::TypesRegistrySpecificationManager::new(db);

        assert_eq!(
            candidates_of(&manager, TOPIC_A).await,
            EventTypeCandidates::Listed(Vec::new())
        );
        assert_eq!(
            candidates_of(&manager, TOPIC_B).await,
            EventTypeCandidates::Listed(vec![candidate(TYPE_A1, &[SUBJECT_S2_ANY])])
        );
    }

    /// Overwrites `column` of the row `gts_id` with `value`, as no writer of
    /// the cache would.
    async fn plant(
        db: &Arc<DBProvider<toolkit_db::DbError>>,
        gts_id: &str,
        column: super::SpecCacheColumn,
        value: &str,
    ) {
        use sea_orm::{ColumnTrait, EntityTrait};
        use toolkit_db::secure::SecureUpdateExt;

        super::SpecCacheEntity::update_many()
            .secure()
            .scope_with(&super::AccessScope::allow_all())
            .filter(super::Condition::all().add(super::SpecCacheColumn::GtsId.eq(gts_id)))
            .col_expr(column, super::Expr::value(value.to_owned()))
            .exec(&db.conn().expect("a connection"))
            .await
            .expect("the row is overwritten");
    }

    #[tokio::test]
    async fn a_stored_subject_type_list_that_does_not_decode_is_an_internal_error_without_it() {
        const PLANTED: &str = r#"["gts.PLANTED.subject"]"#;
        let raw = serde_json::from_str::<super::StoredSubjectTypes>(PLANTED)
            .err()
            .expect("the planted list does not decode")
            .to_string();
        assert!(raw.contains("PLANTED"), "{raw}");
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        load_candidates(
            &db,
            &[TOPIC_A],
            &[(TYPE_A1.to_owned(), TOPIC_A, vec![SUBJECT_S1])],
        )
        .await;
        plant(
            &db,
            TYPE_A1,
            super::SpecCacheColumn::AllowedSubjectTypes,
            PLANTED,
        )
        .await;
        let manager = super::TypesRegistrySpecificationManager::new(db);

        let err = manager
            .get_event_type_candidates(&GtsInstanceId::try_new(TOPIC_A).unwrap())
            .await
            .expect_err("a list that does not decode fails the read");
        assert!(
            matches!(
                &err,
                DomainError::Internal(message)
                    if message == "a cached allowed_subject_types list does not decode"
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_stored_event_type_id_that_does_not_parse_is_an_internal_error_without_it() {
        const PLANTED: &str = "gts.PLANTED.event~";
        let raw = GtsTypeId::try_new(PLANTED)
            .expect_err("the planted id does not parse")
            .to_string();
        assert!(raw.contains("PLANTED"), "{raw}");
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        load_candidates(
            &db,
            &[TOPIC_A],
            &[(TYPE_A1.to_owned(), TOPIC_A, vec![SUBJECT_S1])],
        )
        .await;
        plant(&db, TYPE_A1, super::SpecCacheColumn::GtsId, PLANTED).await;
        let manager = super::TypesRegistrySpecificationManager::new(db);

        let err = manager
            .get_event_type_candidates(&GtsInstanceId::try_new(TOPIC_A).unwrap())
            .await
            .expect_err("an id that does not parse fails the read");
        assert!(
            matches!(
                &err,
                DomainError::Internal(message)
                    if message == "a cached event type id does not parse"
            ),
            "{err:?}"
        );
    }

    /// A topic row stored without the topic's Registry Reference, as rows were
    /// before a topic carried one, reads as no topic: a guessed reference
    /// would address the wrong binding record.
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_topic_row_written_without_its_registry_reference_reads_as_absent() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use toolkit_db::secure::SecureEntityExt;

        let cfg = config(&json!({ BASE_ROUTE: { "partitions": 4 } }));
        let (manager, scratch) =
            manager_with(vec![topic_document(TOPIC_ID)], Vec::new(), &cfg).await;
        let conn = scratch.db().conn().expect("a connection");
        let row = super::SpecCacheEntity::find()
            .filter(super::SpecCacheColumn::GtsId.eq(TOPIC_ID))
            .secure()
            .scope_with(&super::AccessScope::allow_all())
            .one(&conn)
            .await
            .expect("the cache answers")
            .expect("the topic is cached");
        let mut payload: serde_json::Value =
            serde_json::from_str(&row.payload).expect("the payload is JSON");
        payload
            .as_object_mut()
            .expect("the payload is an object")
            .remove("uuid");
        super::upsert_spec_row(&conn, TOPIC_ID, &payload, super::SpecColumns::Topic)
            .await
            .expect("the row is rewritten");

        let topic = manager
            .get_topic(&GtsInstanceId::try_new(TOPIC_ID).unwrap())
            .await
            .expect("the cache reads");
        assert!(topic.is_none(), "the row must read as absent: {topic:?}");
        let topics = manager.list_topics().await.expect("the cache reads");
        assert!(topics.is_empty(), "the row must not be listed: {topics:?}");
        assert!(logs_contain("failed to deserialize cached spec row"));
    }
}
