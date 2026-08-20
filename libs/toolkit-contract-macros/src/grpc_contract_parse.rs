//! Parsing for `#[toolkit::grpc_contract]`.
//!
//! Recognized attributes on trait methods:
//! - `#[rpc(name = "PascalCase")]` — explicit RPC name (default = `PascalCase` from method ident).
//! - `#[idempotency_level(NoSideEffects | Idempotent | NotIdempotent)]` —
//!   proto3 method option (default = `NotIdempotent`).
//! - `#[streaming]` — server-streaming RPC (re-used from base contract).
//! - `#[retryable]` — wrap call in retry-with-backoff (re-used from base).
//!
//! Attribute on the trait itself: `#[grpc_contract(package = "...",
//! service = "...", stubs_module = "crate::path::to::tonic::stubs")]`.
//!
//! Validation rules:
//! - Projection trait must be named `<Base>Grpc` (e.g. `PaymentApiGrpc: PaymentApi`).
//! - Base trait must end in `Api` or `Backend` (Embedded/Extension are local-only).
//! - No two methods may share an `rpc_name`.

use heck::ToUpperCamelCase as _;
use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{Ident, ItemTrait, ReturnType, TraitItem, TraitItemFn, Type};

pub struct GrpcContractAttr {
    pub package: String,
    pub service: Option<String>,
    pub stubs_module: syn::Path,
}

pub struct GrpcContractModel {
    pub item: ItemTrait,
    pub trait_ident: Ident,
    pub base_trait: syn::Path,
    pub package: String,
    pub service: String,
    pub stubs_module: syn::Path,
    pub methods: Vec<GrpcMethodModel>,
}

pub struct GrpcMethodModel {
    pub ident: Ident,
    pub rpc_name: String,
    pub idempotency: GrpcIdempotency,
    pub server_streaming: bool,
    pub retryable: bool,
    pub optional: bool,
    pub params: Vec<GrpcParam>,
    /// `(ok, err)` extracted from `Result<T, E>` declared on the trait method.
    pub result_types: (Type, Type),
}

pub struct GrpcParam {
    pub ident: Ident,
    pub ty: Type,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrpcIdempotency {
    NoSideEffects,
    Idempotent,
    NotIdempotent,
}

mod kw {
    syn::custom_keyword!(package);
    syn::custom_keyword!(service);
    syn::custom_keyword!(stubs_module);
    syn::custom_keyword!(name);
}

impl syn::parse::Parse for GrpcContractAttr {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut package: Option<String> = None;
        let mut service: Option<String> = None;
        let mut stubs_module: Option<syn::Path> = None;

        while !input.is_empty() {
            let lookahead = input.lookahead1();
            if lookahead.peek(kw::package) {
                let _kw: kw::package = input.parse()?;
                let _eq: syn::Token![=] = input.parse()?;
                let lit: syn::LitStr = input.parse()?;
                if package.is_some() {
                    return Err(syn::Error::new(lit.span(), "duplicate `package`"));
                }
                package = Some(lit.value());
            } else if lookahead.peek(kw::service) {
                let _kw: kw::service = input.parse()?;
                let _eq: syn::Token![=] = input.parse()?;
                let lit: syn::LitStr = input.parse()?;
                if service.is_some() {
                    return Err(syn::Error::new(lit.span(), "duplicate `service`"));
                }
                service = Some(lit.value());
            } else if lookahead.peek(kw::stubs_module) {
                let _kw: kw::stubs_module = input.parse()?;
                let _eq: syn::Token![=] = input.parse()?;
                let lit: syn::LitStr = input.parse()?;
                if stubs_module.is_some() {
                    return Err(syn::Error::new(lit.span(), "duplicate `stubs_module`"));
                }
                stubs_module = Some(syn::parse_str(&lit.value())?);
            } else {
                return Err(lookahead.error());
            }
            if input.peek(syn::Token![,]) {
                let _comma: syn::Token![,] = input.parse()?;
            }
        }

        let package =
            package.ok_or_else(|| input.error("missing required `package = \"...\"` parameter"))?;
        let stubs_module = stubs_module.ok_or_else(|| {
            input.error(
                "missing required `stubs_module = \"crate::path::to::tonic::stubs\"` parameter",
            )
        })?;

        Ok(Self {
            package,
            service,
            stubs_module,
        })
    }
}

