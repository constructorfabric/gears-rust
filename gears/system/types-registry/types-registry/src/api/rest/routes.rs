//! REST route registration for the Types Registry gear.

use std::sync::Arc;

use axum::{Extension, Router};
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::StatusCode;
use toolkit::api::operation_builder::{
    CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature, OperationBuilder, ParamLocation, ParamSpec,
    ResponseHeaderSpec, ResponseHeaderType,
};
use utoipa::openapi::schema::{KnownFormat, SchemaFormat};

use super::dto::{
    DeleteEntitiesRequest, EntityDto, GtsEntityDto, ListEntitiesResponse, OperationAcceptedDto,
    OperationDto, RegisterEntitiesRequest, RegisterEntitiesResponse, SubmitEntitiesRequest,
};
use super::handlers;
pub use super::paths::{V1, V2};
use crate::domain::registry_service::RegistryService;
use crate::domain::service::TypesRegistryService;

const API_TAG: &str = "Types Registry";

struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Registers all REST routes for the Types Registry gear.
#[allow(clippy::needless_pass_by_value)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<TypesRegistryService>,
    registry: Option<Arc<RegistryService>>,
) -> Router {
    // ponytail: ceiling C8 — every P0 operation is platform-plane (`plane = 1`),
    // but the plane is expressed by the contract and the data, **not enforced by
    // the transport**: an in-process gear has no inbound platform-identity
    // validator, api-gateway has no platform listener, and `OperationBuilder`
    // cannot mark a route platform-only. Mutation routes therefore stay
    // internal-only (`exposed = false`) until a platform listener can authenticate
    // a platform principal and a PDP decision is enforced before dispatch.
    // `.anonymous()` is deliberately **not** used — without a platform identity to
    // replace the current gate it would be a regression. The upgrade path is a
    // platform listener with `X-ToolKit-Internal-Token` / `PlatformIdentity` plus a
    // declarative route marker: toolkit/api-gateway work outside this gear
    // (SPEC §9 C8, §8.4).
    //
    // The v2 routes below are internal-only for a second reason too: v2 is an
    // interim surface until T24a promotes it onto `V1`. They still register in the
    // `OpenAPI` document — `exposed` gates gateway visibility, not spec inclusion —
    // so the contract check sees them. T24a changes the path constant only; it must
    // not expose mutation routes while ceiling C8 remains open.

    router = register_v1(router, openapi);
    router = register_submit(router, openapi);
    router = register_reads(router, openapi);
    router = register_batch_delete(router, openapi);
    router = register_delete_entity(router, openapi);

    router.layer(Extension(service)).layer(Extension(registry))
}

/// Declare the required mutation header via `param`; `OperationBuilder` has
/// no `header_param` helper (upstream #4614).
fn idempotency_key_param() -> ParamSpec {
    ParamSpec {
        name: "Idempotency-Key".to_owned(),
        location: ParamLocation::Header,
        required: true,
        description: Some(
            "Caller-supplied key scoping the retry of this submission. A replay with the same \
             body returns the same operation; a different body under the same key is a conflict."
                .to_owned(),
        ),
        param_type: "string".to_owned(),
        array: false,
        format: None,
        minimum: None,
    }
}

