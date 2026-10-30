//! REST route registration for the Types Registry gear.

use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::StatusCode;
use toolkit::api::operation_builder::state::{AuthNotSet, AuthSet, Missing};
use toolkit::api::operation_builder::{
    CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature, OperationBuilder, OperationBuilderODataExt,
    ParamSpec, ResponseHeaderSpec, ResponseHeaderType,
};

use super::dto::{
    BatchGetRequest, DeleteEntitiesRequest, EntityDto, EntityLookupsDto, EntityPageDto,
    GtsEntityDto, ListEntitiesResponse, OperationAcceptedDto, OperationDto,
    RegisterEntitiesRequest, RegisterEntitiesResponse, SubmitEntitiesRequest,
};
use super::handlers;
pub use super::paths::{PLATFORM, V1, V2};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::reason;

use crate::config::Limits;
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
    host: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<TypesRegistryService>,
    registry: Option<Arc<RegistryService>>,
) -> Router {
    // D17: platform operations use `.platform_authenticated()` under PLATFORM;
    // tenant entity reads use `.authenticated()` under V2 (V1 after T38).
    // C6: authentication only; any valid internal token may mutate.
    // C8: no `.exposed()` routes, excluding OoP platform routes from gateway
    // discovery. Embedded routes share the gateway listener and token validation.

    let mut router = register_v1(Router::new(), openapi);
    let limits = registry
        .as_deref()
        .map_or_else(Limits::default, |registry| *registry.limits());
    router = register_submit(router, openapi);
    router = register_operation(router, openapi);
    router = register_batch_delete(router, openapi);
    router = register_delete_entity(router, openapi);
    for plane in [Plane::Platform, Plane::Tenant] {
        router = register_exact_read(router, openapi, plane);
        router = register_batch_get(router, openapi, plane);
        router = register_discovery(router, openapi, &limits, plane);
    }

    // Only the registry's own routes are gated; the host's are not ours to refuse.
    host.merge(router.route_layer(from_fn(require_caller)))
        .layer(Extension(service))
        .layer(Extension(registry))
}

/// Return canonical 401 when a registry route has no caller context (SPEC §8.4).
/// Standalone `oop_serve` installs no tenant plane without an authenticator;
/// this prevents unauthenticated legacy reads and database extractor 500s.
/// The listener's platform gate already handles platform routes.
async fn require_caller(request: Request, next: Next) -> Response {
    let extensions = request.extensions();
    if extensions.get::<SecurityContext>().is_some()
        || extensions.get::<PlatformSecurityContext>().is_some()
    {
        return next.run(request).await;
    }
    tracing::warn!(
        path = %request.uri().path(),
        "types_registry refused a request with no validated caller; no tenant authenticator \
         is installed"
    );
    CanonicalError::unauthenticated()
        .with_reason(reason::unauthenticated::TENANT_AUTHENTICATION_UNAVAILABLE)
        .create()
        .into_response()
}

/// Declare the required mutation header via `param`. `OperationBuilder` still
/// has no `header_param` helper beside `path_param` / `query_param` (upstream
/// #4614), but `ParamSpec::header` now names the location, so the capability no
/// longer has to be found by reading `ParamLocation`.
fn idempotency_key_param() -> ParamSpec {
    ParamSpec::header("Idempotency-Key")
        .required(true)
        .description(
            "Caller-supplied key scoping the retry of this submission. A replay with the same \
             body returns the same operation; a different body under the same key is a conflict.",
        )
}

/// The exact read's validator (SPEC §8.5), on its `200` and its `304`.
fn etag_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "The validator of this key under this `$select`",
        ResponseHeaderType::String,
    )
}

