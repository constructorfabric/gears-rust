//! REST handlers for the Types Registry gear.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, OriginalUri};
use axum::http::{HeaderMap, HeaderValue, header};
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract;
use uuid::Uuid;

use super::dto::{
    EntityDto, GtsEntityDto, ListEntitiesQuery, ListEntitiesResponse, OperationAcceptedDto,
    OperationDto, RegisterEntitiesRequest, RegisterEntitiesResponse, RegisterResultDto,
    RegisterSummaryDto, SubmitEntitiesRequest,
};
use super::paths::V2;
use crate::domain::admission::{Candidate, SubmitRequest};
use crate::domain::enums::OperationKind;
use crate::domain::error::DomainError;
use crate::domain::registry_service::{EntityKey, RegistryService};
use crate::domain::service::TypesRegistryService;

/// POST /api/v1/types-registry/entities
///
/// Register GTS entities in batch.
/// REST API always validates entities, regardless of ready state.
/// However, REST API is blocked until service is ready.
pub async fn register_entities(
    Extension(service): Extension<Arc<TypesRegistryService>>,
    extract::Json(req): extract::Json<RegisterEntitiesRequest>,
) -> ApiResult<(StatusCode, Json<RegisterEntitiesResponse>)> {
    if !service.is_ready() {
        return Err(DomainError::NotInReadyMode.into());
    }

    let outcomes = service.register_validated(req.entities);

    let total = outcomes.len();
    let mut succeeded = 0_usize;
    let mut result_dtos: Vec<RegisterResultDto> = Vec::with_capacity(total);
    for (gts_id, outcome) in outcomes {
        match outcome {
            Ok(entity) => {
                succeeded += 1;
                result_dtos.push(RegisterResultDto::Ok {
                    entity: entity.into(),
                });
            }
            Err(e) => result_dtos.push(RegisterResultDto::Error {
                gts_id,
                error: e.to_string(),
            }),
        }
    }
    let failed = total - succeeded;

    let response = RegisterEntitiesResponse {
        summary: RegisterSummaryDto {
            total,
            succeeded,
            failed,
        },
        results: result_dtos,
    };

    Ok((StatusCode::OK, Json(response)))
}

/// GET /api/v1/types-registry/entities
///
/// List GTS entities with optional filtering.
pub async fn list_entities(
    Extension(service): Extension<Arc<TypesRegistryService>>,
    extract::Query(query): extract::Query<ListEntitiesQuery>,
) -> ApiResult<Json<ListEntitiesResponse>> {
    if !service.is_ready() {
        return Err(DomainError::NotInReadyMode.into());
    }

    let list_query = query.to_list_query();

    let entities = service.list(&list_query).map_err(CanonicalError::from)?;

    let entity_dtos: Vec<GtsEntityDto> = entities.into_iter().map(Into::into).collect();
    let count = entity_dtos.len();

    Ok(Json(ListEntitiesResponse {
        entities: entity_dtos,
        count,
    }))
}

/// GET /api/v1/types-registry/entities/{gts_id}
///
/// Get a single GTS entity by its identifier.
pub async fn get_entity(
    Extension(service): Extension<Arc<TypesRegistryService>>,
    extract::Path(gts_id): extract::Path<String>,
) -> ApiResult<Json<GtsEntityDto>> {
    if !service.is_ready() {
        return Err(DomainError::NotInReadyMode.into());
    }

    let entity = service.get(&gts_id).map_err(CanonicalError::from)?;

    Ok(Json(entity.into()))
}

// ---------------------------------------------------------------------------
// The database-backed platform-plane handlers (T9)
// ---------------------------------------------------------------------------
//
// Mapping steps only. Every one of these reads a request, calls exactly one domain
// method, and maps the result — no policy, no limit, no existence check and no
// vocabulary decision lives here, which is what lets a future `api/grpc` adapter
// reuse the same domain surface (SPEC §8.4).
//
// The handlers above this line are the pre-database path T27 deletes.

