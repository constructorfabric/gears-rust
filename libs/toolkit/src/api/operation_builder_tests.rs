//! Tests for the API operation builder.

use super::*;
use axum::Json;

// Mock registry for testing: stores operations; records schema names
struct MockRegistry {
    operations: std::sync::Mutex<Vec<OperationSpec>>,
    schemas: std::sync::Mutex<Vec<String>>,
}

impl MockRegistry {
    fn new() -> Self {
        Self {
            operations: std::sync::Mutex::new(Vec::new()),
            schemas: std::sync::Mutex::new(Vec::new()),
        }
    }
}

enum TestLicenseFeatures {
    FeatureA,
    FeatureB,
}
impl AsRef<str> for TestLicenseFeatures {
    fn as_ref(&self) -> &str {
        match self {
            TestLicenseFeatures::FeatureA => "feature_a",
            TestLicenseFeatures::FeatureB => "feature_b",
        }
    }
}
impl LicenseFeature for TestLicenseFeatures {}

impl OpenApiRegistry for MockRegistry {
    fn register_operation(&self, spec: &OperationSpec) {
        if let Ok(mut ops) = self.operations.lock() {
            ops.push(spec.clone());
        }
    }

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        let name = name.to_owned();
        if let Ok(mut s) = self.schemas.lock() {
            s.push(name.clone());
        }
        name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn test_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

#[toolkit_macros::api_dto(request)]
struct SampleDtoRequest;

#[toolkit_macros::api_dto(response)]
struct SampleDtoResponse;

#[test]
fn builder_descriptive_methods() {
    let builder = OperationBuilder::<Missing, Missing, (), AuthNotSet>::get("/tests/v1/test")
        .operation_id("test.get")
        .summary("Test endpoint")
        .description("A test endpoint for validation")
        .tag("test")
        .path_param("id", "Test ID");

    assert_eq!(builder.spec.method, Method::GET);
    assert_eq!(builder.spec.path, "/tests/v1/test");
    assert_eq!(builder.spec.operation_id, Some("test.get".to_owned()));
    assert_eq!(builder.spec.summary, Some("Test endpoint".to_owned()));
    assert_eq!(
        builder.spec.description,
        Some("A test endpoint for validation".to_owned())
    );
    assert_eq!(builder.spec.tags, vec!["test"]);
    assert_eq!(builder.spec.params.len(), 1);
}

#[tokio::test]
async fn builder_with_request_response_and_handler() {
    let registry = MockRegistry::new();
    let router = Router::new();

    let _router = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/test")
        .summary("Test endpoint")
        .json_request::<SampleDtoRequest>(&registry, "optional body") // registers schema
        .anonymous()
        .handler(test_handler)
        .json_response_with_schema::<SampleDtoResponse>(
            &registry,
            http::StatusCode::OK,
            "Success response",
        ) // registers schema
        .register(router, &registry);

    // Verify that the operation was registered
    let ops = registry.operations.lock().unwrap();
    assert_eq!(ops.len(), 1);
    let op = &ops[0];
    assert_eq!(op.method, Method::POST);
    assert_eq!(op.path, "/tests/v1/test");
    assert!(op.request_body.is_some());
    assert!(op.request_body.as_ref().unwrap().required);
    assert_eq!(op.responses.len(), 1);
    assert_eq!(op.responses[0].status, 200);

    // Verify schemas recorded
    let schemas = registry.schemas.lock().unwrap();
    assert!(!schemas.is_empty());
}

#[test]
fn convenience_constructors() {
    let get_builder = OperationBuilder::<Missing, Missing, (), AuthNotSet>::get("/tests/v1/get");
    assert_eq!(get_builder.spec.method, Method::GET);
    assert_eq!(get_builder.spec.path, "/tests/v1/get");

    let post_builder = OperationBuilder::<Missing, Missing, (), AuthNotSet>::post("/tests/v1/post");
    assert_eq!(post_builder.spec.method, Method::POST);
    assert_eq!(post_builder.spec.path, "/tests/v1/post");

    let put_builder = OperationBuilder::<Missing, Missing, (), AuthNotSet>::put("/tests/v1/put");
    assert_eq!(put_builder.spec.method, Method::PUT);
    assert_eq!(put_builder.spec.path, "/tests/v1/put");

    let delete_builder =
        OperationBuilder::<Missing, Missing, (), AuthNotSet>::delete("/tests/v1/delete");
    assert_eq!(delete_builder.spec.method, Method::DELETE);
    assert_eq!(delete_builder.spec.path, "/tests/v1/delete");

    let patch_builder =
        OperationBuilder::<Missing, Missing, (), AuthNotSet>::patch("/tests/v1/patch");
    assert_eq!(patch_builder.spec.method, Method::PATCH);
    assert_eq!(patch_builder.spec.path, "/tests/v1/patch");
}

#[test]
fn normalize_to_axum_path_should_normalize() {
    // Axum 0.8+ uses {param} syntax, same as OpenAPI
    assert_eq!(
        normalize_to_axum_path("/tests/v1/users/{id}"),
        "/tests/v1/users/{id}"
    );
    assert_eq!(
        normalize_to_axum_path("/tests/v1/projects/{project_id}/items/{item_id}"),
        "/tests/v1/projects/{project_id}/items/{item_id}"
    );
    assert_eq!(
        normalize_to_axum_path("/tests/v1/simple"),
        "/tests/v1/simple"
    );
    assert_eq!(
        normalize_to_axum_path("/tests/v1/users/{id}/edit"),
        "/tests/v1/users/{id}/edit"
    );
}

#[test]
fn axum_to_openapi_path_should_convert() {
    // Regular parameters stay the same
    assert_eq!(
        axum_to_openapi_path("/tests/v1/users/{id}"),
        "/tests/v1/users/{id}"
    );
    assert_eq!(
        axum_to_openapi_path("/tests/v1/projects/{project_id}/items/{item_id}"),
        "/tests/v1/projects/{project_id}/items/{item_id}"
    );
    assert_eq!(axum_to_openapi_path("/tests/v1/simple"), "/tests/v1/simple");
    // Wildcards: Axum uses {*path}, OpenAPI uses {path}
    assert_eq!(
        axum_to_openapi_path("/tests/v1/static/{*path}"),
        "/tests/v1/static/{path}"
    );
    assert_eq!(
        axum_to_openapi_path("/tests/v1/files/{*filepath}"),
        "/tests/v1/files/{filepath}"
    );
}

#[test]
fn path_normalization_in_constructors() {
    // Test that paths are kept as-is (Axum 0.8+ uses same {param} syntax)
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/users/{id}");
    assert_eq!(builder.spec.path, "/tests/v1/users/{id}");

    let builder = OperationBuilder::<Missing, Missing, ()>::post(
        "/tests/v1/projects/{project_id}/items/{item_id}",
    );
    assert_eq!(
        builder.spec.path,
        "/tests/v1/projects/{project_id}/items/{item_id}"
    );

    // Simple paths remain unchanged
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/simple");
    assert_eq!(builder.spec.path, "/tests/v1/simple");
}

#[test]
fn standard_errors() {
    let registry = MockRegistry::new();
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success")
        .standard_errors(&registry);

    // Should have 1 success response + 7 standard error responses
    // (422 is intentionally omitted — canonical InvalidArgument is 400).
    assert_eq!(builder.spec.responses.len(), 8);

    // Check that all standard error status codes are present
    let statuses: Vec<u16> = builder.spec.responses.iter().map(|r| r.status).collect();
    assert!(statuses.contains(&200)); // success response
    assert!(statuses.contains(&400));
    assert!(statuses.contains(&401));
    assert!(statuses.contains(&403));
    assert!(statuses.contains(&404));
    assert!(statuses.contains(&409));
    assert!(!statuses.contains(&422));
    assert!(statuses.contains(&429));
    assert!(statuses.contains(&500));

    // All error responses should use Problem content type
    let error_responses: Vec<_> = builder
        .spec
        .responses
        .iter()
        .filter(|r| r.status >= 400)
        .collect();

    for resp in error_responses {
        assert_eq!(
            resp.content_type,
            toolkit_canonical_errors::problem::APPLICATION_PROBLEM_JSON
        );
        assert!(resp.schema_name().is_some());
    }
}

#[test]
fn response_header_attaches_to_the_most_recent_response() {
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success")
        .json_response(http::StatusCode::ACCEPTED, "Accepted")
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "Resource URI",
            ResponseHeaderType::String,
        ));

    let ok: Vec<_> = builder
        .spec
        .responses
        .iter()
        .filter(|response| response.status == 200)
        .collect();
    let accepted: Vec<_> = builder
        .spec
        .responses
        .iter()
        .filter(|response| response.status == 202)
        .collect();
    assert_eq!(ok.len(), 1);
    assert_eq!(accepted.len(), 1);
    assert!(ok[0].headers.is_empty());
    assert_eq!(accepted[0].headers.len(), 1);
    assert_eq!(accepted[0].headers[0].name, "Location");
}

