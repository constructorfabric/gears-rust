use serde::{Deserialize, Serialize};

// `GTS_ID_PREFIX` is the compile-time configured GTS identifier prefix
// (overridable via the `GTS_ID_PREFIX` env var at build
// time). Used to assemble the canonical error type id prefix without
// hard-coding the literal prefix.
use toolkit_gts::{GTS_ID_PREFIX, GTS_ID_URI_PREFIX, gts_uri};

use crate::context::{
    Aborted, AlreadyExists, Cancelled, DataLoss, DeadlineExceeded, FailedPrecondition, Internal,
    InvalidArgument, NotFound, OutOfRange, PermissionDenied, ResourceExhausted, ServiceUnavailable,
    Unauthenticated, Unimplemented, Unknown,
};
use crate::error::CanonicalError;

/// Media type for RFC 9457 `application/problem+json` responses.
pub const APPLICATION_PROBLEM_JSON: &str = "application/problem+json";

// ---------------------------------------------------------------------------
// Problem (RFC 9457)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Problem {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub context: serde_json::Value,
}

impl Problem {
    /// Convert a `CanonicalError` to a `Problem`.
    ///
    /// # Errors
    ///
    /// Returns `serde_json::Error` if the error-category context type
    /// fails to serialize.  Built-in context types are plain structs and
    /// should never fail, but this keeps the failure visible rather than
    /// silently producing an empty `"context": {}`.
    pub fn from_error(err: &CanonicalError) -> Result<Self, serde_json::Error> {
        let problem_type = gts_uri!(err.gts_type());
        let title = err.title().to_owned();
        let status = err.status_code();
        let detail = err.detail().to_owned();

        let mut context = serialize_context(err)?;

        if let Some(rt) = err.resource_type() {
            context["resource_type"] = serde_json::Value::String(rt.to_owned());
        }

        if let Some(rn) = err.resource_name() {
            context["resource_name"] = serde_json::Value::String(rn.to_owned());
        }

        Ok(Problem {
            problem_type,
            title,
            status,
            detail,
            instance: None,
            trace_id: None,
            context,
        })
    }

    /// Convert a `CanonicalError` to a `Problem`, including the internal
    /// diagnostic string in the `context` for `Internal` and `Unknown`
    /// variants.
    ///
    /// **This method MUST NOT be used in production.** It exists so that
    /// development and test environments can surface the real error cause
    /// in the wire response for easier debugging.
    ///
    /// In production, use [`from_error`](Self::from_error) instead — it
    /// never leaks the diagnostic string.
    ///
    /// # Errors
    ///
    /// Returns `serde_json::Error` if the context fails to serialize.
    pub fn from_error_debug(err: &CanonicalError) -> Result<Self, serde_json::Error> {
        let mut problem = Self::from_error(err)?;

        if let Some(diag) = err.diagnostic() {
            problem.context["description"] = serde_json::Value::String(diag.to_owned());
        }

        Ok(problem)
    }

    /// Set the `trace_id` field, returning `self` for chaining.
    #[must_use]
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    /// Set the `instance` field, returning `self` for chaining.
    #[must_use]
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }
}

