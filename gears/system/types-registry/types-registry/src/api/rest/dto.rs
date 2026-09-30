//! REST DTOs for the Types Registry gear.

use serde_json::value::RawValue;
use uuid::Uuid;

use gts::GtsIdSegment;
use types_registry_sdk::RegisterSummary;

use crate::domain::admission::{AdmissionFailureReason, StoredFailure};
use crate::domain::enums::{
    EntityKind, LifecycleStatus, OperationItemStatus, OperationKind, OperationStatus,
};
use crate::domain::model::{GtsEntity, ListQuery, SegmentMatchScope};
use crate::domain::registry_service::{
    EntityLookup, EntityRecord, MAX_BATCH_GET_KEYS, OperationItemRecord, OperationRecord,
};

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

    fn schema_json<T: utoipa::PartialSchema>() -> serde_json::Value {
        serde_json::to_value(T::schema()).expect("a schema serializes")
    }

    /// `OpenAPI` must say what `$select` does: identity, kind and lifecycle are
    /// always present, metadata is never `null`, and a selected document may be `null`.
    #[test]
    fn the_entity_schema_declares_projection_accurately() {
        let schema = schema_json::<EntityDto>();
        assert_eq!(
            schema["required"],
            serde_json::json!(["gts_id", "gts_uuid", "kind", "lifecycle_status"])
        );
        let properties = &schema["properties"];
        for field in ["gts_id", "gts_uuid"] {
            assert_eq!(properties[field]["type"], "string", "{field}: {properties}");
        }
        assert_eq!(properties["gts_uuid"]["format"], "uuid");
        for field in [
            "content",
            "resolved_schema",
            "effective_traits",
            "effective_traits_schema",
        ] {
            // Any JSON value, `null` included: nothing but the description, as
            // `serde_json::Value` declared it before the `RawValue` switch.
            let keys: Vec<&String> = properties[field]
                .as_object()
                .expect("a property schema")
                .keys()
                .collect();
            assert_eq!(keys, ["description"], "{field}: {}", properties[field]);
        }
    }

    #[test]
    fn the_batch_request_schema_keeps_items_a_plain_array() {
        let schema = schema_json::<BatchGetRequest>();
        assert_eq!(schema["required"], serde_json::json!(["items"]), "{schema}");
        let mut items = schema["properties"]["items"].clone();
        items
            .as_object_mut()
            .expect("a property schema")
            .remove("description");
        assert_eq!(
            items,
            serde_json::json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/BatchGetItemDto" },
                "minItems": 1,
            }),
        );
    }

    fn batch_request(items: &[serde_json::Value]) -> serde_json::Result<BatchGetRequest> {
        serde_json::from_str(&serde_json::json!({ "items": items }).to_string())
    }

    #[test]
    fn a_batch_keeps_the_first_items_up_to_the_ceiling_and_counts_all() {
        for total in [0, 1, MAX_BATCH_GET_KEYS, MAX_BATCH_GET_KEYS + 1, 10_000] {
            let items: Vec<serde_json::Value> = (0..total)
                .map(|i| serde_json::json!({ "key": format!("k{i}") }))
                .collect();
            let request = batch_request(&items).expect("a well-formed batch parses");
            assert_eq!(request.items.count(), total);
            let kept = request.items.into_items();
            assert_eq!(kept.len(), total.min(MAX_BATCH_GET_KEYS), "{total}");
            for (i, item) in kept.iter().enumerate() {
                assert_eq!(item.key, format!("k{i}"));
            }
        }
    }

    #[test]
    fn only_items_within_the_ceiling_are_validated() {
        let mut items: Vec<serde_json::Value> = (0..MAX_BATCH_GET_KEYS)
            .map(|i| serde_json::json!({ "key": format!("k{i}") }))
            .collect();
        items.push(serde_json::json!({ "unknown": true }));
        let past = batch_request(&items).expect("an item past the ceiling is not read");
        assert_eq!(past.items.count(), MAX_BATCH_GET_KEYS + 1);

        items.swap(0, MAX_BATCH_GET_KEYS);
        assert!(batch_request(&items).is_err(), "an item within it still is");
    }

    #[test]
    fn unselected_fields_are_omitted_and_a_selected_null_is_kept() {
        let dto = EntityDto {
            gts_id: "gts.cf.core.example.type.v1~".to_owned(),
            gts_uuid: Uuid::nil(),
            kind: EntityKindDto::TypeSchema,
            origin: None,
            lifecycle_status: LifecycleStatusDto::Deleted,
            content: Some(RawValue::from_string("null".to_owned()).expect("JSON")),
            resolved_schema: None,
            effective_traits: None,
            effective_traits_schema: None,
            provenance: None,
        };
        assert_eq!(
            serde_json::to_value(dto).expect("serialize"),
            serde_json::json!({
                "gts_id": "gts.cf.core.example.type.v1~",
                "gts_uuid": Uuid::nil(),
                "kind": "type_schema",
                "lifecycle_status": "deleted",
                "content": null,
            }),
        );
    }

    #[test]
    fn origin_is_internally_tagged_with_rfc3339_timestamps() {
        let at = time::macros::datetime!(2026-09-23 10:00:00 UTC);
        let origin = OriginDto::Managed {
            resource_version: 3,
            created_at: at,
            updated_at: at,
        };
        assert_eq!(
            serde_json::to_value(origin).expect("serialize"),
            serde_json::json!({
                "type": "managed",
                "resource_version": 3,
                "created_at": "2026-09-23T10:00:00Z",
                "updated_at": "2026-09-23T10:00:00Z",
            }),
        );
    }

    #[test]
    fn operation_item_error_preserves_reason_and_message() -> Result<(), serde_json::Error> {
        let payload = serde_json::json!({
            "reason": "incompatible_with_baseline",
            "message": "PropertyAdded at $.payload",
        });
        let dto = OperationItemDto::from_record(
            OperationItemRecord {
                gts_id: gts_id!("cf.core.compat.thing.v1~").to_owned(),
                status: OperationItemStatus::Failed,
                resource_version: None,
                error: Some(StoredFailure::parse(&payload.to_string())),
            },
            Uuid::nil(),
        );
        assert_eq!(serde_json::to_value(dto)?["error"], payload);
        Ok(())
    }

    fn item_error(stored: Option<&str>) -> serde_json::Value {
        let dto = OperationItemDto::from_record(
            OperationItemRecord {
                gts_id: gts_id!("cf.core.compat.thing.v1~").to_owned(),
                status: OperationItemStatus::Failed,
                resource_version: None,
                error: stored.map(StoredFailure::parse),
            },
            Uuid::nil(),
        );
        serde_json::to_value(dto).expect("serialize")["error"].clone()
    }

    #[test]
    fn operation_item_error_keeps_dependency_and_system_failure_details() {
        for payload in [
            serde_json::json!({
                "reason": "dependency_not_found",
                "message": "$ref target 'cf.core.absent.type.v1~' is not registered",
                "dependency_id": "cf.core.absent.type.v1~",
                "dependency_kind": "ref",
            }),
            serde_json::json!({
                "reason": "system_failure",
                "message": "admission could not complete because of a system failure",
                "error_code": "delivery_exhausted",
                "operation_id": "7f0c3a52-3f58-4c61-9f1e-2d4c2f6c1a10",
            }),
            // An unknown future reason passes through verbatim.
            serde_json::json!({ "reason": "future_refusal", "message": "future details" }),
        ] {
            assert_eq!(item_error(Some(&payload.to_string())), payload);
        }
    }

    #[test]
    fn a_corrupt_stored_error_stays_an_object_without_the_raw_payload() {
        for (stored, reason) in [
            ("secret not JSON", "unparsable_payload"),
            (r#""secret string""#, "unrecognized_payload"),
            (
                r#"{"reason":42,"message":"secret"}"#,
                "unrecognized_payload",
            ),
            (r#"{"reason":"invalid_schema"}"#, "unrecognized_payload"),
            (
                r#"{"reason":"system_failure","message":"secret","operation_id":"secret"}"#,
                "unrecognized_payload",
            ),
            (
                r#"{"reason":"system_failure","message":"secret","error_code":7}"#,
                "unrecognized_payload",
            ),
            (
                r#"{"reason":"dependency_not_found","message":"secret","dependency_id":"secret"}"#,
                "unrecognized_payload",
            ),
        ] {
            let error = item_error(Some(stored));
            assert_eq!(error["reason"], reason, "{stored}");
            let message = error["message"].as_str().expect("message is a string");
            assert!(!message.contains("secret"), "{stored} leaked: {message}");
            assert!(
                !message.contains("invalid_schema"),
                "{stored} leaked: {message}"
            );
            assert_eq!(
                error.as_object().map(serde_json::Map::len),
                Some(2),
                "{stored}: {error}"
            );
        }
    }

    #[test]
    fn an_item_without_a_stored_error_has_none() {
        assert_eq!(item_error(None), serde_json::Value::Null);
    }

    #[test]
    fn the_operation_item_error_schema_is_a_typed_object() {
        let schema = schema_json::<OperationItemErrorDto>();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], serde_json::json!(["reason", "message"]));
        let properties = &schema["properties"];
        for field in [
            "reason",
            "message",
            "dependency_id",
            "dependency_kind",
            "error_code",
        ] {
            assert_eq!(properties[field]["type"], "string", "{field}: {properties}");
        }
        assert_eq!(properties["operation_id"]["format"], "uuid");

        let item = schema_json::<OperationItemDto>();
        let variants = &item["properties"]["error"]["oneOf"];
        assert_eq!(variants.as_array().map(Vec::len), Some(2), "{variants}");
        assert_eq!(variants[0], serde_json::json!({"type": "null"}));
        assert_eq!(
            variants[1]["$ref"],
            "#/components/schemas/OperationItemErrorDto"
        );
    }

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
#[serde(deny_unknown_fields)]
pub struct SubmitEntityDto {
    /// The canonical GTS identifier. A non-canonical spelling is refused rather
    /// than normalized.
    pub gts_id: String,
    /// The authored document.
    pub content: serde_json::Value,
    /// Optimistic precondition. **Omit** to require that the identifier does not
    /// exist; a literal `0` is refused. A positive version names a content
    /// revision: the entity must exist at exactly that `resource_version`, and a
    /// mismatch fails the candidate terminally rather than rebasing it.
    ///
    /// Optional positive precondition; acceptance rejects zero and negatives.
    #[serde(default)]
    #[schema(minimum = 1)]
    pub expected_resource_version: Option<i64>,
    /// ADR-0004: waive one cross-minor check when the deployment permits it.
    /// Intra-entity revisions are never waivable. The revision records the
    /// effective waiver as `compat_forced`.
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
#[serde(deny_unknown_fields)]
pub struct SubmitEntitiesRequest {
    #[schema(min_items = 1)]
    pub items: Vec<SubmitEntityDto>,
    /// Predict the batch using one snapshot and an overlay of prior candidates' effects.
    /// Record outcomes without entity-state writes or reservations.
    ///
    /// Defaults to `false`; participates in the idempotency fingerprint.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// A batch deletion target, resolved like `GET /entities/{entity_key}`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct DeleteEntityDto {
    /// A canonical GTS identifier or a Registry Reference UUID.
    pub key: String,
    /// Required positive version; optional internally for uniform validation errors.
    #[serde(default)]
    #[schema(required, value_type = i64, minimum = 1)]
    pub expected_resource_version: Option<i64>,
}

/// A deletion batch. Unknown fields are rejected to prevent ignored safety options.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct DeleteEntitiesRequest {
    /// Runtime configuration supplies the batch limit.
    #[schema(min_items = 1)]
    pub items: Vec<DeleteEntityDto>,
    /// Predict without changing entities. Defaults to `false`; part of the idempotency fingerprint.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// Single-deletion parameters; the entity key is in the path.
#[derive(Debug, Clone, Default)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct DeleteEntityQuery {
    /// Required positive version, validated by acceptance as on the batch route.
    /// Missing or non-numeric values return `400`.
    #[serde(default)]
    pub expected_resource_version: Option<i64>,
    /// Predict without changing entities. Defaults to `false`.
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
    /// Present when this candidate failed.
    pub error: Option<OperationItemErrorDto>,
}