#[test]
#[should_panic(expected = "response 200 already declares header 'location'")]
fn response_spec_with_headers_rejects_case_insensitive_duplicates_in_batch() {
    let _response = ResponseSpec::new(200, "application/json", "Success", None).with_headers([
        ResponseHeaderSpec::without_description("Location", ResponseHeaderType::String),
        ResponseHeaderSpec::without_description("location", ResponseHeaderType::String),
    ]);
}

#[test]
#[should_panic(expected = "response 200 already declares header 'location'")]
fn response_spec_with_headers_rejects_case_insensitive_existing_duplicate() {
    let _response = ResponseSpec::new(200, "application/json", "Success", None)
        .with_headers([ResponseHeaderSpec::without_description(
            "Location",
            ResponseHeaderType::String,
        )])
        .with_headers([ResponseHeaderSpec::without_description(
            "location",
            ResponseHeaderType::String,
        )]);
}

#[test]
fn redeclared_response_becomes_the_header_target() {
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Original")
        .json_response(http::StatusCode::ACCEPTED, "Accepted")
        .json_response(http::StatusCode::OK, "Replacement")
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "Resource URI",
            ResponseHeaderType::String,
        ));

    assert_eq!(builder.spec.responses.len(), 2);
    assert_eq!(builder.spec.responses.last().unwrap().status, 200);
    assert_eq!(
        builder.spec.responses.last().unwrap().description,
        "Replacement"
    );
    assert_eq!(builder.spec.responses.last().unwrap().headers.len(), 1);
}

