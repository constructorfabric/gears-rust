//! REST DTOs for the Types Registry gear.

use uuid::Uuid;

use gts::GtsIdSegment;
use types_registry_sdk::RegisterSummary;

use crate::domain::enums::{
    EntityKind, LifecycleStatus, OperationItemStatus, OperationKind, OperationStatus,
};
use crate::domain::model::{GtsEntity, ListQuery, SegmentMatchScope};
use crate::domain::registry_service::{EntityRecord, OperationItemRecord, OperationRecord};

/// DTO for a GTS ID segment.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct GtsIdSegmentDto {
    /// Vendor component of the segment.
    pub vendor: String,
    /// Package component of the segment.
    pub package: String,
    /// Namespace component of the segment.
    pub namespace: String,
    /// Type name component of the segment.
    pub type_name: String,
    /// Major version number.
    pub ver_major: u32,
}

impl From<&GtsIdSegment> for GtsIdSegmentDto {
    fn from(segment: &GtsIdSegment) -> Self {
        Self {
            vendor: segment.vendor().to_owned(),
            package: segment.package().to_owned(),
            namespace: segment.namespace().to_owned(),
            type_name: segment.type_name().to_owned(),
            ver_major: segment.ver_major_opt().unwrap_or(0),
        }
    }
}

/// Response DTO for a GTS entity.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct GtsEntityDto {
    /// Deterministic UUID generated from the GTS ID.
    pub id: Uuid,
    /// The full GTS identifier string.
    pub gts_id: String,
    /// All parsed segments from the GTS ID.
    pub segments: Vec<GtsIdSegmentDto>,
    /// Whether this entity is a schema (type definition).
    ///
    /// - `true`: This is a type definition (GTS ID ends with `~`)
    /// - `false`: This is an instance (GTS ID does not end with `~`)
    pub is_schema: bool,
    /// The entity content (schema for types, object for instances).
    pub content: serde_json::Value,
    /// Optional description of the entity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl From<GtsEntity> for GtsEntityDto {
    fn from(entity: GtsEntity) -> Self {
        Self {
            id: entity.uuid,
            gts_id: entity.gts_id.clone(),
            segments: entity.segments.iter().map(GtsIdSegmentDto::from).collect(),
            is_schema: entity.is_type_schema,
            content: entity.content.clone(),
            description: entity.description.clone(),
        }
    }
}

/// Request DTO for registering GTS entities.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct RegisterEntitiesRequest {
    /// Array of GTS entities to register.
    pub entities: Vec<serde_json::Value>,
}

/// Result of registering a single entity.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
#[serde(tag = "status")]
pub enum RegisterResultDto {
    /// Successfully registered entity.
    #[serde(rename = "ok")]
    Ok {
        /// The registered entity.
        entity: GtsEntityDto,
    },
    /// Failed to register entity.
    #[serde(rename = "error")]
    Error {
        /// The GTS ID that was attempted, if available.
        #[serde(skip_serializing_if = "Option::is_none")]
        gts_id: Option<String>,
        /// Error message.
        error: String,
    },
}

/// Response DTO for batch registration.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct RegisterEntitiesResponse {
    /// Summary of the registration operation.
    pub summary: RegisterSummaryDto,
    /// Results for each entity in the request.
    pub results: Vec<RegisterResultDto>,
}

/// Summary of a batch registration operation.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct RegisterSummaryDto {
    /// Total number of entities processed.
    pub total: usize,
    /// Number of successfully registered entities.
    pub succeeded: usize,
    /// Number of failed registrations.
    pub failed: usize,
}

impl From<RegisterSummary> for RegisterSummaryDto {
    fn from(summary: RegisterSummary) -> Self {
        Self {
            total: summary.total(),
            succeeded: summary.succeeded,
            failed: summary.failed,
        }
    }
}

/// Query parameters for listing GTS entities.
#[derive(Debug, Clone, Default)]
#[toolkit_macros::api_dto(request)]
pub struct ListEntitiesQuery {
    /// Optional wildcard pattern for GTS ID matching.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Filter by schema type: true for types, false for instances.
    #[serde(default)]
    pub is_schema: Option<bool>,
    /// Filter by vendor. Applied to segments per `segment_scope`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Filter by package. Applied to segments per `segment_scope`.
    #[serde(default)]
    pub package: Option<String>,
    /// Filter by namespace. Applied to segments per `segment_scope`.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Controls which chain segments the vendor / package / namespace filters
    /// match against. Either `"primary"` (first segment only) or `"any"`
    /// (any segment in the chain). Defaults to `"any"` when omitted.
    #[serde(default)]
    pub segment_scope: Option<String>,
}

