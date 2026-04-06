//! RFC 9457 Problem Details for HTTP APIs (pure data model, no HTTP framework dependencies)

use http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[cfg(feature = "utoipa")]
use utoipa::ToSchema;

/// Content type for Problem Details as per RFC 9457.
pub const APPLICATION_PROBLEM_JSON: &str = "application/problem+json";

/// Custom serializer for `StatusCode` to u16
#[allow(clippy::trivially_copy_pass_by_ref)] // serde requires &T signature
fn serialize_status_code<S>(status: &StatusCode, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u16(status.as_u16())
}

/// Custom deserializer for `StatusCode` from u16
fn deserialize_status_code<'de, D>(deserializer: D) -> Result<StatusCode, D::Error>
where
    D: Deserializer<'de>,
{
    let code = u16::deserialize(deserializer)?;
    StatusCode::from_u16(code).map_err(serde::de::Error::custom)
}

/// RFC 9457 Problem Details for HTTP APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema))]
#[cfg_attr(
    feature = "utoipa",
    schema(
        title = "Problem",
        description = "RFC 9457 Problem Details for HTTP APIs"
    )
)]
#[must_use]
pub struct Problem {
    /// A URI reference that identifies the problem type.
    /// When dereferenced, it might provide human-readable documentation.
    #[serde(rename = "type")]
    pub type_url: String,
    /// A short, human-readable summary of the problem type.
    pub title: String,
    /// The HTTP status code for this occurrence of the problem.
    /// Serializes as u16 for RFC 9457 compatibility.
    #[serde(
        serialize_with = "serialize_status_code",
        deserialize_with = "deserialize_status_code"
    )]
    #[cfg_attr(feature = "utoipa", schema(value_type = u16))]
    pub status: StatusCode,
    /// A human-readable explanation specific to this occurrence of the problem.
    pub detail: String,
    /// A URI reference that identifies the specific occurrence of the problem.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    /// Optional machine-readable error code defined by the application.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// Optional trace id useful for tracing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Optional validation errors for 4xx problems.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<ValidationViolation>>,
    /// Optional structured context (e.g. `resource_type`, `resource_name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
}

/// Individual validation violation for a specific field or property.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema))]
#[cfg_attr(feature = "utoipa", schema(title = "ValidationViolation"))]
pub struct ValidationViolation {
    /// field path, e.g. "email" or "user.email"
    pub field: String,
    /// Human-readable message describing the validation error
    pub message: String,
    /// Optional machine-readable error code
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Collection of validation errors for 422 responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema))]
#[cfg_attr(feature = "utoipa", schema(title = "ValidationError"))]
pub struct ValidationError {
    /// List of individual validation violations
    pub errors: Vec<ValidationViolation>,
}

/// Wrapper for `ValidationError` that can be used as a standalone response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema))]
#[cfg_attr(feature = "utoipa", schema(title = "ValidationErrorResponse"))]
pub struct ValidationErrorResponse {
    /// The validation errors
    #[serde(flatten)]
    pub validation: ValidationError,
}

impl Problem {
    /// Create a new Problem with the given status, title, and detail.
    ///
    /// Note: This function accepts `http::StatusCode` for type safety.
    /// The status is serialized as `u16` for RFC 9457 compatibility.
    pub fn new(status: StatusCode, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            type_url: "about:blank".to_owned(),
            title: title.into(),
            status,
            detail: detail.into(),
            instance: String::new(),
            code: String::new(),
            trace_id: None,
            errors: None,
            context: None,
        }
    }

    pub fn with_type(mut self, type_url: impl Into<String>) -> Self {
        self.type_url = type_url.into();
        self
    }

    pub fn with_instance(mut self, uri: impl Into<String>) -> Self {
        self.instance = uri.into();
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into();
        self
    }

    pub fn with_trace_id(mut self, id: impl Into<String>) -> Self {
        self.trace_id = Some(id.into());
        self
    }

    pub fn with_errors(mut self, errors: Vec<ValidationViolation>) -> Self {
        self.errors = Some(errors);
        self
    }

    pub fn with_context(mut self, context: Value) -> Self {
        self.context = Some(context);
        self
    }
}

/// Axum integration: make Problem directly usable as a response.
///
/// Automatically enriches the Problem with `trace_id` from the current
/// tracing span if not already set.
#[cfg(feature = "axum")]
impl axum::response::IntoResponse for Problem {
    fn into_response(self) -> axum::response::Response {
        use axum::http::HeaderValue;

        // Enrich with trace_id from current span if not already set
        let problem = if self.trace_id.is_none() {
            match tracing::Span::current().id() {
                Some(span_id) => self.with_trace_id(span_id.into_u64().to_string()),
                _ => self,
            }
        } else {
            self
        };

        let status = problem.status;
        let mut resp = axum::Json(problem).into_response();
        *resp.status_mut() = status;
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static(APPLICATION_PROBLEM_JSON),
        );
        resp
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "problem_tests.rs"]
mod problem_tests;
