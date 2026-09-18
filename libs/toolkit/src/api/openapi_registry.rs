// Updated: 2026-04-28 by Constructor Tech
//! `OpenAPI` registry for schema and operation management
//!
//! This gear provides a standalone `OpenAPI` registry that collects operation specs
//! and schemas, and builds a complete `OpenAPI` document from them.

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use dashmap::DashMap;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use utoipa::openapi::{
    OpenApi, OpenApiBuilder, Ref, RefOr, Required,
    content::ContentBuilder,
    header::HeaderBuilder,
    info::InfoBuilder,
    path::{
        HttpMethod, OperationBuilder as UOperationBuilder, ParameterBuilder, ParameterIn,
        PathItemBuilder, PathsBuilder,
    },
    request_body::RequestBodyBuilder,
    response::{Response, ResponsesBuilder},
    schema::{ArrayBuilder, ComponentsBuilder, ObjectBuilder, Schema, SchemaFormat, SchemaType},
    security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
    server::Server,
};

use crate::api::operation_builder;
use toolkit_canonical_errors::problem;
use toolkit_contract::StreamFraming;

/// Type alias for schema collections used in API operations.
type SchemaCollection = Vec<(String, RefOr<Schema>)>;

/// Longest a declared group name may be.
const MAX_TAG_NAME_LEN: usize = 100;
/// Longest a declared group description may be.
const MAX_TAG_DESCRIPTION_LEN: usize = 1_000;
/// Most groups one document may declare.
const MAX_DECLARED_TAGS: usize = 200;

/// One entry of the document-level `tags` list.
///
/// An operation declares the tag it belongs to; this declares the *group* that
/// tag names — the order it appears in, and what it is for. Documentation
/// browsers read the document-level list to order and describe the groups in
/// their sidebar, and fall back to first-appearance order when it is absent.
///
/// `name` is matched against an operation's tag by exact string equality: no
/// trimming, no case folding. `Orders` and `orders` are two different groups,
/// and one of them will be empty.
///
/// The fields are public because this type is deserialised straight from
/// configuration. [`OpenApiTag::new`] is the checked way to build one in Rust,
/// and [`validate_tags`] is what a config loader calls to reject a list that
/// arrived some other way.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenApiTag {
    /// Exact-match name of the group. Must equal the tag an operation declares.
    pub name: String,
    /// Shown under the group heading. Optional, and worth writing: it is the
    /// only place a whole domain can be explained rather than an endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where a reader goes for more than a description can hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<OpenApiExternalDocs>,
    /// `x-*` members to emit on the group, for whatever reads the document.
    ///
    /// A named field rather than a flattened catch-all: serde cannot combine
    /// `flatten` with `deny_unknown_fields`, and the deny is what turns a
    /// mistyped `descrption:` into a startup error instead of a group that
    /// silently lost its description. So an extension copied from an existing
    /// spec is nested under this key rather than written beside `name`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// The `externalDocs` of a tag group: somewhere to send a reader.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenApiExternalDocs {
    /// Target of the link. Required, and the only reason the object exists.
    pub url: String,
    /// What the reader will find there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl OpenApiTag {
    /// A group with the given name and no description.
    ///
    /// # Errors
    /// Returns an error if `name` is blank or longer than 100 characters.
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let tag = Self {
            name: name.into(),
            description: None,
            external_docs: None,
            extensions: BTreeMap::new(),
        };
        tag.validate()?;
        Ok(tag)
    }

    /// The same group, carrying a description.
    ///
    /// # Errors
    /// Returns an error if the description is longer than 1000 characters.
    pub fn with_description(mut self, description: impl Into<String>) -> Result<Self> {
        self.description = Some(description.into());
        self.validate()?;
        Ok(self)
    }

    /// One group on its own: a usable name, a description within bounds.
    fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            anyhow::bail!("the name is empty");
        }
        // Not `trim().is_empty()`: a name is matched against an operation's tag
        // by exact equality, so ` Orders` is not `Orders`. Accepting it would
        // silently produce an empty group beside the real one — and the real
        // one would then be appended as undeclared, which is precisely the
        // placement this feature exists to control.
        if self.name.trim() != self.name {
            anyhow::bail!(
                "the name `{}` has leading or trailing whitespace; names are matched exactly",
                self.name.escape_debug()
            );
        }
        if self.name.chars().count() > MAX_TAG_NAME_LEN {
            anyhow::bail!(
                "the name `{}` is longer than {MAX_TAG_NAME_LEN} characters",
                self.name.escape_debug()
            );
        }
        validate_document_text("name", &self.name, false)?;

        if let Some(description) = &self.description {
            if description.chars().count() > MAX_TAG_DESCRIPTION_LEN {
                anyhow::bail!(
                    "the description is longer than {MAX_TAG_DESCRIPTION_LEN} characters"
                );
            }
            validate_document_text("description", description, true)?;
        }

        if let Some(docs) = &self.external_docs {
            if docs.url.trim().is_empty() {
                anyhow::bail!("the external documentation has no url");
            }
            if docs.url.chars().count() > MAX_TAG_DESCRIPTION_LEN {
                anyhow::bail!(
                    "the external documentation url is longer than \
                     {MAX_TAG_DESCRIPTION_LEN} characters"
                );
            }
            validate_document_text("external documentation url", &docs.url, false)?;
            if let Some(description) = &docs.description {
                validate_document_text("external documentation description", description, true)?;
            }
        }

        for key in self.extensions.keys() {
            // `OpenAPI` reserves every non-`x-` member of an object for the
            // specification, so a key without the prefix is not an extension —
            // it is a member this document is not allowed to invent.
            if !key.starts_with("x-") {
                anyhow::bail!(
                    "the extension `{}` does not start with `x-`",
                    key.escape_debug()
                );
            }
            validate_document_text("extension name", key, false)?;
        }
        Ok(())
    }
}