impl ListEntitiesQuery {
    /// Converts this DTO to the internal `ListQuery`.
    ///
    /// An unknown `segment_scope` value silently falls back to the default
    /// (`Any`) — query params are best-effort. Tightening this to a 400
    /// would change the wire contract.
    #[must_use]
    pub fn to_list_query(&self) -> ListQuery {
        let mut query = ListQuery::default();

        if let Some(ref pattern) = self.pattern {
            query = query.with_pattern(pattern);
        }

        if let Some(is_schema) = self.is_schema {
            query = query.with_is_type(is_schema);
        }

        if let Some(ref vendor) = self.vendor {
            query = query.with_vendor(vendor);
        }

        if let Some(ref package) = self.package {
            query = query.with_package(package);
        }

        if let Some(ref namespace) = self.namespace {
            query = query.with_namespace(namespace);
        }

        if let Some(ref scope) = self.segment_scope {
            let parsed = match scope.as_str() {
                "primary" => SegmentMatchScope::Primary,
                _ => SegmentMatchScope::Any,
            };
            query = query.with_segment_scope(parsed);
        }

        query
    }
}

/// Response DTO for listing GTS entities.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ListEntitiesResponse {
    /// The list of entities.
    pub entities: Vec<GtsEntityDto>,
    /// Total count of entities returned.
    pub count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gts::GtsIdSegment;
    use toolkit_gts::{GTS_ID_PREFIX, gts_id};

    fn seg(full_id: &str, idx: usize) -> GtsIdSegment {
        gts::GtsId::try_new(full_id)
            .unwrap_or_else(|e| panic!("invalid GTS id `{full_id}`: {e}"))
            .into_segments()
            .remove(idx)
    }

    #[test]
    fn test_gts_entity_dto_from_entity() {
        const TYPE_ID: &str = gts_id!("acme.core.events.user_created.v1~");

        let segment = seg(TYPE_ID, 0);
        let entity = GtsEntity::new(
            Uuid::nil(),
            TYPE_ID,
            vec![segment],
            true, // is_schema
            serde_json::json!({"type": "object"}),
            Some("A user created event".to_owned()),
        );

        let dto: GtsEntityDto = entity.into();
        assert_eq!(dto.gts_id, TYPE_ID);
        assert!(dto.is_schema);
        assert_eq!(dto.segments.len(), 1);
        assert_eq!(dto.segments[0].vendor, "acme");
        assert_eq!(dto.segments[0].package, "core");
        assert_eq!(dto.segments[0].namespace, "events");
        assert_eq!(dto.segments[0].type_name, "user_created");
        assert_eq!(dto.segments[0].ver_major, 1);
        assert_eq!(dto.description, Some("A user created event".to_owned()));
    }

    #[test]
    fn test_gts_entity_dto_instance() {
        const INSTANCE_ID: &str =
            gts_id!("acme.core.events.user_created.v1~acme.core.instances.instance1.v1");

        let entity = GtsEntity::new(
            Uuid::nil(),
            INSTANCE_ID,
            vec![],
            false, // is_schema
            serde_json::json!({"data": "value"}),
            None,
        );

        let dto: GtsEntityDto = entity.into();
        assert!(!dto.is_schema);
        assert!(dto.segments.is_empty());
        assert_eq!(dto.description, None);
    }

    #[test]
    fn test_gts_entity_dto_with_multiple_segments() {
        const INSTANCE_ID: &str = gts_id!("acme.core.models.user.v1~acme.core.instances.user1.v1");

        let segment1 = seg(INSTANCE_ID, 0);
        let segment2 = seg(INSTANCE_ID, 1);
        let entity = GtsEntity::new(
            Uuid::nil(),
            INSTANCE_ID,
            vec![segment1, segment2],
            false, // is_schema
            serde_json::json!({"userId": "user-001"}),
            None,
        );

        let dto: GtsEntityDto = entity.into();
        assert!(!dto.is_schema);
        assert_eq!(dto.segments.len(), 2);
        // First segment (type)
        assert_eq!(dto.segments[0].vendor, "acme");
        assert_eq!(dto.segments[0].type_name, "user");
        // Second segment (instance)
        assert_eq!(dto.segments[1].vendor, "acme");
        assert_eq!(dto.segments[1].type_name, "user1");
    }

    #[test]
    fn test_gts_entity_dto_with_different_vendors_in_segments() {
        const INSTANCE_ID: &str =
            gts_id!("acme.core.models.product.v1~globex.retail.instances.prod1.v1");

        // Instance where type and instance have different vendors
        let segment1 = seg(INSTANCE_ID, 0);
        let segment2 = seg(INSTANCE_ID, 1);
        let entity = GtsEntity::new(
            Uuid::nil(),
            INSTANCE_ID,
            vec![segment1, segment2],
            false, // is_schema
            serde_json::json!({"productId": "prod-001"}),
            None,
        );

        let dto: GtsEntityDto = entity.into();
        assert_eq!(dto.segments.len(), 2);
        // Type segment from vendor "acme"
        assert_eq!(dto.segments[0].vendor, "acme");
        assert_eq!(dto.segments[0].package, "core");
        assert_eq!(dto.segments[0].namespace, "models");
        assert_eq!(dto.segments[0].type_name, "product");
        assert_eq!(dto.segments[0].ver_major, 1);
        // Instance segment from different vendor "globex"
        assert_eq!(dto.segments[1].vendor, "globex");
        assert_eq!(dto.segments[1].package, "retail");
        assert_eq!(dto.segments[1].namespace, "instances");
        assert_eq!(dto.segments[1].type_name, "prod1");
        assert_eq!(dto.segments[1].ver_major, 1);
    }

    #[test]
    fn test_gts_id_segment_dto_serialization() {
        let segment = seg(gts_id!("acme.billing.invoices.invoice.v2~"), 0);
        let dto = GtsIdSegmentDto::from(&segment);

        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["vendor"], "acme");
        assert_eq!(json["package"], "billing");
        assert_eq!(json["namespace"], "invoices");
        assert_eq!(json["type_name"], "invoice");
        assert_eq!(json["ver_major"], 2);
    }

    #[test]
    fn test_gts_entity_dto_segments_serialization() {
        const INSTANCE_ID: &str = gts_id!("fabrikam.pkg1.ns1.type1.v1~contoso.pkg2.ns2.inst1.v2");

        let segment1 = seg(INSTANCE_ID, 0);
        let segment2 = seg(INSTANCE_ID, 1);
        let entity = GtsEntity::new(
            Uuid::nil(),
            INSTANCE_ID,
            vec![segment1, segment2],
            false, // is_schema
            serde_json::json!({}),
            None,
        );

        let dto: GtsEntityDto = entity.into();
        let json = serde_json::to_value(&dto).unwrap();

        let json_segments = json["segments"].as_array().unwrap();
        assert_eq!(json_segments.len(), 2);
        assert_eq!(json_segments[0]["vendor"], "fabrikam");
        assert_eq!(json_segments[1]["vendor"], "contoso");
    }

    #[test]
    fn test_list_entities_query_to_list_query() {
        let pattern = format!("{GTS_ID_PREFIX}acme.*");
        let dto = ListEntitiesQuery {
            #[allow(unknown_lints)]
            #[allow(de0901_gts_string_pattern)]
            pattern: Some(pattern.clone()),
            is_schema: Some(true),
            ..ListEntitiesQuery::default()
        };

        let query = dto.to_list_query();
        assert_eq!(query.pattern, Some(pattern));
        assert_eq!(query.is_type, Some(true));
    }

    #[test]
    fn test_list_entities_query_is_schema_false() {
        let dto = ListEntitiesQuery {
            pattern: None,
            is_schema: Some(false),
            ..ListEntitiesQuery::default()
        };

        let query = dto.to_list_query();
        assert_eq!(query.is_type, Some(false));
    }

    #[test]
    fn test_list_entities_query_segment_filters() {
        let dto = ListEntitiesQuery {
            vendor: Some("acme".to_owned()),
            package: Some("core".to_owned()),
            namespace: Some("events".to_owned()),
            segment_scope: Some("primary".to_owned()),
            ..ListEntitiesQuery::default()
        };

        let query = dto.to_list_query();
        assert_eq!(query.vendor, Some("acme".to_owned()));
        assert_eq!(query.package, Some("core".to_owned()));
        assert_eq!(query.namespace, Some("events".to_owned()));
        assert_eq!(query.segment_scope, SegmentMatchScope::Primary);
    }

    #[test]
    fn test_list_entities_query_segment_scope_unknown_falls_back_to_any() {
        let dto = ListEntitiesQuery {
            segment_scope: Some("garbage".to_owned()),
            ..ListEntitiesQuery::default()
        };

        let query = dto.to_list_query();
        assert_eq!(query.segment_scope, SegmentMatchScope::Any);
    }

    #[test]
    fn test_list_entities_query_default() {
        let dto = ListEntitiesQuery::default();
        let query = dto.to_list_query();
        assert_eq!(query.pattern, None);
        assert_eq!(query.is_type, None);
    }
}