#[test]
#[should_panic(expected = "response 200 already declares header 'location'")]
fn response_header_rejects_case_insensitive_duplicates() {
    let _builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success")
        .response_header(ResponseHeaderSpec::new(
            "Location",
            "Resource URI",
            ResponseHeaderType::String,
        ))
        .response_header(ResponseHeaderSpec::new(
            "location",
            "Duplicate URI",
            ResponseHeaderType::String,
        ));
}

#[test]
fn replacing_a_response_preserves_its_headers() {
    let registry = MockRegistry::new();
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success")
        .problem_response(
            &registry,
            http::StatusCode::TOO_MANY_REQUESTS,
            "Custom rate limit response",
        )
        .response_header(ResponseHeaderSpec::new(
            "Retry-After",
            "Delay before retrying",
            ResponseHeaderType::Integer,
        ))
        .standard_errors(&registry);

    let rate_limited: Vec<_> = builder
        .spec
        .responses
        .iter()
        .filter(|response| response.status == 429)
        .collect();
    assert_eq!(rate_limited.len(), 1);
    assert_eq!(rate_limited[0].description, "Too Many Requests");
    assert_eq!(rate_limited[0].headers.len(), 1);
    assert_eq!(rate_limited[0].headers[0].name, "Retry-After");
    assert_eq!(
        rate_limited[0].headers[0].header_type,
        ResponseHeaderType::Integer
    );
}

