//! `#[derive(ContractError)]` — emits `From<MyError> for Problem` plus
//! `TryFrom<Problem> for MyError`, wiring a typed Rust enum into the
//! PRD #1536 RFC 9457 envelope via the `error_code` + `error_domain`
//! extension fields.
//!
//! Variant payload — **named fields only** (unit variants and tuple/unnamed
//! variants are rejected) — is placed at `context["data"]`. The canonical
//! AIP-193 category is selected per variant via `#[canonical(Category)]` and
//! determines GTS URI, HTTP status, and title. Mark one variant
//! `#[contract_error(fallback)]` to also emit a total `From<TransportError>`
//! (see the derive docs in `lib.rs`).

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Attribute, Data, DataEnum, DeriveInput, Fields, Ident, LitStr, Meta, Variant};

pub fn generate(input: DeriveInput) -> syn::Result<TokenStream> {
    let DeriveInput {
        attrs,
        ident,
        generics,
        data,
        ..
    } = input;

    if !generics.params.is_empty() {
        return Err(syn::Error::new(
            generics.span(),
            "#[derive(ContractError)] does not support generic enums",
        ));
    }

    let Data::Enum(DataEnum { variants, .. }) = data else {
        return Err(syn::Error::new(
            ident.span(),
            "#[derive(ContractError)] can only be applied to enums",
        ));
    };

    let enum_attrs = parse_enum_attrs(&attrs)?;

    let mut parsed = Vec::with_capacity(variants.len());
    for variant in variants {
        parsed.push(parse_variant(&variant, &enum_attrs)?);
    }

    // Reject duplicate `(error_domain, error_code)` pairs: they would produce
    // an unreachable `TryFrom` match arm and make wire-error reconstruction
    // ambiguous.
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for v in &parsed {
        if !seen.insert((v.domain.clone(), v.code.clone())) {
            return Err(syn::Error::new(
                v.span,
                format!(
                    "duplicate contract error `({}, {})`; each `(error_domain, error_code)` pair must be unique",
                    v.domain, v.code
                ),
            ));
        }
    }

    // At most one variant may be the transport fallback.
    let fallbacks: Vec<&ParsedVariant> = parsed.iter().filter(|v| v.is_fallback).collect();
    if let Some(second) = fallbacks.get(1) {
        return Err(syn::Error::new(
            second.span,
            "only one variant may be marked `#[contract_error(fallback)]`",
        ));
    }

    let to_arms: Vec<TokenStream> = parsed.iter().map(|v| emit_to_arm(&ident, v)).collect();
    let from_arms: Vec<TokenStream> = parsed.iter().map(|v| emit_from_arm(&ident, v)).collect();

    // Optional total `From<TransportError>` (client-side reconstruction),
    // generated only when a fallback variant is designated.
    let from_transport = match fallbacks.first() {
        Some(fb) => emit_from_transport(&ident, fb)?,
        None => TokenStream::new(),
    };

    let problem_path = quote! { ::toolkit_canonical_errors::Problem };
    let category_path = quote! { ::toolkit_canonical_errors::ProblemCategory };

    Ok(quote! {
        #[automatically_derived]
        impl ::std::convert::From<#ident> for #problem_path {
            fn from(__value: #ident) -> #problem_path {
                #[allow(unused_imports)]
                use #category_path as __Cat;
                match __value {
                    #(#to_arms),*
                }
            }
        }

        #[automatically_derived]
        impl ::std::convert::TryFrom<#problem_path> for #ident {
            type Error = #problem_path;
            fn try_from(
                __problem: #problem_path,
            ) -> ::std::result::Result<#ident, #problem_path> {
                // Match on (error_domain, error_code) pair; unknown values
                // bounce back the original Problem so the caller can keep
                // handling it as a generic envelope.
                let __domain = __problem.error_domain.as_deref();
                let __code = __problem.error_code.as_deref();
                match (__domain, __code) {
                    #(#from_arms,)*
                    _ => ::std::result::Result::Err(__problem),
                }
            }
        }

        #from_transport
    })
}

