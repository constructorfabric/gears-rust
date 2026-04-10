//! Build `OpenAPI` 3.1 documents from `ContractIr` + `HttpBindingIr`.

use serde_json::{Value, json};

use crate::ir::binding::{HttpBindingIr, HttpFieldBinding, HttpMethod, HttpMethodBindingIr};
use crate::ir::contract::{ContractIr, MethodIr, PrimitiveType, TypeRef};

/// A named schema definition supplied by the caller. Typically produced by
/// `schemars::schema_for!(MyType)` and converted to `serde_json::Value`.
pub type SchemaEntry<'a> = (&'a str, Value);

/// Generate an `OpenAPI` 3.1 document.
///
/// `schemas` are placed under `components.schemas`. The generator does not
/// introspect domain types — it relies entirely on the names declared in
/// `MethodIr.input` / `MethodIr.output` / `MethodIr.error` matching the
/// names supplied by the caller.
#[must_use]
pub fn generate_openapi_spec(
    contract: &ContractIr,
    binding: &HttpBindingIr,
    schemas: &[SchemaEntry<'_>],
) -> Value {
    let mut paths = serde_json::Map::new();

    for method_binding in &binding.methods {
        let Some(method_ir) = contract
            .methods
            .iter()
            .find(|m| m.name == method_binding.method_name)
        else {
            continue;
        };
        let path = format!(
            "{}{}",
            binding.base_path.trim_end_matches('/'),
            method_binding.path_template
        );
        let verb = http_method_lowercase(method_binding.http_method);

        let entry = paths
            .entry(path)
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(map) = entry {
            map.insert(
                verb.to_owned(),
                build_operation(method_ir, method_binding, &contract.gear),
            );
        }
    }

    let components = build_components(schemas);

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": contract.name,
            "version": contract.version,
            "x-gear": contract.gear,
        },
        "paths": Value::Object(paths),
        "components": components,
    })
}

fn build_operation(method: &MethodIr, binding: &HttpMethodBindingIr, gear: &str) -> Value {
    let mut op = serde_json::Map::new();
    op.insert(
        "operationId".to_owned(),
        Value::String(format!("{gear}_{}", method.name)),
    );

    let mut tags = Vec::new();
    tags.push(Value::String(gear.to_owned()));
    op.insert("tags".to_owned(), Value::Array(tags));

    let parameters = build_parameters(method, binding);
    if !parameters.is_empty() {
        op.insert("parameters".to_owned(), Value::Array(parameters));
    }

    if let Some(request_body) = build_request_body(method, binding) {
        op.insert("requestBody".to_owned(), request_body);
    }

    op.insert("responses".to_owned(), build_responses(method, binding));

    if binding.retryable {
        op.insert("x-retryable".to_owned(), Value::Bool(true));
    }
    if binding.streaming {
        op.insert("x-streaming".to_owned(), Value::Bool(true));
    }
    if binding.optional || method.optional {
        op.insert("x-optional".to_owned(), Value::Bool(true));
        op.insert(
            "description".to_owned(),
            Value::String("Optional endpoint \u{2014} peers MAY omit this method.".to_owned()),
        );
    }

    Value::Object(op)
}

fn build_parameters(method: &MethodIr, binding: &HttpMethodBindingIr) -> Vec<Value> {
    let mut params = Vec::new();
    for fb in &binding.field_bindings {
        match fb {
            HttpFieldBinding::Path { field, param } => {
                params.push(parameter_object(
                    "path",
                    param,
                    &field_schema(method, field),
                    /* required = */ true,
                ));
            }
            HttpFieldBinding::Query { field, param } => {
                params.push(parameter_object(
                    "query",
                    param,
                    &field_schema(method, field),
                    /* required = */ false,
                ));
            }
            HttpFieldBinding::Header { field, header } => {
                params.push(parameter_object(
                    "header",
                    header,
                    &field_schema(method, field),
                    /* required = */ false,
                ));
            }
            HttpFieldBinding::Body => {}
        }
    }
    params
}

