//! `OpenAPI` registry for schema and operation management
//!
//! This module provides a standalone `OpenAPI` registry that collects operation specs
//! and schemas, and builds a complete `OpenAPI` document from them.

use anyhow::Result;
use arc_swap::ArcSwap;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use utoipa::openapi::{
    OpenApi, OpenApiBuilder, Ref, RefOr, Required,
    content::ContentBuilder,
    info::InfoBuilder,
    path::{
        HttpMethod, OperationBuilder as UOperationBuilder, ParameterBuilder, ParameterIn,
        PathItemBuilder, PathsBuilder,
    },
    request_body::RequestBodyBuilder,
    response::{ResponseBuilder, ResponsesBuilder},
    schema::{ComponentsBuilder, ObjectBuilder, Schema, SchemaFormat, SchemaType},
    security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

use crate::api::{operation_builder, problem};

/// Type alias for schema collections used in API operations.
type SchemaCollection = Vec<(String, RefOr<Schema>)>;

/// `OpenAPI` document metadata (title, version, description)
#[derive(Debug, Clone)]
pub struct OpenApiInfo {
    pub title: String,
    pub version: String,
    pub description: Option<String>,
}

impl Default for OpenApiInfo {
    fn default() -> Self {
        Self {
            title: "API Documentation".to_owned(),
            version: "0.1.0".to_owned(),
            description: None,
        }
    }
}

/// `OpenAPI` registry trait for operation and schema registration
pub trait OpenApiRegistry: Send + Sync {
    /// Register an API operation specification
    fn register_operation(&self, spec: &operation_builder::OperationSpec);

    /// Ensure schema for a type (including transitive dependencies) is registered
    /// under components and return the canonical component name for `$ref`.
    /// This is a type-erased version for dyn compatibility.
    fn ensure_schema_raw(&self, name: &str, schemas: SchemaCollection) -> String;

    /// Downcast support for accessing the concrete implementation if needed.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Helper function to call `ensure_schema` with proper type information
pub fn ensure_schema<T: utoipa::ToSchema + utoipa::PartialSchema + 'static>(
    registry: &dyn OpenApiRegistry,
) -> String {
    use utoipa::PartialSchema;

    // 1) Canonical component name for T as seen by utoipa
    let root_name = T::name().to_string();

    // 2) Always insert T's own schema first (actual object, not a ref)
    //    This avoids self-referential components.
    let mut collected: SchemaCollection = vec![(root_name.clone(), <T as PartialSchema>::schema())];

    // 3) Collect and append all referenced schemas (dependencies) of T
    T::schemas(&mut collected);

    // 4) Pass to registry for insertion
    registry.ensure_schema_raw(&root_name, collected)
}

/// Implementation of `OpenAPI` registry with lock-free data structures
pub struct OpenApiRegistryImpl {
    /// Store operation specs keyed by "METHOD:path"
    pub operation_specs: DashMap<String, operation_builder::OperationSpec>,
    /// Store schema components using arc-swap for lock-free reads
    pub components_registry: ArcSwap<HashMap<String, RefOr<Schema>>>,
}

impl OpenApiRegistryImpl {
    /// Create a new empty registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            operation_specs: DashMap::new(),
            components_registry: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    /// Build `OpenAPI` specification from registered operations and components.
    ///
    /// # Arguments
    /// * `info` - `OpenAPI` document metadata (title, version, description)
    ///
    /// # Errors
    /// Returns an error if the `OpenAPI` specification cannot be built.
    #[allow(unknown_lints, de0205_operation_builder)]
    pub fn build_openapi(&self, info: &OpenApiInfo) -> Result<OpenApi> {
        use http::Method;

        // Log operation count for visibility
        let op_count = self.operation_specs.len();
        tracing::info!("Building OpenAPI: found {op_count} registered operations");

        // 1) Paths
        let mut paths = PathsBuilder::new();

        for spec in self.operation_specs.iter().map(|e| e.value().clone()) {
            let mut op = UOperationBuilder::new()
                .operation_id(spec.operation_id.clone().or(Some(spec.handler_id.clone())))
                .summary(spec.summary.clone())
                .description(spec.description.clone());

            for tag in &spec.tags {
                op = op.tag(tag.clone());
            }

            // Vendor extensions
            let mut ext = utoipa::openapi::extensions::Extensions::default();

            // Rate limit
            if let Some(rl) = spec.rate_limit.as_ref() {
                ext.insert("x-rate-limit-rps".to_owned(), serde_json::json!(rl.rps));
                ext.insert("x-rate-limit-burst".to_owned(), serde_json::json!(rl.burst));
                ext.insert(
                    "x-in-flight-limit".to_owned(),
                    serde_json::json!(rl.in_flight),
                );
            }

            // Pagination
            if let Some(pagination) = spec.vendor_extensions.x_odata_filter.as_ref()
                && let Ok(value) = serde_json::to_value(pagination)
            {
                ext.insert("x-odata-filter".to_owned(), value);
            }
            if let Some(pagination) = spec.vendor_extensions.x_odata_orderby.as_ref()
                && let Ok(value) = serde_json::to_value(pagination)
            {
                ext.insert("x-odata-orderby".to_owned(), value);
            }

            if !ext.is_empty() {
                op = op.extensions(Some(ext));
            }

            // Parameters
            for p in &spec.params {
                let in_ = match p.location {
                    operation_builder::ParamLocation::Path => ParameterIn::Path,
                    operation_builder::ParamLocation::Query => ParameterIn::Query,
                    operation_builder::ParamLocation::Header => ParameterIn::Header,
                    operation_builder::ParamLocation::Cookie => ParameterIn::Cookie,
                };
                let required =
                    if matches!(p.location, operation_builder::ParamLocation::Path) || p.required {
                        Required::True
                    } else {
                        Required::False
                    };

                let schema_type = match p.param_type.as_str() {
                    "integer" => SchemaType::Type(utoipa::openapi::schema::Type::Integer),
                    "number" => SchemaType::Type(utoipa::openapi::schema::Type::Number),
                    "boolean" => SchemaType::Type(utoipa::openapi::schema::Type::Boolean),
                    _ => SchemaType::Type(utoipa::openapi::schema::Type::String),
                };
                let schema = Schema::Object(ObjectBuilder::new().schema_type(schema_type).build());

                let param = ParameterBuilder::new()
                    .name(&p.name)
                    .parameter_in(in_)
                    .required(required)
                    .description(p.description.clone())
                    .schema(Some(schema))
                    .build();

                op = op.parameter(param);
            }

            // Request body
            if let Some(rb) = &spec.request_body {
                let content = match &rb.schema {
                    operation_builder::RequestBodySchema::Ref { schema_name } => {
                        ContentBuilder::new()
                            .schema(Some(RefOr::Ref(Ref::from_schema_name(schema_name.clone()))))
                            .build()
                    }
                    operation_builder::RequestBodySchema::MultipartFile { field_name } => {
                        // Build multipart/form-data schema with a single binary file field
                        // type: object
                        // properties:
                        //   {field_name}: { type: string, format: binary }
                        // required: [ field_name ]
                        let file_schema = Schema::Object(
                            ObjectBuilder::new()
                                .schema_type(SchemaType::Type(
                                    utoipa::openapi::schema::Type::String,
                                ))
                                .format(Some(SchemaFormat::Custom("binary".into())))
                                .build(),
                        );
                        let obj = ObjectBuilder::new()
                            .property(field_name.clone(), file_schema)
                            .required(field_name.clone());
                        let schema = Schema::Object(obj.build());
                        ContentBuilder::new().schema(Some(schema)).build()
                    }
                    operation_builder::RequestBodySchema::Binary => {
                        // Represent raw binary body as type string, format binary.
                        // This is used for application/octet-stream and similar raw binary content.
                        let schema = Schema::Object(
                            ObjectBuilder::new()
                                .schema_type(SchemaType::Type(
                                    utoipa::openapi::schema::Type::String,
                                ))
                                .format(Some(SchemaFormat::Custom("binary".into())))
                                .build(),
                        );

                        ContentBuilder::new().schema(Some(schema)).build()
                    }
                    operation_builder::RequestBodySchema::InlineObject => {
                        // Preserve previous behavior for inline object bodies
                        ContentBuilder::new()
                            .schema(Some(Schema::Object(ObjectBuilder::new().build())))
                            .build()
                    }
                };
                let mut rbld = RequestBodyBuilder::new()
                    .description(rb.description.clone())
                    .content(rb.content_type.to_owned(), content);
                if rb.required {
                    rbld = rbld.required(Some(Required::True));
                }
                op = op.request_body(Some(rbld.build()));
            }

            // Responses
            let mut responses = ResponsesBuilder::new();
            for r in &spec.responses {
                let is_json_like = r.content_type == "application/json"
                    || r.content_type == problem::APPLICATION_PROBLEM_JSON
                    || r.content_type == "text/event-stream";
                let resp = if is_json_like {
                    if let Some(name) = &r.schema_name {
                        // Manually build content to preserve the correct content type
                        let content = ContentBuilder::new()
                            .schema(Some(RefOr::Ref(Ref::new(format!(
                                "#/components/schemas/{name}"
                            )))))
                            .build();
                        ResponseBuilder::new()
                            .description(&r.description)
                            .content(r.content_type, content)
                            .build()
                    } else {
                        let content = ContentBuilder::new()
                            .schema(Some(Schema::Object(ObjectBuilder::new().build())))
                            .build();
                        ResponseBuilder::new()
                            .description(&r.description)
                            .content(r.content_type, content)
                            .build()
                    }
                } else {
                    let schema = Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(utoipa::openapi::schema::Type::String))
                            .format(Some(SchemaFormat::Custom(r.content_type.into())))
                            .build(),
                    );
                    let content = ContentBuilder::new().schema(Some(schema)).build();
                    ResponseBuilder::new()
                        .description(&r.description)
                        .content(r.content_type, content)
                        .build()
                };
                responses = responses.response(r.status.to_string(), resp);
            }
            op = op.responses(responses.build());

            // Add security requirement if operation requires authentication
            if spec.authenticated {
                let sec_req = utoipa::openapi::security::SecurityRequirement::new(
                    "bearerAuth",
                    Vec::<String>::new(),
                );
                op = op.security(sec_req);
            }

            let method = match spec.method {
                Method::POST => HttpMethod::Post,
                Method::PUT => HttpMethod::Put,
                Method::DELETE => HttpMethod::Delete,
                Method::PATCH => HttpMethod::Patch,
                // GET and any other method default to Get
                _ => HttpMethod::Get,
            };

            let item = PathItemBuilder::new().operation(method, op.build()).build();
            // Convert Axum-style path to OpenAPI-style path
            let openapi_path = operation_builder::axum_to_openapi_path(&spec.path);
            paths = paths.path(openapi_path, item);
        }

        // 2) Components (from our registry)
        let reg = self.components_registry.load();
        let mut components = ComponentsBuilder::new();
        for (name, schema) in reg.iter() {
            components = components.schema(name.clone(), schema.clone());
        }

        // Add bearer auth security scheme
        components = components.security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );

        // 3) Info & final OpenAPI doc
        let openapi_info = InfoBuilder::new()
            .title(&info.title)
            .version(&info.version)
            .description(info.description.clone())
            .build();

        let openapi = OpenApiBuilder::new()
            .info(openapi_info)
            .paths(paths.build())
            .components(Some(components.build()))
            .build();

        warn_dangling_refs_in_openapi(&openapi);

        Ok(openapi)
    }
}

