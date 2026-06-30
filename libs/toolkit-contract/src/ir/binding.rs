use serde::{Deserialize, Serialize};

/// HTTP binding projection for a contract.
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
pub enum HttpMethod {
    /// HTTP GET.
    Get,
    /// HTTP POST.
    Post,
    /// HTTP PUT.
    Put,
    /// HTTP DELETE.
    Delete,
}

/// How an input field is bound to the HTTP request.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Field value goes into an HTTP header.
    Header {
        /// Name of the field in `InputShape`.
        field: String,
        /// Name of the HTTP header.
        header: String,
    },
}
