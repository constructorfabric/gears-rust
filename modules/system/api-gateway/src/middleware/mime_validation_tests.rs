use super::*;
use modkit::api::operation_builder::VendorExtensions;

#[test]
fn test_build_mime_validation_map() {
    use modkit::api::operation_builder::{RequestBodySchema, RequestBodySpec};

    let specs = vec![OperationSpec {
        method: Method::POST,
        path: "/files/v1/upload".to_owned(),
        operation_id: None,
        summary: None,
        description: None,
        tags: vec![],
        params: vec![],
        request_body: Some(RequestBodySpec {
            content_type: "multipart/form-data",
            description: None,
            schema: RequestBodySchema::MultipartFile {
                field_name: "file".to_owned(),
            },
            required: true,
        }),
        responses: vec![],
        handler_id: "test".to_owned(),
        authenticated: false,
        is_public: false,
        license_requirement: None,
        rate_limit: None,
        allowed_request_content_types: Some(vec!["multipart/form-data", "application/pdf"]),
        vendor_extensions: VendorExtensions::default(),
    }];

    let map = build_mime_validation_map(&specs);

    assert!(map.contains_key(&(Method::POST, "/files/v1/upload".to_owned())));
    let allowed = map
        .get(&(Method::POST, "/files/v1/upload".to_owned()))
        .unwrap();
    assert_eq!(allowed.len(), 2);
    assert!(allowed.contains(&"multipart/form-data"));
    assert!(allowed.contains(&"application/pdf"));
}

#[test]
fn test_content_type_parameter_stripping() {
    // Test the logic for stripping parameters from Content-Type
    let ct_with_charset = "application/json; charset=utf-8";
    let ct_main = ct_with_charset
        .split(';')
        .next()
        .map_or(ct_with_charset, str::trim);

    assert_eq!(ct_main, "application/json");

    // Test with multiple parameters
    let ct_complex = "multipart/form-data; boundary=----WebKitFormBoundary7MA4YWxkTrZu0gW";
    let ct_main2 = ct_complex.split(';').next().map_or(ct_complex, str::trim);

    assert_eq!(ct_main2, "multipart/form-data");

    // Test without parameters
    let ct_simple = "application/pdf";
    let ct_main3 = ct_simple.split(';').next().map_or(ct_simple, str::trim);

    assert_eq!(ct_main3, "application/pdf");
}