pub fn parse(attr: GrpcContractAttr, item: ItemTrait) -> syn::Result<GrpcContractModel> {
    let trait_ident = item.ident.clone();
    let trait_name = trait_ident.to_string();

    let base_trait = extract_base_trait(&item)?;
    let base_name = base_trait
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();

    // Projection name must be `<Base>Grpc`.
    let expected_projection = format!("{base_name}Grpc");
    if trait_name != expected_projection {
        return Err(syn::Error::new(
            trait_ident.span(),
            format!(
                "grpc_contract trait must be named `{expected_projection}` to project base trait `{base_name}` \
                 (PRD #1536 D1: projection trait extends `{{Base}}` and is named `{{Base}}Grpc`)"
            ),
        ));
    }

    // Base must be remote-capable (`Api` or `Backend`). Classification is
    // delegated to `ContractKind` so the suffix rule — including its handling of
    // a trailing major-version marker (ADR-0007) — lives in exactly one place.
    let base_kind = crate::model::ContractKind::from_suffix(&base_name);
    if !base_kind.is_some_and(crate::model::ContractKind::is_remote_capable) {
        let suffix = base_kind.map_or("<unknown>", crate::model::ContractKind::suffix);
        return Err(syn::Error::new(
            trait_ident.span(),
            format!(
                "gRPC projection is not allowed for `{suffix}` contracts; \
                 only `Api` and `Backend` are remote-capable (PRD #1536 D2/D6)"
            ),
        ));
    }

    let service = attr.service.unwrap_or_else(|| base_name.clone());

    let mut methods = Vec::new();
    let mut seen_rpcs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for trait_item in &item.items {
        let TraitItem::Fn(method) = trait_item else {
            continue;
        };
        let parsed = parse_method(method)?;
        if !seen_rpcs.insert(parsed.rpc_name.clone()) {
            return Err(syn::Error::new(
                method.sig.ident.span(),
                format!(
                    "duplicate gRPC rpc_name `{}`; each method must have a unique RPC name",
                    parsed.rpc_name,
                ),
            ));
        }
        methods.push(parsed);
    }

    Ok(GrpcContractModel {
        item,
        trait_ident,
        base_trait,
        package: attr.package,
        service,
        stubs_module: attr.stubs_module,
        methods,
    })
}

fn extract_base_trait(item: &ItemTrait) -> syn::Result<syn::Path> {
    for bound in &item.supertraits {
        if let syn::TypeParamBound::Trait(t) = bound {
            return Ok(t.path.clone());
        }
    }
    Err(syn::Error::new(
        item.span(),
        "grpc_contract trait must declare its base contract as a supertrait \
         (e.g. `pub trait PaymentApiGrpc: PaymentApi`)",
    ))
}

fn parse_method(method: &TraitItemFn) -> syn::Result<GrpcMethodModel> {
    let ident = method.sig.ident.clone();
    let default_rpc_name = ident.to_string().to_upper_camel_case();
    let mut rpc_name = default_rpc_name;
    let mut idempotency = GrpcIdempotency::NotIdempotent;
    let mut server_streaming = false;
    let mut retryable = false;

    for attr in &method.attrs {
        let path = attr.path();
        if path.is_ident("rpc") {
            let parsed = parse_rpc_attr(attr)?;
            if let Some(name) = parsed {
                rpc_name = name;
            }
        } else if path.is_ident("idempotency_level") {
            idempotency = parse_idempotency_level(attr)?;
        } else if path.is_ident("streaming") {
            server_streaming = true;
        } else if path.is_ident("retryable") {
            retryable = true;
        }
    }

    // Retrying a non-idempotent write duplicates it. The generated client
    // retries on transient transport failures, and a 502/504 can arrive after
    // the server already committed — so `#[retryable]` on `NotIdempotent` is a
    // double-write waiting to happen. The two attributes were previously
    // independent flags with nothing reconciling them, which made the
    // idempotency metadata dead weight.
    if retryable && idempotency == GrpcIdempotency::NotIdempotent {
        return Err(syn::Error::new(
            ident.span(),
            "`#[retryable]` on a `NotIdempotent` method: a transient failure can arrive after \
             the server committed, so retrying would duplicate the write.\n\
             Either make the operation idempotent (e.g. accept an idempotency key) and declare \
             `#[idempotency_level(Idempotent)]`, or drop `#[retryable]` and let the caller \
             decide.",
        ));
    }

    let params = parse_params(method)?;
    let result_types = parse_return_type(&method.sig.output, method.sig.ident.span())?;
    let optional = method.default.is_some();

    Ok(GrpcMethodModel {
        ident,
        rpc_name,
        idempotency,
        server_streaming,
        retryable,
        optional,
        params,
        result_types,
    })
}