/// Why a candidate failed. `reason` is a stable code and may be one the client
/// does not know; `message` is for humans and must not be parsed.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct OperationItemErrorDto {
    pub reason: String,
    pub message: String,
    /// The missing dependency, for `dependency_not_found`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub dependency_id: Option<String>,
    /// `base`, `conforming_type` or `ref`; newer writers may add kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub dependency_kind: Option<String>,
    /// Stable diagnostic code of a `system_failure`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error_code: Option<String>,
    /// The operation a `system_failure` terminalized.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub operation_id: Option<Uuid>,
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

/// One entity, projected by `$select` (SPEC §10.2).
///
/// `gts_id`, `gts_uuid`, `kind` and `lifecycle_status` are always present, so a
/// projected item is identifiable, its kind needs no second read, and a tombstone is
/// never mistaken for an absence. An
/// unselected field is omitted; a selected document that is JSON `null` stays
/// present as `null`. The three artifacts are absent on an Instance. Documents
/// are written as stored, canonical text: keys sorted, no insignificant whitespace.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EntityDto {
    /// Always present.
    pub gts_id: String,
    /// Always present. The Registry Reference: a deterministic `UUIDv5` of the
    /// identifier.
    pub gts_uuid: Uuid,
    /// Always present.
    pub kind: EntityKindDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub origin: Option<OriginDto>,
    /// Always present. A tombstone stays exact-readable and is listed only on request.
    pub lifecycle_status: LifecycleStatusDto,
    /// The whole authored document, either kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<serde_json::Value>)]
    pub content: Option<Box<RawValue>>,
    /// Type Schemas only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<serde_json::Value>)]
    pub resolved_schema: Option<Box<RawValue>>,
    /// Type Schemas only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<serde_json::Value>)]
    pub effective_traits: Option<Box<RawValue>>,
    /// Type Schemas only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<serde_json::Value>)]
    pub effective_traits_schema: Option<Box<RawValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub provenance: Option<ProvenanceDto>,
}