#[test]
fn error_413_is_available_for_extractor_json_operations() {
    let registry = MockRegistry::new();
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success")
        .standard_errors(&registry)
        .error_413(&registry)
        .error_415(&registry)
        .error_422(&registry);

    // 1 success + 7 standard errors + the 3 extractor-specific ones.
    assert_eq!(builder.spec.responses.len(), 11);

    let statuses: Vec<u16> = builder.spec.responses.iter().map(|r| r.status).collect();
    assert!(statuses.contains(&413));
    assert!(statuses.contains(&415));
    assert!(statuses.contains(&422));

    for resp in builder.spec.responses.iter().filter(|r| r.status >= 400) {
        assert_eq!(
            resp.content_type,
            toolkit_canonical_errors::problem::APPLICATION_PROBLEM_JSON
        );
        assert!(resp.schema_name().is_some());
    }
}

#[test]
fn authenticated() {
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .authenticated()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    assert!(builder.spec.authenticated);
    assert!(!builder.spec.exposed);
}

#[test]
fn anonymous_is_internal_by_default() {
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    assert!(!builder.spec.authenticated);
    assert!(!builder.spec.exposed);
}

#[test]
fn exposed_is_independent_of_auth() {
    // Visibility (`exposed`) and auth (`authenticated`) are orthogonal:
    // an exposed route may be authenticated or anonymous.
    let authed = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/a")
        .exposed()
        .authenticated()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "OK");
    assert!(authed.spec.authenticated);
    assert!(authed.spec.exposed);

    let anon = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/b")
        .exposed()
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "OK");
    assert!(!anon.spec.authenticated);
    assert!(anon.spec.exposed);
}

#[test]
#[allow(deprecated)]
fn deprecated_public_maps_to_anonymous_and_exposed() {
    // The `.public()` shim must set both axes: anonymous (no auth) AND
    // exposed (edge-visible), matching the old single-axis semantics.
    let op = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/ping")
        .public()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "OK");
    assert!(!op.spec.authenticated, "public route is anonymous");
    assert!(op.spec.exposed, "public route is edge-exposed");
}

#[test]
fn require_license_features_none() {
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .authenticated()
        .require_license_features::<TestLicenseFeatures>([])
        .handler(|| async {})
        .json_response(http::StatusCode::OK, "OK");

    assert!(builder.spec.license_requirement.is_none());
}

#[test]
fn no_license_required_transitions_and_allows_register() {
    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .authenticated()
        .no_license_required()
        .handler(|| async {})
        .json_response(http::StatusCode::OK, "OK");

    assert!(builder.spec.license_requirement.is_none());
    assert!(!builder.spec.exposed);
}

#[test]
fn require_license_features_one() {
    let feature = TestLicenseFeatures::FeatureA;

    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .authenticated()
        .require_license_features([&feature])
        .handler(|| async {})
        .json_response(http::StatusCode::OK, "OK");

    let license_req = builder
        .spec
        .license_requirement
        .as_ref()
        .expect("Should have license requirement");
    assert_eq!(license_req.license_names, vec!["feature_a".to_owned()]);
}

#[test]
fn require_license_features_many() {
    let feature_a = TestLicenseFeatures::FeatureA;
    let feature_b = TestLicenseFeatures::FeatureB;

    let builder = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .authenticated()
        .require_license_features([&feature_a, &feature_b])
        .handler(|| async {})
        .json_response(http::StatusCode::OK, "OK");

    let license_req = builder
        .spec
        .license_requirement
        .as_ref()
        .expect("Should have license requirement");
    assert_eq!(
        license_req.license_names,
        vec!["feature_a".to_owned(), "feature_b".to_owned()]
    );
}