/// Characters that reorder what a reader sees rather than adding to it.
const BIDI_OVERRIDES: &[char] = &[
    // U+061C is the one that hides: it is a formatting character, so
    // `char::is_control()` says nothing about it, and it reorders text all the
    // same.
    '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

/// Refuse operator text that must not reach a served document or a log line.
///
/// These strings are copied verbatim into `/openapi.json`, which the gateway
/// serves on an anonymous route, and rendered by a documentation browser on the
/// unauthenticated `/docs` page. So a control character here is not a
/// formatting nuisance: a newline in a tag name forges a line in the startup
/// log, and a bidi override reorders what a reader is shown without changing
/// what is stored. `ApiGateway::normalize_prefix_path` allow-lists another
/// operator string that reaches HTML for the same reason.
///
/// `allow_line_breaks` is for prose fields — an `OpenAPI` description is
/// `CommonMark`, where newlines and tabs are content rather than smuggling.
///
/// # Errors
/// Returns an error naming the first offending character and its code point.
pub fn validate_document_text(label: &str, value: &str, allow_line_breaks: bool) -> Result<()> {
    let offending = value.chars().find(|c| {
        let break_ok = allow_line_breaks && matches!(c, '\n' | '\r' | '\t');
        (c.is_control() && !break_ok) || BIDI_OVERRIDES.contains(c)
    });

    if let Some(c) = offending {
        anyhow::bail!(
            "the {label} contains U+{:04X}, a control or direction-override character",
            c as u32
        );
    }
    Ok(())
}

/// Check a declared group list before it reaches a document.
///
/// Every entry has to stand on its own, and the names have to be unique:
/// `OpenAPI` requires document-level `tags` names to be distinct, so two
/// entries with one name produce a document that is invalid rather than merely
/// odd. A config loader calls this, so the failure is a startup error naming
/// the offending group instead of a served document nobody validates.
///
/// # Errors
/// Returns an error on a blank or over-long name, an over-long description, a
/// duplicate name, or more than 200 groups.
pub fn validate_tags(tags: &[OpenApiTag]) -> Result<()> {
    if tags.len() > MAX_DECLARED_TAGS {
        anyhow::bail!(
            "{} OpenAPI tag groups declared, more than the {MAX_DECLARED_TAGS} allowed",
            tags.len()
        );
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (position, tag) in tags.iter().enumerate() {
        // The position is the only handle an operator has on the offending
        // entry when the problem is the name itself — there is nothing else to
        // call it by, and a list of twenty in YAML is not searchable by eye.
        tag.validate()
            .with_context(|| format!("OpenAPI tag group #{position}"))?;
        if !seen.insert(tag.name.as_str()) {
            anyhow::bail!(
                "OpenAPI tag group #{position} repeats the name `{}`; \
                 document-level tag names must be unique",
                tag.name
            );
        }
    }
    Ok(())
}

/// `OpenAPI` document metadata (title, version, description)
#[derive(Debug, Clone)]
pub struct OpenApiInfo {
    pub title: String,
    pub version: String,
    pub description: Option<String>,
    pub servers: Vec<String>,
    /// Document-level tag groups, in the order a reader should meet them.
    ///
    /// Empty — as [`Default`] leaves it — means the document carries no `tags`
    /// key at all, which is the document every caller had before this field
    /// existed.
    pub tags: Vec<OpenApiTag>,
}

impl Default for OpenApiInfo {
    fn default() -> Self {
        Self {
            title: "API Documentation".to_owned(),
            version: "0.1.0".to_owned(),
            description: None,
            servers: Vec::new(),
            tags: Vec::new(),
        }
    }
}

/// The document-level `tags` list, or `None` when the caller declared no groups.
///
/// `None` rather than an empty vector on purpose: an empty list serialises as
/// `tags: []`, which is not the same document as one with no `tags` key.
/// [`OpenApiRegistryImpl::build_openapi`] passes an empty slice, so every
/// caller that predates groups keeps the document it already had — an added
/// empty array would surface as a diff in every committed `OpenAPI` snapshot
/// downstream.
///
/// Once anything is declared, every tag an operation actually uses is listed:
/// the declared ones first, in the order given, then the rest in name order.
/// Leaving the others out would leave their placement to whatever a browser
/// decides on its own, which is the thing this list exists to stop.
///
/// A declared group no operation uses is still emitted — an empty group is
/// occasionally deliberate, and dropping it silently would be its own surprise
/// — but it is logged, because the overwhelmingly likely cause is a typo in a
/// name that is matched by exact string equality.
fn document_tags(
    declared: &[OpenApiTag],
    in_use: &BTreeSet<String>,
) -> Option<Vec<utoipa::openapi::Tag>> {
    if declared.is_empty() {
        return None;
    }

    let mut tags: Vec<utoipa::openapi::Tag> = Vec::with_capacity(declared.len() + in_use.len());
    for tag in declared {
        if !in_use.contains(&tag.name) {
            tracing::warn!(
                tag = %tag.name,
                "declared OpenAPI tag group matches no operation; names are matched exactly, so check spelling and case"
            );
        }
        let mut built = utoipa::openapi::Tag::new(&tag.name);
        built.description.clone_from(&tag.description);
        if let Some(docs) = &tag.external_docs {
            let mut emitted = utoipa::openapi::external_docs::ExternalDocs::new(&docs.url);
            emitted.description.clone_from(&docs.description);
            built.external_docs = Some(emitted);
        }
        if !tag.extensions.is_empty() {
            let mut emitted = utoipa::openapi::extensions::Extensions::default();
            for (key, value) in &tag.extensions {
                emitted.insert(key.clone(), value.clone());
            }
            built.extensions = Some(emitted);
        }
        tags.push(built);
    }

    let declared_names: BTreeSet<&str> = declared.iter().map(|tag| tag.name.as_str()).collect();
    tags.extend(
        in_use
            .iter()
            .filter(|name| !declared_names.contains(name.as_str()))
            .map(utoipa::openapi::Tag::new),
    );

    Some(tags)
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
///
/// # Panics
/// Panics if `T` is a `Vec<_>`. utoipa names every `Vec<_>` `Vec`, so
/// registering one as a component would collide with every other list
/// response; use
/// [`OperationBuilder::json_array_response_with_schema`](crate::api::operation_builder::OperationBuilder::json_array_response_with_schema)
/// instead. Also panics if `T`'s name is already registered with a different
/// definition (see `ensure_schema_raw`).
pub fn ensure_schema<T: utoipa::ToSchema + utoipa::PartialSchema + 'static>(
    registry: &dyn OpenApiRegistry,
) -> String {
    use utoipa::PartialSchema;

    // 1) Canonical component name for T as seen by utoipa
    let root_name = T::name().to_string();

    // utoipa's default `ToSchema::name()` drops generic arguments, so every
    // `Vec<_>` collapses to the single name `Vec` and distinct list responses
    // clobber each other (or, since M-14, panic on collision). `Vec` is a std
    // type, so `#[schema(as = "...")]` cannot rescue it — the caller has to use
    // the array-aware builder instead. Guard only this one name: a legitimate
    // user DTO could plausibly be called `Option`, `Page`, or `Map`.
    assert!(
        root_name != "Vec",
        "ensure_schema::<Vec<_>>() would register the component name `Vec`, which every other \
         Vec<T> also resolves to. Use `OperationBuilder::json_array_response_with_schema::<Item>()` \
         to emit an inline array with only the item type named."
    );

    // 2) Always insert T's own schema first (actual object, not a ref)
    //    This avoids self-referential components.
    let mut collected: SchemaCollection = vec![(root_name.clone(), <T as PartialSchema>::schema())];

    // 3) Collect and append all referenced schemas (dependencies) of T
    T::schemas(&mut collected);

    // 4) Pass to registry for insertion
    registry.ensure_schema_raw(&root_name, collected)
}

/// Build the `x-*` vendor extensions for one operation.
fn operation_vendor_extensions(
    spec: &operation_builder::OperationSpec,
) -> utoipa::openapi::extensions::Extensions {
    let mut ext = utoipa::openapi::extensions::Extensions::default();

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

    // Visibility axis (`OperationSpec.exposed`): mark routes that are
    // registered in the gateway for external access. The `GatewayProvider`
    // reads this vendor extension to select which routes to reverse-proxy.
    // The key is mirrored as a constant in `cf-gears-toolkit-gateway`.
    if spec.exposed {
        ext.insert(
            "x-toolkit-visibility".to_owned(),
            serde_json::Value::String("exposed".to_owned()),
        );
    }

    // Throttling zone bindings. The contract layer only knows zone
    // *names*; the API gateway enriches these operations with the
    // zones' numeric limits (`x-rate-limit-rps` / `x-rate-limit-burst`)
    // when it builds the final document, using the zone name emitted
    // here as the join key.
    if let Some(throttling) = spec.throttling.as_ref() {
        if let Some(zone) = throttling.rate_limit_zone.as_ref() {
            ext.insert(
                "x-throttling-rate-limit-zone".to_owned(),
                serde_json::Value::String(zone.clone()),
            );
        }
        if let Some(zone) = throttling.in_flight_limit_zone.as_ref() {
            ext.insert(
                "x-throttling-in-flight-limit-zone".to_owned(),
                serde_json::Value::String(zone.clone()),
            );
        }
    }

    ext
}

/// Implementation of `OpenAPI` registry with lock-free data structures
pub struct OpenApiRegistryImpl {
    /// Store operation specs keyed by "METHOD:path"
    pub operation_specs: DashMap<String, operation_builder::OperationSpec>,
    /// Store schema components using arc-swap for lock-free reads
    /// `BTreeMap` ensures deterministic ordering of schemas in the `OpenAPI` document
    pub components_registry: ArcSwap<BTreeMap<String, RefOr<Schema>>>,
}

impl OpenApiRegistryImpl {
    /// Create a new empty registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            operation_specs: DashMap::new(),
            components_registry: ArcSwap::from_pointee(BTreeMap::new()),
        }
    }

    /// Every tag some registered operation carries, de-duplicated and ordered.
    fn tags_in_use(&self) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for entry in &self.operation_specs {
            names.extend(entry.value().tags.iter().cloned());
        }
        names
    }

    /// Registered schemas, plus the one security scheme every document declares.
    fn document_components(&self) -> ComponentsBuilder {
        let reg = self.components_registry.load();
        let mut components = ComponentsBuilder::new();
        for (name, schema) in reg.iter() {
            components = components.schema(name.clone(), schema.clone());
        }

        components.security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        )
    }

    /// Build `OpenAPI` specification from registered operations and components.
    ///
    /// An operation declares the tag it belongs to; [`OpenApiInfo::tags`]
    /// declares the groups those tags name — the order a reader meets them in,
    /// and what each is for. Documentation browsers read that list to order and
    /// describe the groups, and fall back to the order the tags first appear in
    /// when it is absent.
    ///
    /// The list is a preference, not a whitelist: a tag an operation uses but
    /// the list omits is still emitted, after the declared ones, so its
    /// placement stays ours rather than the browser's. Leave it empty — as
    /// [`OpenApiInfo::default`] does — and the document carries no `tags` key at
    /// all.
    ///
    /// # Arguments
    /// * `info` - `OpenAPI` document metadata (title, version, description, tag groups)
    ///
    /// # Errors
    /// Returns an error if the tag groups do not pass [`validate_tags`], or if
    /// the `OpenAPI` specification cannot be built.
    // This is the registry that turns registered `OperationSpec`s into utoipa
    // operations, so it necessarily constructs them here rather than through
    // `OperationBuilder` — which is the very thing that feeds it. The lint is
    // spelled with `unknown_lints` because it only exists under
    // `cargo gears lint --dylint`.
    #[allow(
        unknown_lints,
        de0205_operation_builder,
        reason = "this function implements the registry OperationBuilder registers into"
    )]
    pub fn build_openapi(&self, info: &OpenApiInfo) -> Result<OpenApi> {
        use http::Method;

        let tags = info.tags.as_slice();
        validate_tags(tags)?;

        // Only walked when it will be read: `document_tags` returns early on an
        // empty declaration, and every caller that declares nothing — plain
        // `build_openapi` and the OoP runtime among them — would otherwise pay
        // for a full pass over the specs whose result is thrown away.
        let in_use = if tags.is_empty() {
            BTreeSet::new()
        } else {
            self.tags_in_use()
        };

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

            let ext = operation_vendor_extensions(&spec);
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
                let item_object = ObjectBuilder::new().schema_type(schema_type).build();

                let mut builder = ParameterBuilder::new()
                    .name(&p.name)
                    .parameter_in(in_)
                    .required(required)
                    .description(p.description.clone());

                if p.array {
                    // `style: form, explode: true` is the repeated-key encoding
                    // (`?tag=a&tag=b`). Spelling it out matters: the OpenAPI
                    // default for a query array is `form` with `explode: true`,
                    // but generators differ on whether they assume it, and the
                    // wire format has to be unambiguous for a client written
                    // against this spec to interoperate.
                    builder = builder
                        .style(Some(utoipa::openapi::path::ParameterStyle::Form))
                        .explode(Some(true))
                        .schema(Some(Schema::Array(
                            utoipa::openapi::schema::ArrayBuilder::new()
                                .items(item_object)
                                .build(),
                        )));
                } else {
                    builder = builder.schema(Some(Schema::Object(item_object)));
                }

                op = op.parameter(builder.build());
            }

            // Request body
            if let Some(rb) = &spec.request_body {
                let content = build_request_body_content(&rb.schema);
                let mut rbld = RequestBodyBuilder::new()
                    .description(rb.description.clone())
                    .content(rb.content_type.to_owned(), content);
                if rb.required {
                    rbld = rbld.required(Some(Required::True));
                }
                op = op.request_body(Some(rbld.build()));
            }

            // Responses
            let mut responses_by_status = BTreeMap::<u16, Response>::new();
            for r in &spec.responses {
                let response = responses_by_status
                    .entry(r.status)
                    .or_insert_with(|| Response::new(&r.description));
                // Preserve the historical last-declaration-wins behavior for
                // the status-level description while merging media types.
                response.description.clone_from(&r.description);

                // Body-less response (e.g. 204 No Content) is signalled by an
                // empty `content_type`. Emit just `description` — attaching a
                // `content` block would make code-generators expect a body.
                if !r.content_type.is_empty() {
                    // Streaming media types are json-like here too: their
                    // declared schema is the *item* type, so it must render as
                    // a `$ref` rather than as an opaque string blob. Derived
                    // from `StreamFraming` rather than hard-coded, so a new
                    // framing variant is covered automatically instead of
                    // silently falling through to the opaque-string branch.
                    let is_json_like = r.content_type == "application/json"
                        || r.content_type == problem::APPLICATION_PROBLEM_JSON
                        || StreamFraming::is_stream_media_type(r.content_type);
                    let content = if is_json_like {
                        // Manually build content to preserve the correct content type.
                        ContentBuilder::new()
                            .schema(Some(build_response_schema(r.schema.as_ref())))
                            .build()
                    } else {
                        let schema = Schema::Object(
                            ObjectBuilder::new()
                                .schema_type(SchemaType::Type(
                                    utoipa::openapi::schema::Type::String,
                                ))
                                .format(Some(SchemaFormat::Custom(r.content_type.into())))
                                .build(),
                        );
                        ContentBuilder::new().schema(Some(schema)).build()
                    };
                    response.content.insert(r.content_type.to_owned(), content);
                }

                for header in &r.headers {
                    let schema_type = match header.header_type {
                        operation_builder::ResponseHeaderType::String => {
                            SchemaType::Type(utoipa::openapi::schema::Type::String)
                        }
                        operation_builder::ResponseHeaderType::Integer => {
                            SchemaType::Type(utoipa::openapi::schema::Type::Integer)
                        }
                        operation_builder::ResponseHeaderType::Boolean => {
                            SchemaType::Type(utoipa::openapi::schema::Type::Boolean)
                        }
                    };
                    let declared = HeaderBuilder::new()
                        .description(header.description.clone())
                        .schema(ObjectBuilder::new().schema_type(schema_type).build())
                        .build();
                    response.headers.insert(header.name.clone(), declared);
                }
            }
            let responses = ResponsesBuilder::new().responses_from_iter(
                responses_by_status
                    .into_iter()
                    .map(|(status, response)| (status.to_string(), response)),
            );
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
        let components = self.document_components();

        // 3) Info & final OpenAPI doc
        let openapi_info = InfoBuilder::new()
            .title(&info.title)
            .version(&info.version)
            .description(info.description.clone())
            .build();

        let servers = (!info.servers.is_empty()).then(|| {
            info.servers
                .iter()
                .cloned()
                .map(Server::new)
                .collect::<Vec<_>>()
        });

        let mut openapi = OpenApiBuilder::new()
            .info(openapi_info)
            .servers(servers)
            .paths(paths.build())
            .components(Some(components.build()))
            .tags(document_tags(tags, &in_use))
            .build();

        // Document-level vendor extension: this spec is generated from Rust
        // contract traits + `schemars`/`utoipa` schemas, which cover a deliberate
        // subset of REST (ADR-0002). It is the MINIMUM conformance contract —
        // remote services may expose strictly more, never less. Downstream
        // directory validators key their superset semantics off this marker.
        let mut ext = utoipa::openapi::extensions::Extensions::default();
        ext.insert(
            "x-toolkit-spec-scope".to_owned(),
            serde_json::json!("minimum-conformance"),
        );
        openapi.extensions = Some(ext);

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
        // Surface duplicate (method, path) registrations — e.g. a generated
        // `register_<trait>_routes()` colliding with a hand-written route, or two
        // SDKs registering the same path (M-13). Silently overwriting the earlier
        // operation's OpenAPI spec is a hard-to-diagnose drift; the axum router
        // itself will also panic on the duplicate route at bind time.
        if let Some(prev) = self
            .operation_specs
            .insert(operation_key.clone(), spec.clone())
            && prev.handler_id != spec.handler_id
        {
            tracing::warn!(
                operation_key = %operation_key,
                previous_handler = %prev.handler_id,
                new_handler = %spec.handler_id,
                "duplicate OpenAPI operation registration; the earlier operation spec was \
                 overwritten - generated and manual routes must not share a (method, path)"
            );
        }

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
            // Conflict policy: identical → no-op; different → HARD ERROR. Two
            // distinct types resolving to the same schema name (utoipa uses the
            // bare type ident by default) would otherwise silently clobber each
            // other in `components.schemas`, producing a spec where one type
            // masquerades under another's name — a hard-to-diagnose wire
            // mismatch. Fail fast at registration instead.
            if let Some(existing) = reg.get(&name) {
                let a = serde_json::to_value(existing).ok();
                let b = serde_json::to_value(&schema).ok();
                if a == b {
                    continue; // Skip identical schemas
                }
                panic!(
                    "OpenAPI schema name collision: `{name}` is registered with two different \
                     definitions. Two distinct types share the same schema name — rename one, or \
                     give it a distinct `#[schema(as = \"...\")]` alias. For a `Vec<T>` response \
                     use `OperationBuilder::json_array_response_with_schema::<T>()`, which emits \
                     an inline array instead of registering a component named `Vec`. \
                     existing={}, new={}",
                    a.map(|v| truncate_json(&v)).unwrap_or_default(),
                    b.map(|v| truncate_json(&v)).unwrap_or_default(),
                );
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

/// Render a JSON value to a compact, length-bounded string for diagnostics.
/// Bounded by character count (char-boundary safe) rather than bytes.
fn truncate_json(v: &serde_json::Value) -> String {
    const MAX: usize = 200;
    let s = v.to_string();
    if s.chars().count() > MAX {
        let mut out: String = s.chars().take(MAX).collect();
        out.push('\u{2026}');
        out
    } else {
        s
    }
}

/// Build the `OpenAPI` content object for a request body schema variant.
fn build_request_body_content(
    schema: &operation_builder::RequestBodySchema,
) -> utoipa::openapi::content::Content {
    match schema {
        operation_builder::RequestBodySchema::Ref { schema_name } => ContentBuilder::new()
            .schema(Some(RefOr::Ref(Ref::from_schema_name(schema_name.clone()))))
            .build(),
        operation_builder::RequestBodySchema::MultipartFile { field_name } => {
            // Build multipart/form-data schema with a single binary file field
            // type: object
            // properties:
            //   {field_name}: { type: string, format: binary }
            // required: [ field_name ]
            let file_schema = Schema::Object(
                ObjectBuilder::new()
                    .schema_type(SchemaType::Type(utoipa::openapi::schema::Type::String))
                    .format(Some(SchemaFormat::Custom("binary".into())))
                    .build(),
            );
            let obj = ObjectBuilder::new()
                .property(field_name.clone(), file_schema)
                .required(field_name.clone());
            ContentBuilder::new()
                .schema(Some(Schema::Object(obj.build())))
                .build()
        }
        operation_builder::RequestBodySchema::Binary => {
            // Represent raw binary body as type string, format binary.
            // This is used for application/octet-stream and similar raw binary content.
            let schema = Schema::Object(
                ObjectBuilder::new()
                    .schema_type(SchemaType::Type(utoipa::openapi::schema::Type::String))
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
    }
}

/// Build the response body schema for a [`operation_builder::ResponseSchema`].
///
/// `None` — a JSON response with no declared schema — yields a free-form
/// object, preserving the previous behaviour.
fn build_response_schema(schema: Option<&operation_builder::ResponseSchema>) -> RefOr<Schema> {
    match schema {
        Some(operation_builder::ResponseSchema::Ref { schema_name }) => {
            RefOr::Ref(Ref::from_schema_name(schema_name.clone()))
        }
        // Top-level arrays are emitted INLINE, with only the item type
        // registered as a named component. Naming the array itself would use
        // utoipa's `Vec` (generics are stripped from `ToSchema::name()`), so
        // every list endpoint in the process would fight over one component.
        Some(operation_builder::ResponseSchema::Array { items_schema_name }) => {
            RefOr::T(Schema::Array(
                ArrayBuilder::new()
                    .items(RefOr::Ref(Ref::from_schema_name(items_schema_name.clone())))
                    .build(),
            ))
        }
        None => RefOr::T(Schema::Object(ObjectBuilder::new().build())),
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
mod tests {
    use super::*;
    use crate::api::operation_builder::{
        OperationSpec, ParamLocation, ParamSpec, ResponseHeaderSpec, ResponseHeaderType,
        ResponseSchema, ResponseSpec, VendorExtensions,
    };
    use http::Method;

    /// Minimal `OperationSpec` carrying a single 200 response with `schema`.
    fn spec_with_response(
        path: &str,
        handler: &str,
        schema: Option<ResponseSchema>,
    ) -> OperationSpec {
        OperationSpec {
            method: Method::GET,
            path: path.to_owned(),
            operation_id: Some(handler.to_owned()),
            summary: None,
            description: None,
            tags: vec![],
            params: vec![],
            request_body: None,
            responses: vec![ResponseSpec {
                status: 200,
                content_type: "application/json",
                description: "OK".to_owned(),
                schema,
                headers: vec![],
            }],
            handler_id: handler.to_owned(),
            authenticated: false,
            exposed: false,
            throttling: None,
            allowed_request_content_types: None,
            vendor_extensions: VendorExtensions::default(),
            license_requirement: None,
        }
    }

    /// The 200 response schema for `path`, as JSON.
    fn response_schema_json(doc: &serde_json::Value, path: &str) -> serde_json::Value {
        doc["paths"][path]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            .clone()
    }

    fn test_info() -> OpenApiInfo {
        OpenApiInfo {
            title: "T".to_owned(),
            version: "1".to_owned(),
            description: None,
            servers: Vec::new(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn throttling_zone_names_are_emitted_as_vendor_extensions() {
        use crate::api::operation_builder::ThrottlingSpec;

        let registry = OpenApiRegistryImpl::new();
        let mut spec = spec_with_response("/throttled", "throttled_op", None);
        spec.throttling = Some(ThrottlingSpec {
            rate_limit_zone: Some("rl_zone".to_owned()),
            in_flight_limit_zone: Some("ifl_zone".to_owned()),
            require_security_context: false,
            dry_run: false,
        });
        registry.register_operation(&spec);
        // An operation without throttling must not carry the extensions.
        registry.register_operation(&spec_with_response("/plain", "plain_op", None));

        let doc = registry.build_openapi(&test_info()).unwrap();
        let json = serde_json::to_value(&doc).unwrap();

        let throttled = &json["paths"]["/throttled"]["get"];
        assert_eq!(
            throttled["x-throttling-rate-limit-zone"],
            serde_json::json!("rl_zone")
        );
        assert_eq!(
            throttled["x-throttling-in-flight-limit-zone"],
            serde_json::json!("ifl_zone")
        );

        let plain = &json["paths"]["/plain"]["get"];
        assert!(plain.get("x-throttling-rate-limit-zone").is_none());
        assert!(plain.get("x-throttling-in-flight-limit-zone").is_none());
    }

    #[test]
    fn test_registry_creation() {
        let registry = OpenApiRegistryImpl::new();
        assert_eq!(registry.operation_specs.len(), 0);
        assert_eq!(registry.components_registry.load().len(), 0);
    }

    #[test]
    fn test_register_operation() {
        let registry = OpenApiRegistryImpl::new();
        let spec = OperationSpec {
            method: Method::GET,
            path: "/test".to_owned(),
            operation_id: Some("test_op".to_owned()),
            summary: Some("Test operation".to_owned()),
            description: None,
            tags: vec![],
            params: vec![],
            request_body: None,
            responses: vec![ResponseSpec {
                status: 200,
                content_type: "application/json",
                description: "Success".to_owned(),
                schema: None,
                headers: vec![],
            }],
            handler_id: "get_test".to_owned(),
            authenticated: false,
            exposed: false,
            throttling: None,
            allowed_request_content_types: None,
            vendor_extensions: VendorExtensions::default(),
            license_requirement: None,
        };

        registry.register_operation(&spec);
        assert_eq!(registry.operation_specs.len(), 1);
    }

    #[test]
    fn response_headers_are_emitted_with_their_declared_types() {
        let registry = OpenApiRegistryImpl::new();
        let mut spec = spec_with_response("/submit", "submit", None);
        spec.responses[0].headers = vec![
            ResponseHeaderSpec::new(
                "Location",
                "Operation resource URI",
                ResponseHeaderType::String,
            ),
            ResponseHeaderSpec::new(
                "Retry-After",
                "Retry delay in seconds",
                ResponseHeaderType::Integer,
            ),
            ResponseHeaderSpec::new(
                "Idempotency-Replayed",
                "Whether this is a replay",
                ResponseHeaderType::Boolean,
            ),
        ];
        registry.register_operation(&spec);

        let doc = registry.build_openapi(&test_info()).unwrap();
        let json = serde_json::to_value(doc).unwrap();
        let headers = &json["paths"]["/submit"]["get"]["responses"]["200"]["headers"];
        assert_eq!(headers["Location"]["schema"]["type"], "string");
        assert_eq!(headers["Location"]["description"], "Operation resource URI");
        assert_eq!(headers["Retry-After"]["schema"]["type"], "integer");
        assert_eq!(headers["Idempotency-Replayed"]["schema"]["type"], "boolean");
    }

    #[test]
    fn response_content_types_with_the_same_status_are_combined() {
        let registry = OpenApiRegistryImpl::new();
        let mut spec = spec_with_response("/document", "document", None);
        spec.responses[0].content_type = "text/plain";
        spec.responses[0].description = "Plain document".to_owned();
        spec.responses[0].headers = vec![ResponseHeaderSpec::new(
            "X-Plain-Document",
            "Whether plain text is available",
            ResponseHeaderType::Boolean,
        )];
        spec.responses.push(
            ResponseSpec::new(
                http::StatusCode::OK.as_u16(),
                "text/html",
                "HTML document",
                None,
            )
            .with_headers([ResponseHeaderSpec::new(
                "X-HTML-Document",
                "Whether HTML is available",
                ResponseHeaderType::Boolean,
            )]),
        );
        registry.register_operation(&spec);

        let doc = registry.build_openapi(&test_info()).unwrap();
        let json = serde_json::to_value(doc).unwrap();
        let response = &json["paths"]["/document"]["get"]["responses"]["200"];
        assert_eq!(response["description"], "HTML document");
        assert_eq!(
            response["content"]["text/plain"]["schema"]["format"],
            "text/plain"
        );
        assert_eq!(
            response["content"]["text/html"]["schema"]["format"],
            "text/html"
        );
        assert_eq!(
            response["headers"]["X-Plain-Document"]["schema"]["type"],
            "boolean"
        );
        assert_eq!(
            response["headers"]["X-HTML-Document"]["schema"]["type"],
            "boolean"
        );
    }

    #[test]
    fn bodyless_response_can_declare_headers_without_content() {
        let registry = OpenApiRegistryImpl::new();
        let mut spec = spec_with_response("/jobs", "jobs", None);
        spec.responses[0].content_type = "";
        spec.responses[0].schema = None;
        spec.responses[0].headers = vec![ResponseHeaderSpec::without_description(
            "Retry-After",
            ResponseHeaderType::Integer,
        )];
        registry.register_operation(&spec);

        let doc = registry.build_openapi(&test_info()).unwrap();
        let json = serde_json::to_value(doc).unwrap();
        let response = &json["paths"]["/jobs"]["get"]["responses"]["200"];
        assert!(response.get("content").is_none());
        assert_eq!(
            response["headers"]["Retry-After"]["schema"]["type"],
            "integer"
        );
        assert!(
            response["headers"]["Retry-After"]
                .get("description")
                .is_none()
        );
    }

    #[test]
    fn test_build_empty_openapi() {
        let registry = OpenApiRegistryImpl::new();
        let info = OpenApiInfo {
            title: "Test API".to_owned(),
            version: "1.0.0".to_owned(),
            description: Some("Test API Description".to_owned()),
            servers: Vec::new(),
            tags: Vec::new(),
        };
        let doc = registry.build_openapi(&info).unwrap();
        let json = serde_json::to_value(&doc).unwrap();

        // Verify it's valid OpenAPI document structure
        assert!(json.get("openapi").is_some());
        assert!(json.get("info").is_some());
        assert!(json.get("paths").is_some());

        // Verify info section
        let openapi_info = json.get("info").unwrap();
        assert_eq!(openapi_info.get("title").unwrap(), "Test API");
        assert_eq!(openapi_info.get("version").unwrap(), "1.0.0");
        assert_eq!(
            openapi_info.get("description").unwrap(),
            "Test API Description"
        );
    }

    /// A document nobody gave groups to carries no `tags` key at all.
    ///
    /// Not `tags: []`. `build_openapi` is unchanged for every caller that had
    /// it before groups existed, and "unchanged" has to mean the bytes too —
    /// an added empty array is a diff in every committed snapshot downstream.
    #[test]
    fn no_declared_groups_means_no_tags_key() {
        let registry = OpenApiRegistryImpl::new();
        let doc = registry.build_openapi(&OpenApiInfo::default()).unwrap();
        let json = serde_json::to_value(&doc).unwrap();

        assert!(
            json.get("tags").is_none(),
            "an undeclared tag list must not appear in the document: {json}"
        );
    }

    /// An operation carrying `tag`, enough of one to register.
    fn tagged_operation(path: &str, tag: &str) -> OperationSpec {
        OperationSpec {
            method: Method::GET,
            path: path.to_owned(),
            operation_id: Some(format!("get_{tag}")),
            summary: Some("Something".to_owned()),
            description: Some("Something worth a sentence.".to_owned()),
            tags: vec![tag.to_owned()],
            params: vec![],
            request_body: None,
            responses: vec![ResponseSpec {
                status: 200,
                content_type: "application/json",
                description: "OK".to_owned(),
                schema: None,
                headers: vec![],
            }],
            handler_id: format!("handler_{tag}"),
            authenticated: false,
            exposed: false,
            throttling: None,
            allowed_request_content_types: None,
            vendor_extensions: VendorExtensions::default(),
            license_requirement: None,
        }
    }

    /// Declared groups reach the document in the order they were declared, and
    /// a tag nobody declared lands after them rather than wherever.
    ///
    /// The order is the whole point: a browser presents the groups in document
    /// order and falls back to first-appearance order when the list is absent,
    /// which in an assembly means whatever the path alphabet produced. That
    /// fallback is also why the undeclared tag has to be emitted rather than
    /// left out — left out, its placement would be the browser's to choose.
    #[test]
    fn declared_groups_keep_their_order_and_undeclared_ones_follow() {
        let registry = OpenApiRegistryImpl::new();
        registry.register_operation(&tagged_operation("/zulu", "Zulu"));
        registry.register_operation(&tagged_operation("/alpha", "Alpha"));
        // Registered in an order that is not their name order, so the tail
        // being sorted is what the assertion below proves, not a coincidence.
        registry.register_operation(&tagged_operation("/stray", "Yankee"));
        registry.register_operation(&tagged_operation("/other", "Charlie"));
        registry.register_operation(&tagged_operation("/third", "Mike"));

        let groups = [
            OpenApiTag::new("Zulu")
                .unwrap()
                .with_description("Last alphabetically, first on purpose.")
                .unwrap(),
            OpenApiTag::new("Alpha").unwrap(),
        ];

        let info = OpenApiInfo {
            tags: groups.to_vec(),
            ..OpenApiInfo::default()
        };
        let doc = registry.build_openapi(&info).unwrap();
        let json = serde_json::to_value(&doc).unwrap();
        let tags = json
            .get("tags")
            .expect("declared groups reach the document");

        let names: Vec<&str> = tags
            .as_array()
            .unwrap()
            .iter()
            .map(|tag| tag.get("name").unwrap().as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["Zulu", "Alpha", "Charlie", "Mike", "Yankee"],
            "declared order first, then the rest in name order"
        );

        // The feature must not touch an operation's own tag attribution: the
        // top-level list says how groups are ordered, never which group an
        // operation is in.
        let stray_tags = json
            .get("paths")
            .and_then(|paths| paths.get("/stray"))
            .and_then(|item| item.get("get"))
            .and_then(|op| op.get("tags"))
            .expect("the undeclared operation still carries its own tag");
        assert_eq!(
            stray_tags.as_array().map(Vec::as_slice),
            Some([serde_json::json!("Yankee")].as_slice()),
            "an omitted group is not hidden from its own operation: {stray_tags}"
        );

        assert_eq!(
            tags[0].get("description").unwrap(),
            "Last alphabetically, first on purpose."
        );
        assert!(
            tags[1].get("description").is_none(),
            "a group with nothing to say carries no description key: {tags}"
        );
    }

    /// Two groups with one name make an invalid document, so it is not built.
    #[test]
    fn duplicate_group_names_are_rejected() {
        let registry = OpenApiRegistryImpl::new();
        let groups = [
            OpenApiTag::new("Orders").unwrap(),
            OpenApiTag::new("Orders").unwrap(),
        ];

        let info = OpenApiInfo {
            tags: groups.to_vec(),
            ..OpenApiInfo::default()
        };
        let Err(error) = registry.build_openapi(&info) else {
            panic!("a duplicate group name must not reach a document");
        };

        let message = error.to_string();
        assert!(
            message.contains("Orders") && message.contains("#1"),
            "the error names the offending group and where it is: {message}"
        );
    }

    /// Matching is exact, which is the headline promise of the whole feature.
    ///
    /// Without this, a later switch to trimmed or case-folded matching would
    /// pass every other test while quietly changing what an operator's config
    /// means. It is also the only test that exercises the unmatched-group
    /// warning.
    #[test]
    fn group_names_match_operation_tags_exactly() {
        let registry = OpenApiRegistryImpl::new();
        registry.register_operation(&tagged_operation("/orders", "orders"));

        let info = OpenApiInfo {
            tags: vec![OpenApiTag::new("Orders").unwrap()],
            ..OpenApiInfo::default()
        };
        let doc = registry.build_openapi(&info).unwrap();
        let json = serde_json::to_value(&doc).unwrap();

        let names: Vec<&str> = json
            .get("tags")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|tag| tag.get("name").unwrap().as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["Orders", "orders"],
            "case differs, so these are two groups: the declared one is empty \
             and the real one is appended as undeclared"
        );
    }

    /// A name that only looks like the operation's tag is refused, not accepted
    /// as a second, empty group.
    #[test]
    fn a_padded_group_name_is_rejected() {
        assert!(
            OpenApiTag::new(" Orders").is_err(),
            "leading whitespace survives exact matching and would make a ghost group"
        );
        assert!(OpenApiTag::new("Orders ").is_err());
        assert!(OpenApiTag::new("Orders").is_ok());
    }

    /// The name cap holds at the boundary rather than somewhere near it.
    #[test]
    fn the_group_name_length_cap_is_enforced_at_the_boundary() {
        let longest = "n".repeat(MAX_TAG_NAME_LEN);
        assert!(OpenApiTag::new(longest).is_ok(), "the cap is inclusive");

        let over = "n".repeat(MAX_TAG_NAME_LEN + 1);
        assert!(OpenApiTag::new(over).is_err());
    }

    /// So does the description cap, and `with_description` reports it.
    #[test]
    fn the_description_length_cap_is_enforced_at_the_boundary() {
        let longest = "d".repeat(MAX_TAG_DESCRIPTION_LEN);
        assert!(
            OpenApiTag::new("Orders")
                .unwrap()
                .with_description(longest)
                .is_ok()
        );

        let over = "d".repeat(MAX_TAG_DESCRIPTION_LEN + 1);
        let failed = OpenApiTag::new("Orders").unwrap().with_description(over);
        assert!(
            failed.is_err(),
            "with_description has a documented error path"
        );
    }

    /// More groups than a document may declare is refused.
    #[test]
    fn too_many_declared_groups_are_rejected() {
        let at_cap: Vec<OpenApiTag> = (0..MAX_DECLARED_TAGS)
            .map(|i| OpenApiTag::new(format!("Group{i}")).unwrap())
            .collect();
        assert!(validate_tags(&at_cap).is_ok(), "the cap is inclusive");

        let over: Vec<OpenApiTag> = (0..=MAX_DECLARED_TAGS)
            .map(|i| OpenApiTag::new(format!("Group{i}")).unwrap())
            .collect();
        assert!(validate_tags(&over).is_err());
    }

    /// Control characters never reach the served document or the startup log.
    ///
    /// Both strings are copied verbatim into `/openapi.json`, which is served
    /// anonymously, and rendered on the unauthenticated `/docs` page. A newline
    /// in a name also forges a line in the log that reports an unmatched group.
    #[test]
    fn control_and_direction_override_characters_are_rejected() {
        assert!(OpenApiTag::new("Orders\nWARN: forged").is_err());
        assert!(OpenApiTag::new("Orders\u{202E}sredrO").is_err());
        assert!(OpenApiTag::new("Orders\u{1B}[31m").is_err());

        let tag = OpenApiTag::new("Orders").unwrap();
        assert!(
            tag.clone()
                .with_description("Two\nlines, and a\ttab.")
                .is_ok(),
            "a description is CommonMark, so line breaks are content"
        );
        assert!(tag.with_description("Bell\u{7}here").is_err());
    }

    /// U+061C reorders text and is not a control character, so it needs naming.
    #[test]
    fn the_arabic_letter_mark_is_rejected_like_the_other_overrides() {
        assert!(
            !'\u{061C}'.is_control(),
            "if this ever becomes a control character the explicit entry is redundant"
        );
        assert!(OpenApiTag::new("Orders\u{061C}").is_err());
        assert!(
            OpenApiTag::new("Orders")
                .unwrap()
                .with_description("Fine\u{061C}print")
                .is_err()
        );
    }

    /// A group can carry `externalDocs` and `x-*`, and they reach the document.
    #[test]
    fn external_docs_and_extensions_reach_the_document() {
        let registry = OpenApiRegistryImpl::new();
        registry.register_operation(&tagged_operation("/orders", "Orders"));

        let mut declared = OpenApiTag::new("Orders").unwrap();
        declared.external_docs = Some(OpenApiExternalDocs {
            url: "https://example.test/orders".to_owned(),
            description: Some("The long version.".to_owned()),
        });
        declared
            .extensions
            .insert("x-displayName".to_owned(), serde_json::json!("Orders"));

        let info = OpenApiInfo {
            tags: vec![declared],
            ..OpenApiInfo::default()
        };
        let json = serde_json::to_value(registry.build_openapi(&info).unwrap()).unwrap();
        let tag = &json.get("tags").unwrap()[0];

        assert_eq!(
            tag.get("externalDocs").unwrap().get("url").unwrap(),
            "https://example.test/orders"
        );
        assert_eq!(
            tag.get("externalDocs").unwrap().get("description").unwrap(),
            "The long version."
        );
        assert_eq!(
            tag.get("x-displayName").unwrap(),
            "Orders",
            "an extension is emitted as a member of the tag: {tag}"
        );
    }

    /// A member that is not an extension is refused rather than invented.
    #[test]
    fn an_extension_without_the_x_prefix_is_rejected() {
        let mut tag = OpenApiTag::new("Orders").unwrap();
        tag.extensions
            .insert("displayName".to_owned(), serde_json::json!("Orders"));

        assert!(
            validate_tags(&[tag]).is_err(),
            "every non-`x-` member of an OpenAPI object belongs to the specification"
        );
    }

    /// The error points at the entry, since a bad name has nothing to call it by.
    #[test]
    fn a_rejected_group_is_identified_by_position() {
        let tags = [
            OpenApiTag::new("Fine").unwrap(),
            OpenApiTag {
                name: String::new(),
                ..OpenApiTag::default()
            },
        ];

        let Err(error) = validate_tags(&tags) else {
            panic!("an empty name is not a group");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("#1"),
            "an operator with twenty groups needs the index: {message}"
        );
    }

    /// A blank name groups nothing and matches nothing; it is a typo, not a group.
    #[test]
    fn a_blank_group_name_is_rejected() {
        assert!(OpenApiTag::new("   ").is_err());
        assert!(
            validate_tags(&[OpenApiTag {
                name: String::new(),
                ..OpenApiTag::default()
            }])
            .is_err(),
            "a list that arrived from config is checked too, not just the constructor"
        );
    }

    #[test]
    fn test_build_openapi_with_operation() {
        let registry = OpenApiRegistryImpl::new();
        let spec = OperationSpec {
            method: Method::GET,
            path: "/users/{id}".to_owned(),
            operation_id: Some("get_user".to_owned()),
            summary: Some("Get user by ID".to_owned()),
            description: Some("Retrieves a user by their ID".to_owned()),
            tags: vec!["users".to_owned()],
            params: vec![ParamSpec {
                name: "id".to_owned(),
                location: ParamLocation::Path,
                required: true,
                description: Some("User ID".to_owned()),
                param_type: "string".to_owned(),
                array: false,
            }],
            request_body: None,
            responses: vec![ResponseSpec {
                status: 200,
                content_type: "application/json",
                description: "User found".to_owned(),
                schema: None,
                headers: vec![],
            }],
            handler_id: "get_users_id".to_owned(),
            authenticated: false,
            exposed: false,
            throttling: None,
            allowed_request_content_types: None,
            vendor_extensions: VendorExtensions::default(),
            license_requirement: None,
        };

        registry.register_operation(&spec);
        let info = OpenApiInfo::default();
        let doc = registry.build_openapi(&info).unwrap();
        let json = serde_json::to_value(&doc).unwrap();

        // Verify path exists
        let paths = json.get("paths").unwrap();
        assert!(paths.get("/users/{id}").is_some());

        // Verify operation details
        let get_op = paths.get("/users/{id}").unwrap().get("get").unwrap();
        assert_eq!(get_op.get("operationId").unwrap(), "get_user");
        assert_eq!(get_op.get("summary").unwrap(), "Get user by ID");
    }

    #[test]
    fn test_ensure_schema_raw() {
        let registry = OpenApiRegistryImpl::new();
        let schema = Schema::Object(ObjectBuilder::new().build());
        let schemas = vec![("TestSchema".to_owned(), RefOr::T(schema))];

        let name = registry.ensure_schema_raw("TestSchema", schemas);
        assert_eq!(name, "TestSchema");
        assert_eq!(registry.components_registry.load().len(), 1);
    }

    #[test]
    fn test_build_openapi_with_binary_request() {
        use crate::api::operation_builder::RequestBodySchema;

        let registry = OpenApiRegistryImpl::new();
        let spec = OperationSpec {
            method: Method::POST,
            path: "/files/v1/upload".to_owned(),
            operation_id: Some("upload_file".to_owned()),
            summary: Some("Upload a file".to_owned()),
            description: Some("Upload raw binary file".to_owned()),
            tags: vec!["upload".to_owned()],
            params: vec![],
            request_body: Some(crate::api::operation_builder::RequestBodySpec {
                content_type: "application/octet-stream",
                description: Some("Raw file bytes".to_owned()),
                schema: RequestBodySchema::Binary,
                required: true,
            }),
            responses: vec![ResponseSpec {
                status: 200,
                content_type: "application/json",
                description: "Upload successful".to_owned(),
                schema: None,
                headers: vec![],
            }],
            handler_id: "post_upload".to_owned(),
            authenticated: false,
            exposed: false,
            throttling: None,
            allowed_request_content_types: Some(vec!["application/octet-stream"]),
            vendor_extensions: VendorExtensions::default(),
            license_requirement: None,
        };

        registry.register_operation(&spec);
        let info = OpenApiInfo::default();
        let doc = registry.build_openapi(&info).unwrap();
        let json = serde_json::to_value(&doc).unwrap();

        // Verify path exists
        let paths = json.get("paths").unwrap();
        assert!(paths.get("/files/v1/upload").is_some());

        // Verify request body has application/octet-stream with binary schema
        let post_op = paths.get("/files/v1/upload").unwrap().get("post").unwrap();
        let request_body = post_op.get("requestBody").unwrap();
        let content = request_body.get("content").unwrap();
        let octet_stream = content
            .get("application/octet-stream")
            .expect("application/octet-stream content type should exist");

        // Verify schema is type: string, format: binary
        let schema = octet_stream.get("schema").unwrap();
        assert_eq!(schema.get("type").unwrap(), "string");
        assert_eq!(schema.get("format").unwrap(), "binary");

        // Verify required flag
        assert_eq!(request_body.get("required").unwrap(), true);
    }

    #[test]
    fn test_build_openapi_with_pagination() {
        let registry = OpenApiRegistryImpl::new();

        let mut filter: operation_builder::ODataPagination<
            std::collections::BTreeMap<String, Vec<String>>,
        > = operation_builder::ODataPagination::default();
        filter.allowed_fields.insert(
            "name".to_owned(),
            vec!["eq", "ne", "contains", "startswith", "endswith", "in"]
                .into_iter()
                .map(String::from)
                .collect(),
        );
        filter.allowed_fields.insert(
            "age".to_owned(),
            vec!["eq", "ne", "gt", "ge", "lt", "le", "in"]
                .into_iter()
                .map(String::from)
                .collect(),
        );

        let mut order_by: operation_builder::ODataPagination<Vec<String>> =
            operation_builder::ODataPagination::default();
        order_by.allowed_fields.push("name asc".to_owned());
        order_by.allowed_fields.push("name desc".to_owned());
        order_by.allowed_fields.push("age asc".to_owned());
        order_by.allowed_fields.push("age desc".to_owned());

        let mut spec = OperationSpec {
            method: Method::GET,
            path: "/test".to_owned(),
            operation_id: Some("test_op".to_owned()),
            summary: Some("Test".to_owned()),
            description: None,
            tags: vec![],
            params: vec![],
            request_body: None,
            responses: vec![ResponseSpec {
                status: 200,
                content_type: "application/json",
                description: "OK".to_owned(),
                schema: None,
                headers: vec![],
            }],
            handler_id: "get_test".to_owned(),
            authenticated: false,
            exposed: false,
            throttling: None,
            allowed_request_content_types: None,
            vendor_extensions: VendorExtensions::default(),
            license_requirement: None,
        };
        spec.vendor_extensions.x_odata_filter = Some(filter);
        spec.vendor_extensions.x_odata_orderby = Some(order_by);

        registry.register_operation(&spec);
        let info = OpenApiInfo::default();
        let doc = registry.build_openapi(&info).unwrap();
        let json = serde_json::to_value(&doc).unwrap();

        let paths = json.get("paths").unwrap();
        let op = paths.get("/test").unwrap().get("get").unwrap();

        let filter_ext = op
            .get("x-odata-filter")
            .expect("x-odata-filter should be present");

        let allowed_fields = filter_ext.get("allowedFields").unwrap();
        assert!(allowed_fields.get("name").is_some());
        assert!(allowed_fields.get("age").is_some());

        let order_ext = op
            .get("x-odata-orderby")
            .expect("x-odata-orderby should be present");

        let allowed_order = order_ext.get("allowedFields").unwrap().as_array().unwrap();
        assert!(allowed_order.iter().any(|v| v.as_str() == Some("name asc")));
        assert!(allowed_order.iter().any(|v| v.as_str() == Some("age desc")));
    }

    #[test]
    fn test_public_operation_emits_visibility_extension() {
        let registry = OpenApiRegistryImpl::new();
        let public = OperationSpec {
            method: Method::GET,
            path: "/calc/v1/ping".to_owned(),
            operation_id: Some("ping".to_owned()),
            summary: Some("Ping".to_owned()),
            description: None,
            tags: vec![],
            params: vec![],
            request_body: None,
            responses: vec![ResponseSpec {
                status: 200,
                content_type: "application/json",
                description: "OK".to_owned(),
                schema: None,
                headers: vec![],
            }],
            handler_id: "get_ping".to_owned(),
            authenticated: false,
            exposed: true,
            throttling: None,
            allowed_request_content_types: None,
            vendor_extensions: VendorExtensions::default(),
            license_requirement: None,
        };
        // A second, internal operation must NOT carry the extension.
        let mut internal = public.clone();
        internal.path = "/calc/v1/internal".to_owned();
        internal.handler_id = "get_internal".to_owned();
        internal.operation_id = Some("internal".to_owned());
        internal.exposed = false;

        registry.register_operation(&public);
        registry.register_operation(&internal);
        let doc = registry.build_openapi(&OpenApiInfo::default()).unwrap();
        let json = serde_json::to_value(&doc).unwrap();
        let paths = json.get("paths").unwrap();

        let public_op = paths.get("/calc/v1/ping").unwrap().get("get").unwrap();
        assert_eq!(
            public_op
                .get("x-toolkit-visibility")
                .and_then(|v| v.as_str()),
            Some("exposed"),
            "public operation must advertise the gateway visibility extension"
        );

        let internal_op = paths.get("/calc/v1/internal").unwrap().get("get").unwrap();
        assert!(
            internal_op.get("x-toolkit-visibility").is_none(),
            "non-public operation must not carry the visibility extension"
        );
    }

    /// Helper: build a minimal `OpenAPI` doc with the given component schemas.
    fn build_test_openapi(schemas: BTreeMap<String, RefOr<Schema>>) -> OpenApi {
        let mut components = ComponentsBuilder::new();
        for (name, schema) in schemas {
            components = components.schema(name, schema);
        }
        OpenApiBuilder::new()
            .components(Some(components.build()))
            .build()
    }

    #[test]
    fn test_dangling_refs_detects_missing_in_components() {
        let mut schemas: BTreeMap<String, RefOr<Schema>> = BTreeMap::new();
        // Register "Foo" with a $ref to "Bar" which is NOT registered
        let foo_schema = serde_json::from_value::<Schema>(serde_json::json!({
            "type": "object",
            "properties": {
                "bar": { "$ref": "#/components/schemas/Bar" }
            }
        }))
        .unwrap();
        schemas.insert("Foo".to_owned(), RefOr::T(foo_schema));

        let openapi = build_test_openapi(schemas);
        let dangling = collect_all_dangling_refs_in_openapi(&openapi);
        assert_eq!(dangling, vec!["Bar".to_owned()]);
    }

    #[test]
    fn test_dangling_refs_no_false_positives() {
        let mut schemas: BTreeMap<String, RefOr<Schema>> = BTreeMap::new();
        // Register "Bar"
        let bar_schema = Schema::Object(ObjectBuilder::new().build());
        schemas.insert("Bar".to_owned(), RefOr::T(bar_schema));

        // Register "Foo" referencing "Bar"
        let foo_schema = serde_json::from_value::<Schema>(serde_json::json!({
            "type": "object",
            "properties": {
                "bar": { "$ref": "#/components/schemas/Bar" }
            }
        }))
        .unwrap();
        schemas.insert("Foo".to_owned(), RefOr::T(foo_schema));

        let openapi = build_test_openapi(schemas);
        let dangling = collect_all_dangling_refs_in_openapi(&openapi);
        assert!(
            dangling.is_empty(),
            "Expected no dangling refs but got: {dangling:?}"
        );
    }

    #[test]
    fn test_dangling_refs_detects_missing_in_operations() {
        // Build an OpenAPI doc with a response $ref to "MissingDto" but no
        // matching component schema — simulates the scenario CodeRabbit flagged.
        let openapi_json = serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "test", "version": "0.1.0" },
            "paths": {
                "/items": {
                    "get": {
                        "responses": {
                            "200": {
                                "description": "OK",
                                "content": {
                                    "application/json": {
                                        "schema": { "$ref": "#/components/schemas/MissingDto" }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "components": {
                "schemas": {}
            }
        });
        let openapi: OpenApi = serde_json::from_value(openapi_json).unwrap();
        let dangling = collect_all_dangling_refs_in_openapi(&openapi);
        assert_eq!(dangling, vec!["MissingDto".to_owned()]);
    }

    // --- array responses -------------------------------------------------
    //
    // A top-level array must be emitted inline, referencing the item type,
    // and must NOT create a component of its own. utoipa names every `Vec<T>`
    // `Vec`, so a named array component makes all list endpoints collide.

    #[test]
    fn array_response_emits_inline_array_referencing_item() {
        let registry = OpenApiRegistryImpl::new();
        registry.register_operation(&spec_with_response(
            "/gears",
            "list_gears",
            Some(ResponseSchema::Array {
                items_schema_name: "GearDto".to_owned(),
            }),
        ));

        let doc = serde_json::to_value(registry.build_openapi(&test_info()).unwrap()).unwrap();
        let schema = response_schema_json(&doc, "/gears");

        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["$ref"], "#/components/schemas/GearDto");
        // The array itself is not a component.
        assert!(schema.get("$ref").is_none());
        assert!(doc["components"]["schemas"].get("Vec").is_none());
    }

    #[test]
    fn ref_response_still_emits_plain_ref() {
        let registry = OpenApiRegistryImpl::new();
        registry.register_operation(&spec_with_response(
            "/gear",
            "get_gear",
            Some(ResponseSchema::Ref {
                schema_name: "GearDto".to_owned(),
            }),
        ));

        let doc = serde_json::to_value(registry.build_openapi(&test_info()).unwrap()).unwrap();
        let schema = response_schema_json(&doc, "/gear");

        assert_eq!(schema["$ref"], "#/components/schemas/GearDto");
        assert!(schema.get("type").is_none());
    }

    /// `OperationBuilder::multipart_json` declares the *item* schema under the
    /// bare `multipart/mixed` media-type key: no `boundary=` parameter (that is
    /// generated per response at runtime, so it is not a property of the
    /// operation), and a `$ref` rather than the opaque string blob a
    /// non-json-like media type would render as.
    #[test]
    fn multipart_mixed_response_emits_the_item_ref_under_a_bare_media_type() {
        let registry = OpenApiRegistryImpl::new();
        let mut spec = spec_with_response(
            "/events",
            "stream_events",
            Some(ResponseSchema::Ref {
                schema_name: "FrameDto".to_owned(),
            }),
        );
        spec.responses[0].content_type = "multipart/mixed";
        registry.register_operation(&spec);

        let doc = serde_json::to_value(registry.build_openapi(&test_info()).unwrap()).unwrap();
        let content = &doc["paths"]["/events"]["get"]["responses"]["200"]["content"];

        assert_eq!(
            content["multipart/mixed"]["schema"]["$ref"],
            "#/components/schemas/FrameDto"
        );
        // The runtime boundary must not leak into the spec's media-type key.
        assert_eq!(
            content.as_object().map(|o| o.keys().collect::<Vec<_>>()),
            Some(vec![&"multipart/mixed".to_owned()])
        );
        // Not rendered as a string with a custom format — that is what a
        // non-json-like media type would produce, and it would lose the item
        // schema entirely.
        assert!(content["multipart/mixed"]["schema"].get("format").is_none());
    }

    #[test]
    fn schemaless_json_response_still_emits_free_form_object() {
        let registry = OpenApiRegistryImpl::new();
        registry.register_operation(&spec_with_response("/any", "any_op", None));

        let doc = serde_json::to_value(registry.build_openapi(&test_info()).unwrap()).unwrap();
        let schema = response_schema_json(&doc, "/any");

        assert!(schema.get("$ref").is_none());
        assert_ne!(schema["type"], "array");
    }

    /// The regression this whole change exists for: two list endpoints
    /// returning different item types used to both register a component named
    /// `Vec`, silently clobbering each other (and, after M-14, panicking).
    #[test]
    fn two_distinct_array_responses_do_not_collide() {
        #[derive(utoipa::ToSchema)]
        #[allow(dead_code)]
        struct AlphaDto {
            alpha: String,
        }
        #[derive(utoipa::ToSchema)]
        #[allow(dead_code)]
        struct BetaDto {
            beta: i32,
        }

        let registry = OpenApiRegistryImpl::new();
        // Registering the ITEM types is what the array builder method does.
        let a = ensure_schema::<AlphaDto>(&registry);
        let b = ensure_schema::<BetaDto>(&registry);
        assert_eq!((a.as_str(), b.as_str()), ("AlphaDto", "BetaDto"));

        registry.register_operation(&spec_with_response(
            "/alphas",
            "list_alphas",
            Some(ResponseSchema::Array {
                items_schema_name: a,
            }),
        ));
        registry.register_operation(&spec_with_response(
            "/betas",
            "list_betas",
            Some(ResponseSchema::Array {
                items_schema_name: b,
            }),
        ));

        let openapi = registry.build_openapi(&test_info()).unwrap();
        assert!(
            collect_all_dangling_refs_in_openapi(&openapi).is_empty(),
            "array item refs must point at registered components"
        );

        let doc = serde_json::to_value(&openapi).unwrap();
        let schemas = &doc["components"]["schemas"];
        assert!(schemas.get("AlphaDto").is_some());
        assert!(schemas.get("BetaDto").is_some());
        assert!(schemas.get("Vec").is_none());
        assert_eq!(
            response_schema_json(&doc, "/alphas")["items"]["$ref"],
            "#/components/schemas/AlphaDto"
        );
        assert_eq!(
            response_schema_json(&doc, "/betas")["items"]["$ref"],
            "#/components/schemas/BetaDto"
        );
    }

    #[test]
    #[should_panic(expected = "would register the component name `Vec`")]
    fn ensure_schema_rejects_vec_directly() {
        #[derive(utoipa::ToSchema)]
        #[allow(dead_code)]
        struct ItemDto {
            x: u8,
        }
        let registry = OpenApiRegistryImpl::new();
        let _ = ensure_schema::<Vec<ItemDto>>(&registry);
    }

    #[test]
    #[should_panic(expected = "OpenAPI schema name collision")]
    fn ensure_schema_raw_panics_on_conflicting_definition() {
        let registry = OpenApiRegistryImpl::new();
        registry.ensure_schema_raw(
            "Dup",
            vec![("Dup".to_owned(), RefOr::Ref(Ref::from_schema_name("First")))],
        );
        registry.ensure_schema_raw(
            "Dup",
            vec![(
                "Dup".to_owned(),
                RefOr::Ref(Ref::from_schema_name("Second")),
            )],
        );
    }

    #[test]
    fn ensure_schema_raw_allows_identical_reregistration() {
        let registry = OpenApiRegistryImpl::new();
        let entry = || {
            vec![(
                "Same".to_owned(),
                RefOr::Ref(Ref::from_schema_name("Target")),
            )]
        };
        registry.ensure_schema_raw("Same", entry());
        registry.ensure_schema_raw("Same", entry());
        assert_eq!(registry.components_registry.load().len(), 1);
    }
}
