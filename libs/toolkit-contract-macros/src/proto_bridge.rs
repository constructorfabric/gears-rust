//! `#[derive(ProtoBridge)]` — generates `From`/`Into` between an SDK serde
//! DTO and its prost-generated stub message.
//!
//! Type-level attribute (required): `#[proto_bridge(stub = "Path::To::Proto")]`.
//!
//! Field-level attributes (optional):
//! - `#[proto_bridge(via_string)]` — cross via `to_string()` out and
//!   `FromStr::from_str(&s)` back. For `Uuid` and similar string-encoded
//!   primitives; handles `Option<T>` via `.map(...)`. Decoding a wire string
//!   can fail, and panicking on a hostile peer's input is a remote-DoS
//!   surface, so there is **no infallible decode**: a struct that contains a
//!   `via_string` field gets no `From<Proto> for Rust` impl, and all of its
//!   decoding goes through the fallible `try_from_proto` / `TryFromProto`
//!   path. The encode direction (`From<Rust> for Proto`) is unaffected.
//! - `#[proto_bridge(message)]` — for a **required nested message**. prost renders
//!   every proto3 message-typed field as `Option<T>`, so a DTO field that is
//!   semantically required (`LeaseRef { token: LeaseToken }`) does not line up with
//!   its stub. This wraps on the way out and unwraps on the way in: absent decodes
//!   as `Default::default()` in the infallible `From`, and as a
//!   `MissingRequiredMessage` error in the fallible `TryFromProto`, so a peer that
//!   omits it is a decode error rather than a panic or a silently zeroed field.
//!   On a `Vec<T>` field it means `repeated T` and converts element-wise, which
//!   plain `Into::into` cannot do for a message element.
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
//!   defining crate even with `#[non_exhaustive]`) — except that
//!   `From<Proto> for Rust` is omitted when any field is `via_string`, whose
//!   decode is fallible-only (see above). `try_from_proto` / `TryFromProto`
//!   are always emitted. Generics on the struct are propagated to all emitted
//!   impls verbatim — bounds on the input are reused, no extra bounds are
//!   synthesized.
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
    /// Required nested message: `T` on the Rust side, `Option<T>` on the stub.
    Message,
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
            } else if meta.path.is_ident("message") {
                if seen {
                    return Err(meta.error("duplicate field-level `proto_bridge` attribute"));
                }
                conv = FieldConversion::Message;
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
                Err(meta.error("unknown attribute; expected `via_string`, `message`, or `skip`"))
            }
        })?;
    }
    Ok(conv)
}

