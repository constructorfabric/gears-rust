use axum::{
    http::StatusCode,
    response::{Html, Json},
    routing::{MethodRouter, get},
};
use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};

/// Returns a 501 Not Implemented handler for operations without implementations
#[allow(dead_code)]
pub fn placeholder_handler_501() -> MethodRouter {
    get(|| async move {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "message": "Handler not implemented - will be routed via gRPC in future",
                "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
            })),
        )
    })
}

pub async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "healthy",
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    }))
}

#[cfg(not(feature = "embed_elements"))]
pub fn serve_docs(prefix_path: &str) -> Html<String> {
    let openapi_url = if prefix_path.is_empty() {
        String::from("/openapi.json")
    } else {
        format!("{prefix_path}/openapi.json")
    };
    // External mode: load from CDN @latest
    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8"/>
  <title>API Docs</title>
  <script src="https://unpkg.com/@stoplight/elements@latest/web-components.min.js"></script>
  <link rel="stylesheet" href="https://unpkg.com/@stoplight/elements@latest/styles.min.css">
</head>
<body>
  <elements-api apiDescriptionUrl="{openapi_url}" router="hash" layout="sidebar"></elements-api>
</body>
</html>"#,
    ))
}

#[cfg(feature = "embed_elements")]
pub fn serve_docs(prefix_path: &str) -> Html<String> {
    let (openapi_url, js_url, css_url) = if prefix_path.is_empty() {
        (
            String::from("/openapi.json"),
            String::from("/docs/assets/web-components.min.js"),
            String::from("/docs/assets/styles.min.css"),
        )
    } else {
        (
            format!("{prefix_path}/openapi.json"),
            format!("{prefix_path}/docs/assets/web-components.min.js"),
            format!("{prefix_path}/docs/assets/styles.min.css"),
        )
    };

    // Embedded mode: reference local embedded assets
    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8"/>
  <title>API Docs</title>
  <script src="{js_url}"></script>
  <link rel="stylesheet" href="{css_url}">
</head>
<body>
  <elements-api apiDescriptionUrl="{openapi_url}" router="hash" layout="sidebar"></elements-api>
</body>
</html>"#,
    ))
}
