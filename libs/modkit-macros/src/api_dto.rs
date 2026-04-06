use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use syn::punctuated::Punctuated;
use syn::{DeriveInput, Ident, Token};

const ALLOWED_FLAGS: &[&str] = &["request", "response"];

/// Validates `api_dto` flags for unknown or duplicate identifiers.
/// Returns Ok(()) if valid, or Err(TokenStream) with compile error if invalid.
pub fn validate_flags(args: &Punctuated<Ident, Token![,]>) -> Result<(), TokenStream> {
    let mut seen_flags = HashSet::new();

    for ident in args {
        let flag_str = ident.to_string();

        // Check if flag is allowed
        if !ALLOWED_FLAGS.contains(&flag_str.as_str()) {
            let err = syn::Error::new_spanned(
                ident,
                format!(
                    "unknown flag '{flag_str}'; expected one of: {}",
                    ALLOWED_FLAGS.join(", ")
                ),
            );
            return Err(err.to_compile_error());
        }

        // Check for duplicates
        if !seen_flags.insert(flag_str.clone()) {
            let err = syn::Error::new_spanned(ident, format!("duplicate flag '{flag_str}'"));
            return Err(err.to_compile_error());
        }
    }

    Ok(())
}

pub fn expand_api_dto(args: &Punctuated<Ident, Token![,]>, input: &DeriveInput) -> TokenStream {
    if let Err(err) = validate_flags(args) {
        return err;
    }

    let has_request = args.iter().any(|id| id == "request");
    let has_response = args.iter().any(|id| id == "response");

    if !has_request && !has_response {
        return quote! {
            compile_error!("api_dto macro requires at least one of 'request' or 'response' arguments");
        };
    }

    let (serialize, deserialize) = (has_response, has_request);
    let name = &input.ident;
    let ser = if serialize {
        quote! { ::serde::Serialize, }
    } else {
        quote! {}
    };
    let resp_trait_impl = if serialize {
        quote! { impl ::modkit::api::api_dto::ResponseApiDto for #name {} }
    } else {
        quote! {}
    };
    let de = if deserialize {
        quote! { ::serde::Deserialize, }
    } else {
        quote! {}
    };
    let req_trait_impl = if deserialize {
        quote! { impl ::modkit::api::api_dto::RequestApiDto for #name {} }
    } else {
        quote! {}
    };

    let has_serde = serialize || deserialize;
    let serde_attr = if has_serde {
        quote! { #[serde(rename_all = "snake_case")] }
    } else {
        quote! {}
    };

    quote! {
        #[derive(#ser #de utoipa::ToSchema)]
        #serde_attr
        #input
        #req_trait_impl
        #resp_trait_impl
    }
}

#[cfg(test)]
#[path = "api_dto_tests.rs"]
mod tests;
