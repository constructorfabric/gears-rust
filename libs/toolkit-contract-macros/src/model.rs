pub struct ContractModel {
    pub gear: String,
    pub version: String,
    pub trait_name: syn::Ident,
    pub vis: syn::Visibility,
    pub supertraits: syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>,
    pub methods: Vec<MethodModel>,
    pub attrs: Vec<syn::Attribute>,
    pub kind: ContractKind,
}

/// Mirror of `toolkit_contract::descriptor::ContractKind` used inside the
/// macro crate. Codegen converts it back to absolute-path token form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractKind {
    Api,
    Embedded,
    Backend,
    Extension,
}

impl ContractKind {
    /// Match a trait-name suffix to a [`ContractKind`].
    ///
    /// A trailing major-version marker is ignored, so `PaymentApiV2` classifies
    /// as [`ContractKind::Api`] exactly like `PaymentApi` (ADR-0007: parallel
    /// versioned traits). See [`strip_version_suffix`].
    #[must_use]
    pub fn from_suffix(name: &str) -> Option<Self> {
        let name = crate::support::strip_version_suffix(name);
        if name.ends_with("Api") {
            Some(ContractKind::Api)
        } else if name.ends_with("Embedded") {
            Some(ContractKind::Embedded)
        } else if name.ends_with("Backend") {
            Some(ContractKind::Backend)
        } else if name.ends_with("Extension") {
            Some(ContractKind::Extension)
        } else {
            None
        }
    }

    /// Whether this kind permits a transport projection (`*Rest`, `*Grpc`).
    ///
    /// Consumed by the REST and gRPC projection parsers to gate remote
    /// capability, so the suffix rule is encoded here only.
    #[must_use]
    pub const fn is_remote_capable(self) -> bool {
        matches!(self, ContractKind::Api | ContractKind::Backend)
    }

    /// The contract-type suffix this kind corresponds to, for diagnostics.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            ContractKind::Api => "Api",
            ContractKind::Embedded => "Embedded",
            ContractKind::Backend => "Backend",
            ContractKind::Extension => "Extension",
        }
    }
}

pub struct MethodModel {
    pub name: syn::Ident,
    pub kind: MethodKind,
    /// How a server-streaming method's stream is obtained. Meaningless when
    /// `kind` is [`MethodKind::Unary`], where it stays at its default.
    pub open: StreamOpen,
    pub idempotency: Idempotency,
    pub params: Vec<ParamModel>,
    pub output_type: syn::Type,
    pub error_type: syn::Type,
    pub attrs: Vec<syn::Attribute>,
    pub sig: syn::Signature,
    /// `true` when the trait declares a default body — peers MAY omit
    /// this method (`PoC` convention).
    pub optional: bool,
}

pub struct ParamModel {
    pub name: syn::Ident,
    pub ty: syn::Type,
    pub role: ParamRole,
}

/// Semantic role of a contract method parameter as determined by the macro
/// front-end. Mirrors `toolkit_contract::ir::contract::FieldRole`; emitted into
/// the IR via codegen so back-ends (protogen, `OpenAPI`) can filter without
/// re-running a name/type heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParamRole {
    #[default]
    Wire,
    SecurityContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Unary,
    ServerStreaming,
}

/// How a server-streaming method's stream is obtained.
///
/// [`StreamOpen::Immediate`] is the historical shape: the method is a plain
/// `fn` and hands back the stream synchronously, so an open-time failure can
/// only be reported as the stream's first item. [`StreamOpen::Awaited`] is
/// `async fn` and returns `Result<Stream, E>`, so the open is a distinct,
/// fallible operation that completes before any item exists.
///
/// In the base [`MethodModel`] this is carried beside [`MethodKind`] rather
/// than folded into it, because `MethodKind` is *also* emitted into the runtime
/// IR descriptor (`method_kind_tokens`) — a second role a shape enum would not
/// carry. The REST/gRPC method models instead use [`MethodShape`], which does
/// fold the streaming flag and the open together (they have no such second
/// role).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamOpen {
    /// `#[streaming] fn` — the stream is returned synchronously.
    #[default]
    Immediate,
    /// `#[streaming] async fn` — the open is awaited and may fail before the
    /// first item.
    Awaited,
}

/// The shape of a projected contract method: unary, or server-streaming with a
/// given [`StreamOpen`] strategy.
///
/// Replaces the independent `streaming: bool` (REST) / `server_streaming: bool`
/// (gRPC) flag plus a separate `open: StreamOpen` field. That pair could
/// represent `(streaming = false, open = Awaited)` — meaningless, since `open`
/// is only consulted for a streaming method — and the two were read at ~20
/// sites where nothing tied them together. Folding them here makes the illegal
/// state unrepresentable: an `open` exists exactly when the method streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodShape {
    /// A unary request/response method.
    Unary,
    /// A server-streaming method, tagged with how its stream is opened.
    Stream(StreamOpen),
}

impl MethodShape {
    /// `true` for any streaming shape.
    #[must_use]
    pub const fn is_streaming(self) -> bool {
        matches!(self, Self::Stream(_))
    }

    /// The open strategy for a streaming method, or `None` when unary.
    #[must_use]
    pub const fn stream_open(self) -> Option<StreamOpen> {
        match self {
            Self::Stream(open) => Some(open),
            Self::Unary => None,
        }
    }
}

/// Wire framing for a server-streaming REST method, selected by
/// `#[streaming(sse | multipart_mixed)]`.
///
/// A macro-local mirror of `toolkit_contract::ir::binding::StreamFraming`. The
/// two cannot be one definition: this is a proc-macro crate that the contract
/// crate depends on, not the other way round. What crosses the gap is
/// [`Self::ir_variant`] — the variant path the emitted binding IR and generated
/// client name — and [`Self::media_type`], which MUST agree with the runtime
/// enum's own `media_type`, since the client advertises this value in `Accept`
/// while the runtime parser is selected from the variant.
///
/// Framing *parameterises* `#[streaming]` rather than being an attribute of its
/// own: which wire format a stream speaks is intrinsic to what the marker
/// means, not orthogonal configuration alongside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamFraming {
    /// `text/event-stream` — W3C Server-Sent Events. What a bare
    /// `#[streaming]` has always meant, and still does.
    #[default]
    ServerSentEvents,
    /// `multipart/mixed` — one JSON item per body part.
    MultipartMixed,
}

impl StreamFraming {
    /// Variant name in `toolkit_contract::ir::binding::StreamFraming`.
    pub fn ir_variant(self) -> &'static str {
        match self {
            StreamFraming::ServerSentEvents => "ServerSentEvents",
            StreamFraming::MultipartMixed => "MultipartMixed",
        }
    }

    /// Media type the generated client advertises in `Accept`. Must agree with
    /// the runtime enum's `media_type` for the same variant.
    pub fn media_type(self) -> &'static str {
        match self {
            StreamFraming::ServerSentEvents => "text/event-stream",
            StreamFraming::MultipartMixed => "multipart/mixed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotency {
    SafeRead,
    IdempotentWrite,
    NonIdempotentWrite,
}
