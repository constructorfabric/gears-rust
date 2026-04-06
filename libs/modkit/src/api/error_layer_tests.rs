use super::*;

#[test]
fn test_odata_error_mapping() {
    let error = ODataError::InvalidFilter("malformed".to_owned());
    let problem = error.into_problem("/tests/v1/test", Some("trace123".to_owned()));

    assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(problem.code.contains("invalid_filter"));
    assert_eq!(problem.instance, "/tests/v1/test");
    assert_eq!(problem.trace_id, Some("trace123".to_owned()));
}

#[test]
fn test_config_error_mapping() {
    let error = ConfigError::ModuleNotFound {
        module: "test_module".to_owned(),
    };
    let problem = error.into_problem("/tests/v1/test", None);

    assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(problem.code, "CONFIG_MODULE_NOT_FOUND");
    assert_eq!(problem.instance, "/tests/v1/test");
    assert!(problem.detail.contains("test_module"));
}

#[test]
fn test_anyhow_error_mapping() {
    let error = anyhow::anyhow!("Something went wrong");
    let problem = error.into_problem("/tests/v1/test", Some("trace456".to_owned()));

    assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(problem.code, "INTERNAL_ERROR");
    assert_eq!(problem.instance, "/tests/v1/test");
    assert_eq!(problem.trace_id, Some("trace456".to_owned()));
}

#[test]
fn test_config_var_expand_error_sanitizes_detail() {
    let source = modkit_utils::var_expand::ExpandVarsError::Var {
        name: "SECRET_API_KEY".to_owned(),
        source: std::env::VarError::NotPresent,
    };
    let error = ConfigError::VarExpand {
        module: "my_mod".to_owned(),
        source,
    };
    let problem = error.into_problem("/tests/v1/test", Some("trace789".to_owned()));

    assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(problem.code, "CONFIG_ENV_EXPAND");
    assert_eq!(
        problem.type_url,
        "https://errors.example.com/CONFIG_ENV_EXPAND"
    );
    assert_eq!(problem.instance, "/tests/v1/test");
    assert_eq!(problem.trace_id, Some("trace789".to_owned()));

    // Detail MUST NOT leak the env var name or the underlying error message.
    assert!(
        !problem.detail.contains("SECRET_API_KEY"),
        "detail must not contain env var name, got: {}",
        problem.detail,
    );
    assert!(
        !problem.detail.contains("not present"),
        "detail must not contain source error text, got: {}",
        problem.detail,
    );
    // It should still mention the module name (non-sensitive).
    assert!(problem.detail.contains("my_mod"));
}

#[test]
fn test_extract_trace_id_from_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("x-trace-id", "test-trace-123".parse().unwrap());

    let trace_id = extract_trace_id(&headers);
    assert_eq!(trace_id, Some("test-trace-123".to_owned()));
}