/// Where an entity comes from. P0 has only the managed variant; federation adds
/// `external` without changing this one.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
#[serde(tag = "type")]
pub enum OriginDto {
    Managed {
        resource_version: i64,
        #[serde(with = "time::serde::rfc3339")]
        created_at: time::OffsetDateTime,
        #[serde(with = "time::serde::rfc3339")]
        updated_at: time::OffsetDateTime,
    },
}

/// Who admitted the current revision and how. Selected as one group.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ProvenanceDto {
    pub gts_spec_version: String,
    pub gts_impl_version: String,
    /// Whether ADR-0004 `force` waived a cross-minor check; `null` for Instances.
    pub compat_forced: Option<bool>,
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

impl OperationItemDto {
    fn from_record(item: OperationItemRecord, operation_id: Uuid) -> Self {
        let error = item.error.map(|stored| {
            stored.map_or_else(
                |unreadable| {
                    tracing::error!(
                        %operation_id,
                        gts_id = %item.gts_id,
                        reason = unreadable.reason.as_str(),
                        cause = %unreadable.cause,
                        "types_registry cannot read a stored item failure"
                    );
                    OperationItemErrorDto::unreadable(&unreadable.reason)
                },
                Into::into,
            )
        });
        Self {
            gts_id: item.gts_id,
            status: item.status.into(),
            resource_version: item.resource_version,
            error,
        }
    }
}

