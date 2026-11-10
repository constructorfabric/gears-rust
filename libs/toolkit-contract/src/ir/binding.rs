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
    /// Whether this binding represents a server-streaming endpoint.
    #[serde(default)]
    pub streaming: bool,
    /// Wire framing for a streaming binding. Meaningless when
    /// [`streaming`](Self::streaming) is `false`, where it stays at its
    /// default.
    ///
    /// Kept alongside `streaming` rather than folded into an
    /// `Option<StreamFraming>`: `streaming` is read on its own by IR validation
    /// and the `OpenAPI` back-ends, and collapsing the two would be a semantic
    /// change to an already-serialized shape for no gain.
    ///
    /// `#[serde(default)]` so IR serialized before this field existed
    /// deserializes to [`StreamFraming::ServerSentEvents`], the historical
    /// behavior. That buys nothing for Rust construction — the struct is
    /// deliberately not `#[non_exhaustive]` because the macro emits a struct
    /// literal of it — so a new construction site must name the field.
    #[serde(default)]
    pub stream_framing: StreamFraming,
    /// Whether the underlying contract method has a default body (peers
    /// MAY omit this endpoint). Mirrors `MethodIr.optional`.
    #[serde(default)]
    pub optional: bool,
}

/// HTTP method verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// HTTP HEAD. Only valid in a passthrough method set.
    Head,
    /// HTTP OPTIONS. Only valid in a passthrough method set.
    Options,
    /// The request method comes from the opaque request at call time, limited
    /// to the set in the method's [`HttpFieldBinding::Passthrough`] binding.
    Passthrough,
}

impl HttpMethod {
    /// Upper-case wire name, or `None` for [`Self::Passthrough`], which names
    /// no single method.
    #[must_use]
    pub const fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Get => Some("GET"),
            Self::Post => Some("POST"),
            Self::Put => Some("PUT"),
            Self::Patch => Some("PATCH"),
            Self::Delete => Some("DELETE"),
            Self::Head => Some("HEAD"),
            Self::Options => Some("OPTIONS"),
            Self::Passthrough => None,
        }
    }
}

/// Wire framing for a server-streaming HTTP binding.
///
/// Two framings at two paths are two operations, not variants of one — this
/// selects which wire format a single streaming binding speaks, not a
/// content-negotiation set.
///
/// `#[non_exhaustive]` is safe here (unlike [`HttpMethodBindingIr`], which the
/// macro emits as a struct literal): `#[toolkit::rest_contract]` only ever
/// emits a *variant path* of this enum, which `#[non_exhaustive]` permits.
///
/// `Default` is [`Self::ServerSentEvents`] so IR serialized before a framing
/// selector existed deserializes to the historical behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum StreamFraming {
    /// `text/event-stream` — W3C Server-Sent Events. The historical default.
    #[default]
    ServerSentEvents,
    /// `multipart/mixed` — one JSON item per body part.
    MultipartMixed,
}

impl StreamFraming {
    /// Every framing variant, in declaration order.
    ///
    /// Lets consumers derive the set of streaming media types from the enum
    /// (via [`media_type`](Self::media_type)) instead of hard-coding the
    /// strings — e.g. the `OpenAPI` registry deciding which response media types
    /// carry a JSON *item* schema rather than an opaque string. Kept complete by
    /// the exhaustiveness guard below, so a new variant cannot be silently
    /// omitted.
    pub const ALL: &'static [Self] = &[Self::ServerSentEvents, Self::MultipartMixed];

    /// The media type the client advertises in `Accept` and the server emits
    /// in `Content-Type`.
    ///
    /// For `multipart/mixed` the emitted header additionally carries a
    /// runtime-generated `boundary=` parameter, which is not part of this
    /// value — see `toolkit::http::multipart::MultipartJsonStream`.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::ServerSentEvents => "text/event-stream",
            Self::MultipartMixed => "multipart/mixed",
        }
    }

    /// Whether `media_type` is the media type of some streaming framing.
    ///
    /// Derived from [`ALL`](Self::ALL) so a new framing is included
    /// automatically; the `OpenAPI` registry uses it to decide whether a response
    /// media type renders its schema as a `$ref` (streaming JSON item) rather
    /// than an opaque string.
    #[must_use]
    pub fn is_stream_media_type(media_type: &str) -> bool {
        Self::ALL
            .iter()
            .any(|framing| framing.media_type() == media_type)
    }
}

// Exhaustiveness guard for [`StreamFraming::ALL`]. `#[non_exhaustive]` only
// forces a wildcard on *downstream* crates; within this crate this match must
// cover every variant, so adding one fails to compile here — the reminder to
// append it to `ALL` above (and to `ALL.len()` below).
const _: () = {
    fn _assert_all_variants_listed(framing: StreamFraming) {
        match framing {
            StreamFraming::ServerSentEvents | StreamFraming::MultipartMixed => {}
        }
    }
    // A variant added to the match but not to `ALL` (or vice versa) trips this.
    assert!(StreamFraming::ALL.len() == 2);
};

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
    /// The field is an opaque HTTP request forwarded as is (ADR-0009 §5).
    ///
    /// Only valid with [`HttpMethod::Passthrough`]. The request path and query
    /// fill the trailing `{*path_param}` wildcard of the template; its method
    /// must be one of `methods`. Request and response bodies are opaque: no JSON
    /// schema is implied for either.
    Passthrough {
        /// Name of the request field in `InputShape`.
        field: String,
        /// Name of the trailing wildcard parameter in the path template.
        param: String,
        /// HTTP methods the request may use.
        methods: Vec<HttpMethod>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ir_still_deserializes_and_passthrough_round_trips() {
        let legacy: HttpMethodBindingIr = serde_json::from_str(
            r#"{"method_name":"get","http_method":"Get","path_template":"/x","field_bindings":["Body"]}"#,
        )
        .unwrap();
        assert_eq!(legacy.http_method, HttpMethod::Get);

        let passthrough = HttpMethodBindingIr {
            method_name: "proxy".into(),
            http_method: HttpMethod::Passthrough,
            path_template: "/proxy/{*path}".into(),
            field_bindings: vec![HttpFieldBinding::Passthrough {
                field: "req".into(),
                param: "path".into(),
                methods: vec![HttpMethod::Get, HttpMethod::Options],
            }],
            retryable: false,
            streaming: false,
            stream_framing: StreamFraming::default(),
            optional: false,
        };
        let json = serde_json::to_string(&passthrough).unwrap();
        let back: HttpMethodBindingIr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.http_method, HttpMethod::Passthrough);
        assert!(matches!(
            back.field_bindings.as_slice(),
            [HttpFieldBinding::Passthrough { methods, .. }] if methods == &[HttpMethod::Get, HttpMethod::Options]
        ));
    }

    #[test]
    fn every_framing_media_type_is_recognised_as_streaming() {
        // Locks the derivation the OpenAPI registry relies on: each variant's
        // own media type must be recognised, so a streaming response renders
        // its item `$ref` rather than an opaque string.
        for framing in StreamFraming::ALL {
            assert!(
                StreamFraming::is_stream_media_type(framing.media_type()),
                "{framing:?} media type not recognised as streaming",
            );
        }
    }

    #[test]
    fn non_streaming_media_types_are_not_recognised() {
        assert!(!StreamFraming::is_stream_media_type("application/json"));
        assert!(!StreamFraming::is_stream_media_type("text/plain"));
    }
}