// ---------------------------------------------------------------------------
// The submit-then-poll contract (D10, SPEC §8.1)
// ---------------------------------------------------------------------------
//
// `POST /entities` breaks its old synchronous `200` shape: the response is now a
// receipt for an operation, and the outcome is polled. These DTOs are the wire
// form of that contract, and they exist only here — nothing outside `api/rest`
// references them (DE0201).

/// One entity in a submission.
///
/// `SubmitEntityDto` and not `SubmitCandidateDto`: "candidate" is the domain's
/// word for a submitted-but-not-yet-admitted entity, and this type is the wire
/// form, whose name reaches callers as an `OpenAPI` component.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct SubmitEntityDto {
    /// The canonical GTS identifier. A non-canonical spelling is refused rather
    /// than normalized.
    pub gts_id: String,
    /// The authored document.
    pub content: serde_json::Value,
    /// Optimistic precondition. **Omit** to require that the identifier does not
    /// exist; a literal `0` is refused. `>= 1` is the version to match.
    #[serde(default)]
    pub expected_resource_version: Option<i64>,
    /// ADR-0004 `force`: waive one cross-minor compatibility check. Refused where
    /// the deployment disallows it, or where the candidate has no such check.
    #[serde(default)]
    pub force: Option<bool>,
}

/// A submission of one or more entities.
///
/// `items` and not `entities`: the operation result, the discovery page and
/// `Page<T>` all call their array `items`, so this is the house word for "the
/// array in this envelope". It is also the name v1 does *not* use, which keeps
/// the T24a promotion a loud break rather than one that turns on the element
/// shape.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct SubmitEntitiesRequest {
    #[schema(min_items = 1)]
    pub items: Vec<SubmitEntityDto>,
    /// Run every check and commit nothing. Part of the request fingerprint, so
    /// reusing one `Idempotency-Key` for a dry run and then a commit is a
    /// conflict, not a replay of the dry-run result.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// The receipt returned by a submission: `202` for accepted work, `200` only when