#[tokio::test]
async fn public_does_not_require_license_features_and_can_register() {
    let registry = MockRegistry::new();
    let router = Router::new();

    let _router = OperationBuilder::<Missing, Missing, ()>::get("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success")
        .register(router, &registry);

    let ops = registry.operations.lock().unwrap();
    assert_eq!(ops.len(), 1);
    assert!(ops[0].license_requirement.is_none());
}

#[test]
fn with_400_validation_error() {
    let registry = MockRegistry::new();
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/test")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::CREATED, "Created")
        .with_400_validation_error(&registry);

    // Should have success response + validation error response
    assert_eq!(builder.spec.responses.len(), 2);

    let validation_response = builder
        .spec
        .responses
        .iter()
        .find(|r| r.status == 400)
        .expect("Should have 400 response");

    assert_eq!(validation_response.description, "Validation Error");
    assert_eq!(
        validation_response.content_type,
        toolkit_canonical_errors::problem::APPLICATION_PROBLEM_JSON
    );
    assert!(validation_response.schema_name().is_some());
}

#[test]
fn allow_content_types_with_existing_request_body() {
    let registry = MockRegistry::new();
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/test")
        .json_request::<SampleDtoRequest>(&registry, "Test request")
        .allow_content_types(&["application/json", "application/xml"])
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    // allowed_content_types should be on OperationSpec, not RequestBodySpec
    assert!(builder.spec.request_body.is_some());
    assert!(builder.spec.allowed_request_content_types.is_some());
    let allowed = builder.spec.allowed_request_content_types.as_ref().unwrap();
    assert_eq!(allowed.len(), 2);
    assert!(allowed.contains(&"application/json"));
    assert!(allowed.contains(&"application/xml"));
}

#[test]
fn allow_content_types_without_existing_request_body() {
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/test")
        .allow_content_types(&["multipart/form-data"])
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    // Should NOT create synthetic request body, only set allowed_request_content_types
    assert!(builder.spec.request_body.is_none());
    assert!(builder.spec.allowed_request_content_types.is_some());
    let allowed = builder.spec.allowed_request_content_types.as_ref().unwrap();
    assert_eq!(allowed.len(), 1);
    assert!(allowed.contains(&"multipart/form-data"));
}

#[test]
fn allow_content_types_can_be_chained() {
    let registry = MockRegistry::new();
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/test")
        .operation_id("test.post")
        .summary("Test endpoint")
        .json_request::<SampleDtoRequest>(&registry, "Test request")
        .allow_content_types(&["application/json"])
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success")
        .problem_response(
            &registry,
            http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type",
        );

    assert_eq!(builder.spec.operation_id, Some("test.post".to_owned()));
    assert!(builder.spec.request_body.is_some());
    assert!(builder.spec.allowed_request_content_types.is_some());
    assert_eq!(builder.spec.responses.len(), 2);
}

#[test]
fn multipart_file_request() {
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/upload")
        .operation_id("test.upload")
        .summary("Upload file")
        .multipart_file_request("file", Some("Upload a file"))
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    // Should set request body with multipart/form-data
    assert!(builder.spec.request_body.is_some());
    let rb = builder.spec.request_body.as_ref().unwrap();
    assert_eq!(rb.content_type, "multipart/form-data");
    assert!(rb.description.is_some());
    assert!(rb.description.as_ref().unwrap().contains("file"));
    assert!(rb.required);

    // Should use MultipartFile schema variant
    assert_eq!(
        rb.schema,
        RequestBodySchema::MultipartFile {
            field_name: "file".to_owned()
        }
    );

    // Should also set allowed_request_content_types
    assert!(builder.spec.allowed_request_content_types.is_some());
    let allowed = builder.spec.allowed_request_content_types.as_ref().unwrap();
    assert_eq!(allowed.len(), 1);
    assert!(allowed.contains(&"multipart/form-data"));
}

