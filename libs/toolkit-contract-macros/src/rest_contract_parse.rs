//! Parsing for `#[rest_contract]`.
//!
//! Recognized attributes on trait methods:
//! - `#[get("/path/{param}")]`, `#[post(...)]`, `#[put(...)]`, `#[delete(...)]`
//! - `#[retryable]` — marks the method as safe to retry on transport failure.
//! - `#[streaming]` — marks the method as server-streaming (SSE).
//!
//! The trait's first non-marker supertrait is recorded as the *base contract*
//! the projection refines (e.g. `pub trait PaymentServiceRest: PaymentService`).

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{Ident, ItemTrait, ReturnType, TraitItem, TraitItemFn, Type};

pub struct RestContractAttr {
    pub base_path: String,
}

pub struct RestContractModel {
    pub item: ItemTrait,
    pub trait_ident: Ident,
    pub base_trait: syn::Path,
    pub base_path: String,
    pub methods: Vec<RestMethodModel>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "these are independent per-method projection flags (retryable / streaming / optional / server_manual) parsed from distinct attributes; a bitflags enum would obscure rather than clarify the 1:1 attribute mapping"
)]
pub struct RestMethodModel {
    pub ident: Ident,
    pub http_method: HttpVerb,
    pub path_template: String,
    pub retryable: bool,
    pub streaming: bool,
    pub params: Vec<RestParam>,
    /// `Some((ok_ty, err_ty))` for unary methods returning `Result<T, E>`.
    /// `None` for streaming methods or otherwise non-`Result` returns.
    pub result_types: Option<(Type, Type)>,
    /// `true` when the projection method declares a default body — peers
    /// MAY omit this endpoint (mirrored into `HttpMethodBindingIr.optional`).
    pub optional: bool,
    /// `true` when the method is marked `#[server_manual]` — the server-side
    /// route generator (`register_<trait>_routes()`) SKIPS this method so the
    /// author can register it by hand via `OperationBuilder`. The method stays
    /// in the generated client and the binding IR.
    pub server_manual: bool,
}

pub struct RestParam {
    pub ident: Ident,
    pub ty: Type,
}

#[derive(Clone, Copy)]
pub enum HttpVerb {
    Get,
    Post,
    Put,
    Delete,
}

mod kw {
    syn::custom_keyword!(base_path);
}

impl syn::parse::Parse for RestContractAttr {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self {
                base_path: String::new(),
            });
        }
        let mut base_path = None;
        while !input.is_empty() {
            let lookahead = input.lookahead1();
            if lookahead.peek(kw::base_path) {
                let _kw: kw::base_path = input.parse()?;
                let _eq: syn::Token![=] = input.parse()?;
                let lit: syn::LitStr = input.parse()?;
                if base_path.is_some() {
                    return Err(syn::Error::new(lit.span(), "duplicate `base_path`"));
                }
                base_path = Some(lit.value());
            } else {
                return Err(lookahead.error());
            }
            if input.peek(syn::Token![,]) {
                let _comma: syn::Token![,] = input.parse()?;
            }
        }
        Ok(Self {
            base_path: base_path.unwrap_or_default(),
        })
    }
}