/// a replayed operation is already terminal.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct OperationAcceptedDto {
    pub operation_id: Uuid,
    pub status: OperationStatusDto,
    /// `true` when this submission resolved to an operation that already existed
    /// under its `Idempotency-Key` with a matching request.
    pub replayed: bool,
}

/// One candidate's durable outcome.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct OperationItemDto {
    pub gts_id: String,
    pub status: OperationItemStatusDto,
    pub resource_version: Option<i64>,
    /// The structured refusal reason, when this candidate failed.
    pub error: Option<serde_json::Value>,
}

/// An operation as a caller polls it.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct OperationDto {
    pub operation_id: Uuid,
    pub kind: OperationKindDto,
    pub dry_run: bool,
    /// Progress only — the outcomes are on the items and are deliberately not
    /// aggregated here.
    pub status: OperationStatusDto,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<time::OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<time::OffsetDateTime>,
    pub items: Vec<OperationItemDto>,
}

/// One entity with its authored content and the artifacts materialized at
/// admission (D3). No consumer recomputes an effective form.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EntityDto {
    pub gts_id: String,
    /// The Registry Reference: a deterministic `UUIDv5` of the identifier.
    pub gts_uuid: Uuid,
    pub kind: EntityKindDto,
    /// A tombstone stays exact-readable and only leaves discovery.
    pub lifecycle_status: LifecycleStatusDto,
    pub resource_version: i64,
    /// Caller-declared attribution. It MUST NOT be used to authorize.
    pub owning_gear: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    pub content: Option<serde_json::Value>,
    pub resolved_schema: Option<serde_json::Value>,
    pub effective_traits: Option<serde_json::Value>,
    pub effective_traits_schema: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Domain -> DTO. Mapping only: every value below is already decided by the
