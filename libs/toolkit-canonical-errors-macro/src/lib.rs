#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! Proc-macro for canonical error resource types.
//!
//! Provides the `#[resource_error(...)]` attribute macro.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::LitStr;
use syn::parse_macro_input;

/// Attribute macro that generates a resource error type with builder-returning
/// constructors for the 13 canonical error categories that carry a
/// `resource_type`.
///
/// # Usage
///
/// ```rust,ignore
/// use toolkit_canonical_errors::resource_error;
///
/// #[resource_error(gts_id!("cf.core.users.user.v1~"))]
/// struct UserResourceError;
/// ```
///
/// The GTS resource-type literal is validated at compile time.
///
/// Generated constructors either accept a detail string or are zero-argument
/// (using a default message). Each returns a `ResourceErrorBuilder` with
/// typestate enforcement.
enum ResourceTypeInput {
    Literal(LitStr),
    PrefixMacro(LitStr),
}

impl ResourceTypeInput {
    /// Full literal used only for validation performed by this proc macro.
    fn full_lit(&self) -> LitStr {
        match self {
            Self::Literal(lit) => lit.clone(),
            Self::PrefixMacro(suffix) => LitStr::new(
                &format!("{}{}", gts_id::GTS_ID_PREFIX, suffix.value()),
                suffix.span(),
            ),
        }
    }
}

fn parse_resource_type_input(attr: TokenStream2) -> syn::Result<ResourceTypeInput> {
    match syn::parse2::<syn::Expr>(attr) {
        Ok(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        })) => Ok(ResourceTypeInput::Literal(s)),
        Ok(syn::Expr::Macro(syn::ExprMacro { mac, .. }))
            if mac
                .path
                .segments
                .last()
                .is_some_and(|seg| seg.ident == "gts_id") =>
        {
            let suffix: LitStr = mac.parse_body().map_err(|_| {
                syn::Error::new_spanned(
                    mac,
                    "`gts_id!` takes a single string-literal suffix, \
                     e.g. `gts_id!(\"cf.core.users.user.v1~\")`",
                )
            })?;
            Ok(ResourceTypeInput::PrefixMacro(suffix))
        }
        Ok(other) => Err(syn::Error::new_spanned(
            other,
            "expected a string literal or `gts_id!(\"...\")`",
        )),
        Err(e) => Err(e),
    }
}