/// Advisory only: how long a client should wait before its first poll. The
/// operation may well be terminal sooner — while admission is inline (T21) it
/// already is — so this is a hint, not a contract.
const RETRY_AFTER_SECONDS: &str = "1";

/// Response signal required by the workspace idempotency convention.
const IDEMPOTENCY_REPLAYED_HEADER: &str = "idempotency-replayed";

/// `POST /types-registry/v2/entities`
///
/// Submit a registration. `202` with the operation's `Location`, or `200` when a
/// replayed operation is already terminal (SPEC §8.1, D10).
///
/// The `Idempotency-Key` is **read** from the header and passed as a parameter.
/// It is never interpreted here: no default, no generated value, no trimming
/// decision — an absent or unusable key is the domain's refusal to make.
pub async fn submit_entities(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    extract::Json(req): extract::Json<SubmitEntitiesRequest>,
) -> ApiResult<(StatusCode, HeaderMap, Json<OperationAcceptedDto>)> {
    let service = require_registry(service)?;
    // Absent and unusable are kept apart. An absent header is the domain's refusal
    // to make — `validate` takes the empty string and answers "an Idempotency-Key
    // header is required" — but a header that *was* sent and cannot be decoded is
    // this layer's own answer, because a `String` cannot carry the distinction any
    // further and "required" would be a lie about what the caller did.
    let idempotency_key = match headers.get("idempotency-key") {
        None => String::new(),
        Some(value) => value
            .to_str()
            .map_err(|_| super::error::idempotency_key_not_utf8())?
            .to_owned(),
    };

    let request = SubmitRequest {
        idempotency_key,
        kind: OperationKind::Registration,
        dry_run: req.dry_run.unwrap_or(false),
        candidates: req
            .items
            .into_iter()
            .map(|c| Candidate {
                gts_id: c.gts_id,
                content: Some(c.content),
                expected_resource_version: c.expected_resource_version,
                force: c.force.unwrap_or(false),
            })
            .collect(),
    };

    let accepted = service
        .submit(&request, time::OffsetDateTime::now_utc())
        .await
        .map_err(CanonicalError::from)?;

    let status = if accepted.replayed && accepted.terminal() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };

    let mut out = HeaderMap::new();
    let location = operation_location(uri.path(), accepted.operation_id);
    let location_value = HeaderValue::from_str(&location).map_err(|e| {
        tracing::error!(
            error = %e,
            location = %location,
            "types_registry could not encode the operation Location header"
        );
        CanonicalError::internal("the registry could not construct an operation receipt").create()
    })?;
    out.insert(header::LOCATION, location_value);
    if status == StatusCode::ACCEPTED {
        out.insert(
            header::RETRY_AFTER,
            HeaderValue::from_static(RETRY_AFTER_SECONDS),
        );
    }
    if accepted.replayed {
        out.insert(
            IDEMPOTENCY_REPLAYED_HEADER,
            HeaderValue::from_static("true"),
        );
    }

    // The status comes from the receipt, not from a second read: `submit` already
    // knows it — `pending` when the operation is queued, `completed` once inline
    // admission has run — so a constant `pending` here would be wrong and a re-read
    // would cost a snapshot transaction over two statements.
    Ok((
        status,
        out,
        Json(OperationAcceptedDto {
            operation_id: accepted.operation_id,
            status: accepted.status.into(),
            replayed: accepted.replayed,
        }),
    ))
}

