//! Proc-macro implementation for `#[domain_model]` attribute.
//!
//! This macro marks structs and enums as domain models and validates that they don't
//! contain infrastructure types. Validation is performed at macro expansion time by
//! checking field type paths against forbidden crates and type names.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Fields, Type, TypePath};

/// Forbidden crate names for domain models.
///
/// These are external infrastructure crates that should not appear in domain models.
/// We check if the FIRST segment of a type path matches any of these.
///
/// Example: `http::StatusCode` → first segment is `http` → BLOCKED
/// Example: `company_api::Thing` → first segment is `company_api` → ALLOWED
const FORBIDDEN_CRATES: &[&str] = &[
    // Database frameworks
    "sqlx", "sea_orm", // HTTP/Web frameworks
    "http", "axum", "hyper", // External service clients
    "reqwest", "tonic",
];

/// Forbidden two-segment path prefixes.
///
/// Some forbidden paths require checking the first TWO segments.
/// Format: (`first_segment`, `second_segment`)
const FORBIDDEN_PATH_PREFIXES: &[(&str, &str)] = &[
    // File system (should be abstracted)
    ("std", "fs"),
    ("tokio", "fs"),
];

/// Forbidden type names that are database-specific.
///
/// These are checked as the LAST segment of a type path.
/// Only includes names that are unambiguously database-related
/// and would never be legitimate domain type names.
///
/// Note: Generic names like `StatusCode`, `Request`, `Response` are NOT included
/// because they could be legitimate domain types. The crate-level check handles
/// `http::StatusCode` etc.
const FORBIDDEN_TYPE_NAMES: &[&str] = &["PgPool", "MySqlPool", "SqlitePool", "DatabaseConnection"];

/// Expands the `#[domain_model]` attribute macro.
///
/// This function:
/// 1. Validates that all field types are free of infrastructure dependencies
/// 2. Returns clear error messages if forbidden types are found
/// 3. Generates `impl DomainModel for T {}` if validation passes
///
/// Unlike the previous implementation that used trait bounds (which produced
/// generic "trait not satisfied" errors), this validates type names directly
/// during macro expansion, providing clear, actionable error messages.
pub fn expand_domain_model(input: &DeriveInput) -> TokenStream {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // Collect all fields with their types and optional names
    let fields_with_context: Vec<FieldContext> = match &input.data {
        syn::Data::Struct(data) => collect_struct_fields(&data.fields),
        syn::Data::Enum(data) => collect_enum_fields(data),
        syn::Data::Union(_) => {
            return syn::Error::new_spanned(name, "domain_model cannot be applied to unions")
                .to_compile_error();
        }
    };

    // Validate each field type
    for field_ctx in &fields_with_context {
        if let Err(err) = validate_field_type(field_ctx.ty, &field_ctx.context) {
            return err.to_compile_error();
        }
    }

    // If validation passed, generate the struct/enum and implement DomainModel trait
    quote! {
        #input

        impl #impl_generics ::modkit::domain::DomainModel for #name #ty_generics #where_clause {}
    }
}

/// Context information about a field for error reporting.
struct FieldContext<'a> {
    ty: &'a Type,
    context: String,
}

/// Collects fields from a struct with context for error messages.
fn collect_struct_fields(fields: &Fields) -> Vec<FieldContext<'_>> {
    match fields {
        Fields::Named(fields) => fields
            .named
            .iter()
            .map(|f| {
                // Named fields always have an ident by syn's definition
                #[allow(clippy::unwrap_used)]
                let field_name = &f.ident.as_ref().unwrap();
                FieldContext {
                    ty: &f.ty,
                    context: format!("field '{field_name}'"),
                }
            })
            .collect(),
        Fields::Unnamed(fields) => fields
            .unnamed
            .iter()
            .enumerate()
            .map(|(idx, f)| FieldContext {
                ty: &f.ty,
                context: format!("tuple field {idx}"),
            })
            .collect(),
        Fields::Unit => vec![],
    }
}

/// Collects fields from enum variants with context for error messages.
fn collect_enum_fields(data: &syn::DataEnum) -> Vec<FieldContext<'_>> {
    data.variants
        .iter()
        .flat_map(|variant| {
            let variant_name = &variant.ident;
            match &variant.fields {
                Fields::Named(fields) => fields
                    .named
                    .iter()
                    .map(|f| {
                        // Named fields always have an ident by syn's definition
                        #[allow(clippy::unwrap_used)]
                        let field_name = &f.ident.as_ref().unwrap();
                        FieldContext {
                            ty: &f.ty,
                            context: format!("field '{field_name}' in variant '{variant_name}'"),
                        }
                    })
                    .collect::<Vec<_>>(),
                Fields::Unnamed(fields) => fields
                    .unnamed
                    .iter()
                    .enumerate()
                    .map(|(idx, f)| FieldContext {
                        ty: &f.ty,
                        context: format!("tuple field {idx} in variant '{variant_name}'"),
                    })
                    .collect::<Vec<_>>(),
                Fields::Unit => vec![],
            }
        })
        .collect()
}

