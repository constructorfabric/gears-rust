use serde::{Deserialize, Serialize};

/// HTTP binding projection for a contract.
///
/// Deliberately NOT `#[non_exhaustive]` (see [`super::contract::ContractIr`]'s
/// doc): `#[toolkit::rest_contract]` emits a struct-literal `HttpBindingIr { .. }`
/// into the SDK crate's generated `<trait>_http_binding()` function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpBindingIr {
    /// Base path prefix.
    pub base_path: String,
    /// Per-method HTTP bindings.
    pub methods: Vec<HttpMethodBindingIr>,
}

impl HttpBindingIr {
    /// Find the binding for a specific method by name.
    #[must_use]
    pub fn find_method(&self, method_name: &str) -> Option<&HttpMethodBindingIr> {
        self.methods.iter().find(|m| m.method_name == method_name)
    }
}

/// HTTP binding for a single method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpMethodBindingIr {
    /// Method name, matching a `MethodIr.name` in the contract.
    pub method_name: String,
    /// HTTP method.
    pub http_method: HttpMethod,
    /// Path template relative to `base_path`.
    pub path_template: String,
    /// How each input field maps to the HTTP request.
    pub field_bindings: Vec<HttpFieldBinding>,
    /// Whether the client may retry this call automatically when the
    /// transport fails or the response is a retryable HTTP status.
    #[serde(default)]
    pub retryable: bool,
    /// Whether this binding represents a server-streaming endpoint
    /// (Server-Sent Events).
    #[serde(default)]
    pub streaming: bool,
    /// Whether the underlying contract method has a default body (peers
    /// MAY omit this endpoint). Mirrors `MethodIr.optional`.
    #[serde(default)]
    pub optional: bool,
}

/// HTTP method verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HttpMethod {
    /// HTTP GET.
    Get,
    /// HTTP POST.
    Post,
    /// HTTP PUT.
    Put,
    /// HTTP PATCH.
    Patch,
    /// HTTP DELETE.
    Delete,
}

/// How an input field is bound to the HTTP request.
///
/// `#[non_exhaustive]` at the enum level only: codegen only ever constructs
/// the variants that exist today (`Path`/`Query`/`Body`), so this doesn't
/// block macro-generated construction — it only forces downstream `match`
/// arms to include a wildcard, so adding a future binding kind isn't a
/// breaking change for crates that match on this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HttpFieldBinding {
    /// Field value goes into a URL path parameter.
    Path {
        /// Name of the field in `InputShape`.
        field: String,
        /// Name of the path parameter in the template.
        param: String,
    },
    /// Field value goes into a query parameter.
    Query {
        /// Name of the field in `InputShape`.
        field: String,
        /// Name of the query parameter.
        param: String,
    },
    /// Field value goes into the request body.
    Body,
}