fn serialize_context(err: &CanonicalError) -> Result<serde_json::Value, serde_json::Error> {
    match err {
        CanonicalError::Cancelled { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::Unknown { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::InvalidArgument { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::DeadlineExceeded { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::NotFound { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::AlreadyExists { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::PermissionDenied { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::ResourceExhausted { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::FailedPrecondition { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::Aborted { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::OutOfRange { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::Unimplemented { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::Internal { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::ServiceUnavailable { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::DataLoss { ctx, .. } => serde_json::to_value(ctx),
        CanonicalError::Unauthenticated { ctx, .. } => serde_json::to_value(ctx),
    }
}

// `Problem.context` is `serde_json::Value`, so stringifying the serialization
// error is the intended fallback here. The original CanonicalError is already
// preserved in the other Problem fields.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<CanonicalError> for Problem {
    fn from(err: CanonicalError) -> Self {
        match Problem::from_error(&err) {
            Ok(p) => p,
            Err(ser_err) => Problem {
                problem_type: gts_uri!(err.gts_type()),
                title: err.title().to_owned(),
                status: err.status_code(),
                detail: err.detail().to_owned(),
                instance: None,
                trace_id: None,
                context: serde_json::Value::String(ser_err.to_string()),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Round-trip: Problem → CanonicalError
//
// Reverse direction of `From<CanonicalError> for Problem`. Out-of-process SDK
// consumers receive `application/problem+json` over the wire, deserialize into
// `Problem`, and reconstruct the typed `CanonicalError` via this `TryFrom`.
// In-process consumers do not need this hop — they hold `CanonicalError`
// directly from the ClientHub call.
//
// Lossy by design:
// * `Internal.description` and `Unknown.description` are `#[serde(skip)]` on
//   the wire, so they reconstruct as empty strings. This is intentional —
//   production wire responses never carry the server-side diagnostic.
// * Transport fields (`instance`, `trace_id`) live on `Problem`, not on
//   `CanonicalError`. Callers that need them should read them off the
//   `Problem` before converting.
// ---------------------------------------------------------------------------

/// Prefix on `Problem.problem_type` produced by the forward conversion.
const PROBLEM_TYPE_PREFIX: &str = GTS_ID_URI_PREFIX;

/// Reasons a `Problem` cannot be reconstructed as a `CanonicalError`.
#[derive(Debug, thiserror::Error)]
pub enum ProblemConversionError {
    /// The `problem_type` URI does not match any of the 16 canonical
    /// category identifiers. Either the server emitted a non-canonical
    /// problem or the wire format has drifted.
    #[error("unrecognized problem_type: {0}")]
    UnknownProblemType(String),

    /// The `context` payload could not be deserialized into the context
    /// type for the matched category. The category and underlying serde
    /// error are surfaced for diagnostics.
    #[error("invalid context for canonical category {category}: {source}")]
    InvalidContext {
        category: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// Prefix of every canonical GTS identifier. Stripped to expose the category
/// name (e.g. `cancelled`, `invalid_argument`). Not a complete GTS string by
/// itself — only the concatenation `{prefix}{category}{suffix}` is a valid
/// GTS identifier.
#[allow(unknown_lints, de0901_gts_string_pattern)]
/// Prefix of every canonical GTS error type id. Built by concatenating the
/// configured GTS ID prefix (overridable via `GTS_ID_PREFIX`
/// at compile time) with the fixed "cf.core.errors.err.v1~cf.core.err."
/// suffix. This is *not* a complete GTS id (the trailing per-error token is
/// appended at runtime in `CanonicalError::gts_type`), so `gts_id!` (which
/// validates a full id at macro-expansion time) is not applicable here.
/// `concat!` also cannot be used since it only accepts literals; the value
/// is therefore materialised once via a `OnceLock` and exposed as a
/// `&'static str`.
fn gts_type_prefix() -> &'static str {
    use std::sync::OnceLock;
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| format!("{GTS_ID_PREFIX}cf.core.errors.err.v1~cf.core.err."))
}
/// Suffix of every canonical GTS identifier. See [`gts_type_prefix`].
const GTS_TYPE_SUFFIX: &str = ".v1~";

/// Strip the canonical problem-type URI down to
/// `<category>`. Returns `None` if the URI doesn't match the canonical shape.
fn category_from_problem_type(problem_type: &str) -> Option<&str> {
    let rest = problem_type.strip_prefix(PROBLEM_TYPE_PREFIX)?;
    let after_prefix = rest.strip_prefix(gts_type_prefix())?;
    after_prefix.strip_suffix(GTS_TYPE_SUFFIX)
}

/// Extract `resource_type` and `resource_name` from the `Problem.context`
/// JSON, returning the pair (both `None` if absent). The forward conversion
/// stamps these as plain string fields alongside the category-specific
/// payload; here we read them back without disturbing the serde
/// deserialization of the category context (serde ignores unknown fields).
fn extract_resource_fields(context: &serde_json::Value) -> (Option<String>, Option<String>) {
    let resource_type = context
        .get("resource_type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let resource_name = context
        .get("resource_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    (resource_type, resource_name)
}

fn deserialize_ctx<T>(
    category: &'static str,
    context: serde_json::Value,
) -> Result<T, ProblemConversionError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(context)
        .map_err(|source| ProblemConversionError::InvalidContext { category, source })
}

impl TryFrom<Problem> for CanonicalError {
    type Error = ProblemConversionError;

    fn try_from(problem: Problem) -> Result<Self, Self::Error> {
        let category = category_from_problem_type(&problem.problem_type).ok_or_else(|| {
            ProblemConversionError::UnknownProblemType(problem.problem_type.clone())
        })?;

        let detail = problem.detail;
        let (resource_type, resource_name) = extract_resource_fields(&problem.context);
        let ctx_value = problem.context;

        let canonical = match category {
            "cancelled" => Self::Cancelled {
                ctx: deserialize_ctx::<Cancelled>("cancelled", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "unknown" => Self::Unknown {
                ctx: deserialize_ctx::<Unknown>("unknown", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "invalid_argument" => Self::InvalidArgument {
                ctx: deserialize_ctx::<InvalidArgument>("invalid_argument", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "deadline_exceeded" => Self::DeadlineExceeded {
                ctx: deserialize_ctx::<DeadlineExceeded>("deadline_exceeded", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "not_found" => Self::NotFound {
                ctx: deserialize_ctx::<NotFound>("not_found", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "already_exists" => Self::AlreadyExists {
                ctx: deserialize_ctx::<AlreadyExists>("already_exists", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "permission_denied" => Self::PermissionDenied {
                ctx: deserialize_ctx::<PermissionDenied>("permission_denied", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "resource_exhausted" => Self::ResourceExhausted {
                ctx: deserialize_ctx::<ResourceExhausted>("resource_exhausted", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "failed_precondition" => Self::FailedPrecondition {
                ctx: deserialize_ctx::<FailedPrecondition>("failed_precondition", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "aborted" => Self::Aborted {
                ctx: deserialize_ctx::<Aborted>("aborted", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "out_of_range" => Self::OutOfRange {
                ctx: deserialize_ctx::<OutOfRange>("out_of_range", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "unimplemented" => Self::Unimplemented {
                ctx: deserialize_ctx::<Unimplemented>("unimplemented", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "internal" => Self::Internal {
                // `Internal.description` is `#[serde(skip)]`; the wire
                // never carries it, so it reconstructs as an empty string.
                ctx: deserialize_ctx::<Internal>("internal", ctx_value)?,
                detail,
            },
            "service_unavailable" => Self::ServiceUnavailable {
                ctx: deserialize_ctx::<ServiceUnavailable>("service_unavailable", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "data_loss" => Self::DataLoss {
                ctx: deserialize_ctx::<DataLoss>("data_loss", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            "unauthenticated" => Self::Unauthenticated {
                ctx: deserialize_ctx::<Unauthenticated>("unauthenticated", ctx_value)?,
                detail,
                resource_type,
                resource_name,
            },
            _ => {
                return Err(ProblemConversionError::UnknownProblemType(
                    problem.problem_type,
                ));
            }
        };

        Ok(canonical)
    }
}

// ---------------------------------------------------------------------------
// axum integration (feature = "axum")
// ---------------------------------------------------------------------------

#[cfg(feature = "axum")]
impl axum::response::IntoResponse for Problem {
    fn into_response(self) -> axum::response::Response {
        match serde_json::to_vec(&self) {
            Ok(body) => {
                let status = http::StatusCode::from_u16(self.status)
                    .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);
                (
                    status,
                    [(http::header::CONTENT_TYPE, APPLICATION_PROBLEM_JSON)],
                    body,
                )
                    .into_response()
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    problem_type = %self.problem_type,
                    status = self.status,
                    "failed to serialize Problem; emitting fallback body",
                );
                let body = format!(
                    r#"{{"type":"{}{}internal{}","title":"Internal","status":500,"detail":"failed to serialize problem","context":{{}}}}"#,
                    PROBLEM_TYPE_PREFIX,
                    gts_type_prefix(),
                    GTS_TYPE_SUFFIX
                );
                (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    [(http::header::CONTENT_TYPE, APPLICATION_PROBLEM_JSON)],
                    body,
                )
                    .into_response()
            }
        }
    }
}

#[cfg(feature = "axum")]
impl axum::response::IntoResponse for CanonicalError {
    fn into_response(self) -> axum::response::Response {
        // Stash a clone of self into the response extensions so the canonical
        // error middleware (DESIGN.md §3.6) can recover `diagnostic()` and log
        // the unredacted description server-side without putting it on the
        // wire. The `description` fields on `Internal` / `Unknown` are
        // `#[serde(skip)]`, so the bytes-roundtrip path alone cannot surface
        // them.
        let for_extension = self.clone();
        let mut response = Problem::from(self).into_response();
        response.extensions_mut().insert(for_extension);
        response
    }
}

// ---------------------------------------------------------------------------
// utoipa integration (feature = "utoipa")
// ---------------------------------------------------------------------------

#[cfg(feature = "utoipa")]
impl utoipa::PartialSchema for Problem {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, SchemaFormat, SchemaType, Type};

        ObjectBuilder::new()
            .property(
                "type",
                ObjectBuilder::new().schema_type(SchemaType::Type(Type::String)),
            )
            .required("type")
            .property(
                "title",
                ObjectBuilder::new().schema_type(SchemaType::Type(Type::String)),
            )
            .required("title")
            .property(
                "status",
                ObjectBuilder::new()
                    .schema_type(SchemaType::Type(Type::Integer))
                    .format(Some(SchemaFormat::KnownFormat(KnownFormat::Int32))),
            )
            .required("status")
            .property(
                "detail",
                ObjectBuilder::new().schema_type(SchemaType::Type(Type::String)),
            )
            .required("detail")
            .property(
                "instance",
                ObjectBuilder::new().schema_type(SchemaType::Type(Type::String)),
            )
            .property(
                "trace_id",
                ObjectBuilder::new().schema_type(SchemaType::Type(Type::String)),
            )
            .property(
                "context",
                ObjectBuilder::new().schema_type(SchemaType::Type(Type::Object)),
            )
            .required("context")
            .description(Some(
                "RFC 9457 problem+json. `context` varies by error category.",
            ))
            .into()
    }
}

#[cfg(feature = "utoipa")]
impl utoipa::ToSchema for Problem {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Problem")
    }
}