impl OperationItemErrorDto {
    /// Reported by reason; the stored text is never echoed.
    fn unreadable(reason: &AdmissionFailureReason) -> Self {
        Self {
            reason: reason.as_str().to_owned(),
            message: "the recorded failure could not be read".to_owned(),
            dependency_id: None,
            dependency_kind: None,
            error_code: None,
            operation_id: None,
        }
    }
}

impl From<StoredFailure> for OperationItemErrorDto {
    fn from(failure: StoredFailure) -> Self {
        Self {
            reason: failure.reason,
            message: failure.message,
            dependency_id: failure.dependency_id,
            dependency_kind: failure.dependency_kind,
            error_code: failure.error_code,
            operation_id: failure.operation_id,
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
            items: record
                .items
                .into_iter()
                .map(|item| OperationItemDto::from_record(item, record.operation_id))
                .collect(),
        }
    }
}

impl From<EntityRecord> for EntityDto {
    fn from(record: EntityRecord) -> Self {
        Self {
            gts_id: record.gts_id,
            gts_uuid: record.gts_uuid,
            kind: record.kind.into(),
            origin: record.origin.map(|origin| OriginDto::Managed {
                resource_version: origin.resource_version,
                created_at: origin.created_at,
                updated_at: origin.updated_at,
            }),
            lifecycle_status: record.lifecycle_status.into(),
            content: record.content,
            resolved_schema: record.resolved_schema,
            effective_traits: record.effective_traits,
            effective_traits_schema: record.effective_traits_schema,
            provenance: record.provenance.map(|p| ProvenanceDto {
                gts_spec_version: p.gts_spec_version,
                gts_impl_version: p.gts_impl_version,
                compat_forced: p.compat_forced,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// The two read surfaces: `:batchGet` and discovery (T22a, T22b)
// ---------------------------------------------------------------------------
//
// Exact read and `batchGet` share [`EntityDto`] and one `$select` normalization, so
// one key answers identically on both (SPEC §10.2).

/// One key in a batch read.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct BatchGetItemDto {
    /// A canonical GTS identifier or a Registry Reference UUID, classified exactly
    /// as `GET /entities/{entity_key}` classifies its path segment.
    #[schema(max_length = 1024)]
    pub key: String,
    /// This key's validator from an earlier read, which makes just this key
    /// conditional. The field the two header names lowercase to — `if_none_match`
    /// going out, `etag` coming back — because one `If-None-Match` header cannot
    /// represent a batch of them (DESIGN §3.3).
    ///
    /// **No read emits a validator yet:** T29 adds `etag` to a result and the
    /// comparison behind it. Until then no value a caller could hold can match, so
    /// every present key is read unconditionally and answers `found` — which is
    /// what a stale validator does after T29 too. The field is declared now so the
    /// wire shape does not change under the callers T23 migrates.
    #[serde(default)]
    #[schema(max_length = 1024)]
    pub if_none_match: Option<String>,
}

/// A batch read. Unknown fields are refused, so a misspelled `$select` is not
/// answered with the default set.
// `$select` is the OData wire name, not a snake_case choice.
#[allow(unknown_lints, de0803_api_snake_case)]
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct BatchGetRequest {
    /// No `max_items`: the ceiling is `MAX_BATCH_GET_KEYS`, applied while the body
    /// is parsed. Items past it are counted, not kept.
    #[schema(value_type = Vec<BatchGetItemDto>, min_items = 1)]
    pub items: BatchGetItems,
    /// Fields to return for every key, spelled as the GET routes' `$select`.
    /// Absent is the document-free default.
    #[serde(default, rename = "$select")]
    #[schema(max_length = 2048)]
    pub select: Option<String>,
}

/// The first [`MAX_BATCH_GET_KEYS`] items and the count of all of them: an
/// over-long batch is refused by count without one DTO per element.
#[derive(Debug, Clone)]
pub struct BatchGetItems {
    items: Vec<BatchGetItemDto>,
    count: usize,
}

impl BatchGetItems {
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }

    #[must_use]
    pub fn into_items(self) -> Vec<BatchGetItemDto> {
        self.items
    }
}

impl<'de> serde::Deserialize<'de> for BatchGetItems {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Items;

        impl<'de> serde::de::Visitor<'de> for Items {
            type Value = BatchGetItems;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an array of batch read items")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while items.len() < MAX_BATCH_GET_KEYS {
                    let Some(item) = seq.next_element::<BatchGetItemDto>()? else {
                        let count = items.len();
                        return Ok(BatchGetItems { items, count });
                    };
                    items.push(item);
                }
                // Ignore excess item payloads; only their count matters.
                let mut count = items.len();
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    count += 1;
                }
                Ok(BatchGetItems { items, count })
            }
        }