fn parameter_object(loc: &str, name: &str, schema: &Value, required: bool) -> Value {
    json!({
        "name": name,
        "in": loc,
        "required": required,
        "schema": schema,
    })
}

fn field_schema(method: &MethodIr, field_name: &str) -> Value {
    let Some(field) = method.input.fields.iter().find(|f| f.name == field_name) else {
        return json!({ "type": "string" });
    };
    typeref_to_schema(&field.ty)
}

fn typeref_to_schema(ty: &TypeRef) -> Value {
    match ty {
        TypeRef::Primitive(p) => primitive_to_schema(*p),
        TypeRef::Named(name) => json!({ "$ref": format!("#/components/schemas/{name}") }),
        TypeRef::Optional(inner) => json!({
            "anyOf": [ typeref_to_schema(inner), { "type": "null" } ]
        }),
        TypeRef::List(inner) => json!({
            "type": "array",
            "items": typeref_to_schema(inner),
        }),
        TypeRef::Map(key, value) => json!({
            "type": "object",
            "additionalProperties": typeref_to_schema(value),
            "x-key-schema": typeref_to_schema(key),
        }),
    }
}

fn field_is_optional(field: &crate::ir::contract::FieldIr) -> bool {
    field.optional || matches!(field.ty, TypeRef::Optional(_))
}

fn build_object_schema_from_fields(fields: &[crate::ir::contract::FieldIr]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for field in fields {
        properties.insert(field.name.clone(), typeref_to_schema(&field.ty));
        if !field_is_optional(field) {
            required.push(Value::String(field.name.clone()));
        }
    }
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_owned(), Value::String("object".to_owned()));
    schema.insert("properties".to_owned(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_owned(), Value::Array(required));
    }
    Value::Object(schema)
}

fn primitive_to_schema(p: PrimitiveType) -> Value {
    match p {
        PrimitiveType::String => json!({ "type": "string" }),
        PrimitiveType::Bool => json!({ "type": "boolean" }),
        PrimitiveType::Bytes => json!({ "type": "string", "format": "byte" }),
        PrimitiveType::Uuid => json!({ "type": "string", "format": "uuid" }),
        PrimitiveType::I32 => json!({ "type": "integer", "format": "int32" }),
        PrimitiveType::I64 => json!({ "type": "integer", "format": "int64" }),
        PrimitiveType::U64 => json!({ "type": "integer", "format": "int64", "minimum": 0 }),
        PrimitiveType::F64 => json!({ "type": "number", "format": "double" }),
    }
}

fn build_request_body(method: &MethodIr, binding: &HttpMethodBindingIr) -> Option<Value> {
    if matches!(binding.http_method, HttpMethod::Get | HttpMethod::Delete) {
        return None;
    }

    let unbound_fields: Vec<&crate::ir::contract::FieldIr> = method
        .input
        .fields
        .iter()
        .filter(|f| !is_path_or_query_field(&binding.field_bindings, &f.name))
        .collect();

    if unbound_fields.is_empty() {
        return None;
    }

    let has_body_marker = binding
        .field_bindings
        .iter()
        .any(|fb| matches!(fb, HttpFieldBinding::Body));

    let schema = if unbound_fields.len() == 1 && has_body_marker {
        typeref_to_schema(&unbound_fields[0].ty)
    } else {
        let owned: Vec<crate::ir::contract::FieldIr> =
            unbound_fields.iter().map(|f| (*f).clone()).collect();
        build_object_schema_from_fields(&owned)
    };

    let all_required = unbound_fields.iter().all(|f| !field_is_optional(f));
    Some(json!({
        "required": all_required,
        "content": {
            "application/json": {
                "schema": schema,
            }
        }
    }))
}

