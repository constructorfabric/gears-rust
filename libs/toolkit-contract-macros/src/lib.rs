#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use proc_macro::TokenStream;
use syn::parse_macro_input;

mod codegen;
mod consumes;
mod contract_error;
mod grpc_contract;
mod grpc_contract_parse;
mod model;
mod parse;
mod projection;
mod proto_bridge;
mod provides;
mod query_params;
mod rest_contract;
mod rest_contract_parse;
mod support;

#[proc_macro_attribute]
pub fn contract(attr: TokenStream, item: TokenStream) -> TokenStream {
    let contract_attr = parse_macro_input!(attr as parse::ContractAttr);
    let item_trait = parse_macro_input!(item as syn::ItemTrait);

    match parse::parse_trait(contract_attr, &item_trait) {
        Ok(model) => codegen::generate(&model).into(),
        Err(err) => err.to_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn rest_contract(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = parse_macro_input!(attr as rest_contract_parse::RestContractAttr);
    let item = parse_macro_input!(item as syn::ItemTrait);

    match rest_contract_parse::parse(attr, item) {
        Ok(model) => rest_contract::generate(&model).into(),
        Err(err) => err.to_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn grpc_contract(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = parse_macro_input!(attr as grpc_contract_parse::GrpcContractAttr);
    let item = parse_macro_input!(item as syn::ItemTrait);

    match grpc_contract_parse::parse(attr, item) {
        Ok(model) => grpc_contract::generate(&model).into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `#[toolkit::provides(contract = ..., local = ..., transports = [...])]` —
/// auto-wire a generated contract client into the host `ClientHub`.
///
/// Applied on a module struct in the provider crate; generates an inherent
/// `wire_<contract_snake>` async method that validates the contract IR,
/// reads typed wiring config, and registers the appropriate Local/REST/gRPC
/// client. See `toolkit_contract_macros::provides` for the full attribute
/// surface.
#[proc_macro_attribute]
pub fn provides(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = parse_macro_input!(attr as provides::ProvidesAttr);
    let item = parse_macro_input!(item as syn::ItemStruct);
    match provides::generate(&attr, &item) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `#[toolkit::consumes(contract = ..., from = "gear")]` — declare a contract
/// dependency wired via eventual-readiness directory discovery.
///
/// Applied on the gear struct (alongside `#[toolkit::gear]`). Emits a
/// `ConsumerRegistration` that the runtime's
/// proxy-wiring phase replays: a compile-time local impl wins, otherwise a
/// directory-resolving REST client is registered. Does NOT inject a topo-sort
/// dependency — see `toolkit_contract_macros::consumes` docs.
#[proc_macro_attribute]
pub fn consumes(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = parse_macro_input!(attr as consumes::ConsumesAttr);
    let item = parse_macro_input!(item as syn::ItemStruct);
    match consumes::generate(&attr, &item) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

#[proc_macro_derive(ProtoBridge, attributes(proto_bridge))]
pub fn derive_proto_bridge(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as syn::DeriveInput);
    match proto_bridge::generate(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `#[derive(QueryParams)]` — mark a struct as a REST query parameter.
///
/// Generates `impl QueryParams`, whose `openapi_params()` describes each field
/// for the `OpenAPI` document. The generated route registers those parameters, so
/// the spec is derived from the same declaration that determines the wire
/// format instead of being inferred separately.
///
/// Field rules, both enforced at compile time:
/// - every field's leaf type must implement
///   [`QueryScalar`](toolkit_contract::query::QueryScalar) — scalars,
///   `Option<scalar>`, and `Vec<scalar>`. Nested structs are rejected: a query
///   string is a flat key/value list and cannot represent them unambiguously.
/// - a `Vec<..>` field must carry `#[serde(default)]`, since an empty vector
///   emits no key and would otherwise fail to deserialize.
///
/// `#[serde(rename = "...")]` and `#[serde(skip)]` are honoured so the spec
/// matches what serde actually puts on the wire.
#[proc_macro_derive(QueryParams)]
pub fn derive_query_params(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as syn::DeriveInput);
    match query_params::generate(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `#[derive(ContractError)]` — wire a typed Rust error enum into the
/// PRD #1536 RFC 9457 envelope.
///
/// Per-variant attributes:
/// - `#[error_code("INSUFFICIENT_FUNDS")]` (required)
/// - `#[error_domain("billing.v1")]` (required, or set once on the enum)
/// - `#[canonical(FailedPrecondition)]` (required — one of the 16
///   `ProblemCategory` variants)
///
/// Generates `From<MyError> for Problem` (server-side) and
/// `TryFrom<Problem> for MyError` (client-side); unknown
/// `error_code`/`error_domain` pairs round-trip back as the original
/// `Problem` so callers can still handle them as generic envelopes.
///
/// Mark exactly one variant `#[contract_error(fallback)]` (unit, or a single
/// named field receiving the original `Problem`) to additionally generate a
/// **total** `From<TransportError> for MyError` (gated on the SDK `rest-client`
/// feature). The generated REST client uses it to reconstruct typed variants
/// from an RFC 9457 response and to route un-reconstructable transport/protocol
/// failures into the fallback variant.
#[proc_macro_derive(
    ContractError,
    attributes(error_code, error_domain, canonical, contract_error)
)]
pub fn derive_contract_error(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as syn::DeriveInput);
    match contract_error::generate(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