fn parse_rpc_attr(attr: &syn::Attribute) -> syn::Result<Option<String>> {
    let mut name: Option<String> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("name") {
            let value = meta.value()?;
            let lit: syn::LitStr = value.parse()?;
            name = Some(lit.value());
            Ok(())
        } else {
            Err(meta.error("unknown attribute; expected `name = \"...\"`"))
        }
    })?;
    Ok(name)
}

fn parse_idempotency_level(attr: &syn::Attribute) -> syn::Result<GrpcIdempotency> {
    let syn::Meta::List(list) = &attr.meta else {
        return Err(syn::Error::new_spanned(
            attr,
            "expected #[idempotency_level(NoSideEffects | Idempotent | NotIdempotent)]",
        ));
    };
    let variant: syn::Ident = syn::parse2(list.tokens.clone())?;
    match variant.to_string().as_str() {
        "NoSideEffects" => Ok(GrpcIdempotency::NoSideEffects),
        "Idempotent" => Ok(GrpcIdempotency::Idempotent),
        "NotIdempotent" => Ok(GrpcIdempotency::NotIdempotent),
        other => Err(syn::Error::new(
            variant.span(),
            format!("unknown idempotency level `{other}`"),
        )),
    }
}

fn parse_params(method: &TraitItemFn) -> syn::Result<Vec<GrpcParam>> {
    let mut params = Vec::new();
    for arg in &method.sig.inputs {
        let syn::FnArg::Typed(pat_type) = arg else {
            continue;
        };
        let syn::Pat::Ident(pat_ident) = pat_type.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &pat_type.pat,
                "expected an identifier pattern for method parameter",
            ));
        };
        params.push(GrpcParam {
            ident: pat_ident.ident.clone(),
            ty: (*pat_type.ty).clone(),
        });
    }

    // Exactly one security-plane context per method: the client codegen
    // (`security_context_param` / plane detection in `grpc_contract.rs`)
    // picks a single param to source the auth metadata from. A second
    // context param (e.g. a stray `PlatformSecurityContext` alongside a
    // tenant `SecurityContext`) would silently be dropped from the wire
    // without ever being consulted for token attachment, mixing planes.
    let secctx_count = params
        .iter()
        .filter(|p| crate::projection::is_security_context_type(&p.ty))
        .count();
    if secctx_count > 1 {
        return Err(syn::Error::new(
            method.sig.ident.span(),
            "grpc_contract method must take at most one security-plane context \
             parameter (`SecurityContext` or `PlatformSecurityContext`); found more \
             than one",
        ));
    }

    Ok(params)
}

fn parse_return_type(ret: &ReturnType, span: Span) -> syn::Result<(Type, Type)> {
    let ReturnType::Type(_, ty) = ret else {
        return Err(syn::Error::new(
            span,
            "grpc_contract methods must return `Result<T, E>`",
        ));
    };
    let Type::Path(p) = ty.as_ref() else {
        return Err(syn::Error::new_spanned(
            ty,
            "expected `Result<T, E>` return type",
        ));
    };
    let Some(last) = p.path.segments.last() else {
        return Err(syn::Error::new_spanned(ty, "expected `Result<T, E>`"));
    };
    if last.ident != "Result" {
        return Err(syn::Error::new_spanned(
            ty,
            "expected `Result<T, E>` return type",
        ));
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "expected `Result<T, E>` with generic arguments",
        ));
    };
    let mut iter = args.args.iter();
    let ok = iter
        .next()
        .ok_or_else(|| syn::Error::new_spanned(ty, "Result must have two type arguments"))?;
    let err = iter
        .next()
        .ok_or_else(|| syn::Error::new_spanned(ty, "Result must have two type arguments"))?;
    let syn::GenericArgument::Type(ok_ty) = ok else {
        return Err(syn::Error::new_spanned(ok, "expected a type argument"));
    };
    let syn::GenericArgument::Type(err_ty) = err else {
        return Err(syn::Error::new_spanned(err, "expected a type argument"));
    };
    Ok((ok_ty.clone(), err_ty.clone()))
}