/// The pre-database v1 contract, verbatim from `main` (T9a).
fn register_v1(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // Legacy v1 uses only the in-memory TypesRegistryService. Until T38, consumers must read
    // and write the same store; routing v1 writes to RegistryService would break that invariant.

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

// The database API (T9): every operation on the platform plane, and the entity reads again
// on the tenant plane. No `.exposed()`: see C8 above.

/// Which plane a route set serves (D17); a route serves exactly one.
#[derive(Clone, Copy)]
enum Plane {
    /// `.platform_authenticated()` under [`PLATFORM`].
    Platform,
    /// `.authenticated()` under [`V2`], until T38 promotes it to [`V1`].
    Tenant,
}

impl Plane {
    const fn base(self) -> &'static str {
        match self {
            Self::Platform => PLATFORM,
            Self::Tenant => V2,
        }
    }

    /// Distinct per plane, since a read is declared on both.
    fn operation_id(self, name: &str) -> String {
        match self {
            Self::Platform => format!("types_registry.platform.{name}"),
            Self::Tenant => format!("types_registry.{name}"),
        }
    }

    /// Applied before the handler, while the builder is still bare.
    fn authenticate(
        self,
        builder: OperationBuilder<Missing, Missing, (), AuthNotSet>,
    ) -> OperationBuilder<Missing, Missing, (), AuthSet> {
        match self {
            Self::Platform => builder.platform_authenticated(),
            Self::Tenant => builder.authenticated(),
        }
    }
}

/// The receipt headers of a `202`, plus `Retry-After` when `accepted`.
fn receipt_headers(accepted: bool) -> Vec<ResponseHeaderSpec> {
    let mut headers = vec![ResponseHeaderSpec::new(
        "Location",
        "URI of the admission operation",
        ResponseHeaderType::String,
    )];
    if accepted {
        headers.push(ResponseHeaderSpec::new(
            "Retry-After",
            "Suggested delay in seconds before polling the operation",
            ResponseHeaderType::Integer,
        ));
    }
    headers.push(ResponseHeaderSpec::new(
        "Idempotency-Replayed",
        "Whether this submission replayed an existing operation",
        ResponseHeaderType::Boolean,
    ));
    headers
}

/// `POST {PLATFORM}/entities` (D10).
fn register_submit(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // The async shape DESIGN specifies: the response is a receipt for an
    // operation and the outcome is polled. `200` is returned only for a replay of
    // an operation that is already terminal.
    let mut builder = OperationBuilder::post(format!("{PLATFORM}/entities"))
        .operation_id(Plane::Platform.operation_id("submit_entities"))
        .summary("Submit GTS entities for registration")
        .description(format!(
            "Submit one or more GTS entities for admission. Returns 202 with the operation's \
             Location; poll GET {PLATFORM}/operations/{{operation_id}} for the \
             per-candidate outcome. A replay of a terminal operation returns 200. An \
             Idempotency-Key header is required: a replay with the same body returns the same \
             operation, and a different body under the same key is a conflict.",
        ))
        .param(idempotency_key_param())
        .tag(API_TAG)
        .platform_authenticated()
        .require_license_features::<License>([])
        .json_request::<SubmitEntitiesRequest>(openapi, "Entities to admit")
        .handler(handlers::submit_entities)
        .json_response_with_schema::<OperationAcceptedDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Accepted; poll the operation at the returned Location",
        );
    for header in receipt_headers(true) {
        builder = builder.response_header(header);
    }
    builder = builder.json_response_with_schema::<OperationAcceptedDto>(
        openapi,
        StatusCode::OK,
        "Replay of an operation that is already terminal",
    );
    for header in receipt_headers(false) {
        builder = builder.response_header(header);
    }
    builder
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
        .register(router, openapi)
}

/// `GET {PLATFORM}/operations/{operation_id}`: platform only, like every mutation.
fn register_operation(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::get(format!("{PLATFORM}/operations/{{operation_id}}"))
        .operation_id(Plane::Platform.operation_id("get_operation"))
        .summary("Get an admission operation")
        .description(
            "Return one operation and the durable per-candidate outcomes. `status` is progress \
             only: `completed` means every item is terminal, and the outcomes are on the items.",
        )
        .tag(API_TAG)
        .platform_authenticated()
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
        .register(router, openapi)
}

