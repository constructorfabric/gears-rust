//! `#[toolkit::consumes(contract = path, from = "gear")]` — declare that a gear
//! consumes a contract provided by another gear, wired via eventual-readiness
//! directory discovery.
//!
//! Applied on the **gear struct** (alongside `#[toolkit::gear]`). Emits a
//! non-capturing `fn` plus an `inventory::submit!` of a
//! [`toolkit::discovery::ConsumerRegistration`] that the runtime's proxy-wiring
//! phase replays at startup. The wiring closure:
//!   1. short-circuits if a compile-time (local) impl is already in the
//!      `ClientHub` — a co-located provider wins;
//!   2. otherwise registers a directory-resolving REST client under the
//!      contract trait, which lazily resolves the provider endpoint per call.
//!
//! **Requirements on the consuming crate.** The emission is unconditional, so
//! the crate needs a `toolkit` dependency (for `ClientHub`, `discovery` and the
//! `inventory` re-export) and its contract SDK built with `rest-client`, which
//! is what makes `<Contract>RestResolvingClient` exist. A missing SDK feature is
//! a hard `cannot find type` error rather than silently-dropped wiring; this
//! attribute used to sit behind a `directory-rest-client` feature evaluated in
//! the consuming crate, which turned a forgotten feature into a gear that
//! started fine and never found its provider.
//!
//! **Gear name is derived from the struct ident.** The emitted `owner_gear` is
//! the kebab-case of the annotated struct's name, because the runtime uses it
//! as a config key for the ADR-0004 static-endpoint override
//! (`gears.<owner_gear>.config.consumer_wiring.<dep>`) and in log/error fields.
//! A separate attribute cannot read the `name = "..."` given to
//! `#[toolkit::gear]`, so `#[toolkit::gear(name = ...)]` MUST be the kebab form
//! of the struct ident for the override to resolve — the convention everywhere
//! in this repo, but load-bearing here. A mismatch is reported at `warn!` by the
//! proxy-wiring phase rather than failing silently.
//!
//! **Topology:** `#[toolkit::consumes]` does NOT inject a topo-sort dependency.
//! A separate attribute cannot mutate the `&'static` deps baked by
//! `#[toolkit::gear]`, and auto-injecting `from` would make topo-sort fail for
//! a *remote* provider (not in the local registry) — contradicting the
//! non-blocking-startup model. Declare co-located hard deps explicitly in
//! `#[toolkit::gear(deps = [...])]`; the resolving client tolerates a
//! not-yet-ready or remote provider lazily.

use heck::{ToKebabCase, ToSnakeCase};
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

    // `owner_gear` is the *gear name*, not the Rust type name: the runtime uses
    // it as a config key (`gears.<owner_gear>.config.consumer_wiring.<dep>`) for
    // the ADR-0004 static-endpoint override, and as a log/error field. Gear
    // names are kebab-case, so derive it from the struct ident rather than
    // emitting `stringify!` — `stringify!(ApiContractsConsumer)` would look up
    // `gears.ApiContractsConsumer`, which no config declares, and the override
    // would silently never fire.
    let owner_gear = struct_ident.to_string().to_kebab_case();

    Ok(quote! {
        #item

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #wire_fn(
            __hub: &::toolkit::ClientHub,
            __resolver: ::std::sync::Arc<dyn ::toolkit::discovery::EndpointResolver>,
            __internal_token_provider: ::std::option::Option<
                &::toolkit::contract_support::runtime::config::InternalTokenProvider,
            >,
        ) -> ::anyhow::Result<::toolkit::discovery::WireOutcome> {
            // A compile-time (local) impl already registered wins — Profile 1.
            // Report `Local` so the runtime treats the dep as readiness-resolved
            // without a directory lookup.
            //
            // `try_get_local` rather than `try_get`: the hub is keyed by type,
            // so a proxy registered by another consumer of the same contract in
            // this process would otherwise look exactly like a local impl and
            // wrongly mark the dependency resolved.
            if __hub.try_get_local::<dyn #contract_path>().is_some() {
                return ::std::result::Result::Ok(::toolkit::discovery::WireOutcome::Local);
            }
            // Another consumer of this contract already wired a proxy: reuse it
            // rather than building a second identical one, but still report
            // `Remote` so this dependency gates readiness like any other.
            if __hub.has_remote_proxy::<dyn #contract_path>() {
                return ::std::result::Result::Ok(::toolkit::discovery::WireOutcome::Remote);
            }
            // Otherwise register the directory-resolving REST client. The tuning
            // carries the process's platform-plane credential source so the
            // resolved client attaches `X-ToolKit-Internal-Token` on
            // platform-plane methods (`cpt-cf-adr-two-plane-auth`); `None`
            // (Profile 1 / no credential) attaches nothing.
            let __tuning = ::toolkit::contract_support::wiring::ClientTuning::default()
                .with_internal_token_provider(__internal_token_provider.cloned());
            let __client = #resolving_client_path::new(
                __resolver,
                #from,
                __tuning,
            );
            __hub.register_remote_proxy::<dyn #contract_path>(::std::sync::Arc::new(__client));
            ::std::result::Result::Ok(::toolkit::discovery::WireOutcome::Remote)
        }

        ::toolkit::inventory::submit! {
            ::toolkit::discovery::ConsumerRegistration {
                owner_gear: #owner_gear,
                dep_gear: #from,
                wire: #wire_fn,
            }
        }
    })
}

// `parent_module` / `append_segment` are shared with `provides.rs` via
// `crate::support`.

#[cfg(test)]
mod tests {
    use super::{ConsumesAttr, generate};
    use syn::parse_quote;

    fn expand(item: &syn::ItemStruct) -> String {
        let attr: ConsumesAttr = parse_quote!(contract = billing_sdk::BillingApi, from = "billing");
        generate(&attr, item).unwrap().to_string()
    }

    /// `owner_gear` is a config key (`gears.<owner_gear>.config.consumer_wiring.<dep>`),
    /// so it must be the kebab gear name — not the Rust type name. Emitting
    /// `stringify!(ApiContractsConsumer)` made the static override unresolvable.
    #[test]
    fn owner_gear_is_kebab_case_of_the_struct_ident() {
        let out = expand(&parse_quote!(
            pub struct ApiContractsConsumer;
        ));
        assert!(
            out.contains(r#"owner_gear : "api-contracts-consumer""#),
            "expected kebab owner_gear, got:\n{out}"
        );
        assert!(
            !out.contains("stringify ! (ApiContractsConsumer)"),
            "owner_gear must not be the Rust type name"
        );
    }

    #[test]
    fn single_word_struct_ident_is_unchanged() {
        let out = expand(&parse_quote!(
            pub struct Orders;
        ));
        assert!(out.contains(r#"owner_gear : "orders""#), "got:\n{out}");
    }

    #[test]
    fn dep_gear_is_the_verbatim_from_value() {
        let out = expand(&parse_quote!(
            pub struct OrdersGear;
        ));
        // `from` is a directory lookup key and must survive untouched.
        assert!(out.contains(r#"dep_gear : "billing""#), "got:\n{out}");
    }
}