/// The `Location` of an accepted operation, derived from the path the request
/// actually arrived on.
///
/// **Not the gear-relative constant.** api-gateway mounts every gear under
/// `prefix_path` with `Router::nest`, so a hardcoded
/// `/types-registry/v2/operations/{id}` is a `404` for any client that follows it —
/// RFC 9110 §10.2.2 resolves `Location` against the effective request URI, and an
/// absolute-path reference discards the prefix (measured on a live server; see the
/// Checkpoint 1 report §8.1). [`OriginalUri`] rather than `Uri` for the same
/// reason: `nest` strips exactly the prefix that has to survive here.
///
/// The receipt is a **sibling** of the submit path — `…/v1/entities` answers with
/// `…/v1/operations/{id}` — so the last segment is replaced, not appended to. If the
/// route is ever mounted somewhere not ending in `/entities`, the gear-relative path
/// is the fallback: a wrong prefix beats a path with two resources in it.
fn operation_location(request_path: &str, operation_id: Uuid) -> String {
    let trimmed = request_path.trim_end_matches('/');
    match trimmed.strip_suffix("/entities") {
        Some(base) => format!("{base}/operations/{operation_id}"),
        None => format!("{V2}/operations/{operation_id}"),
    }
}

/// `GET /types-registry/v2/operations/{operation_id}`
pub async fn get_operation(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    extract::Path(operation_id): extract::Path<Uuid>,
) -> ApiResult<Json<OperationDto>> {
    let service = require_registry(service)?;
    let record = service
        .operation(operation_id)
        .await
        .map_err(CanonicalError::from)?
        .ok_or_else(|| CanonicalError::from(DomainError::not_found_by_uuid(operation_id)))?;
    Ok(Json(record.into()))
}

/// `GET /types-registry/v2/entities/{entity_key}`
///
/// The key is a GTS identifier or a Registry Reference; which one it is is the
/// domain's classification, not this handler's.
pub async fn get_entity_by_key(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    extract::Path(key): extract::Path<String>,
) -> ApiResult<Json<EntityDto>> {
    let service = require_registry(service)?;
    let parsed = EntityKey::parse(&key);
    let record = service
        .entity(&parsed)
        .await
        .map_err(CanonicalError::from)?
        .ok_or_else(|| CanonicalError::from(DomainError::not_found_by_id(key)))?;
    Ok(Json(record.into()))
}

/// The database-backed path is only wired where a database is bound to this gear
/// (`no-db.yaml`, `--mock`). A problem document naming the cause beats a `404`
/// suggesting the API changed, so the routes exist and answer `503`.
fn require_registry(
    service: Option<Arc<RegistryService>>,
) -> Result<Arc<RegistryService>, CanonicalError> {
    service.ok_or_else(|| {
        CanonicalError::service_unavailable()
            .with_detail(
                "types-registry has no database bound; admission and database-backed reads are \
                 unavailable in this deployment",
            )
            .create()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::InMemoryGtsRepository;
    use gts::GtsConfig;
    use serde_json::json;
    use toolkit_gts::gts_uri;

    const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

    fn default_config() -> GtsConfig {
        crate::config::TypesRegistryConfig::default().to_gts_config()
    }

    fn create_service() -> Arc<TypesRegistryService> {
        let repo = Arc::new(InMemoryGtsRepository::new(default_config()));
        Arc::new(TypesRegistryService::new(
            repo,
            crate::config::TypesRegistryConfig::default(),
        ))
    }

    #[tokio::test]
    async fn test_list_entities_returns_503_when_not_ready() {
        let service = create_service();
        // Service is not ready yet

        let query = ListEntitiesQuery::default();
        let result = list_entities(Extension(service), extract::Query(query)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_entities_handler_when_ready() {
        let service = create_service();

        // Register entities via internal API (before ready)
        _ = service.register(vec![
            json!({
                "$id": gts_uri!("acme.core.events.user_created.v1~"),
                "$schema": JSON_SCHEMA_DRAFT_07,
                "type": "object"
            }),
            json!({
                "$id": gts_uri!("globex.core.events.order_placed.v1~"),
                "$schema": JSON_SCHEMA_DRAFT_07,
                "type": "object"
            }),
        ]);
        service.switch_to_ready().unwrap();

        let query = ListEntitiesQuery::default();
        let result = list_entities(Extension(service), extract::Query(query)).await;
        assert!(result.is_ok());

        let Json(response) = result.unwrap();
        assert_eq!(response.count, 2);
    }
}
