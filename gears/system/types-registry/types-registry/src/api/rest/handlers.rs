//! REST handlers for the Types Registry gear.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, OriginalUri};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract;
use uuid::Uuid;

use super::cursor::Binding;
use super::dto::{
    BatchGetRequest, DeleteEntitiesRequest, DeleteEntityQuery, EntityDto, EntityLookupDto,
    EntityLookupsDto, EntityPageDto, GtsEntityDto, ListEntitiesQuery, ListEntitiesResponse,
    OperationAcceptedDto, OperationDto, PageInfoDto, RegisterEntitiesRequest,
    RegisterEntitiesResponse, RegisterResultDto, RegisterSummaryDto, SubmitEntitiesRequest,
};
use super::params::{DiscoveryParams, ExactReadSelection, NoQuery};
use super::paths::V2;
use crate::domain::admission::{Accepted, Candidate, SubmitRequest};
use crate::domain::enums::OperationKind;
use crate::domain::error::DomainError;
use crate::domain::registry_service::{
    DeleteRequest, DeleteTarget, DiscoveryQuery, EntityKey, EntityLookup, MAX_BATCH_GET_KEYS,
    MAX_KEY_LEN, RegistryService, ServiceError,
};
use crate::domain::selection::FieldSelection;
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
// method, and maps the result — no policy, no existence check and no vocabulary
// decision lives here, which is what lets a future `api/grpc` adapter reuse the
// same domain surface (SPEC §8.4). Size bounds checked here only fail early; the
// domain enforces the same ones for every adapter. The one exception is a batch
// item's `if_none_match`, which no domain method receives until T29 compares it.
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
    let request = SubmitRequest {
        idempotency_key: idempotency_key(&headers)?,
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

    receipt(uri.path(), accepted)
}

/// Submit a deletion batch with per-item preconditions (DESIGN §3.3).
pub async fn batch_delete_entities(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    extract::Json(req): extract::Json<DeleteEntitiesRequest>,
) -> ApiResult<(StatusCode, HeaderMap, Json<OperationAcceptedDto>)> {
    let service = require_registry(service)?;
    let request = DeleteRequest {
        idempotency_key: idempotency_key(&headers)?,
        dry_run: req.dry_run.unwrap_or(false),
        targets: req
            .items
            .into_iter()
            .map(|item| DeleteTarget {
                key: EntityKey::parse(&item.key),
                expected_resource_version: item.expected_resource_version,
            })
            .collect(),
    };

    let accepted = service
        .delete(&request, time::OffsetDateTime::now_utc())
        .await
        .map_err(CanonicalError::from)?;

    receipt(uri.path(), accepted)
}

/// Submit a single deletion through the same [`DeleteRequest`] as batch deletion.
pub async fn delete_entity(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    extract::Path(key): extract::Path<String>,
    extract::Query(query): extract::Query<DeleteEntityQuery>,
) -> ApiResult<(StatusCode, HeaderMap, Json<OperationAcceptedDto>)> {
    let service = require_registry(service)?;
    // Reject HTTP conditionals here; acceptance validates the version precondition.
    if headers.contains_key(header::IF_MATCH) {
        return Err(super::error::if_match_not_supported());
    }

    let request = DeleteRequest {
        idempotency_key: idempotency_key(&headers)?,
        dry_run: query.dry_run.unwrap_or(false),
        targets: vec![DeleteTarget {
            key: EntityKey::parse(&key),
            expected_resource_version: query.expected_resource_version,
        }],
    };

    let accepted = service
        .delete(&request, time::OffsetDateTime::now_utc())
        .await
        .map_err(CanonicalError::from)?;

    receipt(uri.path(), accepted)
}

/// Decode `Idempotency-Key`; acceptance handles absence, this layer rejects invalid bytes.
fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, CanonicalError> {
    headers
        .get("idempotency-key")
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| super::error::idempotency_key_not_utf8())
        })
        .transpose()
}

