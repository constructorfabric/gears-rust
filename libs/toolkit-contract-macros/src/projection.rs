//! Shared codegen helpers for projection-trait macros (`rest_contract`,
//! `grpc_contract`).
//!
//! Both macros emit a parallel set of artifacts from a projection trait:
//! a cleaned trait with delegating defaults (PRD #1536 D3), a binding fn,
//! a generated client struct, an `impl <BaseTrait>` for that client, and an
//! empty `impl <ProjectionTrait>` so it picks up the delegating defaults.
//!
//! The procedural shape is shared even though the source models differ;
//! these helpers operate on generic inputs (idents, types, attribute names)
//! so each contract macro can stay close to its own model while delegating
//! the boilerplate here.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Ident, Path, TraitItemFn, Type};

/// Naming convention: `{TraitIdent}Client`. Shared between REST and gRPC.
pub fn client_struct_ident(trait_ident: &Ident) -> Ident {
    format_ident!("{}Client", trait_ident)
}

/// Returns `true` when the type path ends in a segment named `name`.
/// Used to detect `SecurityContext` and similar marker parameters whose
/// type may be re-exported under different paths.
pub fn type_path_ends_with(ty: &Type, name: &str) -> bool {
    if let Type::Path(p) = ty
        && let Some(last) = p.path.segments.last()
    {
        return last.ident == name;
    }
    false
}

/// Strip method-level helper attributes by ident name (e.g. `get`, `post`,
/// `rpc`, `streaming`, `retryable`). Mutates in place.
pub fn strip_method_attrs(method: &mut TraitItemFn, attr_names: &[&str]) {
    method
        .attrs
        .retain(|attr| !attr_names.iter().any(|name| attr.path().is_ident(name)));
}

/// Return type for a streaming projection method:
/// `-> Pin<Box<dyn Stream<Item = Result<#ok, #err>> + Send + 'static>>`.
pub fn streaming_return_type(ok: &Type, err: &Type) -> TokenStream {
    quote! {
        -> ::std::pin::Pin<::std::boxed::Box<
            dyn ::futures_core::Stream<Item = ::std::result::Result<#ok, #err>>
                + ::std::marker::Send + 'static
        >>
    }
}

/// Rewrite a method signature so its return type matches the streaming
/// projection convention (`Pin<Box<dyn Stream>>`) and drops `async`.
pub fn rewrite_streaming_signature(method: &mut TraitItemFn, ok: &Type, err: &Type) {
    method.sig.asyncness = None;
    method.sig.output = syn::parse_quote! {
        -> ::std::pin::Pin<::std::boxed::Box<
            dyn ::futures_core::Stream<Item = ::std::result::Result<#ok, #err>>
                + ::std::marker::Send + 'static
        >>
    };
}

/// Build a default delegating body: `<Self as BaseTrait>::method(self, args).await?`
/// (or sync `()` for streaming methods that return a non-async stream type).
///
/// `streaming = true` skips the `.await` since streaming methods are non-async
/// (they return a stream synchronously).
pub fn build_delegation_body<'a, I>(
    base_trait: &Path,
    method_ident: &Ident,
    arg_idents: I,
    streaming: bool,
) -> syn::Block
where
    I: IntoIterator<Item = &'a Ident>,
{
    let args: Vec<&Ident> = arg_idents.into_iter().collect();
    if streaming {
        syn::parse_quote! {
            {
                <Self as #base_trait>::#method_ident(self, #(#args),*)
            }
        }
    } else {
        syn::parse_quote! {
            {
                <Self as #base_trait>::#method_ident(self, #(#args),*).await
            }
        }
    }
}

/// Empty `impl <ProjectionTrait> for {Trait}Client {}` gated on a feature.
/// PRD #1536 D3: with delegating defaults on the projection trait, the
/// empty impl is enough to satisfy `Arc<dyn ProjectionTrait>`.
pub fn generate_projection_impl_for_client(
    projection_ident: &Ident,
    client_ident: &Ident,
    feature: &str,
) -> TokenStream {
    quote! {
        #[cfg(feature = #feature)]
        #[::async_trait::async_trait]
        impl #projection_ident for #client_ident {}
    }
}

/// Render `( &self, p1: T1, p2: T2, ... )` from an iterator of `(ident, type)`.
/// Filters out `self` automatically.
pub fn render_method_inputs<'a, I>(params: I) -> TokenStream
where
    I: IntoIterator<Item = (&'a Ident, &'a Type)>,
{
    let entries = params.into_iter().filter_map(|(ident, ty)| {
        if ident == "self" {
            None
        } else {
            Some(quote! { #ident: #ty })
        }
    });
    quote! { ( &self, #(#entries),* ) }
}

/// Render the unary or streaming return type for a generated client method.
pub fn render_method_return_ty(ok: &Type, err: &Type, streaming: bool) -> TokenStream {
    if streaming {
        streaming_return_type(ok, err)
    } else {
        quote! { -> ::std::result::Result<#ok, #err> }
    }
}
