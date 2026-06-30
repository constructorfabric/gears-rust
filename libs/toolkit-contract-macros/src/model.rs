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
    #[must_use]
    pub fn from_suffix(name: &str) -> Option<Self> {
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

    /// Whether this kind permits a transport projection.
    #[must_use]
    #[allow(dead_code, reason = "consumed by future projection validation")]
    pub const fn is_remote_capable(self) -> bool {
        matches!(self, ContractKind::Api | ContractKind::Backend)
    }
}

pub struct MethodModel {
    pub name: syn::Ident,
    pub kind: MethodKind,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotency {
    SafeRead,
    IdempotentWrite,
    NonIdempotentWrite,
}