/// The three per-field conversion bodies — into the stub, out of it infallibly, and
/// out of it fallibly — keyed by the field's declared mode and its shape
/// (plain / `Option` / `Vec`).
///
/// Extracted from [`derive_struct`] because the three matches are one unit and
/// belong together, and because `derive_struct` sits at the function-length limit.
fn field_conversions(
    field: &Field,
    field_ident: &Ident,
    conv: &FieldConversion,
    support: &TokenStream,
) -> syn::Result<(TokenStream, Option<TokenStream>, TokenStream)> {
    let field_ty = &field.ty;
    let is_optional = is_option_type(field_ty);
    // `repeated T` of a *message* element: `Into::into` on the `Vec` itself does
    // not exist, so the conversion has to run per element.
    let is_repeated = matches!(field_ty, Type::Path(p)
        if p.path.segments.last().is_some_and(|seg| seg.ident == "Vec"));
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
        // `message` adds nothing when the field is already `Option`: the stub is
        // one `Option` deep either way.
        (FieldConversion::Direct | FieldConversion::Message, true) => {
            quote! { v.#field_ident.map(::std::convert::Into::into) }
        }
        (FieldConversion::ViaString, false) => {
            quote! { v.#field_ident.to_string() }
        }
        (FieldConversion::ViaString, true) => {
            quote! { v.#field_ident.map(|x| x.to_string()) }
        }
        (FieldConversion::Message, false) if is_repeated => {
            quote! {
                v.#field_ident
                    .into_iter()
                    .map(::std::convert::Into::into)
                    .collect()
            }
        }
        // Required on the Rust side, `Option` on the stub: always `Some`.
        (FieldConversion::Message, false) => {
            quote! { ::std::option::Option::Some(::std::convert::Into::into(v.#field_ident)) }
        }
        (FieldConversion::Skip, _) => unreachable!("skipped above"),
    };

    // The infallible `From<Proto>` decode body. `via_string` fields have none:
    // `FromStr` can fail on a hostile peer's wire string, and panicking there is
    // a remote-DoS surface, so a struct carrying one gets no `From<Proto>` impl
    // at all (see `derive_struct`) — hence `None`, and its decoding runs solely
    // through the fallible `try_from_proto` below.
    let from_proto = if matches!(conv, FieldConversion::ViaString) {
        None
    } else {
        Some(match (&conv, is_optional) {
            (FieldConversion::Direct, false) => {
                quote! { ::std::convert::Into::into(v.#field_ident) }
            }
            // `message` adds nothing when the field is already `Option`: the stub
            // is one `Option` deep either way.
            (FieldConversion::Direct | FieldConversion::Message, true) => {
                quote! { v.#field_ident.map(::std::convert::Into::into) }
            }
            (FieldConversion::Message, false) if is_repeated => {
                quote! {
                    v.#field_ident
                        .into_iter()
                        .map(::std::convert::Into::into)
                        .collect()
                }
            }
            // Absent where the DTO requires a value. The infallible impl cannot
            // report that, so it defaults — `TryFromProto` is the surface that
            // reports it, and is what the generated client uses.
            (FieldConversion::Message, false) => {
                quote! {
                    v.#field_ident
                        .map(::std::convert::Into::into)
                        .unwrap_or_default()
                }
            }
            (FieldConversion::ViaString, _) => unreachable!("handled above"),
            (FieldConversion::Skip, _) => unreachable!("skipped above"),
        })
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
        // An optional field is not H1's concern: absence is legitimately `None`,
        // never a silently-zeroed required value. `message` here also covers an
        // optional **enum** (`Option<LeaderStatusDto>`), which prost renders as
        // `Option<i32>` and crosses via `From<i32>` — it has no
        // `TryFromProto<i32>` to recurse through — so both `Direct` and `Message`
        // stay on the infallible `Into`, exactly as before H1.
        (FieldConversion::Direct | FieldConversion::Message, true) => {
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
        // `repeated` of a message element: each element decodes fallibly. A bad
        // element one level down is a decode error, not a panic in `Into::into`.
        (FieldConversion::Message, false) if is_repeated => {
            quote! {
                ::std::clone::Clone::clone(&v.#field_ident)
                    .into_iter()
                    .map(#support::grpc_repr::TryFromProto::try_from_proto_wire)
                    .collect::<::std::result::Result<_, _>>()?
            }
        }
        // A peer that omitted a required message is a wire-shape error, not a
        // silently zeroed field. When present, recurse through
        // `try_from_proto_wire` so the fallible guarantee holds all the way down
        // rather than falling back to the infallible `From` one level in.
        (FieldConversion::Message, false) => {
            quote! {
                ::std::clone::Clone::clone(&v.#field_ident)
                    .ok_or(#support::grpc_repr::ProtoDecodeError::MissingMessage(
                        #support::grpc_repr::MissingRequiredMessage { field: #field_name },
                    ))
                    .and_then(
                        <#inner_ty as #support::grpc_repr::TryFromProto<_>>::try_from_proto_wire,
                    )?
            }
        }
        (FieldConversion::Skip, _) => unreachable!("skipped above"),
    };

    Ok((to_proto, from_proto, try_from_proto))
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
    let mut has_via_string = false;

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
        if matches!(conv, FieldConversion::ViaString) {
            has_via_string = true;
        }
        let (to_proto, from_proto, try_from_proto) =
            field_conversions(field, field_ident, &conv, &support)?;
        to_proto_inits.push(quote! { #field_ident: #to_proto });
        // `None` only for `via_string`, which suppresses the whole `From<Proto>`
        // impl below — so the remaining `from_proto_inits` are never emitted.
        if let Some(from_proto) = from_proto {
            from_proto_inits.push(quote! { #field_ident: #from_proto });
        }
        try_from_proto_inits.push(quote! { #field_ident: #try_from_proto });
    }

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    // A `via_string` field has no infallible decode — `FromStr` can fail on a
    // hostile peer's wire string and panicking there is a remote-DoS surface —
    // so a struct that contains one gets NO `From<Proto> for Rust` impl. Its
    // only decode path is the fallible `try_from_proto` / `TryFromProto` below.
    // The encode direction (`From<Rust> for Proto`) is always infallible and is
    // emitted regardless.
    let from_proto_impl = if has_via_string {
        quote! {}
    } else {
        quote! {
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
        }
    };

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

        #from_proto_impl

        // The fallible decode path. For a `via_string`-bearing struct this is the
        // *only* `Proto -> Rust` conversion (the infallible `From<Proto>` is
        // omitted); elsewhere it is the wire-safe alternative to it. Use it on
        // wire-input paths (e.g. tonic server handlers) where a malformed
        // `via_string` field would otherwise be undecodable.
        #[automatically_derived]
        impl #impl_generics #rust_ty #ty_generics #where_clause {
            pub fn try_from_proto(
                v: &#stub_path,
            ) -> ::std::result::Result<Self, #support::grpc_repr::ProtoDecodeError> {
                let _ = &v;
                ::std::result::Result::Ok(Self {
                    #(#try_from_proto_inits,)*
                    #(#skipped_from_inits),*
                })
            }
        }

        // Trait form of the same conversion, so `#[toolkit::grpc_contract]`
        // codegen can convert any response type uniformly without knowing
        // whether it derives from a struct or an enum.
        #[automatically_derived]
        impl #impl_generics #support::grpc_repr::TryFromProto<#stub_path>
            for #rust_ty #ty_generics #where_clause
        {
            fn try_from_proto_wire(
                proto: #stub_path,
            ) -> ::std::result::Result<Self, #support::grpc_repr::ProtoDecodeError> {
                Self::try_from_proto(&proto)
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

        // Trait form, so `#[toolkit::grpc_contract]` codegen can convert any
        // response type uniformly. An enum has no `via_string` fields, so this
        // is infallible in practice; it keeps the forward-compatible
        // unknown-discriminant fallback of `From<#stub_path>` rather than
        // turning peer-side schema drift into a hard error.
        #[automatically_derived]
        impl #impl_generics #support::grpc_repr::TryFromProto<#stub_path>
            for #rust_ty #ty_generics #where_clause
        {
            fn try_from_proto_wire(
                proto: #stub_path,
            ) -> ::std::result::Result<Self, #support::grpc_repr::ProtoDecodeError> {
                ::std::result::Result::Ok(
                    <#rust_ty #ty_generics as ::std::convert::From<#stub_path>>::from(proto),
                )
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