#[proc_macro_attribute]
pub fn resource_error(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Accept either a plain full-id string literal
    // or the `gts_id!("<suffix>")` marker form to avoid hard-coding the
    // configured GTS ID prefix. It is materialised inside this macro, so
    // callers do not need a direct `toolkit-gts` dependency.
    let attr_ts: proc_macro2::TokenStream = attr.into();
    let resource_type = match parse_resource_type_input(attr_ts) {
        Ok(input) => input,
        Err(e) => return e.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as syn::ItemStruct);

    match generate_resource_error(&resource_type, &input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

const CANONICAL_ERRORS_PKG: &str = "cf-gears-toolkit-canonical-errors";
const CANONICAL_ERRORS_LIB: &str = "toolkit_canonical_errors";

/// Resolves the path to the `toolkit_canonical_errors` crate at the expansion site.
///
/// Uses `CARGO_PKG_NAME` to detect when the macro is invoked from within the
/// canonical-errors package itself (e.g. integration tests), where the lib name
/// (`toolkit_canonical_errors`) differs from the package name
/// (`cf-gears-toolkit-canonical-errors`). For external consumers the resolution is
/// delegated to `proc_macro_crate`.
fn resolve_crate_path(gts_lit: &LitStr) -> syn::Result<TokenStream2> {
    let in_self = std::env::var("CARGO_PKG_NAME").is_ok_and(|p| p == CANONICAL_ERRORS_PKG);

    if in_self {
        // Inside the cf-gears-toolkit-canonical-errors package.
        // `crate` is correct only for the lib target; integration tests and
        // examples access the library as an extern crate by its [lib] name.
        let is_lib = std::env::var("CARGO_CRATE_NAME").is_ok_and(|c| c == CANONICAL_ERRORS_LIB);

        if is_lib {
            return Ok(quote!(crate));
        }

        let ident = syn::Ident::new(CANONICAL_ERRORS_LIB, proc_macro2::Span::call_site());
        return Ok(quote!(::#ident));
    }

    match proc_macro_crate::crate_name(CANONICAL_ERRORS_PKG) {
        Ok(proc_macro_crate::FoundCrate::Itself) => Ok(quote!(crate)),
        Ok(proc_macro_crate::FoundCrate::Name(n)) => {
            // When the dependency is not renamed, `proc_macro_crate` returns the
            // package name normalised to a Rust identifier.  If [lib].name differs
            // from the package name (as it does here) we must map back to the actual
            // lib name, otherwise the generated code references a non-existent crate.
            let pkg_normalized = CANONICAL_ERRORS_PKG.replace('-', "_");
            let effective = if n == pkg_normalized {
                CANONICAL_ERRORS_LIB
            } else {
                &n
            };
            let ident = syn::Ident::new(effective, proc_macro2::Span::call_site());
            Ok(quote!(::#ident))
        }
        Err(_) => Err(syn::Error::new_spanned(
            gts_lit,
            "cf-gears-toolkit-canonical-errors must be a direct dependency",
        )),
    }
}

fn generate_resource_error(
    resource_type: &ResourceTypeInput,
    input: &syn::ItemStruct,
) -> syn::Result<TokenStream2> {
    let gts_lit = resource_type.full_lit();
    let gts_type = gts_lit.value();
    validate_gts_resource_type_str(&gts_type, gts_lit.span())?;

    if !matches!(input.fields, syn::Fields::Unit) {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[resource_error] only supports unit structs (e.g. `struct MyError;`)",
        ));
    }
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[resource_error] does not support generics or where-clauses",
        ));
    }

    let crate_path = resolve_crate_path(&gts_lit)?;

    let vis = &input.vis;
    let name = &input.ident;

    Ok(quote! {
        #input

        impl #name {
            // --- resource_name required ---

            #vis fn not_found(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceMissing,
                    #crate_path::builder::NoContext,
                >
            {
                #crate_path::ResourceErrorBuilder::__not_found(#gts_lit, detail)
            }

            #vis fn already_exists(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceMissing,
                    #crate_path::builder::NoContext,
                >
            {
                #crate_path::ResourceErrorBuilder::__already_exists(#gts_lit, detail)
            }

            #vis fn data_loss(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceMissing,
                    #crate_path::builder::NoContext,
                >
            {
                #crate_path::ResourceErrorBuilder::__data_loss(#gts_lit, detail)
            }

            // --- resource_name optional ---

            #vis fn aborted(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceOptional,
                    #crate_path::builder::NeedsReason,
                >
            {
                #crate_path::ResourceErrorBuilder::__aborted(#gts_lit, detail)
            }

            #vis fn unknown(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceOptional,
                    #crate_path::builder::NoContext,
                >
            {
                #crate_path::ResourceErrorBuilder::__unknown(#gts_lit, detail)
            }

            #vis fn deadline_exceeded(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceOptional,
                    #crate_path::builder::NoContext,
                >
            {
                #crate_path::ResourceErrorBuilder::__deadline_exceeded(#gts_lit, detail)
            }

            // --- resource_name absent ---

            #vis fn permission_denied()
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceAbsent,
                    #crate_path::builder::NeedsReason,
                >
            {
                #crate_path::ResourceErrorBuilder::__permission_denied(#gts_lit, "You do not have permission to perform this operation")
            }

            #vis fn unimplemented(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceOptional,
                    #crate_path::builder::NoContext,
                >
            {
                #crate_path::ResourceErrorBuilder::__unimplemented(#gts_lit, detail)
            }

            #vis fn cancelled()
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceAbsent,
                    #crate_path::builder::NoContext,
                >
            {
                #crate_path::ResourceErrorBuilder::__cancelled(#gts_lit, "Operation cancelled by the client")
            }

            // --- resource_name optional, needs field violations ---

            #vis fn invalid_argument()
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceOptional,
                    #crate_path::builder::NeedsFieldViolation,
                >
            {
                #crate_path::ResourceErrorBuilder::__invalid_argument(#gts_lit, "Request validation failed")
            }

            #vis fn out_of_range(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceOptional,
                    #crate_path::builder::NeedsFieldViolation,
                >
            {
                #crate_path::ResourceErrorBuilder::__out_of_range(#gts_lit, detail)
            }

            // --- resource_name optional, needs quota violations ---

            #vis fn resource_exhausted(detail: impl Into<String>)
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceOptional,
                    #crate_path::builder::NeedsQuotaViolation,
                >
            {
                #crate_path::ResourceErrorBuilder::__resource_exhausted(#gts_lit, detail)
            }

            // --- resource_name optional, needs precondition violations ---

            #vis fn failed_precondition()
                -> #crate_path::ResourceErrorBuilder<
                    #crate_path::builder::ResourceOptional,
                    #crate_path::builder::NeedsPreconditionViolation,
                >
            {
                #crate_path::ResourceErrorBuilder::__failed_precondition(#gts_lit, "Operation precondition not met")
            }
        }
    })
}

/// Validates a GTS resource-type literal at proc-macro time by delegating
/// to the canonical `GtsId` parser from the `gts-id` crate.
///
/// This is equivalent to `GtsTypeId::try_new` from the `gts` crate (which
/// internally calls `GtsId::try_new` + `is_type()`), but avoids pulling the
/// heavy `gts` crate (jsonschema, schemars, …) into a proc-macro.
fn validate_gts_resource_type_str(s: &str, span: Span) -> syn::Result<()> {
    let parsed = gts_id::GtsId::try_new(s)
        .map_err(|e| syn::Error::new(span, format!("invalid GTS resource type: {e}")))?;
    if !parsed.is_type() {
        return Err(syn::Error::new(
            span,
            "GTS resource type must end with '~' (type id, not instance id)",
        ));
    }
    Ok(())
}