impl GrpcIdempotency {
    pub fn ir_variant(self) -> &'static str {
        match self {
            GrpcIdempotency::NoSideEffects => "NoSideEffects",
            GrpcIdempotency::Idempotent => "Idempotent",
            GrpcIdempotency::NotIdempotent => "NotIdempotent",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn parse_trait(tokens: proc_macro2::TokenStream) -> syn::Result<GrpcContractModel> {
        let attr: GrpcContractAttr = syn::parse2(quote! {
            package = "test.v1",
            stubs_module = "crate::stubs"
        })?;
        let item: syn::ItemTrait = syn::parse2(tokens)?;
        parse(attr, item)
    }

    /// Like [`parse_trait`], but discards the (non-`Debug`) model on success
    /// so the error path can use `.expect_err`.
    fn parse_trait_err(tokens: proc_macro2::TokenStream) -> syn::Error {
        parse_trait(tokens)
            .map(|_| ())
            .expect_err("expected a parse error")
    }

    #[test]
    fn rejects_methods_with_two_security_context_params() {
        let err = parse_trait_err(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn do_it(
                    &self,
                    ctx: SecurityContext,
                    other: PlatformSecurityContext,
                    id: String,
                ) -> Result<Resp, Err>;
            }
        });
        assert!(
            err.to_string()
                .contains("at most one security-plane context"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_single_security_context_param() {
        let model = parse_trait(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn do_it(&self, ctx: SecurityContext, id: String) -> Result<Resp, Err>;
            }
        })
        .expect("a single security-context param is valid");
        assert_eq!(model.methods.len(), 1);
        // `self` (a `Receiver`, not `FnArg::Typed`) never lands in `params`.
        assert_eq!(model.methods[0].params.len(), 2);
    }

    #[test]
    fn accepts_platform_security_context_param() {
        let model = parse_trait(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn do_it(&self, ctx: PlatformSecurityContext, id: String) -> Result<Resp, Err>;
            }
        })
        .expect("a single platform security-context param is valid");
        // Assert the CLASSIFICATION, not just the param count: counting params
        // cannot tell a `PlatformSecurityContext` recognized as a plane marker
        // from one mistakenly treated as an ordinary wire param.
        assert_eq!(model.methods[0].params.len(), 2);
        assert!(
            crate::projection::is_platform_security_context_type(&model.methods[0].params[0].ty),
            "first param must be recognized as the platform-plane marker"
        );
        // And the wire body is the OTHER param (`id`) — a non-context payload,
        // i.e. the platform context is excluded from the wire message.
        assert_eq!(model.methods[0].params[1].ident, "id");
        assert!(
            !crate::projection::is_security_context_type(&model.methods[0].params[1].ty),
            "the wire body param must not be classified as a security context"
        );
    }

    #[test]
    fn methods_without_a_security_context_are_still_valid() {
        // gRPC parsing does not itself require a security-context parameter
        // (unlike the REST projection) — absence just means no auth metadata
        // is attached by the generated client.
        let model = parse_trait(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn do_it(&self, id: String) -> Result<Resp, Err>;
            }
        })
        .expect("no security-context param is valid");
        assert_eq!(model.methods[0].params.len(), 1);
    }

    #[test]
    fn retryable_non_idempotent_is_rejected() {
        let err = parse_trait_err(quote! {
            pub trait FooApiGrpc: FooApi {
                #[retryable]
                async fn do_it(&self, ctx: SecurityContext, id: String) -> Result<Resp, Err>;
            }
        });
        assert!(err.to_string().contains("NotIdempotent"), "got: {err}");
    }

    #[test]
    fn retryable_idempotent_is_accepted() {
        let model = parse_trait(quote! {
            pub trait FooApiGrpc: FooApi {
                #[retryable]
                #[idempotency_level(Idempotent)]
                async fn do_it(&self, ctx: SecurityContext, id: String) -> Result<Resp, Err>;
            }
        })
        .expect("retryable + Idempotent is valid");
        assert!(model.methods[0].retryable);
        assert_eq!(model.methods[0].idempotency, GrpcIdempotency::Idempotent);
    }

    #[test]
    fn duplicate_rpc_names_are_rejected() {
        let err = parse_trait_err(quote! {
            pub trait FooApiGrpc: FooApi {
                #[rpc(name = "Shared")]
                async fn one(&self, ctx: SecurityContext) -> Result<Resp, Err>;
                #[rpc(name = "Shared")]
                async fn two(&self, ctx: SecurityContext) -> Result<Resp, Err>;
            }
        });
        assert!(
            err.to_string().contains("duplicate gRPC rpc_name"),
            "got: {err}"
        );
    }

    #[test]
    fn wrong_projection_name_is_rejected() {
        let err = parse_trait_err(quote! {
            pub trait NotTheRightNameGrpc: FooApi {
                async fn do_it(&self, ctx: SecurityContext) -> Result<Resp, Err>;
            }
        });
        assert!(err.to_string().contains("FooApiGrpc"), "got: {err}");
    }

    #[test]
    fn non_remote_capable_base_is_rejected() {
        let err = parse_trait_err(quote! {
            pub trait FooEmbeddedGrpc: FooEmbedded {
                async fn do_it(&self, ctx: SecurityContext) -> Result<Resp, Err>;
            }
        });
        assert!(err.to_string().contains("not allowed"), "got: {err}");
    }
}