/// Build the shared mutation response from the receipt's admission status.
fn receipt(
    request_path: &str,
    accepted: Accepted,
) -> Result<(StatusCode, HeaderMap, Json<OperationAcceptedDto>), CanonicalError> {
    let status = if accepted.replayed && accepted.terminal() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };

    // Receipts are caller-specific and their operation status can advance.
    let mut out = no_store();
    let location = operation_location(request_path, accepted.operation_id);
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
/// Build the operation location while preserving the route's mount prefix.
fn operation_location(request_path: &str, operation_id: Uuid) -> String {
    let trimmed = request_path.trim_end_matches('/');
    match trimmed.rfind("/entities") {
        Some(cut) => format!("{}/operations/{operation_id}", &trimmed[..cut]),
        None => format!("{V2}/operations/{operation_id}"),
    }
}

/// `GET /types-registry/v2/operations/{operation_id}`
///
/// Poll an operation without caching its caller-specific, changing state.
pub async fn get_operation(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    extract::Path(operation_id): extract::Path<Uuid>,
) -> ApiResult<(HeaderMap, Json<OperationDto>)> {
    let service = require_registry(service)?;
    let record = service
        .operation(operation_id)
        .await
        .map_err(CanonicalError::from)?
        .ok_or_else(|| CanonicalError::from(DomainError::not_found_by_uuid(operation_id)))?;
    Ok((no_store(), Json(record.into())))
}

/// Add `Cache-Control: no-store` to caller-specific, changing responses.
fn no_store() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers
}

/// `GET /types-registry/v2/entities/{entity_key}`
///
/// The key is a GTS identifier or a Registry Reference; which one it is is the
/// domain's classification, not this handler's.
pub async fn get_entity_by_key(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    extract::Path(key): extract::Path<String>,
    ExactReadSelection(selection): ExactReadSelection,
) -> ApiResult<Response> {
    let service = require_registry(service)?;
    let parsed = EntityKey::parse(&key);
    let record = service
        .entity(&parsed, selection)
        .await
        .map_err(CanonicalError::from)?
        .ok_or_else(|| CanonicalError::from(DomainError::not_found_by_id(key)))?;
    json_body(EntityDto::from(record), selection).await
}

