//! Gear-scoped OpenAPI document and the API reference page that renders it.
//!
//! The gateway publishes one document per *process* at `{prefix}/openapi.json`
//! and `{prefix}/docs`, covering every gear the host mounted. A client
//! integrating with Chat Engine wants the gear's own surface, so this module
//! publishes the gear-scoped equivalent under `{prefix}/chat-engine/v1/`.
//!
//! Scoping is done by observation, not by filtering. [`TeeRegistry`] wraps the
//! registry the host handed to
//! [`register_routes`](crate::api::rest::register_routes) and mirrors every
//! operation and schema into a private [`OpenApiRegistryImpl`] on the way
//! through. What that private registry ends up holding is, by construction,
//! exactly what this gear registered — no path-prefix guessing, no `$ref`
//! reachability pruning, and no way to drift from the live surface the way a
//! checked-in spec file can.
//
// @cpt-cf-chat-engine-api-rest-docs

use std::any::Any;
use std::sync::{Arc, OnceLock};

use axum::{Extension, Router};
use http::StatusCode;
use serde_json::Value;
use toolkit::api::{
    OpenApiInfo, OpenApiRegistry, OpenApiRegistryImpl, OperationBuilder, OperationSpec,
};
use utoipa::openapi::{RefOr, schema::Schema};

/// The gear's own path root. [`GearOpenApiDoc::render`] splits a request path
/// on this to recover whatever the gateway mounted the gear under.
///
/// Not `/chat-engine/v1/`: the split has to work for any future version prefix.
pub(crate) const GEAR_BASE_PATH: &str = "/chat-engine/";

/// Registry decorator that records what passes through it.
///
/// Forwards every call to the host registry — the gateway still sees the whole
/// gear — while keeping a private copy that covers this gear alone.
pub struct TeeRegistry<'host> {
    host: &'host dyn OpenApiRegistry,
    gear: OpenApiRegistryImpl,
}

impl<'host> TeeRegistry<'host> {
    /// Wrap the registry the host supplied.
    pub fn new(host: &'host dyn OpenApiRegistry) -> Self {
        Self {
            host,
            gear: OpenApiRegistryImpl::new(),
        }
    }

    /// The gear-only registry, holding everything registered through `self`.
    pub fn gear_registry(&self) -> &OpenApiRegistryImpl {
        &self.gear
    }
}

impl OpenApiRegistry for TeeRegistry<'_> {
    fn register_operation(&self, spec: &OperationSpec) {
        // Host first: it owns duplicate detection and the "first wins" policy,
        // so its log line should precede the private copy.
        self.host.register_operation(spec);
        self.gear.register_operation(spec);
    }

    fn ensure_schema_raw(&self, root_name: &str, schemas: Vec<(String, RefOr<Schema>)>) -> String {
        self.gear.ensure_schema_raw(root_name, schemas.clone());
        self.host.ensure_schema_raw(root_name, schemas)
    }

    fn as_any(&self) -> &dyn Any {
        // A transparent decorator: anything downcasting the registry wants the
        // host it stands in for, not the wrapper.
        self.host.as_any()
    }
}

/// The gear's own OpenAPI document, built once and served as a static string.
///
/// Filled by [`GearOpenApiDoc::build`] at the end of route registration. Empty
/// means building the document failed — the handler then answers `503` rather
/// than serving a half-truth.
#[derive(Debug, Default)]
pub struct GearOpenApiDoc {
    /// Document without a `servers` entry: the mount prefix is deployment
    /// configuration owned by the gateway, not by this gear.
    ///
    /// Only the prefix-free form is stored. Serializing per request costs a
    /// pretty-print of a document this endpoint serves to a human opening the
    /// reference page, and it keeps `servers` a property of the request rather
    /// than of whichever request happened to arrive first.
    document: OnceLock<Value>,
}