// domain, and nothing here consults a policy, a limit or the database.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The wire vocabularies
// ---------------------------------------------------------------------------
//
// Neither the domain nor the storage enums derive `Serialize`, on purpose (T3): the
// storage side must not leak its smallint numbering and the domain side has no wire
// spelling of its own. These types are the wire spellings.
//
// Enums rather than `&'static str` helpers, and the difference is not internal: a
// `String` field publishes an unconstrained `string` in the served `OpenAPI`, so a
// generated client gets no vocabulary and a value this gear never emits type-checks
// against it. `#[api_dto]` renames variants to `snake_case`.

/// `pending`, `running` or `completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum OperationStatusDto {
    Pending,
    Running,
    Completed,
}

impl From<OperationStatus> for OperationStatusDto {
    fn from(status: OperationStatus) -> Self {
        match status {
            OperationStatus::Pending => Self::Pending,
            OperationStatus::Running => Self::Running,
            OperationStatus::Completed => Self::Completed,
        }
    }
}

/// `pending`, `running`, `succeeded`, `unchanged` or `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum OperationItemStatusDto {
    Pending,
    Running,
    Succeeded,
    Unchanged,
    Failed,
}

impl From<OperationItemStatus> for OperationItemStatusDto {
    fn from(status: OperationItemStatus) -> Self {
        match status {
            OperationItemStatus::Pending => Self::Pending,
            OperationItemStatus::Running => Self::Running,
            OperationItemStatus::Succeeded => Self::Succeeded,
            OperationItemStatus::Unchanged => Self::Unchanged,
            OperationItemStatus::Failed => Self::Failed,
        }
    }
}

/// `registration` or `deletion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum OperationKindDto {
    Registration,
    Deletion,
}

impl From<OperationKind> for OperationKindDto {
    fn from(kind: OperationKind) -> Self {
        match kind {
            OperationKind::Registration => Self::Registration,
            OperationKind::Deletion => Self::Deletion,
        }
    }
}

/// `type_schema` or `instance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum EntityKindDto {
    TypeSchema,
    Instance,
}

impl From<EntityKind> for EntityKindDto {
    fn from(kind: EntityKind) -> Self {
        match kind {
            EntityKind::TypeSchema => Self::TypeSchema,
            EntityKind::Instance => Self::Instance,
        }
    }
}

/// `active` or `deleted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum LifecycleStatusDto {
    Active,
    Deleted,
}

impl From<LifecycleStatus> for LifecycleStatusDto {
    fn from(status: LifecycleStatus) -> Self {
        match status {
            LifecycleStatus::Active => Self::Active,
            LifecycleStatus::Deleted => Self::Deleted,
        }
    }
}

impl From<OperationItemRecord> for OperationItemDto {
    fn from(item: OperationItemRecord) -> Self {
        Self {
            gts_id: item.gts_id,
            status: item.status.into(),
            resource_version: item.resource_version,
            // The stored payload is already a JSON object; re-parsing it keeps the
            // response a document rather than a string containing one. A payload
            // that does not parse is surfaced as a string field rather than
            // dropped, so a corrupt row is visible instead of invisible.
            error: item.error.map(|payload| {
                match serde_json::from_str::<serde_json::Value>(&payload) {
                    Ok(value) => value,
                    Err(_) => serde_json::Value::String(payload),
                }
            }),
        }
    }
}

impl From<OperationRecord> for OperationDto {
    fn from(record: OperationRecord) -> Self {
        Self {
            operation_id: record.operation_id,
            kind: record.kind.into(),
            dry_run: record.dry_run,
            status: record.status.into(),
            created_at: record.created_at,
            started_at: record.started_at,
            completed_at: record.completed_at,
            items: record.items.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<EntityRecord> for EntityDto {
    fn from(record: EntityRecord) -> Self {
        Self {
            gts_id: record.gts_id,
            gts_uuid: record.gts_uuid,
            kind: record.kind.into(),
            lifecycle_status: record.lifecycle_status.into(),
            resource_version: record.resource_version,
            owning_gear: record.owning_gear,
            created_at: record.created_at,
            updated_at: record.updated_at,
            content: record.content,
            resolved_schema: record.resolved_schema,
            effective_traits: record.effective_traits,
            effective_traits_schema: record.effective_traits_schema,
        }
    }
}
