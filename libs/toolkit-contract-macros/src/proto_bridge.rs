//! `#[derive(ProtoBridge)]` — generates `From`/`Into` between an SDK serde
//! DTO and its prost-generated stub message.
//!
//! Type-level attribute (required): `#[proto_bridge(stub = "Path::To::Proto")]`.
//!
//! Field-level attributes (optional):
//! - `#[proto_bridge(via_string)]` — convert via `to_string()` /
//!   `FromStr::from_str(&s).unwrap_or_default()`. For `Uuid` and similar
//!   string-encoded primitives. Handles `Option<T>` via `.map(...)`.
//! - `#[proto_bridge(skip)]` — exclude the field from both `to_proto` and
//!   `from_proto` initializers. Reconstructed via `Default::default()` on
//!   the `from_proto` side. Useful for `PhantomData<_>` markers and other
//!   non-wire fields on generic types.
//! - (default) — direct `Into::into`. For `Option<T>` fields the macro emits
//!   `.map(Into::into)` so Rust enums with `From<i32>`/`From<Rust> for i32`
//!   work transparently.
//!
//! Supports:
//! - struct-with-named-fields → emits `From<Rust> for Proto` and
//!   `From<Proto> for Rust` (direct field assignment, allowed inside the
//!   defining crate even with `#[non_exhaustive]`). Generics on the struct
//!   are propagated to all emitted impls verbatim — bounds on the input
//!   are reused, no extra bounds are synthesized.
//! - enum-with-unit-variants → emits `From<Rust> for Proto`,
//!   `From<Proto> for Rust`, plus `From<Rust> for i32` and `From<i32> for
//!   Rust` (the latter requires `Rust: Default` for unknown-variant fallback).
//!
//! Variants of an enum must match 1:1 by name with the proto enum
//! variants — the macro emits an exhaustive `match`, and any drift between
//! Rust and proto enum sets surfaces as a compile error.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{
    Data, DataEnum, DataStruct, DeriveInput, Field, Fields, Generics, Ident, LitStr, Path, Type,
};

use crate::support::contract_support_path;

pub fn generate(input: &DeriveInput) -> syn::Result<TokenStream> {
    let stub_path = parse_stub_path(input)?;
    let bridge = match &input.data {
        Data::Struct(data) => derive_struct(&input.ident, &input.generics, &stub_path, data)?,
        Data::Enum(data) => derive_enum(&input.ident, &input.generics, &stub_path, data)?,
        Data::Union(_) => {
            return Err(syn::Error::new(
                input.span(),
                "ProtoBridge cannot be derived for unions",
            ));
        }
    };
    let repr = derive_grpc_repr(&input.ident, &input.generics);
    Ok(quote! { #bridge #repr })
}

/// Emit `GrpcRepr` + `GrpcReprScalar` impls so the type can be used in
/// `#[toolkit::grpc_contract]` method signatures without further opt-in.
/// Generics on the input are propagated verbatim — no extra bounds are
/// synthesized; the user adds them in the struct's where-clause when needed.
fn derive_grpc_repr(rust_ty: &Ident, generics: &Generics) -> TokenStream {
    let support = contract_support_path();
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    quote! {
        #[automatically_derived]
        impl #impl_generics #support::grpc_repr::GrpcRepr for #rust_ty #ty_generics #where_clause {}
        #[automatically_derived]
        impl #impl_generics #support::grpc_repr::GrpcReprScalar for #rust_ty #ty_generics #where_clause {}
    }
}

// ---------------------------------------------------------------------------
// Type-level attribute parsing.
// ---------------------------------------------------------------------------

fn parse_stub_path(input: &DeriveInput) -> syn::Result<Path> {
    let mut stub: Option<Path> = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("proto_bridge") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("stub") {
                let value = meta.value()?;
                let lit: LitStr = value.parse()?;
                if stub.is_some() {
                    return Err(meta.error("duplicate `stub` parameter"));
                }
                stub = Some(syn::parse_str(&lit.value())?);
                Ok(())
            } else {
                Err(meta.error("unknown attribute; expected `stub = \"...\"`"))
            }
        })?;
    }
    stub.ok_or_else(|| {
        syn::Error::new(
            input.span(),
            "missing required `#[proto_bridge(stub = \"...\")]` on the type",
        )
    })
}

