//! Route registration: one chain per endpoint, describing the route, its
//! `OpenAPI` schema, authentication, and every Problem status its runtime can
//! actually produce — so a generated client or gateway sees every failure it
//! may observe, not a generic set.

use std::sync::Arc;

use axum::{Extension, Router};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{
    CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature, OperationBuilder, OperationBuilderODataExt,
};

use crate::api::rest::{dto, handlers};
use crate::domain::service::GraphServices;

const API_TAG: &str = "Graph Storage";
const BASE: &str = "/graph-storage/v1";

pub(crate) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Register every REST route of the gear.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    services: Arc<GraphServices>,
) -> Router {
    let router = ontology_routes(router, openapi);
    let router = write_routes(router, openapi);
    let router = read_routes(router, openapi);
    let router = query_routes(router, openapi);
    router.layer(Extension(services))
}

/// Type registration and lookup.
fn ontology_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(format!("{BASE}/types"))
        .operation_id("graph_storage.register_types")
        .summary("Register a type batch, atomically")
        .description(
            "Registers GTS node, edge and attribute types. A byte-identical \
             re-registration converges. A different schema under a registered \
             identifier is a conflict by default; with \
             `options.on_existing: \"update\"` it is admitted when it is \
             backward compatible (types-registry ADR-0003, computed by GTS \
             OP#8) and, with `options.revalidate`, also when every stored row \
             of the type still validates against it. Anything else is refused \
             naming the offending schema locations",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::GraphRegisterTypesRequest>(openapi, "Types to register")
        .handler(handlers::register_types)
        // An array response is emitted inline: registering `Vec<T>` as a
        // component would name it `Vec`, which every other array resolves to.
        .json_array_response_with_schema::<dto::GraphRegisteredTypeDto>(
            openapi,
            http::StatusCode::OK,
            "The registered types, each with its chain-resolved traits and what \
             this call did to it",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post(format!("{BASE}/types/compatibility"))
        .operation_id("graph_storage.type_compatibility")
        .summary("What changing these types would cost: a dry run")
        .description(
            "Runs the identical admission path and writes nothing. Per type: \
             the state against what is registered, both directional verdicts, \
             every diagnostic with its schema location, which traits moved, \
             how many rows the type has, the object levels a later edit will \
             not be able to extend in place, and whether the change would be \
             admitted. Re-validation of stored rows is on unless \
             `options.revalidate` turns it off",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::GraphRegisterTypesRequest>(openapi, "Candidate definitions")
        .handler(handlers::type_compatibility)
        .json_response_with_schema::<dto::GraphTypeCompatibilityDto>(
            openapi,
            http::StatusCode::OK,
            "What each candidate would do",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get(format!("{BASE}/types"))
        .operation_id("graph_storage.list_types")
        .summary("List registered types")
        .description("Lists types, optionally narrowed by kind and by a GTS identifier pattern")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param("kind", false, "node / edge / attribute")
        .query_param("pattern", false, "GTS identifier pattern")
        .query_param_typed("limit", false, "Maximum rows", "integer")
        .handler(handlers::list_types)
        .json_response_with_schema::<dto::GraphTypeListDto>(
            openapi,
            http::StatusCode::OK,
            "Registered types",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get(format!("{BASE}/source-namespaces"))
        .operation_id("graph_storage.list_source_namespaces")
        .summary("Source namespaces and the producer principal bound to each")
        .description(
            "A reference node's identity names a source, and a payload proves \
             nothing about who may speak for it. This is the ownership \
             boundary itself: who may write each namespace, not what is stored \
             under it",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_source_namespaces)
        .json_response_with_schema::<dto::GraphSourceNamespaceListDto>(
            openapi,
            http::StatusCode::OK,
            "Claimed namespaces",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post(format!("{BASE}/source-namespaces/{{namespace}}/owner"))
        .operation_id("graph_storage.transfer_source_namespace")
        .summary("Move a source namespace to another producer principal")
        .description(
            "The only way a namespace changes hands: writing under someone \
             else's namespace is refused rather than treated as a claim. \
             Requires ontology administration, and records who moved it and \
             from whom",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("namespace", "The `source.system` value to transfer")
        .json_request::<dto::GraphTransferNamespaceRequest>(openapi, "The new owner")
        .handler(handlers::transfer_source_namespace)
        .json_response_with_schema::<dto::GraphSourceNamespaceDto>(
            openapi,
            http::StatusCode::OK,
            "The namespace after the transfer",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    OperationBuilder::get(format!("{BASE}/types/{{gts_type_id}}"))
        .operation_id("graph_storage.get_type")
        .summary("One type with its schema and effective traits")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("gts_type_id", "Canonical GTS identifier")
        .handler(handlers::get_type)
        .json_response_with_schema::<dto::GraphTypeDto>(
            openapi,
            http::StatusCode::OK,
            "The registered type",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// Ingest and the two soft deletes.
fn write_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::post(format!("{BASE}/ingest"))
        .operation_id("graph_storage.ingest")
        .summary("Nodes and edges in one transaction")
        .description(
            "Applies one atomic batch. Upserts replace a row's mutable state \
             wholesale, so an omitted field is cleared rather than preserved, \
             and the graph revision advances only when stored state actually \
             changed. Send `Idempotency-Key` to make a retry replay rather \
             than re-execute",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::GraphIngestRequest>(openapi, "Nodes and edges to apply")
        .handler(handlers::ingest)
        .json_response_with_schema::<dto::GraphIngestResultDto>(
            openapi,
            http::StatusCode::OK,
            "Counts and the revision the batch committed",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// Node read, tabular projection and the revision surface.
fn read_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get(format!("{BASE}/nodes/{{node_key}}"))
        .operation_id("graph_storage.get_node")
        .summary("Node with payload and bounded adjacency")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("node_key", "Producer-supplied node key")
        .query_param_typed(
            "adjacency_limit",
            false,
            "Maximum incident edges per direction",
            "integer",
        )
        .handler(handlers::get_node)
        .json_response_with_schema::<dto::GraphNodeDto>(openapi, http::StatusCode::OK, "The node")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get(format!("{BASE}/nodes"))
        .operation_id("graph_storage.project_nodes")
        .summary("Tabular projection")
        .description(
            "Binds the five accepted OData system query options; any other \
             option is rejected rather than ignored. `$filter` and `$orderby` \
             accept the four column fields and, when `type_pattern` selects \
             types, any payload path every selected type declares in its \
             `index` trait, spelled as an OData path (`payload/severity`, \
             `payload/loc/line`); an undeclared path is refused naming the \
             declared alternatives",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed("limit", false, "Page size hint", "integer")
        .query_param("cursor", false, "Opaque CursorV1 continuation token")
        .query_param(
            "type_pattern",
            false,
            "Comma-separated GTS identifier patterns narrowing the projection",
        )
        .handler(handlers::project_nodes)
        .json_response_with_schema::<toolkit_odata::Page<dto::GraphNodeRowDto>>(
            openapi,
            http::StatusCode::OK,
            "One page of nodes",
        )
        .with_odata_filter::<graph_storage_sdk::models::NodeFilterField>()
        .with_odata_orderby::<graph_storage_sdk::models::NodeFilterField>()
        .with_odata_select()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::delete(format!("{BASE}/nodes/{{node_key}}"))
        .operation_id("graph_storage.delete_node")
        .summary("Soft-delete a node and its incident edges")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("node_key", "Producer-supplied node key")
        .handler(handlers::delete_node)
        .json_response_with_schema::<dto::GraphDeleteResultDto>(
            openapi,
            http::StatusCode::OK,
            "What was tombstoned",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    OperationBuilder::delete(format!("{BASE}/edges/{{edge_key}}"))
        .operation_id("graph_storage.delete_edge")
        .summary("Soft-delete one edge")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("edge_key", "Derived edge key")
        .handler(handlers::delete_edge)
        .json_response_with_schema::<dto::GraphDeleteResultDto>(
            openapi,
            http::StatusCode::OK,
            "What was tombstoned",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// Search and traversal — the retrieval surface.
fn query_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(format!("{BASE}/search"))
        .operation_id("graph_storage.search")
        .summary("Lexical, vector or hybrid search")
        .description(
            "Hybrid runs both arms independently and fuses them with \
             Reciprocal Rank Fusion; every hit reports which arms matched and \
             at what rank in each",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::GraphSearchRequest>(openapi, "The search request")
        .handler(handlers::search)
        .json_response_with_schema::<dto::GraphSearchResponseDto>(
            openapi,
            http::StatusCode::OK,
            "Fused hits",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post(format!("{BASE}/graph/traverse"))
        .operation_id("graph_storage.traverse")
        .summary("Seeded, depth-bounded traversal")
        .description(
            "Breadth-first expansion from authorized seeds. Seeds always \
             survive truncation, and a stopped walk always says why",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::GraphTraverseRequest>(openapi, "Seeds and bounds")
        .handler(handlers::traverse)
        .json_response_with_schema::<dto::GraphTraversalResponseDto>(
            openapi,
            http::StatusCode::OK,
            "The reached subgraph",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post(format!("{BASE}/graph/neighborhood"))
        .operation_id("graph_storage.neighborhood")
        .summary("Bounded neighborhood projection")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::GraphNeighborhoodRequest>(openapi, "Root and bounds")
        .handler(handlers::neighborhood)
        .json_response_with_schema::<dto::GraphTraversalResponseDto>(
            openapi,
            http::StatusCode::OK,
            "The neighborhood",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get(format!("{BASE}/health/ready"))
        .operation_id("graph_storage.readiness")
        .summary("Readiness per capability, with named problems")
        .description(
            "Per-component state: `healthy`, `degraded`, `unhealthy`, or \
             `not_implemented` for a capability this build does not ship, \
             each with what it blocks and the condition being waited on. The \
             aggregate is ready unless a component whose failure blocks \
             everything is unhealthy; an embedding-space mismatch is unhealthy \
             and leaves the gear ready, blocking only the vector arms. \
             Answers 200 whatever the state: readiness is a state to read",
        )
        .tag(API_TAG)
        .anonymous()
        .exposed()
        .handler(handlers::readiness)
        .json_response_with_schema::<dto::GraphReadinessDto>(
            openapi,
            http::StatusCode::OK,
            "Per-capability readiness",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    OperationBuilder::get(format!("{BASE}/revision"))
        .operation_id("graph_storage.revision")
        .summary("The caller-visible graph revision")
        .description("The `(source_epoch, graph_revision)` identity every read reports")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::revision)
        .json_response_with_schema::<dto::GraphRevisionDto>(
            openapi,
            http::StatusCode::OK,
            "The current revision",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit::api::OperationSpec;

    /// Registers nothing, but the builder's own schema checks still run — and
    /// those are what this test is for.
    struct NoopRegistry;

    impl OpenApiRegistry for NoopRegistry {
        fn register_operation(&self, _spec: &OperationSpec) {}

        fn ensure_schema_raw(
            &self,
            name: &str,
            _schemas: Vec<(
                String,
                utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
            )>,
        ) -> String {
            name.to_owned()
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Every route registers its `OpenAPI` schemas without panicking.
    ///
    /// The platform refuses a component named `Vec` — every `Vec<T>` resolves
    /// to it, so two list responses would clobber each other — and it refuses
    /// it by assertion, at registration time. Without this test that assertion
    /// fires during boot, which is a long way from the line that caused it.
    ///
    /// The service-carrying `register_routes` is deliberately not exercised:
    /// it only adds the `Extension` layer, and constructing a `PolicyEnforcer`
    /// would drag a PDP into a test about schemas.
    #[test]
    fn every_route_registers_its_schemas() {
        let registry = NoopRegistry;
        let router = Router::new();
        let router = ontology_routes(router, &registry);
        let router = write_routes(router, &registry);
        let router = read_routes(router, &registry);
        drop(query_routes(router, &registry));
    }
}