/// Serialized off the executor when the body carries documents.
async fn json_body<T: serde::Serialize + Send + 'static>(
    value: T,
    selection: FieldSelection,
) -> Result<Response, CanonicalError> {
    if !selection.selects_any_document() {
        return Ok(Json(value).into_response());
    }
    let bytes = tokio::task::spawn_blocking(move || serde_json::to_vec(&value))
        .await
        .map_err(ServiceError::Blocking)?
        .map_err(|e| super::error::response_not_serialized(&e))?;
    let content_type = HeaderValue::from_static("application/json");
    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

/// `POST /types-registry/v2/entities:batchGet`
///
/// An exact read of a bounded key set, with one explicit result per key. A `POST`
/// rather than a `GET`: identifiers run to 1024 characters, which a query string
/// cannot carry safely, and portable `GET` has no body (DESIGN §3.3).
///
/// Absence is a `200` with `not_found` on that key, not a `404`: one missing key
/// must not lose the answers for the others.
pub async fn batch_get_entities(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    headers: HeaderMap,
    _: NoQuery,
    extract::Json(req): extract::Json<BatchGetRequest>,
) -> ApiResult<Response> {
    let service = require_registry(service)?;
    let selection = super::select::parse(req.select.as_deref())?;
    // Refused before the read, not ignored: a caller that sent one believes its
    // request is conditional, and answering `200` with full snapshots would be
    // answering a different question.
    if headers.contains_key(header::IF_NONE_MATCH) {
        return Err(super::error::if_none_match_not_supported());
    }

    // Bounded before any per-item work, and on the raw count: duplicates still cost
    // parsing and must not stretch the ceiling.
    let count = req.items.count();
    if count == 0 || count > MAX_BATCH_GET_KEYS {
        return Err(ServiceError::BatchReadOutOfRange { count }.into());
    }
    let items = req.items.into_items();
    // `if_none_match` is length-checked but not compared: no read emits a
    // validator until T29.
    let mut spelling_map: HashMap<EntityKey, String> = HashMap::with_capacity(items.len());
    let mut keys: Vec<EntityKey> = Vec::with_capacity(items.len());
    for item in items {
        // Before `EntityKey::parse` copies the key; the domain repeats the check.
        if item.key.len() > MAX_KEY_LEN {
            return Err(super::error::key_too_long(item.key.len()));
        }
        if let Some(validator) = &item.if_none_match
            && validator.len() > MAX_KEY_LEN
        {
            return Err(super::error::validator_too_long(validator.len()));
        }
        let key = EntityKey::parse(&item.key);
        // The service dedups; the echo keeps the first spelling.
        if let Entry::Vacant(e) = spelling_map.entry(key.clone()) {
            e.insert(item.key);
        }
        keys.push(key);
    }

    let results = service
        .batch_get(&keys, selection)
        .await
        .map_err(CanonicalError::from)?;

    let body = EntityLookupsDto {
        items: results
            .into_iter()
            .map(|(key, lookup)| {
                let key = spelling_map.remove(&key).ok_or_else(|| {
                    tracing::error!(
                        unexpected_key = ?key,
                        batch_size = keys.len(),
                        "types_registry batch read answered a key it was not asked"
                    );
                    CanonicalError::internal("the registry could not match a batch read result")
                        .create()
                })?;
                Ok(EntityLookupDto {
                    key,
                    status: (&lookup).into(),
                    entity: match lookup {
                        EntityLookup::Found(record) => Some(EntityDto::from(record)),
                        EntityLookup::NotFound => None,
                    },
                })
            })
            .collect::<Result<_, CanonicalError>>()?,
    };
    json_body(body, selection).await
}

/// `GET /types-registry/v2/entities`
///
/// One bounded page ordered by canonical identifier, projected by `$select`, plus
/// the cursor for the next one (D12).
pub async fn discover_entities(
    Extension(service): Extension<Option<Arc<RegistryService>>>,
    params: DiscoveryParams,
) -> ApiResult<Response> {
    let service = require_registry(service)?;
    let mut query = DiscoveryQuery {
        pattern: params.pattern,
        after: None,
        limit: params.limit,
        kind: params.kind,
        lifecycle: params.lifecycle,
        max_chain_depth: params.max_chain_depth,
        selection: params.selection,
    };
    if let Some(cursor) = &params.cursor {
        query.after = Some(super::cursor::resume(cursor, &Binding::from(&query))?);
    }

    let page = service
        .discover(&query)
        .await
        .map_err(CanonicalError::from)?;

    let next_cursor = page
        .next_after
        .as_deref()
        .map(|after| super::cursor::encode(after, &Binding::from(&query)))
        .transpose()?;
    let body = EntityPageDto {
        items: page.items.into_iter().map(Into::into).collect(),
        page_info: PageInfoDto {
            next_cursor,
            limit: page.limit,
        },
    };
    json_body(body, query.selection).await
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

    /// A body `serde_json` refuses, as an out-of-range timestamp would be.
    struct Unserializable;

    impl serde::Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("injected serialization failure"))
        }
    }

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn a_body_that_does_not_serialize_is_not_reported_as_a_blocking_task() {
        let selection = FieldSelection::parse(&["content"]).expect("valid");
        let Err(refused) = json_body(Unserializable, selection).await else {
            panic!("the body cannot be serialized");
        };
        assert_eq!(
            toolkit_canonical_errors::Problem::from(refused).status,
            Some(500)
        );
        assert!(logs_contain("injected serialization failure"));
        assert!(logs_contain(r#"at="response serialization""#));
        assert!(!logs_contain("blocking task"));
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