/// Emit a **total** `From<TransportError> for MyError` (gated on the SDK
/// `rest-client` feature) so the generated REST client reconstructs typed
/// variants and routes un-reconstructable failures into the fallback variant.
///
/// A `Problem` payload is first offered to `TryFrom<Problem>`; on success the
/// exact variant is returned. On failure (unknown code/domain) or for any
/// non-`Problem` transport error, a canonical `Problem` is routed into the
/// fallback variant (unit → discarded; single named field → receives it).
fn emit_from_transport(enum_ident: &Ident, fb: &ParsedVariant) -> syn::Result<TokenStream> {
    let support = crate::support::contract_support_path();
    let fb_ident = &fb.ident;

    let construct = match &fb.fields {
        VariantFields::Unit => quote! {
            {
                let _ = __problem;
                #enum_ident::#fb_ident
            }
        },
        VariantFields::Named(fields) if fields.len() == 1 => {
            let field = &fields[0];
            quote! { #enum_ident::#fb_ident { #field: __problem } }
        }
        VariantFields::Named(_) => {
            return Err(syn::Error::new(
                fb.span,
                "`#[contract_error(fallback)]` variant must be a unit variant or have exactly \
                 one named field (which receives the original `Problem`)",
            ));
        }
    };

    Ok(quote! {
        #[automatically_derived]
        #[cfg(feature = "rest-client")]
        impl ::std::convert::From<#support::runtime::transport_error::TransportError> for #enum_ident {
            fn from(
                __err: #support::runtime::transport_error::TransportError,
            ) -> #enum_ident {
                use #support::runtime::transport_error::TransportError as __TE;
                let __problem: ::toolkit_canonical_errors::Problem = match __err {
                    __TE::Problem { problem: __p, .. } => {
                        match <#enum_ident as ::std::convert::TryFrom<
                            ::toolkit_canonical_errors::Problem,
                        >>::try_from(*__p)
                        {
                            ::std::result::Result::Ok(__typed) => return __typed,
                            ::std::result::Result::Err(__p) => __p,
                        }
                    }
                    __other => <::toolkit_canonical_errors::Problem as ::std::convert::From<
                        ::toolkit_canonical_errors::CanonicalError,
                    >>::from(
                        <::toolkit_canonical_errors::CanonicalError as ::std::convert::From<
                            #support::runtime::transport_error::TransportError,
                        >>::from(__other),
                    ),
                };
                #construct
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Attribute parsing
// ---------------------------------------------------------------------------

struct EnumAttrs {
    /// Default `error_domain` for variants that don't override it.
    default_domain: Option<String>,
}

struct ParsedVariant {
    ident: Ident,
    span: Span,
    code: String,
    domain: String,
    category: Ident,
    fields: VariantFields,
    /// Marked `#[contract_error(fallback)]` — receives un-reconstructable
    /// transport/protocol errors in the generated `From<TransportError>`.
    is_fallback: bool,
}

enum VariantFields {
    Unit,
    Named(Vec<Ident>),
}

fn parse_enum_attrs(attrs: &[Attribute]) -> syn::Result<EnumAttrs> {
    let mut default_domain: Option<String> = None;
    for attr in attrs {
        if attr.path().is_ident("error_domain") {
            let lit: LitStr = attr.parse_args()?;
            default_domain = Some(lit.value());
        }
    }
    Ok(EnumAttrs { default_domain })
}

fn parse_variant(variant: &Variant, enum_attrs: &EnumAttrs) -> syn::Result<ParsedVariant> {
    let mut code: Option<String> = None;
    let mut domain: Option<String> = None;
    let mut category: Option<Ident> = None;
    let mut is_fallback = false;

    for attr in &variant.attrs {
        if attr.path().is_ident("error_code") {
            let lit: LitStr = attr.parse_args()?;
            code = Some(lit.value());
        } else if attr.path().is_ident("error_domain") {
            let lit: LitStr = attr.parse_args()?;
            domain = Some(lit.value());
        } else if attr.path().is_ident("contract_error") {
            // Only `fallback` is recognized inside the group today.
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("fallback") {
                    is_fallback = true;
                    Ok(())
                } else {
                    Err(meta.error("unknown `#[contract_error(...)]` key; expected `fallback`"))
                }
            })?;
        } else if attr.path().is_ident("canonical") {
            // `#[canonical(NotFound)]` — capture the ident inside.
            match &attr.meta {
                Meta::List(list) => {
                    let inner: Ident = syn::parse2(list.tokens.clone())?;
                    category = Some(inner);
                }
                _ => {
                    return Err(syn::Error::new(
                        attr.span(),
                        "expected `#[canonical(<Category>)]` with one identifier",
                    ));
                }
            }
        }
    }

    let code = code.ok_or_else(|| {
        syn::Error::new(variant.span(), "variant requires `#[error_code(\"...\")]`")
    })?;
    let domain = domain
        .or_else(|| enum_attrs.default_domain.clone())
        .ok_or_else(|| {
            syn::Error::new(
                variant.span(),
                "variant requires `#[error_domain(\"...\")]` \
             (or set it on the enum for the default)",
            )
        })?;
    let category = category.ok_or_else(|| {
        syn::Error::new(
            variant.span(),
            "variant requires `#[canonical(<Category>)]` \
             (one of the 16 ProblemCategory variants)",
        )
    })?;

    let fields = match &variant.fields {
        Fields::Unit => VariantFields::Unit,
        Fields::Named(named) => {
            // `Fields::Named` guarantees every field has an ident; the
            // alternative would be `Fields::Unnamed` / `Fields::Unit`.
            let idents = named
                .named
                .iter()
                .filter_map(|f| f.ident.clone())
                .collect::<Vec<_>>();
            VariantFields::Named(idents)
        }
        Fields::Unnamed(_) => {
            return Err(syn::Error::new(
                variant.span(),
                "tuple variants are not supported by #[derive(ContractError)] \
                 yet \u{2014} use named-field variants",
            ));
        }
    };

    Ok(ParsedVariant {
        ident: variant.ident.clone(),
        span: variant.span(),
        code,
        domain,
        category,
        fields,
        is_fallback,
    })
}

// ---------------------------------------------------------------------------
// Codegen
// ---------------------------------------------------------------------------

fn emit_to_arm(enum_ident: &Ident, v: &ParsedVariant) -> TokenStream {
    let variant_ident = &v.ident;
    let code = &v.code;
    let domain = &v.domain;
    let category_ident = &v.category;

    match &v.fields {
        VariantFields::Unit => {
            // Detail message defaults to "<EnumName>::<Variant>" when the
            // variant has no payload to summarize. Callers always see a
            // stable string — useful when the wire response is the only
            // surface a human reads.
            let detail = format!("{enum_ident}::{variant_ident}");
            quote! {
                #enum_ident::#variant_ident => {
                    ::toolkit_canonical_errors::Problem::contract_error(
                        __Cat::#category_ident,
                        #code,
                        #domain,
                        #detail,
                        ::serde_json::Value::Object(::serde_json::Map::new()),
                    )
                }
            }
        }
        VariantFields::Named(fields) => {
            let detail = format!("{enum_ident}::{variant_ident}");
            // Serialize struct shape into `context["data"]`. `serde_json::json!`
            // gives us an inline `Map<String, Value>` keyed by field name.
            let entries = fields.iter().map(|f| {
                let key = f.to_string();
                quote! { #key: #f }
            });
            quote! {
                #enum_ident::#variant_ident { #( #fields ),* } => {
                    let __data = ::serde_json::json!({ #(#entries),* });
                    ::toolkit_canonical_errors::Problem::contract_error(
                        __Cat::#category_ident,
                        #code,
                        #domain,
                        #detail,
                        __data,
                    )
                }
            }
        }
    }
}

fn emit_from_arm(enum_ident: &Ident, v: &ParsedVariant) -> TokenStream {
    let variant_ident = &v.ident;
    let code = &v.code;
    let domain = &v.domain;
    let span = v.span;

    match &v.fields {
        VariantFields::Unit => {
            quote! {
                (Some(#domain), Some(#code)) => {
                    ::std::result::Result::Ok(#enum_ident::#variant_ident)
                }
            }
        }
        VariantFields::Named(fields) => {
            // Each field is deserialized from `context.data.<field>`. An ABSENT
            // key is treated as JSON `null` (not an immediate failure) so that:
            //   - `Option<_>` fields reconstruct as `None` — tolerating a
            //     cross-language/hand-written peer, or a newer/older variant,
            //     that omits an optional field (serde's field-level `Option`
            //     semantics);
            //   - required fields still fail (`null` is not a valid
            //     `String`/number/struct) and bounce the ORIGINAL `Problem`
            //     back to the caller — a typed reconstruction failure is
            //     preferable to a half-populated variant.
            let field_reads = fields.iter().map(|f| {
                let key = f.to_string();
                let var = format_ident!("__f_{}", f);
                quote_spanned_eq(
                    span,
                    quote! {
                        let #var = match ::serde_json::from_value(
                            __data
                                .get(#key)
                                .cloned()
                                .unwrap_or(::serde_json::Value::Null),
                        ) {
                            Ok(__x) => __x,
                            Err(_) => return ::std::result::Result::Err(__problem),
                        };
                    },
                )
            });
            let assignments = fields.iter().map(|f| {
                let var = format_ident!("__f_{}", f);
                quote! { #f: #var }
            });
            quote! {
                (Some(#domain), Some(#code)) => {
                    let __data = __problem
                        .context
                        .get("data")
                        .cloned()
                        .unwrap_or_else(|| ::serde_json::Value::Object(
                            ::serde_json::Map::new()
                        ));
                    #(#field_reads)*
                    ::std::result::Result::Ok(#enum_ident::#variant_ident {
                        #(#assignments),*
                    })
                }
            }
        }
    }
}

fn quote_spanned_eq(span: Span, tokens: TokenStream) -> TokenStream {
    // Helper to keep diagnostics anchored to the variant if a field-read
    // ever errors out. Currently a thin wrapper, kept as a seam in case we
    // later want different span behaviour per field.
    let _ = span;
    tokens
}