/// The pre-database v1 contract, verbatim from `main` (T9a).
fn register_v1(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // -----------------------------------------------------------------------
    // v1 — the pre-database contract, unchanged from `main`
    // -----------------------------------------------------------------------
    //
    // Every v1 route is served by `TypesRegistryService` from the in-memory
    // repository, and no v1 route touches `RegistryService`. That separation is
    // the point of T9a rather than an implementation detail: T9 repointed these
    // two routes at the database, which changed `POST /v1/entities`'s request
    // body under its existing callers and left `oagw` and `account-management`
    // writing to the database while resolving from process memory. The database
    // path has no consumer until T24 (SPEC §10.2, `plan.md` P12).

    // POST /types-registry/v1/entities - Register GTS entities
    router = OperationBuilder::post(format!("{V1}/entities"))
        .operation_id("types_registry.register")
        .summary("Register GTS entities")
        .description(
            "Register one or more GTS entities (types or instances) in batch. Returns per-item results.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<RegisterEntitiesRequest>(openapi, "GTS entities to register")
        .handler(handlers::register_entities)
        .json_response_with_schema::<RegisterEntitiesResponse>(
            openapi,
            StatusCode::OK,
            "Registration results",
        )
        .standard_errors(openapi)
        .error_413(openapi)
        .error_415(openapi)
        .error_422(openapi)
        .register(router, openapi);

    // GET /types-registry/v1/entities - List GTS entities
    router = OperationBuilder::get(format!("{V1}/entities"))
        .operation_id("types_registry.list")
        .summary("List GTS entities")
        .description(
            "List registered GTS entities with optional filtering by pattern, kind, vendor, package, or namespace.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param("pattern", false, "Wildcard pattern for GTS ID matching (e.g., gts.acme.*)")
        .query_param("kind", false, "Filter by entity kind: 'type' or 'instance'")
        .query_param("vendor", false, "Filter by vendor")
        .query_param("package", false, "Filter by package")
        .query_param("namespace", false, "Filter by namespace")
        .query_param("segmentScope", false, "Segment match scope: 'primary' or 'any' (default)")
        .handler(handlers::list_entities)
        .json_response_with_schema::<ListEntitiesResponse>(
            openapi,
            StatusCode::OK,
            "List of entities",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // GET /types-registry/v1/entities/{gts_id} - Get GTS entity by ID
    router = OperationBuilder::get(format!("{V1}/entities/{{gts_id}}"))
        .operation_id("types_registry.get")
        .summary("Get GTS entity by ID")
        .description("Retrieve a single GTS entity by its identifier.")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param(
            "gts_id",
            "The GTS identifier (e.g., gts.acme.core.events.user_created.v1~)",
        )
        .handler(handlers::get_entity)
        .json_response_with_schema::<GtsEntityDto>(openapi, StatusCode::OK, "The requested entity")
        .problem_response(openapi, StatusCode::NOT_FOUND, "Entity not found")
        .standard_errors(openapi)
        .register(router, openapi);
    router
}

// -----------------------------------------------------------------------
// v2 — the database-backed async surface (T9)
// -----------------------------------------------------------------------
//
// Interim: T24a promotes these onto v1 once the in-memory path is deleted.
// They are internal-only — no `.exposed()`, so the gateway does not publish
// the surface (see the ceiling-C8 note above). The path promotion does not
// change that posture for mutations.

/// `POST {V2}/entities` (D10).
fn register_submit(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // POST /types-registry/v2/entities — submit a registration (D10)
    //
    // The async shape DESIGN specifies: the response is a receipt for an
    // operation and the outcome is polled. `200` is returned only for a replay of
    // an operation that is already terminal.
    router = OperationBuilder::post(format!("{V2}/entities"))
        .operation_id("types_registry.submit_entities")
        .summary("Submit GTS entities for registration")
        .description(
            "Submit one or more GTS entities for admission. Returns 202 with the operation's \
             Location; poll GET /types-registry/v2/operations/{operation_id} for the \
             per-candidate outcome. A replay of a terminal operation returns 200. An \
             Idempotency-Key header is required: a replay with the same body returns the same \
             operation, and a different body under the same key is a conflict.",
        )
        .param(idempotency_key_param())
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<SubmitEntitiesRequest>(openapi, "Entities to admit")
        .handler(handlers::submit_entities)
        .json_response_with_schema::<OperationAcceptedDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Accepted; poll the operation at the returned Location",
        )
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "URI of the admission operation",
            ResponseHeaderType::String,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Retry-After",
            "Suggested delay in seconds before polling the operation",
            ResponseHeaderType::Integer,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Idempotency-Replayed",
            "Whether this submission replayed an existing operation",
            ResponseHeaderType::Boolean,
        ))
        .json_response_with_schema::<OperationAcceptedDto>(
            openapi,
            StatusCode::OK,
            "Replay of an operation that is already terminal",
        )
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "URI of the admission operation",
            ResponseHeaderType::String,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Idempotency-Replayed",
            "Whether this submission replayed an existing operation",
            ResponseHeaderType::Boolean,
        ))
        .problem_response(
            openapi,
            StatusCode::CONFLICT,
            "The Idempotency-Key is bound to a different request",
        )
        .standard_errors(openapi)
        .error_413(openapi)
        .error_415(openapi)
        .error_422(openapi)
        // An unbound database is a deployment state, so this 503 has no `Retry-After`.
        .error_503(openapi)
        .register(router, openapi);
    router
}

/// The two v2 reads: one operation, one entity.
fn register_reads(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /types-registry/v2/operations/{operation_id} — poll an operation
    router = OperationBuilder::get(format!("{V2}/operations/{{operation_id}}"))
        .operation_id("types_registry.get_operation")
        .summary("Get an admission operation")
        .description(
            "Return one operation and the durable per-candidate outcomes. `status` is progress \
             only: `completed` means every item is terminal, and the outcomes are on the items.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param(
            "operation_id",
            "The operation UUID returned by a submission",
        )
        .handler(handlers::get_operation)
        .json_response_with_schema::<OperationDto>(openapi, StatusCode::OK, "The operation")
        .problem_response(openapi, StatusCode::NOT_FOUND, "No such operation")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // GET /types-registry/v2/entities/{entity_key} — exact read from the database
    router = OperationBuilder::get(format!("{V2}/entities/{{entity_key}}"))
        .operation_id("types_registry.get_entity")
        .summary("Get a GTS entity by identifier or Registry Reference")
        .description(
            "Return one entity with its authored document and the effective artifacts \
             materialized at admission. The key is either a canonical GTS identifier or the \
             Registry Reference UUID derived from it. A deleted entity is still readable and \
             reports its lifecycle status.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param(
            "entity_key",
            "A GTS identifier (e.g. gts.acme.core.events.user_created.v1~) or a Registry \
             Reference UUID",
        )
        .handler(handlers::get_entity_by_key)
        .json_response_with_schema::<EntityDto>(openapi, StatusCode::OK, "The requested entity")
        .problem_response(openapi, StatusCode::NOT_FOUND, "Entity not found")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router
}