impl GearOpenApiDoc {
    /// Build the document from the gear-only side of `registry`.
    ///
    /// Call once, after the last `OperationBuilder::register` — operations
    /// registered later are missing from the document. A second call is a no-op.
    pub fn build(&self, registry: &TeeRegistry<'_>) {
        let info = OpenApiInfo {
            title: "Chat Engine API".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            description: Some(
                "REST surface of the Chat Engine gear: session types, session lifecycle, \
                 messages and SSE streaming, variants, reactions, search, session \
                 intelligence, export and sharing."
                    .to_owned(),
            ),
            servers: Vec::new(),
        };

        match registry
            .gear_registry()
            .build_openapi(&info)
            .and_then(|doc| serde_json::to_value(doc).map_err(Into::into))
        {
            Ok(document) => {
                if self.document.set(document).is_err() {
                    tracing::debug!("chat-engine: gear OpenAPI document already built");
                }
            }
            Err(err) => tracing::warn!(
                error = %err,
                "chat-engine: failed to build the gear OpenAPI document; \
                 {GEAR_BASE_PATH}v1/openapi will report 503"
            ),
        }
    }

    /// Render the document for a request that arrived at `request_path`.
    ///
    /// `request_path` is the full path as the client sent it
    /// (`/cf/chat-engine/v1/openapi`); whatever precedes [`GEAR_BASE_PATH`] is
    /// the gateway's mount point and becomes the document's single server entry,
    /// so "try it" in an API browser targets the real base URL. Resolved per
    /// call: a gear mounted under two paths must describe each honestly.
    ///
    /// Returns `None` when the document could not be built at startup.
    pub fn render(&self, request_path: &str) -> Option<String> {
        let mut document = self.document.get()?.clone();
        let prefix = mount_prefix(request_path);
        if !prefix.is_empty()
            && let Some(object) = document.as_object_mut()
        {
            object.insert(
                "servers".to_owned(),
                Value::Array(vec![serde_json::json!({ "url": prefix })]),
            );
        }
        // Pretty-printed: this document is read by humans as often as by
        // code generators.
        Some(serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned()))
    }
}

/// Split the gateway's mount prefix off a request path.
///
/// `/cf/chat-engine/v1/openapi` → `/cf`; `/chat-engine/v1/docs` → `""`.
fn mount_prefix(request_path: &str) -> &str {
    match request_path.rfind(GEAR_BASE_PATH) {
        Some(index) => &request_path[..index],
        None => "",
    }
}

/// Mount the two documentation routes and snapshot the document they serve.
///
/// Call this **last** in [`register_routes`](super::register_routes): the
/// snapshot covers whatever has passed through `registry` by then, so any
/// operation registered afterwards would be missing from it.
///
/// Both routes are anonymous. An API reference that demands a bearer token
/// before it will list the endpoints is useless to whoever is trying to work
/// out how to obtain one.
///
/// Lives here rather than beside the versioned endpoints because it needs none
/// of their service wiring — only a registry and a router — which is also what
/// makes it testable on its own.
pub fn mount(router: Router, registry: &TeeRegistry<'_>) -> Router {
    let mut router = OperationBuilder::get("/chat-engine/v1/openapi")
        .operation_id("chat_engine.docs.openapi")
        .summary("OpenAPI document describing the Chat Engine REST surface")
        .tag(super::routes::API_TAG)
        .anonymous()
        .handler(super::handlers::docs::openapi_json)
        .json_response(StatusCode::OK, "OpenAPI 3.1 document")
        .text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "The document could not be assembled at startup",
            "text/plain",
        )
        .register(router, registry);

    router = OperationBuilder::get("/chat-engine/v1/docs")
        .operation_id("chat_engine.docs.reference")
        .summary("Interactive API reference for the Chat Engine REST surface")
        .tag(super::routes::API_TAG)
        .anonymous()
        .handler(super::handlers::docs::docs_page)
        .html_response(StatusCode::OK, "API reference page")
        .register(router, registry);

    let doc = Arc::new(GearOpenApiDoc::default());
    doc.build(registry);
    router.layer(Extension(doc))
}

/// The API reference page.
///
/// `./openapi` is deliberately relative: the page is served at
/// `{prefix}/chat-engine/v1/docs`, so the browser resolves the spec to
/// `{prefix}/chat-engine/v1/openapi` without this gear ever learning what
/// the gateway's `prefix_path` is.
pub const DOCS_PAGE: &str = r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8"/>
  <title>Chat Engine API</title>
  <script src="https://unpkg.com/@stoplight/elements@latest/web-components.min.js"></script>
  <link rel="stylesheet" href="https://unpkg.com/@stoplight/elements@latest/styles.min.css">
</head>
<body>
  <elements-api apiDescriptionUrl="./openapi" router="hash" layout="sidebar"></elements-api>
</body>
</html>"#;

#[cfg(test)]
#[path = "docs_tests.rs"]
mod docs_tests;