fn is_path_or_query_field(bindings: &[HttpFieldBinding], field_name: &str) -> bool {
    bindings.iter().any(|fb| match fb {
        HttpFieldBinding::Path { field, .. }
        | HttpFieldBinding::Query { field, .. }
        | HttpFieldBinding::Header { field, .. } => field == field_name,
        HttpFieldBinding::Body => false,
    })
}

fn build_responses(method: &MethodIr, binding: &HttpMethodBindingIr) -> Value {
    let mut responses = serde_json::Map::new();

    let item_schema = typeref_to_schema(&method.output);
    let ok_response = if binding.streaming {
        json!({
            "description": "Server-Sent Events stream",
            "content": {
                "text/event-stream": { "schema": { "type": "string" } },
            },
            "x-sse-event-schema": item_schema,
        })
    } else {
        json!({
            "description": "Successful response",
            "content": {
                "application/json": { "schema": item_schema },
            }
        })
    };
    responses.insert("200".to_owned(), ok_response);

    responses.insert(
        "default".to_owned(),
        json!({
            "description": "Error response (RFC 9457 Problem)",
            "content": {
                "application/problem+json": {
                    "schema": { "$ref": "#/components/schemas/Problem" },
                }
            }
        }),
    );

    Value::Object(responses)
}

fn build_components(schemas: &[SchemaEntry<'_>]) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("Problem".to_owned(), canonical_problem_schema());
    for (name, schema) in schemas {
        map.insert((*name).to_owned(), schema.clone());
    }
    json!({ "schemas": Value::Object(map) })
}

/// `OpenAPI` schema for `toolkit_canonical_errors::Problem` — RFC 9457
/// Problem Details with the `CyberFabric` extension members `trace_id` and
/// `context` as documented in `docs/arch/errors/DESIGN.md` §3.3.
fn canonical_problem_schema() -> Value {
    json!({
        "type": "object",
        "required": ["type", "title", "status", "detail", "context"],
        "properties": {
            "type": {
                "type": "string",
                "description": "GTS type identifier for the canonical error category"
            },
            "title": {
                "type": "string",
                "description": "Human-readable category title"
            },
            "status": {
                "type": "integer",
                "description": "HTTP status code from the category mapping"
            },
            "detail": {
                "type": "string",
                "description": "Human-readable explanation of this occurrence"
            },
            "instance": {
                "type": "string",
                "description": "URI identifying this specific occurrence"
            },
            "trace_id": {
                "type": "string",
                "description": "W3C trace ID for correlation, injected by middleware"
            },
            "context": {
                "type": "object",
                "description": "Category-specific structured details"
            }
        }
    })
}

fn http_method_lowercase(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Put => "put",
        HttpMethod::Delete => "delete",
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::ir::binding::{HttpFieldBinding, HttpMethod, HttpMethodBindingIr};
    use crate::ir::contract::{
        FieldIr, Idempotency, InputShape, MethodIr, MethodKind, ServiceIr, TypeRef,
    };

    fn sample_contract() -> ContractIr {
        ServiceIr {
            name: "PaymentService".into(),
            gear: "payment".into(),
            version: "v1".into(),
            methods: vec![
                MethodIr {
                    name: "charge".into(),
                    kind: MethodKind::Unary,
                    input: InputShape {
                        fields: vec![FieldIr {
                            name: "req".into(),
                            ty: TypeRef::Named("ChargeRequest".into()),
                            optional: false,
                            role: crate::ir::contract::FieldRole::Wire,
                        }],
                    },
                    output: TypeRef::Named("ChargeResponse".into()),
                    error: Some(TypeRef::Named("PaymentError".into())),
                    idempotency: Idempotency::NonIdempotentWrite,
                    optional: false,
                },
                MethodIr {
                    name: "get_invoice".into(),
                    kind: MethodKind::Unary,
                    input: InputShape {
                        fields: vec![FieldIr {
                            name: "invoice_id".into(),
                            ty: TypeRef::Primitive(PrimitiveType::String),
                            optional: false,
                            role: crate::ir::contract::FieldRole::Wire,
                        }],
                    },
                    output: TypeRef::Named("Invoice".into()),
                    error: Some(TypeRef::Named("PaymentError".into())),
                    idempotency: Idempotency::SafeRead,
                    optional: false,
                },
            ],
        }
    }

    fn sample_binding() -> HttpBindingIr {
        HttpBindingIr {
            base_path: "/api/payment/v1".into(),
            methods: vec![
                HttpMethodBindingIr {
                    method_name: "charge".into(),
                    http_method: HttpMethod::Post,
                    path_template: "/charge".into(),
                    field_bindings: vec![HttpFieldBinding::Body],
                    retryable: false,
                    streaming: false,
                    optional: false,
                },
                HttpMethodBindingIr {
                    method_name: "get_invoice".into(),
                    http_method: HttpMethod::Get,
                    path_template: "/invoices/{invoice_id}".into(),
                    field_bindings: vec![HttpFieldBinding::Path {
                        field: "invoice_id".into(),
                        param: "invoice_id".into(),
                    }],
                    retryable: true,
                    streaming: false,
                    optional: false,
                },
            ],
        }
    }

    #[test]
    fn produces_openapi_3_1_envelope() {
        let spec = generate_openapi_spec(&sample_contract(), &sample_binding(), &[]);
        assert_eq!(spec["openapi"], "3.1.0");
        assert_eq!(spec["info"]["title"], "PaymentService");
        assert_eq!(spec["info"]["version"], "v1");
        assert_eq!(spec["info"]["x-gear"], "payment");
    }

    #[test]
    fn registers_path_per_binding() {
        let spec = generate_openapi_spec(&sample_contract(), &sample_binding(), &[]);
        assert!(spec["paths"]["/api/payment/v1/charge"].is_object());
        assert!(spec["paths"]["/api/payment/v1/invoices/{invoice_id}"].is_object());
    }

    #[test]
    fn maps_post_to_request_body() {
        let spec = generate_openapi_spec(&sample_contract(), &sample_binding(), &[]);
        let op = &spec["paths"]["/api/payment/v1/charge"]["post"];
        assert!(op["requestBody"].is_object());
        let schema = &op["requestBody"]["content"]["application/json"]["schema"];
        assert_eq!(schema["$ref"], "#/components/schemas/ChargeRequest");
    }

    #[test]
    fn marks_retryable_with_extension() {
        let spec = generate_openapi_spec(&sample_contract(), &sample_binding(), &[]);
        let op = &spec["paths"]["/api/payment/v1/invoices/{invoice_id}"]["get"];
        assert_eq!(op["x-retryable"], true);
    }

    #[test]
    fn includes_problem_details_schema() {
        let spec = generate_openapi_spec(&sample_contract(), &sample_binding(), &[]);
        assert!(spec["components"]["schemas"]["Problem"].is_object());
    }

    #[test]
    fn merges_user_supplied_schemas() {
        let charge_request_schema = json!({
            "type": "object",
            "properties": { "amount_cents": { "type": "integer" } },
        });
        let spec = generate_openapi_spec(
            &sample_contract(),
            &sample_binding(),
            &[("ChargeRequest", charge_request_schema)],
        );
        assert_eq!(
            spec["components"]["schemas"]["ChargeRequest"]["type"],
            "object"
        );
    }

    #[test]
    fn streaming_flag_emits_event_stream_content_type() {
        let mut binding = sample_binding();
        binding.methods[0].streaming = true;
        let spec = generate_openapi_spec(&sample_contract(), &binding, &[]);
        let op = &spec["paths"]["/api/payment/v1/charge"]["post"];
        assert!(op["responses"]["200"]["content"]["text/event-stream"].is_object());
        assert_eq!(op["x-streaming"], true);
    }

    #[test]
    fn streaming_response_uses_string_schema_and_typed_extension() {
        let mut binding = sample_binding();
        binding.methods[0].streaming = true;
        let spec = generate_openapi_spec(&sample_contract(), &binding, &[]);
        let resp = &spec["paths"]["/api/payment/v1/charge"]["post"]["responses"]["200"];
        assert_eq!(
            resp["content"]["text/event-stream"]["schema"]["type"],
            "string"
        );
        assert_eq!(
            resp["x-sse-event-schema"]["$ref"],
            "#/components/schemas/ChargeResponse"
        );
    }

    #[test]
    fn optional_fields_emit_nullable_and_are_skipped_from_required() {
        let contract = ServiceIr {
            name: "Svc".into(),
            gear: "m".into(),
            version: "v1".into(),
            methods: vec![MethodIr {
                name: "do_thing".into(),
                kind: MethodKind::Unary,
                input: InputShape {
                    fields: vec![
                        FieldIr {
                            name: "required_field".into(),
                            ty: TypeRef::Primitive(PrimitiveType::String),
                            optional: false,
                            role: crate::ir::contract::FieldRole::Wire,
                        },
                        FieldIr {
                            name: "optional_field".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::Primitive(PrimitiveType::I64))),
                            optional: true,
                            role: crate::ir::contract::FieldRole::Wire,
                        },
                    ],
                },
                output: TypeRef::Named("Out".into()),
                error: None,
                idempotency: Idempotency::NonIdempotentWrite,
                optional: false,
            }],
        };
        let binding = HttpBindingIr {
            base_path: "/api/m/v1".into(),
            methods: vec![HttpMethodBindingIr {
                method_name: "do_thing".into(),
                http_method: HttpMethod::Post,
                path_template: "/do".into(),
                field_bindings: vec![HttpFieldBinding::Body],
                retryable: false,
                streaming: false,
                optional: false,
            }],
        };
        let spec = generate_openapi_spec(&contract, &binding, &[]);
        let schema = &spec["paths"]["/api/m/v1/do"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"];
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("required_field"));
        assert!(props.contains_key("optional_field"));
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "required_field");
        let opt_schema = &props["optional_field"];
        let any_of = opt_schema["anyOf"].as_array().unwrap();
        assert!(any_of.iter().any(|v| v["type"] == "null"));
    }

    #[test]
    fn multi_field_body_emits_object_schema() {
        let contract = ServiceIr {
            name: "Svc".into(),
            gear: "m".into(),
            version: "v1".into(),
            methods: vec![MethodIr {
                name: "make".into(),
                kind: MethodKind::Unary,
                input: InputShape {
                    fields: vec![
                        FieldIr {
                            name: "name".into(),
                            ty: TypeRef::Primitive(PrimitiveType::String),
                            optional: false,
                            role: crate::ir::contract::FieldRole::Wire,
                        },
                        FieldIr {
                            name: "count".into(),
                            ty: TypeRef::Primitive(PrimitiveType::I64),
                            optional: false,
                            role: crate::ir::contract::FieldRole::Wire,
                        },
                        FieldIr {
                            name: "note".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::Primitive(
                                PrimitiveType::String,
                            ))),
                            optional: true,
                            role: crate::ir::contract::FieldRole::Wire,
                        },
                    ],
                },
                output: TypeRef::Named("Out".into()),
                error: None,
                idempotency: Idempotency::NonIdempotentWrite,
                optional: false,
            }],
        };
        let binding = HttpBindingIr {
            base_path: "/api/m/v1".into(),
            methods: vec![HttpMethodBindingIr {
                method_name: "make".into(),
                http_method: HttpMethod::Post,
                path_template: "/make".into(),
                field_bindings: vec![],
                retryable: false,
                streaming: false,
                optional: false,
            }],
        };
        let spec = generate_openapi_spec(&contract, &binding, &[]);
        let schema = &spec["paths"]["/api/m/v1/make"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"];
        assert_eq!(schema["type"], "object");
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("count"));
        assert!(props.contains_key("note"));
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
    }
}