/// `POST {V2}/entities:batchDelete` (T20a).
fn register_batch_delete(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::post(format!("{V2}/entities:batchDelete"))
        .operation_id("types_registry.batch_delete_entities")
        .summary("Submit GTS entities for deletion")
        .description(
            "Submit one or more entities for deletion. Each item names its target in `key` \
             (a canonical GTS identifier or the Registry Reference UUID derived from it) and \
             carries a required positive `expected_resource_version`. Returns 202 with the \
             operation's Location; poll GET /types-registry/v2/operations/{operation_id} for \
             the per-item outcome. Outcomes are keyed by GTS identifier and reported in \
             request order, so a caller that deleted by Registry Reference matches results to \
             requests by position. A stale version is not a 412: it is reported as a terminal \
             `precondition_failed` item on the operation.",
        )
        .param(idempotency_key_param())
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<DeleteEntitiesRequest>(openapi, "Entities to delete")
        .handler(handlers::batch_delete_entities)
        .json_response_with_schema::<OperationAcceptedDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Accepted; poll the operation at the returned Location",
        )
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "URI of the admission operation",
            ResponseHeaderType::String,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Retry-After",
            "Suggested delay in seconds before polling the operation",
            ResponseHeaderType::Integer,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Idempotency-Replayed",
            "Whether this submission replayed an existing operation",
            ResponseHeaderType::Boolean,
        ))
        .json_response_with_schema::<OperationAcceptedDto>(
            openapi,
            StatusCode::OK,
            "Replay of an operation that is already terminal",
        )
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "URI of the admission operation",
            ResponseHeaderType::String,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Idempotency-Replayed",
            "Whether this submission replayed an existing operation",
            ResponseHeaderType::Boolean,
        ))
        .problem_response(
            openapi,
            StatusCode::NOT_FOUND,
            "An item names a Registry Reference that resolves to no entity",
        )
        .problem_response(
            openapi,
            StatusCode::CONFLICT,
            "The Idempotency-Key is bound to a different request",
        )
        .standard_errors(openapi)
        .error_413(openapi)
        .error_415(openapi)
        .error_422(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router
}

/// `DELETE {V2}/entities/{{entity_key}}` (T20a).
fn register_delete_entity(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::delete(format!("{V2}/entities/{{entity_key}}"))
        .operation_id("types_registry.delete_entity")
        .summary("Delete one GTS entity")
        .description(
            "Delete a single entity named by canonical GTS identifier or Registry Reference \
             UUID, resolved exactly as GET /types-registry/v2/entities/{entity_key} resolves \
             it. One item's worth of :batchDelete. Returns 202 with the operation's Location; \
             a stale version is reported as a terminal `precondition_failed` item rather than \
             a 412, and If-Match is refused rather than ignored.",
        )
        .param(idempotency_key_param())
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param(
            "entity_key",
            "A GTS identifier (e.g. gts.acme.core.events.user_created.v1~) or a Registry \
             Reference UUID",
        )
        // Declared as a `ParamSpec` rather than through `query_param_typed`: that
        // helper cannot carry `format`/`minimum`, and its positional `description`
        // before `param_type` is easy to swap silently into a `string`. Named
        // fields state the same precondition the batch DTO declares — a positive
        // `int64` — so a generated client rejects what acceptance would reject.
        .param(ParamSpec {
            name: "expected_resource_version".to_owned(),
            location: ParamLocation::Query,
            required: true,
            description: Some(
                "Required and positive: the resource_version the caller observed. Absent, \
                 non-numeric or zero is a 400; a mismatch is reported on the operation item"
                    .to_owned(),
            ),
            param_type: "integer".to_owned(),
            array: false,
            format: Some(SchemaFormat::KnownFormat(KnownFormat::Int64)),
            minimum: Some(1.0),
        })
        .query_param_typed(
            "dry_run",
            false,
            "Run the whole check sequence and commit nothing. Defaults to false",
            "boolean",
        )
        .handler(handlers::delete_entity)
        .json_response_with_schema::<OperationAcceptedDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Accepted; poll the operation at the returned Location",
        )
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "URI of the admission operation",
            ResponseHeaderType::String,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Retry-After",
            "Suggested delay in seconds before polling the operation",
            ResponseHeaderType::Integer,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Idempotency-Replayed",
            "Whether this submission replayed an existing operation",
            ResponseHeaderType::Boolean,
        ))
        .json_response_with_schema::<OperationAcceptedDto>(
            openapi,
            StatusCode::OK,
            "Replay of an operation that is already terminal",
        )
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "URI of the admission operation",
            ResponseHeaderType::String,
        ))
        .response_header(ResponseHeaderSpec::new(
            "Idempotency-Replayed",
            "Whether this submission replayed an existing operation",
            ResponseHeaderType::Boolean,
        ))
        .problem_response(
            openapi,
            StatusCode::NOT_FOUND,
            "The entity_key is a Registry Reference that resolves to no entity",
        )
        .problem_response(
            openapi,
            StatusCode::CONFLICT,
            "The Idempotency-Key is bound to a different request",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router
}
