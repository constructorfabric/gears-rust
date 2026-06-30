//! `#[toolkit::consumes(contract = path, from = "gear")]` — declare that a gear
//! consumes a contract provided by another gear, wired via eventual-readiness
//! directory discovery.
//!
//! Applied on the **gear struct** (alongside `#[toolkit::gear]`). Emits, behind
//! the `directory-rest-client` feature, a non-capturing `fn` plus an
//! `inventory::submit!` of a [`toolkit::discovery::ConsumerRegistration`] that
//! the runtime's proxy-wiring phase replays at startup. The wiring closure:
//!   1. short-circuits if a compile-time (local) impl is already in the
//!      `ClientHub` — a co-located provider wins;
//!   2. otherwise registers a directory-resolving REST client under the
//!      contract trait, which lazily resolves the provider endpoint per call.
//!
//! **Required feature.** The generated wire fn + `inventory::submit!` are gated
//! on `#[cfg(feature = "directory-rest-client")]`, evaluated in the *consuming
//! gear crate*. That crate MUST declare a feature named exactly
//! `directory-rest-client` that turns on both `<sdk>/directory-rest-client`
//! (so `<Contract>RestResolvingClient` exists) and
//! `toolkit/contract-directory-rest-client` (so `toolkit::discovery` + the
//! runtime proxy-wiring phase exist). If that feature is absent the wiring
//! silently compiles out — declare it (see the api-contracts example crate).
//!
//! **Topology:** `#[toolkit::consumes]` does NOT inject a topo-sort dependency.
//! A separate attribute cannot mutate the `&'static` deps baked by
//! `#[toolkit::gear]`, and auto-injecting `from` would make topo-sort fail for
//! a *remote* provider (not in the local registry) — contradicting the
//! non-blocking-startup model. Declare co-located hard deps explicitly in
//! `#[toolkit::gear(deps = [...])]`; the resolving client tolerates a
//! not-yet-ready or remote provider lazily.

use heck::ToSnakeCase;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Result as SynResult;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, Ident, ItemStruct, Path, Token};

use crate::support::{append_segment, parent_module};

/// Parsed `#[toolkit::consumes(...)]` attribute.
pub struct ConsumesAttr {
    pub contract: Path,
    pub from: String,
    /// Optional override for the resolving-client path.
    ///
    /// The default (`<parent>::<Contract>RestResolvingClient`) assumes the SDK
    /// re-exports the resolving client at the **same module level** as the base
    /// contract trait (as `#[toolkit::rest_contract]` SDKs do). Set this
    /// explicitly when the base trait and its REST projection live in different
    /// submodules.
    pub resolving_client: Option<Path>,
}

struct KeyValue {
    key: Ident,
    value: Expr,
}

impl Parse for KeyValue {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let key: Ident = input.parse()?;
        let _: Token![=] = input.parse()?;
        let value: Expr = input.parse()?;
        Ok(Self { key, value })
    }
}

impl Parse for ConsumesAttr {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let mut contract: Option<Path> = None;
        let mut from: Option<String> = None;
        let mut resolving_client: Option<Path> = None;

        let items: Punctuated<KeyValue, Token![,]> = Punctuated::parse_terminated(input)?;
        for KeyValue { key, value } in items {
            match key.to_string().as_str() {
                "contract" => contract = Some(expr_into_path(value, &key)?),
                "from" => from = Some(expr_into_string(value, &key)?),
                "resolving_client" => resolving_client = Some(expr_into_path(value, &key)?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown #[toolkit::consumes] arg `{other}`"),
                    ));
                }
            }
        }

        let contract = contract.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "missing required arg `contract = path::to::TraitName`",
            )
        })?;
        let from = from.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "missing required arg `from = \"provider-gear-name\"`",
            )
        })?;

        Ok(Self {
            contract,
            from,
            resolving_client,
        })
    }
}

fn expr_into_path(e: Expr, key: &Ident) -> SynResult<Path> {
    match e {
        Expr::Path(p) => Ok(p.path),
        other => Err(syn::Error::new_spanned(
            other,
            format!("`{key}` expects a path"),
        )),
    }
}

fn expr_into_string(e: Expr, key: &Ident) -> SynResult<String> {
    match e {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Ok(s.value()),
        other => Err(syn::Error::new_spanned(
            other,
            format!("`{key}` expects a string literal"),
        )),
    }
}

/// Top-level entry point invoked by the proc-macro shim in `lib.rs`.
pub fn generate(attr: &ConsumesAttr, item: &ItemStruct) -> SynResult<TokenStream> {
    let struct_ident = &item.ident;
    let contract_path = &attr.contract;
    let contract_ident = contract_path
        .segments
        .last()
        .map(|s| s.ident.clone())
        .ok_or_else(|| syn::Error::new_spanned(contract_path, "`contract` path is empty"))?;
    let from = &attr.from;

    // Default resolving-client path: `<parent>::<Contract>RestResolvingClient`,
    // mirroring how `#[toolkit::rest_contract]` names it (the REST projection of
    // `PaymentApi` is `PaymentApiRest`, whose resolving client is
    // `PaymentApiRestResolvingClient`). The SDK re-exports it at the same level
    // as the base contract trait.
    let resolving_client_path = if let Some(p) = &attr.resolving_client {
        p.clone()
    } else {
        let parent = parent_module(contract_path)?;
        append_segment(
            &parent,
            &format_ident!("{}RestResolvingClient", contract_ident),
        )
    };

    let wire_fn = format_ident!(
        "__toolkit_wire_{}_from_{}",
        contract_ident.to_string().to_snake_case(),
        from.replace(['-', '.'], "_"),
    );

    Ok(quote! {
        #item

        #[cfg(feature = "directory-rest-client")]
        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #wire_fn(
            __hub: &::toolkit::ClientHub,
            __resolver: ::std::sync::Arc<dyn ::toolkit::discovery::EndpointResolver>,
        ) -> ::anyhow::Result<()> {
            // A compile-time (local) impl already registered wins — Profile 1.
            if __hub.try_get::<dyn #contract_path>().is_some() {
                return ::std::result::Result::Ok(());
            }
            // Otherwise register the directory-resolving REST client. `tuning`
            // is `Default::default()` (inferred as `ClientTuning`).
            let __client = #resolving_client_path::new(
                __resolver,
                #from,
                ::core::default::Default::default(),
            );
            __hub.register::<dyn #contract_path>(::std::sync::Arc::new(__client));
            ::std::result::Result::Ok(())
        }

        #[cfg(feature = "directory-rest-client")]
        ::toolkit::inventory::submit! {
            ::toolkit::discovery::ConsumerRegistration {
                owner_gear: ::std::stringify!(#struct_ident),
                dep_gear: #from,
                wire: #wire_fn,
            }
        }
    })
}

// `parent_module` / `append_segment` are shared with `provides.rs` via
// `crate::support`.