pub fn parse(attr: RestContractAttr, item: ItemTrait) -> syn::Result<RestContractModel> {
    let trait_ident = item.ident.clone();
    let trait_name = trait_ident.to_string();

    let base_trait = extract_base_trait(&item)?;
    let base_name = base_trait
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();

    // Projection trait name must be `<Base>Rest`.
    let expected_projection = format!("{base_name}Rest");
    if trait_name != expected_projection {
        return Err(syn::Error::new(
            trait_ident.span(),
            format!(
                "rest_contract trait must be named `{expected_projection}` to project base trait `{base_name}` \
                 (PRD #1536 D1: projection trait extends `{{Base}}` and is named `{{Base}}Rest`)"
            ),
        ));
    }

    // Base must be remote-capable (`Api` or `Backend`).
    let base_kind_ok = base_name.ends_with("Api") || base_name.ends_with("Backend");
    if !base_kind_ok {
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
                "REST projection is not allowed for `{suffix}` contracts; \
                 only `Api` and `Backend` are remote-capable (PRD #1536 D2/D6)"
            ),
        ));
    }

    let mut methods = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for trait_item in &item.items {
        let TraitItem::Fn(method) = trait_item else {
            continue;
        };
        let parsed = parse_method(method)?;
        let key = (
            parsed.http_method.ir_variant().to_owned(),
            parsed.path_template.clone(),
        );
        if !seen.insert(key.clone()) {
            return Err(syn::Error::new(
                method.sig.ident.span(),
                format!(
                    "duplicate REST binding `{} {}`; each method must have a unique (verb, path) pair",
                    key.0.to_uppercase(),
                    key.1
                ),
            ));
        }
        methods.push(parsed);
    }

    Ok(RestContractModel {
        item,
        trait_ident,
        base_trait,
        base_path: attr.base_path,
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
        "rest_contract trait must declare its base contract as a supertrait \
         (e.g. `pub trait PaymentServiceRest: PaymentService`)",
    ))
}

fn parse_method(method: &TraitItemFn) -> syn::Result<RestMethodModel> {
    let ident = method.sig.ident.clone();

    let mut http: Option<(HttpVerb, String, Span)> = None;
    let mut retryable = false;
    let mut streaming = false;
    let mut server_manual = false;

    for attr in &method.attrs {
        let path = attr.path();
        if path.is_ident("get") {
            http = Some((HttpVerb::Get, parse_path_lit(attr)?, attr.span()));
        } else if path.is_ident("post") {
            http = Some((HttpVerb::Post, parse_path_lit(attr)?, attr.span()));
        } else if path.is_ident("put") {
            http = Some((HttpVerb::Put, parse_path_lit(attr)?, attr.span()));
        } else if path.is_ident("delete") {
            http = Some((HttpVerb::Delete, parse_path_lit(attr)?, attr.span()));
        } else if path.is_ident("retryable") {
            retryable = true;
        } else if path.is_ident("streaming") {
            streaming = true;
        } else if path.is_ident("server_manual") {
            server_manual = true;
        }
    }

    let (http_method, path_template, _span) = http.ok_or_else(|| {
        syn::Error::new(
            method.span(),
            "rest_contract method requires one of `#[get(\"...\")]`, `#[post(\"...\")]`, \
             `#[put(\"...\")]`, or `#[delete(\"...\")]`",
        )
    })?;

    let params = parse_params(method)?;
    // Both unary and streaming methods declare their return types as
    // `Result<T, E>`. For streaming methods the macro rewrites the emitted
    // signature to `Pin<Box<dyn Stream<Item = Result<T, E>>>>`.
    let result_types = Some(parse_return_type(
        &method.sig.output,
        method.sig.ident.span(),
    )?);
    let optional = method.default.is_some();

    Ok(RestMethodModel {
        ident,
        http_method,
        path_template,
        retryable,
        streaming,
        params,
        result_types,
        optional,
        server_manual,
    })
}

fn parse_path_lit(attr: &syn::Attribute) -> syn::Result<String> {
    let lit: syn::LitStr = attr.parse_args()?;
    Ok(lit.value())
}

fn parse_params(method: &TraitItemFn) -> syn::Result<Vec<RestParam>> {
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
        params.push(RestParam {
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
            "rest_contract methods must return `Result<T, E>`",
        ));
    };
    extract_result_types(ty)
}

fn extract_result_types(ty: &Type) -> syn::Result<(Type, Type)> {
    let Type::Path(p) = ty else {
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

impl HttpVerb {
    pub fn ir_variant(self) -> &'static str {
        match self {
            HttpVerb::Get => "Get",
            HttpVerb::Post => "Post",
            HttpVerb::Put => "Put",
            HttpVerb::Delete => "Delete",
        }
    }

    pub fn allows_body(self) -> bool {
        matches!(self, HttpVerb::Post | HttpVerb::Put)
    }
}