// ---------------------------------------------------------------------------
// Struct derive.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum FieldConversion {
    Direct,
    ViaString,
    /// Field is excluded from wire impls; reconstructed via `Default::default()`.
    Skip,
}

fn parse_field_conversion(field: &Field) -> syn::Result<FieldConversion> {
    let mut conv = FieldConversion::Direct;
    let mut seen = false;
    for attr in &field.attrs {
        if !attr.path().is_ident("proto_bridge") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("via_string") {
                if seen {
                    return Err(meta.error("duplicate field-level `proto_bridge` attribute"));
                }
                conv = FieldConversion::ViaString;
                seen = true;
                Ok(())
            } else if meta.path.is_ident("skip") {
                if seen {
                    return Err(meta.error("duplicate field-level `proto_bridge` attribute"));
                }
                conv = FieldConversion::Skip;
                seen = true;
                Ok(())
            } else {
                Err(meta.error("unknown attribute; expected `via_string` or `skip`"))
            }
        })?;
    }
    Ok(conv)
}

fn derive_struct(
    rust_ty: &Ident,
    generics: &Generics,
    stub_path: &Path,
    data: &DataStruct,
) -> syn::Result<TokenStream> {
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new(
            data.fields.span(),
            "ProtoBridge only supports structs with named fields",
        ));
    };

    let support = contract_support_path();

    let mut to_proto_inits = Vec::new();
    let mut from_proto_inits = Vec::new();
    let mut try_from_proto_inits = Vec::new();
    let mut skipped_from_inits = Vec::new();

    for field in &named.named {
        let field_ident = field.ident.as_ref().ok_or_else(|| {
            syn::Error::new(field.span(), "tuple-struct fields are not supported")
        })?;
        let conv = parse_field_conversion(field)?;
        if matches!(conv, FieldConversion::Skip) {
            // Skipped fields don't appear in the proto stub at all. The
            // `from_proto` rebuild uses `Default::default()` to populate them
            // — typically a no-op for `PhantomData<T>` and similar markers.
            skipped_from_inits.push(quote! { #field_ident: ::std::default::Default::default() });
            continue;
        }
        let field_ty = &field.ty;
        let is_optional = is_option_type(field_ty);
        let inner_ty = if is_optional {
            extract_option_inner(field_ty).ok_or_else(|| {
                syn::Error::new(field_ty.span(), "could not extract Option<T> inner type")
            })?
        } else {
            field_ty
        };

        let to_proto = match (&conv, is_optional) {
            (FieldConversion::Direct, false) => {
                quote! { ::std::convert::Into::into(v.#field_ident) }
            }
            (FieldConversion::Direct, true) => {
                quote! { v.#field_ident.map(::std::convert::Into::into) }
            }
            (FieldConversion::ViaString, false) => {
                quote! { v.#field_ident.to_string() }
            }
            (FieldConversion::ViaString, true) => {
                quote! { v.#field_ident.map(|x| x.to_string()) }
            }
            (FieldConversion::Skip, _) => unreachable!("skipped above"),
        };

        let from_proto = match (&conv, is_optional) {
            (FieldConversion::Direct, false) => {
                quote! { ::std::convert::Into::into(v.#field_ident) }
            }
            (FieldConversion::Direct, true) => {
                quote! { v.#field_ident.map(::std::convert::Into::into) }
            }
            (FieldConversion::ViaString, false) => {
                let msg = format!("proto bridge: invalid string for field `{field_ident}`");
                quote! {
                    <#inner_ty as ::std::str::FromStr>::from_str(&v.#field_ident)
                        .expect(#msg)
                }
            }
            (FieldConversion::ViaString, true) => {
                let msg = format!("proto bridge: invalid string for field `{field_ident}`");
                quote! {
                    v.#field_ident.map(|s| {
                        <#inner_ty as ::std::str::FromStr>::from_str(&s)
                            .expect(#msg)
                    })
                }
            }
            (FieldConversion::Skip, _) => unreachable!("skipped above"),
        };

        // Fallible counterpart to `from_proto`. Operates on `&Proto` so it
        // can be called without consuming the proto value; non-string fields
        // are cloned so the same surface works regardless of whether their
        // type is Copy.
        let field_name = field_ident.to_string();
        let try_from_proto = match (&conv, is_optional) {
            (FieldConversion::Direct, false) => {
                quote! { ::std::convert::Into::into(::std::clone::Clone::clone(&v.#field_ident)) }
            }
            (FieldConversion::Direct, true) => {
                quote! {
                    ::std::clone::Clone::clone(&v.#field_ident)
                        .map(::std::convert::Into::into)
                }
            }
            (FieldConversion::ViaString, false) => {
                quote! {
                    <#inner_ty as ::std::str::FromStr>::from_str(&v.#field_ident)
                        .map_err(|e| #support::grpc_repr::ViaStringParseError {
                            field: #field_name,
                            source: ::std::boxed::Box::new(e),
                        })?
                }
            }
            (FieldConversion::ViaString, true) => {
                quote! {
                    v.#field_ident
                        .as_ref()
                        .map(|s| {
                            <#inner_ty as ::std::str::FromStr>::from_str(s)
                                .map_err(|e| #support::grpc_repr::ViaStringParseError {
                                    field: #field_name,
                                    source: ::std::boxed::Box::new(e),
                                })
                        })
                        .transpose()?
                }
            }
            (FieldConversion::Skip, _) => unreachable!("skipped above"),
        };

        to_proto_inits.push(quote! { #field_ident: #to_proto });
        from_proto_inits.push(quote! { #field_ident: #from_proto });
        try_from_proto_inits.push(quote! { #field_ident: #try_from_proto });
    }

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::std::convert::From<#rust_ty #ty_generics> for #stub_path #where_clause {
            fn from(v: #rust_ty #ty_generics) -> Self {
                // `v` may be unused if every field is `#[proto_bridge(skip)]`.
                let _ = &v;
                Self {
                    #(#to_proto_inits),*
                }
            }
        }

        #[automatically_derived]
        impl #impl_generics ::std::convert::From<#stub_path> for #rust_ty #ty_generics #where_clause {
            fn from(v: #stub_path) -> Self {
                let _ = &v;
                Self {
                    #(#from_proto_inits,)*
                    #(#skipped_from_inits),*
                }
            }
        }

        // Fallible alternative to the panicking `From<Proto>`. Use this on
        // wire-input paths (e.g. tonic server handlers) where a malformed
        // `via_string` field would otherwise panic the receiving process —
        // a remote-DoS surface.
        #[automatically_derived]
        impl #impl_generics #rust_ty #ty_generics #where_clause {
            pub fn try_from_proto(
                v: &#stub_path,
            ) -> ::std::result::Result<Self, #support::grpc_repr::ViaStringParseError> {
                let _ = &v;
                ::std::result::Result::Ok(Self {
                    #(#try_from_proto_inits,)*
                    #(#skipped_from_inits),*
                })
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Enum derive.
// ---------------------------------------------------------------------------

fn derive_enum(
    rust_ty: &Ident,
    generics: &Generics,
    stub_path: &Path,
    data: &DataEnum,
) -> syn::Result<TokenStream> {
    if data.variants.is_empty() {
        return Err(syn::Error::new(
            data.variants.span(),
            "ProtoBridge enum must have at least one variant",
        ));
    }
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new(
                variant.span(),
                "ProtoBridge enum variants must be unit (no payload)",
            ));
        }
    }

    let to_proto_arms = data.variants.iter().map(|v| {
        let ident = &v.ident;
        quote! { #rust_ty::#ident => #stub_path::#ident }
    });
    let from_proto_arms = data.variants.iter().map(|v| {
        let ident = &v.ident;
        quote! { #stub_path::#ident => #rust_ty::#ident }
    });

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let support = contract_support_path();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::std::convert::From<#rust_ty #ty_generics> for #stub_path #where_clause {
            fn from(v: #rust_ty #ty_generics) -> Self {
                match v {
                    #(#to_proto_arms,)*
                }
            }
        }

        #[automatically_derived]
        impl #impl_generics ::std::convert::From<#stub_path> for #rust_ty #ty_generics #where_clause {
            fn from(v: #stub_path) -> Self {
                // Proto3 enums always carry a zero-valued `Unspecified`
                // sentinel that has no Rust counterpart — fall back to the
                // Rust enum's `Default` for it (and for any future proto
                // variants not yet known to this Rust crate). The same
                // default-fallback is used by the i32 -> Rust impl below
                // so the two paths stay consistent.
                match v {
                    #(#from_proto_arms,)*
                    #[allow(unreachable_patterns)]
                    _ => <#rust_ty #ty_generics as ::std::default::Default>::default(),
                }
            }
        }

        #[automatically_derived]
        impl #impl_generics ::std::convert::From<#rust_ty #ty_generics> for i32 #where_clause {
            fn from(v: #rust_ty #ty_generics) -> Self {
                <#stub_path as ::std::convert::From<#rust_ty #ty_generics>>::from(v) as i32
            }
        }

        // Unknown discriminants from the wire used to silently fall back to
        // `Default::default()` here, hiding peer-side schema drift. We keep
        // the forward-compatible fallback (panicking on unknown variants
        // would turn schema evolution into a remote-DoS surface) but emit a
        // `tracing::warn!` so the event is observable. Callers that need to
        // distinguish the unknown case must use the inherent `try_from_i32`
        // method below.
        #[automatically_derived]
        impl #impl_generics ::std::convert::From<i32> for #rust_ty #ty_generics #where_clause {
            fn from(v: i32) -> Self {
                match <#stub_path as ::std::convert::TryFrom<i32>>::try_from(v) {
                    ::std::result::Result::Ok(s) => {
                        <#rust_ty #ty_generics as ::std::convert::From<#stub_path>>::from(s)
                    }
                    ::std::result::Result::Err(_) => {
                        #support::grpc_repr::log_unknown_enum_discriminant(
                            v,
                            ::std::stringify!(#rust_ty),
                        );
                        <#rust_ty #ty_generics as ::std::default::Default>::default()
                    }
                }
            }
        }

        // Inherent fallible counterpart to `From<i32>`. A separate
        // `impl TryFrom<i32>` would conflict with the blanket `TryFrom`
        // implied by `From<i32>` (whose `Error = Infallible` hides unknown
        // discriminants from callers).
        #[automatically_derived]
        impl #impl_generics #rust_ty #ty_generics #where_clause {
            pub fn try_from_i32(
                v: i32,
            ) -> ::std::result::Result<Self, #support::grpc_repr::UnknownEnumDiscriminant> {
                <#stub_path as ::std::convert::TryFrom<i32>>::try_from(v)
                    .map(<#rust_ty #ty_generics as ::std::convert::From<#stub_path>>::from)
                    .map_err(|_| #support::grpc_repr::UnknownEnumDiscriminant(v))
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Type introspection helpers.
// ---------------------------------------------------------------------------

fn is_option_type(ty: &Type) -> bool {
    if let Type::Path(p) = ty
        && let Some(last) = p.path.segments.last()
    {
        return last.ident == "Option";
    }
    false
}

fn extract_option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(p) = ty else { return None };
    let last = p.path.segments.last()?;
    if last.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    let first = args.args.first()?;
    let syn::GenericArgument::Type(inner) = first else {
        return None;
    };
    Some(inner)
}
