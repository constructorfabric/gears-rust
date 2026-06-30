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

    // Base must be remote-capable (`Api` or `Backend`).
    if !(base_name.ends_with("Api") || base_name.ends_with("Backend")) {
        let suffix = if base_name.ends_with("Embedded") {
            "Embedded"
        } else if base_name.ends_with("Extension") {
            "Extension"
        } else {
            "<unknown>"
        };
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
