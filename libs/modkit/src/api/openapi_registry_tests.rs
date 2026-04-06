use super::*;
use crate::api::operation_builder::{
    OperationSpec, ParamLocation, ParamSpec, ResponseSpec, VendorExtensions,
};
use http::Method;

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
            schema_name: None,
        }],
        handler_id: "get_test".to_owned(),
        authenticated: false,
        is_public: false,
        rate_limit: None,
        allowed_request_content_types: None,
        vendor_extensions: VendorExtensions::default(),
        license_requirement: None,
    };

    registry.register_operation(&spec);
    assert_eq!(registry.operation_specs.len(), 1);
}

#[test]
fn test_build_empty_openapi() {
    let registry = OpenApiRegistryImpl::new();
    let info = OpenApiInfo {
        title: "Test API".to_owned(),
        version: "1.0.0".to_owned(),
        description: Some("Test API Description".to_owned()),
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
        }],
        request_body: None,
        responses: vec![ResponseSpec {
            status: 200,
            content_type: "application/json",
            description: "User found".to_owned(),
            schema_name: None,
        }],
        handler_id: "get_users_id".to_owned(),
        authenticated: false,
        is_public: false,
        rate_limit: None,
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
            schema_name: None,
        }],
        handler_id: "post_upload".to_owned(),
        authenticated: false,
        is_public: false,
        rate_limit: None,
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
            schema_name: None,
        }],
        handler_id: "get_test".to_owned(),
        authenticated: false,
        is_public: false,
        rate_limit: None,
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

/// Helper: build a minimal `OpenAPI` doc with the given component schemas.
fn build_test_openapi(schemas: HashMap<String, RefOr<Schema>>) -> OpenApi {
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
    let mut schemas: HashMap<String, RefOr<Schema>> = HashMap::new();
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
    let mut schemas: HashMap<String, RefOr<Schema>> = HashMap::new();
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