/// Validates that a type doesn't contain forbidden infrastructure types.
///
/// This function checks type paths against forbidden crates and type names.
/// It recursively checks generic arguments (e.g., `Option<http::StatusCode>`).
///
/// Returns Ok(()) if the type is valid, or Err with a descriptive error.
fn validate_field_type(ty: &Type, context: &str) -> syn::Result<()> {
    match ty {
        // Check path types (most common case)
        Type::Path(type_path) => validate_type_path(type_path, context),

        // Recursively check inner types
        Type::Reference(type_ref) => validate_field_type(&type_ref.elem, context),
        Type::Slice(type_slice) => validate_field_type(&type_slice.elem, context),
        Type::Array(type_array) => validate_field_type(&type_array.elem, context),
        Type::Ptr(type_ptr) => validate_field_type(&type_ptr.elem, context),
        Type::Tuple(type_tuple) => {
            for elem_ty in &type_tuple.elems {
                validate_field_type(elem_ty, context)?;
            }
            Ok(())
        }

        // TraitObject: check trait bounds and their generic arguments
        Type::TraitObject(trait_obj) => {
            for bound in &trait_obj.bounds {
                if let syn::TypeParamBound::Trait(trait_bound) = bound {
                    if let Some(reason) = check_forbidden_path(&trait_bound.path) {
                        return Err(syn::Error::new_spanned(
                            ty,
                            format!(
                                "{context} uses forbidden trait ({reason}). \
                                 Domain models must be free of infrastructure dependencies. \
                                 Move infrastructure types to the infra/ or api/ layers."
                            ),
                        ));
                    }
                    // Check generic arguments in trait bounds (e.g., dyn Trait<http::StatusCode>)
                    for segment in &trait_bound.path.segments {
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            for arg in &args.args {
                                match arg {
                                    syn::GenericArgument::Type(inner_ty) => {
                                        validate_field_type(inner_ty, context)?;
                                    }
                                    syn::GenericArgument::AssocType(assoc) => {
                                        validate_field_type(&assoc.ty, context)?;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }

        // Other type kinds are typically safe or will be caught by other means
        _ => Ok(()),
    }
}

/// Validates a type path (e.g., `http::StatusCode`, `Option<String>`).
fn validate_type_path(type_path: &TypePath, context: &str) -> syn::Result<()> {
    let path = &type_path.path;

    // Check qualified self type if present (e.g., <http::StatusCode as Trait>::Output)
    if let Some(qself) = &type_path.qself {
        validate_field_type(&qself.ty, context)?;
    }

    // Check if the type path is forbidden
    if let Some(reason) = check_forbidden_path(path) {
        let path_str = type_path_to_string(path);
        return Err(syn::Error::new_spanned(
            type_path,
            format!(
                "{context} has type '{path_str}' which is forbidden ({reason}). \
                 Domain models must be free of infrastructure dependencies like \
                 database types (sqlx, sea_orm) or HTTP types (http, axum, hyper). \
                 Move infrastructure types to the infra/ or api/ layers."
            ),
        ));
    }

    // Recursively check generic arguments in ALL segments (not just last)
    // This catches cases like Outer<http::StatusCode>::Inner
    for segment in &path.segments {
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
            for arg in &args.args {
                if let syn::GenericArgument::Type(inner_ty) = arg {
                    validate_field_type(inner_ty, context)?;
                }
            }
        }
    }

    Ok(())
}

/// Checks if a path is forbidden and returns the reason if so.
///
/// Uses segment-based checking to avoid false positives:
/// - `http::StatusCode` → first segment `http` is forbidden
/// - `company_api::Thing` → first segment `company_api` is NOT forbidden
/// - `std::fs::File` → first two segments `std::fs` are forbidden
fn check_forbidden_path(path: &syn::Path) -> Option<String> {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect();

    if segments.is_empty() {
        return None;
    }

    // Check first segment against forbidden crates
    let first = &segments[0];
    if FORBIDDEN_CRATES.contains(&first.as_str()) {
        return Some(format!("crate '{first}'"));
    }

    // Check first two segments for special prefixes (std::fs, tokio::fs)
    if segments.len() >= 2 {
        let second = &segments[1];
        for &(crate_name, module_name) in FORBIDDEN_PATH_PREFIXES {
            if first == crate_name && second == module_name {
                return Some(format!("path '{crate_name}::{module_name}'"));
            }
        }
    }

    // Check last segment against forbidden type names (DB-specific only)
    if let Some(last) = segments.last()
        && FORBIDDEN_TYPE_NAMES.contains(&last.as_str())
    {
        return Some(format!("type name '{last}'"));
    }

    None
}

/// Converts a `syn::Path` to a string (e.g., `http::StatusCode`).
fn type_path_to_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

#[cfg(test)]
#[path = "domain_model_tests.rs"]
mod tests;
