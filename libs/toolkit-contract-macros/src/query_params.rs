//! `#[derive(QueryParams)]` — makes a struct usable as a REST query parameter.
//!
//! Emits `impl QueryParams`, whose `openapi_params()` is the list the generated
//! route registers into the `OpenAPI` document. Generating the spec from the same
//! declaration that drives the wire format is the point: the previous design
//! inferred the spec separately from the Rust type at macro-expansion time and
//! could not see a struct's fields at all, so flat-struct query parameters went
//! undocumented entirely.
//!
//! Two things are checked here because both fail confusingly at runtime
//! otherwise:
//!
//! * every field's leaf type must implement `QueryScalar`, which is what keeps
//!   nested structs out (a query string cannot represent them unambiguously);
//! * a `Vec<..>` field must carry `#[serde(default)]`, because an empty vector
//!   emits no key and would otherwise come back as `missing field`.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, Type};

use crate::support::contract_support_path;

pub fn generate(input: &DeriveInput) -> syn::Result<TokenStream> {
    let support = contract_support_path();
    let ident = &input.ident;

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            ident,
            "QueryParams can only be derived for a struct: a query string is a flat list of \
             key/value pairs, which an enum or union cannot describe",
        ));
    };

    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            ident,
            "QueryParams requires named fields: each field name is the query-parameter name",
        ));
    };

    let mut specs = Vec::new();
    let mut assertions = Vec::new();

    for field in &named.named {
        // Unwrap-safe: `Fields::Named` guarantees an ident.
        let Some(field_ident) = field.ident.as_ref() else {
            continue;
        };
        if is_skipped(field) {
            continue;
        }

        let name = serde_name(field, &field_ident.to_string());
        let (leaf, kind) = classify(&field.ty);

        if kind == FieldKind::Array && !has_serde_default(field) {
            return Err(syn::Error::new(
                field.span(),
                format!(
                    "`{field_ident}` is a repeated query parameter and needs `#[serde(default)]`.\n\
                     An empty `Vec` emits no key at all, so without a default the request fails to \
                     deserialize with `missing field `{field_ident}``."
                ),
            ));
        }

        let required = kind == FieldKind::Required;
        let array = kind == FieldKind::Array;

        // Naming the bound in a const item gives the "not a query scalar"
        // diagnostic a span on the offending field rather than on the derive.
        assertions.push(quote_spanned! { field.ty.span() =>
            const _: fn() = || {
                fn assert_query_scalar<T: #support::query::QueryScalar + ?Sized>() {}
                let _ = assert_query_scalar::<#leaf>;
            };
        });

        specs.push(quote_spanned! { field.ty.span() =>
            #support::query::QueryParamSpec {
                name: #name,
                openapi_type: <#leaf as #support::query::QueryScalar>::OPENAPI_TYPE,
                required: #required,
                array: #array,
            }
        });
    }

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        #(#assertions)*

        #[automatically_derived]
        impl #impl_generics #support::query::QueryParams for #ident #ty_generics #where_clause {
            fn openapi_params() -> &'static [#support::query::QueryParamSpec] {
                &[#(#specs),*]
            }
        }
    })
}

#[derive(PartialEq, Eq)]
enum FieldKind {
    /// A plain scalar: present on the wire, required in the spec.
    Required,
    /// `Option<..>`: omitted when `None`.
    Optional,
    /// `Vec<..>`: zero or more repeated keys.
    Array,
}

/// Peel one `Option<..>` / `Vec<..>` wrapper and report which it was.
///
/// Only one layer: `Option<Vec<T>>` and other nestings are left as-is so the
/// `QueryScalar` bound rejects them with a diagnostic that names the actual
/// type, rather than silently treating them as something they are not.
fn classify(ty: &Type) -> (&Type, FieldKind) {
    if let Some(inner) = generic_arg(ty, "Option") {
        return (inner, FieldKind::Optional);
    }
    if let Some(inner) = generic_arg(ty, "Vec") {
        return (inner, FieldKind::Array);
    }
    (ty, FieldKind::Required)
}

fn generic_arg<'a>(ty: &'a Type, wrapper: &str) -> Option<&'a Type> {
    let Type::Path(path) = ty else { return None };
    let seg = path.path.segments.last()?;
    if seg.ident != wrapper {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Honour `#[serde(rename = "...")]` so the spec matches the wire name.
fn serde_name(field: &syn::Field, fallback: &str) -> String {
    let mut name = fallback.to_owned();
    for attr in &field.attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        drop(attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename")
                && let Ok(value) = meta.value()
                && let Ok(lit) = value.parse::<syn::LitStr>()
            {
                name = lit.value();
            }
            // Unknown serde keys are serde's business, not ours.
            let _ = meta.input;
            Ok(())
        }));
    }
    name
}

fn has_serde_default(field: &syn::Field) -> bool {
    serde_flag(field, "default")
}

fn is_skipped(field: &syn::Field) -> bool {
    serde_flag(field, "skip")
}

fn serde_flag(field: &syn::Field, flag: &str) -> bool {
    let mut found = false;
    for attr in &field.attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        drop(attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(flag) {
                found = true;
            }
            Ok(())
        }));
    }
    found
}