/// `GET {base}/entities/{entity_key}` — the exact read, on either plane.
fn register_exact_read(router: Router, openapi: &dyn OpenApiRegistry, plane: Plane) -> Router {
    let base = plane.base();
    let builder = OperationBuilder::get(format!("{base}/entities/{{entity_key}}"))
        .operation_id(plane.operation_id("get_entity"))
        .summary("Get a GTS entity by identifier or Registry Reference")
        .description(
            "Return one entity, projected by `$select`. The key is either a canonical GTS \
             identifier or the Registry Reference UUID derived from it; a key over 1024 \
             bytes is a 400, and any other key naming no entity is a 404. Absent `$select` is \
             the document-free default `gts_id,gts_uuid,kind,origin,lifecycle_status`; \
             documents are selected individually from `content`, \
             `resolved_schema`, `effective_traits` and `effective_traits_schema` (the last \
             three Type Schemas only), plus the `provenance` group of a Type Schema \
             (omitted on an Instance). Names are \
             case-insensitive; an empty, duplicate, unknown or nested name is a 400. \
             `gts_id`, `gts_uuid`, `kind` and `lifecycle_status` are always returned, \
             whether or not `$select` names them, so a deleted entity is still readable and \
             reports it. No other query parameter is accepted. The `ETag` is scoped to the \
             `$select`; `If-None-Match` with a current one returns a bodyless 304.",
        )
        .tag(API_TAG);
    let builder = plane
        .authenticate(builder)
        .require_license_features::<License>([])
        .path_param(
            "entity_key",
            "A GTS identifier (e.g. gts.acme.core.events.user_created.v1~) or a Registry \
             Reference UUID",
        )
        .param(
            ParamSpec::header("If-None-Match")
                .required(false)
                .description("An earlier `ETag` of this key under the same `$select`, or `*`"),
        )
        .with_odata_select();
    let builder = match plane {
        Plane::Platform => builder.handler(handlers::platform_get_entity_by_key),
        Plane::Tenant => builder.handler(handlers::get_entity_by_key),
    };
    builder
        .json_response_with_schema::<EntityDto>(openapi, StatusCode::OK, "The requested entity")
        .response_header(etag_header())
        .no_content_response(
            StatusCode::NOT_MODIFIED,
            "The If-None-Match validator is current",
        )
        .response_header(etag_header())
        .problem_response(openapi, StatusCode::NOT_FOUND, "Entity not found")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// `POST {base}/entities:batchGet` (T22a), on either plane.
fn register_batch_get(router: Router, openapi: &dyn OpenApiRegistry, plane: Plane) -> Router {
    let base = plane.base();
    // A read-only custom action, and a `POST` for two reasons that are about the
    // transport rather than the semantics: an identifier runs to 1024 characters,
    // which a query string cannot carry safely, and portable `GET` has no body
    // (DESIGN §3.3). It is still a read — no `Idempotency-Key`, nothing to replay.
    let builder = OperationBuilder::post(format!("{base}/entities:batchGet"))
        .operation_id(plane.operation_id("batch_get_entities"))
        .summary("Read a set of GTS entities by key")
        .description(format!(
            "Read up to 100 entities in one round trip. Each item names one entity in `entity_key` \
             (a canonical GTS identifier or the Registry Reference UUID derived from it), \
             resolved exactly as GET {base}/entities/{{entity_key}} resolves it. \
             A top-level `$select` string applies to every key and follows that route's \
             `$select` rules; absent, the document-free default. Tombstones are `found`. Returns 200 with one result \
             per distinct requested key, in no contractual order, echoing the key it was asked by, a Registry \
             Reference in canonical lowercase hyphenated form: `found` \
             with the selected fields, exactly as the exact read returns them and always \
             including `gts_id`, `gts_uuid`, `kind` and `lifecycle_status`, or `not_found`. \
             Every result but `not_found` carries the key's `etag`; an item whose \
             `if_none_match` is still current answers `unchanged` without the entity. \
             Query parameters are refused, `$select` included. A key \
             named twice, in any UUID spelling, collapses onto its first mention and its \
             condition; the identifier \
             and the Registry Reference of one entity are \
             two keys and get two results. An absent key is not a 404: one missing key must \
             not lose the answers for the others. The If-None-Match header is refused rather \
             than ignored: validators are per key and belong in each item's `if_none_match`.",
        ))
        .tag(API_TAG);
    let builder = plane
        .authenticate(builder)
        .require_license_features::<License>([])
        .json_request::<BatchGetRequest>(openapi, "Keys to read");
    let builder = match plane {
        Plane::Platform => builder.handler(handlers::platform_batch_get_entities),
        Plane::Tenant => builder.handler(handlers::batch_get_entities),
    };
    builder
        .json_response_with_schema::<EntityLookupsDto>(
            openapi,
            StatusCode::OK,
            "One result per requested key",
        )
        .standard_errors(openapi)
        .error_413(openapi)
        .error_415(openapi)
        .error_422(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// `GET {base}/entities` — the bounded, projected discovery page (D12, T22a, T22b), on
/// either plane.
fn register_discovery(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    limits: &Limits,
    plane: Plane,
) -> Router {
    let base = plane.base();
    let (default, max) = (limits.page_size_default, limits.page_size_max);
    let builder = OperationBuilder::get(format!("{base}/entities"))
        .operation_id(plane.operation_id("list_entities"))
        .summary("Discover GTS entities")
        .description(format!(
            "Return one bounded page of entities, ordered by canonical identifier, with a \
             cursor only when another match remains; a page with a cursor is full. \
             `lifecycle_status` is `active` (default), `deleted` \
             (tombstones only) or `all`. Each item is projected by `$select` \
             exactly as GET {base}/entities/{{entity_key}} projects it; absent, the \
             document-free default; `gts_id`, `gts_uuid`, `kind` and `lifecycle_status` are \
             always returned. A page never carries a validator. `depth` bounds the number of \
             identifier segments and `kind` narrows to Type Schemas or Instances; \
             `lifecycle_status`, `depth` and `kind` intersect with `pattern` before the page \
             limit. `limit` (alias `$top`) \
             defaults to {default} and may not exceed {max}; a caller selecting documents should \
             page smaller. `cursor` (alias `$skiptoken`) is opaque, versioned and bound to \
             the pattern, `depth`, `kind`, `lifecycle_status` and the normalized `$select` \
             it was issued for: resuming under any of them changed, or with a token of \
             another version, is a 400, while an absent and an explicit default value of \
             `$select` or `lifecycle_status` are interchangeable. Any other query parameter \
             is refused.",
        ))
        .tag(API_TAG);
    let builder = plane
        .authenticate(builder)
        .require_license_features::<License>([])
        .query_param(
            "pattern",
            false,
            "A GTS identifier pattern (GTS spec section 10), with or without a wildcard. With one \
             trailing `*`, starting at a segment token or the version, it matches every \
             identifier under that prefix, derived types and Instances included (e.g. \
             gts.acme.core.*, gts.acme.core.events.user_created.v1~*). Without `*` it must \
             be a valid GTS identifier: a Type Schema identifier matches itself, every type \
             derived from it and every Instance of them (e.g. \
             gts.acme.core.events.user_created.v1~); an Instance identifier matches that \
             Instance. In either form a segment that gives only a major version (`v1`) \
             matches every minor of that major (`v1`, `v1.0`, `v1.3`), while a given minor \
             matches only itself. A pattern that does not parse is a 400",
        )
        // `ParamSpec` has no `maximum`, so the upper bound is stated in the description.
        .param(
            ParamSpec::query("depth")
                .param_type("integer")
                .minimum(1.0)
                .description(
                    "Inclusive maximum number of GTS identifier segments, 1 to 255: a \
                     one-segment root has depth 1 and each derived type or Instance tail \
                     adds one. Applies with or without `pattern`",
                ),
        )
        // `ParamSpec` has no `enum`, so the vocabulary is stated in the description.
        .param(ParamSpec::query("kind").param_type("string").description(
            "Only entities of this kind: `type_schema` or `instance`. Absent means both; \
             any other value is a 400",
        ))
        .param(
            ParamSpec::query("lifecycle_status")
                .param_type("string")
                .description(
                    "`active` (default), `deleted` (tombstones only) or `all` (both). Any other \
                     value, empty or repeated, is a 400",
                ),
        )
        // `ParamSpec` has no `maximum`, and this one is configured, so the description states it.
        .param(
            ParamSpec::query("limit")
                .param_type("integer")
                .minimum(1.0)
                .description(format!(
                    "Page size, 1 to {max}. Defaults to {default}. Alias: $top"
                )),
        )
        .query_param(
            "cursor",
            false,
            "The previous page's page_info.next_cursor, under the same pattern, depth, kind, \
             lifecycle_status and $select. \
             Absent starts at the beginning. Alias: $skiptoken",
        )
        .with_odata_select();
    let builder = match plane {
        Plane::Platform => builder.handler(handlers::platform_discover_entities),
        Plane::Tenant => builder.handler(handlers::discover_entities),
    };
    builder
        .json_response_with_schema::<EntityPageDto>(
            openapi,
            StatusCode::OK,
            "One page and, while more remains, its cursor",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// `POST {PLATFORM}/entities:batchDelete` (T20a).
fn register_batch_delete(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let mut builder = OperationBuilder::post(format!("{PLATFORM}/entities:batchDelete"))
        .operation_id(Plane::Platform.operation_id("batch_delete_entities"))
        .summary("Submit GTS entities for deletion")
        .description(format!(
            "Submit one or more entities for deletion. Each item names its target in `entity_key` \
             (a canonical GTS identifier or the Registry Reference UUID derived from it) and \
             carries a required positive `expected_resource_version`. Returns 202 with the \
             operation's Location; poll GET {PLATFORM}/operations/{{operation_id}} for \
             the per-item outcome. Outcomes are reported in request order and echo each key as \
             `entity_key`, a Registry Reference in canonical lowercase hyphenated form; a \
             Registry Reference naming no entity fails only its own item. A \
             stale version is not a 412: \
             it is reported as a terminal `precondition_failed` item on the operation, and \
             If-Match is refused rather than ignored.",
        ))
        .param(idempotency_key_param())
        .tag(API_TAG)
        .platform_authenticated()
        .require_license_features::<License>([])
        .json_request::<DeleteEntitiesRequest>(openapi, "Entities to delete")
        .handler(handlers::batch_delete_entities)
        .json_response_with_schema::<OperationAcceptedDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Accepted; poll the operation at the returned Location",
        );
    for header in receipt_headers(true) {
        builder = builder.response_header(header);
    }
    builder = builder.json_response_with_schema::<OperationAcceptedDto>(
        openapi,
        StatusCode::OK,
        "Replay of an operation that is already terminal",
    );
    for header in receipt_headers(false) {
        builder = builder.response_header(header);
    }
    builder
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
        .register(router, openapi)
}

/// `DELETE {PLATFORM}/entities/{{entity_key}}` (T20a).
fn register_delete_entity(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let mut builder = OperationBuilder::delete(format!("{PLATFORM}/entities/{{entity_key}}"))
        .operation_id(Plane::Platform.operation_id("delete_entity"))
        .summary("Delete one GTS entity")
        .description(format!(
            "Delete a single entity named by canonical GTS identifier or Registry Reference \
             UUID, spelled as GET {PLATFORM}/entities/{{entity_key}} takes it and \
             resolved by the worker. One item's worth of :batchDelete. Returns 202 with the operation's Location; \
             a stale version is reported as a terminal `precondition_failed` item rather than \
             a 412, and If-Match is refused rather than ignored.",
        ))
        .param(idempotency_key_param())
        .tag(API_TAG)
        .platform_authenticated()
        .require_license_features::<License>([])
        .path_param(
            "entity_key",
            "A GTS identifier (e.g. gts.acme.core.events.user_created.v1~) or a Registry \
             Reference UUID",
        )
        // Declared as a `ParamSpec` rather than through `query_param_typed`: that
        // helper cannot carry `format`/`minimum`, and its positional `description`
        // before `param_type` is easy to swap silently into a `string`. Named
        // setters state the same precondition the batch DTO declares — a positive
        // `int64` — so a generated client rejects what acceptance would reject.
        .param(
            ParamSpec::query("expected_resource_version")
                .required(true)
                .param_type("integer")
                .format("int64")
                .minimum(1.0)
                .description(
                    "Required and positive: the resource_version the caller observed. Absent, \
                     non-numeric or zero is a 400; a mismatch is reported on the operation item",
                ),
        )
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
        );
    for header in receipt_headers(true) {
        builder = builder.response_header(header);
    }
    builder = builder.json_response_with_schema::<OperationAcceptedDto>(
        openapi,
        StatusCode::OK,
        "Replay of an operation that is already terminal",
    );
    for header in receipt_headers(false) {
        builder = builder.response_header(header);
    }
    builder
        .problem_response(
            openapi,
            StatusCode::CONFLICT,
            "The Idempotency-Key is bound to a different request",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi)
}
