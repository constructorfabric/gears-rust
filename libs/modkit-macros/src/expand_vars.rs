//! Implementation of `#[derive(ExpandVars)]` with `#[expand_vars]` field attribute.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields};

pub fn derive(input: &DeriveInput) -> TokenStream {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(
                    &input.ident,
                    "ExpandVars can only be derived for structs with named fields",
                )
                .to_compile_error();
            }
        },
        _ => {
            return syn::Error::new_spanned(
                &input.ident,
                "ExpandVars can only be derived for structs",
            )
            .to_compile_error();
        }
    };

    let expand_stmts: Vec<_> = fields
        .iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("expand_vars")))
        .filter_map(|f| f.ident.as_ref())
        .map(|ident| {
            quote! { self.#ident.expand_vars()?; }
        })
        .collect();

    quote! {
        impl #impl_generics ::modkit::var_expand::ExpandVars for #name #ty_generics
            #where_clause
        {
            fn expand_vars(
                &mut self,
            ) -> ::std::result::Result<(), ::modkit::var_expand::ExpandVarsError> {
                use ::modkit::var_expand::ExpandVars as _;
                #(#expand_stmts)*
                Ok(())
            }
        }
    }
}