impl Default for OpenApiRegistryImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenApiRegistry for OpenApiRegistryImpl {
    fn register_operation(&self, spec: &operation_builder::OperationSpec) {
        let operation_key = format!("{}:{}", spec.method.as_str(), spec.path);
        self.operation_specs
            .insert(operation_key.clone(), spec.clone());

        tracing::debug!(
            handler_id = %spec.handler_id,
            method = %spec.method.as_str(),
            path = %spec.path,
            summary = %spec.summary.as_deref().unwrap_or("No summary"),
            operation_key = %operation_key,
            "Registered API operation in registry"
        );
    }

    fn ensure_schema_raw(&self, root_name: &str, schemas: SchemaCollection) -> String {
        // Snapshot & copy-on-write
        let current = self.components_registry.load();
        let mut reg = (**current).clone();

        for (name, schema) in schemas {
            // Conflict policy: identical → no-op; different → warn & override
            if let Some(existing) = reg.get(&name) {
                let a = serde_json::to_value(existing).ok();
                let b = serde_json::to_value(&schema).ok();
                if a == b {
                    continue; // Skip identical schemas
                }
                tracing::warn!(%name, "Schema content conflict; overriding with latest");
            }
            reg.insert(name, schema);
        }

        self.components_registry.store(Arc::new(reg));
        root_name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Walk the finalized `OpenAPI` document and warn about dangling `$ref` targets.
///
/// Scans the entire document (operations, request bodies, responses, and schemas)
/// so that `$ref`s emitted outside `components.schemas` are also caught.
fn warn_dangling_refs_in_openapi(openapi: &OpenApi) {
    for ref_name in &collect_all_dangling_refs_in_openapi(openapi) {
        tracing::warn!(
            schema = %ref_name,
            "Dangling $ref: schema '{}' is referenced but not registered. \
             Add an explicit `ensure_schema::<T>(registry)` call.",
            ref_name,
        );
    }
}

/// Serialize the full `OpenAPI` document to JSON, collect every
/// `#/components/schemas/{name}` reference, and return those not defined
/// in `components.schemas`.
fn collect_all_dangling_refs_in_openapi(openapi: &OpenApi) -> Vec<String> {
    let value = match serde_json::to_value(openapi) {
        Ok(v) => v,
        Err(err) => {
            tracing::debug!(error = %err, "Failed to serialize OpenAPI doc for dangling $ref check");
            return Vec::new();
        }
    };

    let mut all_refs = HashSet::new();
    collect_refs_from_json(&value, &mut all_refs);

    // Defined schema names live under components.schemas keys
    let defined: HashSet<&str> = value
        .pointer("/components/schemas")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().map(String::as_str).collect())
        .unwrap_or_default();

    all_refs
        .into_iter()
        .filter(|name| !defined.contains(name.as_str()))
        .collect()
}

/// Recursively extract `#/components/schemas/{name}` targets from a JSON value.
fn collect_refs_from_json(value: &serde_json::Value, refs: &mut HashSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(ref_str)) = map.get("$ref")
                && let Some(name) = ref_str.strip_prefix("#/components/schemas/")
            {
                refs.insert(name.to_owned());
            }
            for v in map.values() {
                collect_refs_from_json(v, refs);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_refs_from_json(v, refs);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "openapi_registry_tests.rs"]
mod tests;