#[test]
fn multipart_file_request_without_description() {
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/upload")
        .multipart_file_request("file", None)
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    assert!(builder.spec.request_body.is_some());
    let rb = builder.spec.request_body.as_ref().unwrap();
    assert_eq!(rb.content_type, "multipart/form-data");
    assert!(rb.description.is_none());
    assert_eq!(
        rb.schema,
        RequestBodySchema::MultipartFile {
            field_name: "file".to_owned()
        }
    );
}

#[test]
fn octet_stream_request() {
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/upload")
        .operation_id("test.upload")
        .summary("Upload raw file")
        .octet_stream_request(Some("Raw file bytes"))
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    // Should set request body with application/octet-stream
    assert!(builder.spec.request_body.is_some());
    let rb = builder.spec.request_body.as_ref().unwrap();
    assert_eq!(rb.content_type, "application/octet-stream");
    assert_eq!(rb.description, Some("Raw file bytes".to_owned()));
    assert!(rb.required);

    // Should use Binary schema variant
    assert_eq!(rb.schema, RequestBodySchema::Binary);

    // Should also set allowed_request_content_types
    assert!(builder.spec.allowed_request_content_types.is_some());
    let allowed = builder.spec.allowed_request_content_types.as_ref().unwrap();
    assert_eq!(allowed.len(), 1);
    assert!(allowed.contains(&"application/octet-stream"));
}

#[test]
fn octet_stream_request_without_description() {
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/upload")
        .octet_stream_request(None)
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    assert!(builder.spec.request_body.is_some());
    let rb = builder.spec.request_body.as_ref().unwrap();
    assert_eq!(rb.content_type, "application/octet-stream");
    assert!(rb.description.is_none());
    assert_eq!(rb.schema, RequestBodySchema::Binary);
}

#[test]
fn json_request_uses_ref_schema() {
    let registry = MockRegistry::new();
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/test")
        .json_request::<SampleDtoRequest>(&registry, "Test request body")
        .anonymous()
        .handler(test_handler)
        .json_response(http::StatusCode::OK, "Success");

    assert!(builder.spec.request_body.is_some());
    let rb = builder.spec.request_body.as_ref().unwrap();
    assert_eq!(rb.content_type, "application/json");

    // Should use Ref schema variant with the registered schema name
    match &rb.schema {
        RequestBodySchema::Ref { schema_name } => {
            assert!(!schema_name.is_empty());
        }
        _ => panic!("Expected RequestBodySchema::Ref for JSON request"),
    }
}

#[test]
fn response_content_types_must_not_contain_parameters() {
    // This test ensures OpenAPI correctness: media type keys cannot include
    // parameters like "; charset=utf-8"
    let registry = MockRegistry::new();
    let builder = OperationBuilder::<Missing, Missing, ()>::post("/tests/v1/test")
        .operation_id("test.content_type_purity")
        .summary("Test response content types")
        .json_request::<SampleDtoRequest>(&registry, "Test")
        .anonymous()
        .handler(test_handler)
        .text_response(http::StatusCode::OK, "Text", "text/plain")
        .text_response(http::StatusCode::OK, "Markdown", "text/markdown")
        .html_response(http::StatusCode::OK, "HTML")
        .json_response(http::StatusCode::OK, "JSON")
        .problem_response(&registry, http::StatusCode::BAD_REQUEST, "Error");

    // Verify no response content_type contains semicolon (parameter separator)
    for response in &builder.spec.responses {
        assert!(
            !response.content_type.contains(';'),
            "Response content_type '{}' must not contain parameters. \
             Use pure media type without charset or other parameters. \
             OpenAPI media type keys cannot include parameters.",
            response.content_type
        );
    }
}