        deserializer.deserialize_seq(Items)
    }
}

/// `found` or `not_found`.
///
/// DESIGN's `unchanged` arrives with T29's validators and its `failed` needs
/// federation, which is out of P0 (SPEC §2). Declaring either now would publish a
/// vocabulary value this gear never emits, which a generated client would
/// type-check against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum EntityLookupStatusDto {
    Found,
    NotFound,
}

impl From<&EntityLookup> for EntityLookupStatusDto {
    fn from(lookup: &EntityLookup) -> Self {
        match lookup {
            EntityLookup::Found(_) => Self::Found,
            EntityLookup::NotFound => Self::NotFound,
        }
    }
}

/// One key's answer, echoing the key it was asked by.
///
/// The echo is not redundant: a caller that mixed identifiers and Registry
/// References matches answers to questions without re-deriving either, and an
/// absence has no entity to carry the key for it.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EntityLookupDto {
    pub key: String,
    pub status: EntityLookupStatusDto,
    /// The selected fields, exactly as the exact read returns them for the same
    /// `$select`. Absent on `not_found`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityDto>,
}

/// The results of one batch read, in request order.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EntityLookupsDto {
    pub items: Vec<EntityLookupDto>,
}

/// Where the next page starts.
///
/// `next_cursor` and the page size only. No `prev_cursor`, because discovery pages
/// forward only, and no total, because counting a set this page did not read would
/// be a second unbounded query.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PageInfoDto {
    /// Present exactly when another match exists, and then the page is full.
    /// Absent means this page is the last one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The page size actually applied, which is the default when the caller named none.
    #[schema(minimum = 1)]
    pub limit: u32,
}

/// One bounded page of discovery results (SPEC §10.1's `EntityPage`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EntityPageDto {
    /// Each item projected exactly as the exact read projects it.
    pub items: Vec<EntityDto>,
    pub page_info: PageInfoDto,
}
