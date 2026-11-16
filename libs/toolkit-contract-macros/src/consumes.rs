//! `#[toolkit::consumes(contract = path, from = "gear")]` — declare that a gear
//! consumes a contract provided by another gear, wired via eventual-readiness
//! directory discovery.
//!
//! Several contracts fit in one attribute:
//! `contract = [sdk::Api, sdk::OtherApi(from = "other", resolving_client = ...)]`.
//! A top-level `from` applies to every entry unless the entry sets its own;
//! `resolving_client` names one contract, so with several it goes on the entry.
//! The list expands exactly like one attribute per contract.
//!
//! `transport = grpc` (top level, or on an entry) wires the contract's
//! `<Contract>GrpcResolvingClient` from `#[toolkit::grpc_contract]` instead:
//! it resolves the gRPC service name from the directory and calls it over a
//! lazily connecting channel. `transport = rest` is the default.
//!
//! Applied on the **gear struct** (alongside `#[toolkit::gear]`). Emits a
//! non-capturing `fn` plus an `inventory::submit!` of a
//! [`toolkit::discovery::ConsumerRegistration`] that the runtime's proxy-wiring
//! phase replays at startup. The wiring closure:
//!   1. short-circuits if a compile-time (local) impl is already in the
//!      `ClientHub` — a co-located provider wins;
//!   2. otherwise registers a directory-resolving REST or gRPC client under the
//!      contract trait, which lazily resolves the provider endpoint per call.
//!
//! The runtime passes the consuming gear's view of the hub
//! (`ClientHub::for_gear`), so the proxy belongs to that gear and is built from
//! its own `clients.consumes.<dep>` settings. Two gears consuming one contract
//! get two proxies. One gear declaring the same contract twice would get one
//! proxy for both: on one struct that is a compile error, and the wire fn also
//! refuses it at boot for spellings the macro cannot compare.
//!
//! **Requirements on the consuming crate.** The emission is unconditional, so
//! the crate needs a `toolkit` dependency (for `ClientHub`, `discovery` and the
//! `inventory` re-export) and its contract SDK built with `rest-client` (or
//! `grpc-client` for `transport = grpc`), which is what makes the resolving
//! client exist. A missing SDK feature is
//! a hard `cannot find type` error rather than silently-dropped wiring; this
//! attribute used to sit behind a `directory-rest-client` feature evaluated in
//! the consuming crate, which turned a forgotten feature into a gear that
//! started fine and never found its provider.
//!
//! **Gear name.** The emitted `owner_gear` is the gear's name, because the
//! runtime uses it as a config key for the ADR-0004 static-endpoint override and
//! the criticality flag (`gears.<owner_gear>.clients.consumes.<dep>`), as
//! the owner of the hub view the proxy is wired into, and in log/error fields.
//! When this attribute sits **above** `#[toolkit::gear]`, the gear attribute is
//! still on the item and its `name = "..."` is used, so a
//! struct named `CredStoreGear` for gear `credstore` keys correctly. Otherwise
//! (gear attribute already expanded, or absent) the kebab-case of the struct
//! ident is used, and `#[toolkit::gear(name = ...)]` must match it.
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
use quote::{ToTokens, format_ident, quote};
use syn::Result as SynResult;
use syn::parse::{Parse, ParseStream};
use syn::{ItemStruct, Path};

use crate::support::{
    ContractArgs, append_segment, expr_into_path, expr_into_string, parent_module,
};

/// Parsed `#[toolkit::consumes(...)]` attribute: one entry per contract.
pub struct ConsumesAttr {
    /// Consumed contracts in declaration order; never empty.
    pub contracts: Vec<ConsumedContract>,
}

/// One consumed contract, with the top-level defaults applied.
pub struct ConsumedContract {
    /// Path of the contract trait being consumed.
    pub contract: Path,
    /// Name of the providing gear (the `from = "..."` argument).
    pub from: String,
    /// Optional override for the resolving-client path.
    ///
    /// The default (`<parent>::<Contract>RestResolvingClient`, or
    /// `<parent>::<Contract>GrpcResolvingClient` for `transport = grpc`)
    /// assumes the SDK re-exports the resolving client at the **same module
    /// level** as the base contract trait. Set this explicitly when the base
    /// trait and its projection live in different submodules.
    pub resolving_client: Option<Path>,
    /// Transport the client talks to the provider over.
    pub transport: Transport,
}

/// The transport of a consumed contract's client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// `<Contract>RestResolvingClient`, resolved by gear name. The default.
    #[default]
    Rest,
    /// `<Contract>GrpcResolvingClient`, resolved by gRPC service name.
    Grpc,
}

fn expr_into_transport(value: &syn::Expr, key: &syn::Ident) -> SynResult<Transport> {
    let name = match value {
        syn::Expr::Path(p) if p.qself.is_none() => p.path.get_ident().map(ToString::to_string),
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    };
    match name.as_deref() {
        Some("rest") => Ok(Transport::Rest),
        Some("grpc") => Ok(Transport::Grpc),
        _ => Err(syn::Error::new_spanned(
            value,
            format!("`{key}` is `rest` or `grpc`"),
        )),
    }
}

impl Parse for ConsumesAttr {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let args: ContractArgs = input.parse()?;

        let mut from: Option<String> = None;
        let mut resolving_client: Option<Path> = None;
        let mut transport = Transport::default();
        for kv in &args.rest {
            match kv.key.to_string().as_str() {
                "from" => from = Some(expr_into_string(kv.value.clone(), &kv.key)?),
                "transport" => transport = expr_into_transport(&kv.value, &kv.key)?,
                "resolving_client" => {
                    args.check_per_contract_key(&kv.key)?;
                    resolving_client = Some(expr_into_path(kv.value.clone(), &kv.key)?);
                }
                other => {
                    return Err(syn::Error::new(
                        kv.key.span(),
                        format!("unknown #[toolkit::consumes] arg `{other}`"),
                    ));
                }
            }
        }
        args.require_contract()?;

        let mut contracts = Vec::with_capacity(args.contracts.len());
        for entry in &args.contracts {
            let mut entry_from = from.clone();
            let mut entry_resolving_client = resolving_client.clone();
            let mut entry_transport = transport;
            for kv in &entry.opts {
                match kv.key.to_string().as_str() {
                    "from" => entry_from = Some(expr_into_string(kv.value.clone(), &kv.key)?),
                    "transport" => entry_transport = expr_into_transport(&kv.value, &kv.key)?,
                    "resolving_client" => {
                        entry_resolving_client = Some(expr_into_path(kv.value.clone(), &kv.key)?);
                    }
                    other => {
                        return Err(syn::Error::new(
                            kv.key.span(),
                            format!("unknown #[toolkit::consumes] arg `{other}`"),
                        ));
                    }
                }
            }
            let from = entry_from.ok_or_else(|| {
                args.error_at(
                    entry,
                    "missing required arg `from = \"provider-gear-name\"`",
                )
            })?;
            contracts.push(ConsumedContract {
                contract: entry.path.clone(),
                from,
                resolving_client: entry_resolving_client,
                transport: entry_transport,
            });
        }
        Ok(Self { contracts })
    }
}

/// A contract this attribute consumes that another `#[toolkit::consumes]` on
/// the same struct also consumes is an error: the gear would get one client for
/// both. Each attribute sees the ones written below it, so the topmost of any
/// pair reports it. At runtime the same duplicate is caught only when the
/// provider is remote, so a monolith would boot and a split deployment would not.
fn reject_stacked_duplicates(attr: &ConsumesAttr, item: &ItemStruct) -> SynResult<()> {
    let below: std::collections::HashSet<String> = item
        .attrs
        .iter()
        .filter(|a| {
            let path = a.path();
            path.segments.last().is_some_and(|s| s.ident == "consumes")
                && (path.segments.len() == 1
                    || path
                        .segments
                        .first()
                        .is_some_and(|s| s.ident == "toolkit" || s.ident == "toolkit_contract"))
        })
        // An attribute that does not parse reports its own error when it expands.
        .filter_map(|a| a.parse_args::<ConsumesAttr>().ok())
        .flat_map(|other| other.contracts)
        .map(|c| c.contract.to_token_stream().to_string())
        .collect();
    for c in &attr.contracts {
        if below.contains(&c.contract.to_token_stream().to_string()) {
            return Err(syn::Error::new_spanned(
                &c.contract,
                "contract is consumed by another #[toolkit::consumes] on this struct; \
                 a gear gets one client per contract",
            ));
        }
    }
    Ok(())
}

/// The `name = "..."` of a `#[toolkit::gear(...)]` (or `#[gear(...)]`) still on
/// the item, i.e. one written below this attribute.
fn declared_gear_name(item: &ItemStruct) -> Option<String> {
    item.attrs.iter().find_map(|attr| {
        let path = attr.path();
        let is_gear = path.segments.last().is_some_and(|s| s.ident == "gear")
            && (path.segments.len() == 1
                || path.segments.first().is_some_and(|s| s.ident == "toolkit"));
        if !is_gear {
            return None;
        }
        let mut name = None;
        // An attribute this cannot parse falls back to the struct ident.
        let parsed = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: syn::LitStr = meta.value()?.parse()?;
                name = Some(value.value());
            } else if meta.input.peek(syn::Token![=]) {
                let _: syn::Expr = meta.value()?.parse()?;
            } else if meta.input.peek(syn::token::Paren) {
                // Consume the group whole: syn reports a buffer dropped with
                // tokens left as a parse error, which would discard `name`.
                let content;
                syn::parenthesized!(content in meta.input);
                let _: proc_macro2::TokenStream = content.parse()?;
            }
            Ok(())
        });
        parsed.ok().and(name)
    })
}

/// Top-level entry point invoked by the proc-macro shim in `lib.rs`.
pub fn generate(attr: &ConsumesAttr, item: &ItemStruct) -> SynResult<TokenStream> {
    // `owner_gear` is the *gear name*, not the Rust type name: the runtime uses
    // it as a config key (`gears.<owner_gear>.clients.consumes.<dep>`) for
    // the ADR-0004 static-endpoint override, and as a log/error field. Gear
    // names are kebab-case, so derive it from the struct ident rather than
    // emitting `stringify!` — `stringify!(ApiContractsConsumer)` would look up
    // `gears.ApiContractsConsumer`, which no config declares, and the override
    // would silently never fire.
    let owner_gear =
        declared_gear_name(item).unwrap_or_else(|| item.ident.to_string().to_kebab_case());
    reject_stacked_duplicates(attr, item)?;
    let wirings = attr
        .contracts
        .iter()
        .map(|c| generate_one(c, &owner_gear))
        .collect::<SynResult<Vec<_>>>()?;
    Ok(quote! {
        #item
        #(#wirings)*
    })
}

/// The wire fn and `ConsumerRegistration` for one consumed contract.
fn generate_one(attr: &ConsumedContract, owner_gear: &str) -> SynResult<TokenStream> {
    let contract_path = &attr.contract;
    let contract_ident = contract_path
        .segments
        .last()
        .map(|s| s.ident.clone())
        .ok_or_else(|| syn::Error::new_spanned(contract_path, "`contract` path is empty"))?;
    let from = &attr.from;

    // Default resolving-client path: `<parent>::<Contract>RestResolvingClient`
    // (or `GrpcResolvingClient`), mirroring how `#[toolkit::rest_contract]` and
    // `#[toolkit::grpc_contract]` name it: the REST projection of `PaymentApi` is
    // `PaymentApiRest`, whose resolving client is `PaymentApiRestResolvingClient`.
    // The SDK re-exports it at the same level as the base contract trait.
    let suffix = match attr.transport {
        Transport::Rest => "RestResolvingClient",
        Transport::Grpc => "GrpcResolvingClient",
    };
    let resolving_client_path = if let Some(p) = &attr.resolving_client {
        p.clone()
    } else {
        let parent = parent_module(contract_path)?;
        append_segment(&parent, &format_ident!("{contract_ident}{suffix}"))
    };
    // What the runtime probes for readiness: the provider's REST endpoint, or
    // the gRPC service the client resolves.
    let remote = match attr.transport {
        Transport::Rest => quote! { ::toolkit::discovery::WireOutcome::Remote },
        Transport::Grpc => quote! {
            ::toolkit::discovery::WireOutcome::RemoteGrpc {
                service: #resolving_client_path::GRPC_SERVICE,
            }
        },
    };

    let wire_fn = format_ident!(
        "__toolkit_wire_{}_from_{}",
        contract_ident.to_string().to_snake_case(),
        from.replace(['-', '.'], "_"),
    );

    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #wire_fn(
            __hub: &::toolkit::ClientHub,
            __resolver: ::std::sync::Arc<dyn ::toolkit::discovery::EndpointResolver>,
            __tuning: ::toolkit::contract_support::wiring::ClientTuning,
        ) -> ::anyhow::Result<::toolkit::discovery::WireOutcome> {
            // A compile-time (local) impl already registered wins — Profile 1.
            // Report `Local` so the runtime treats the dep as readiness-resolved
            // without a directory lookup.
            //
            // `try_get_local` rather than `try_get`: `try_get` also returns a
            // process-wide proxy, which is not a local impl and must not mark
            // the dependency resolved.
            if __hub.try_get_local::<dyn #contract_path>().is_some() {
                return ::std::result::Result::Ok(::toolkit::discovery::WireOutcome::Local);
            }
            // This view already holds a proxy for the contract. In a gear's view
            // that means a second `#[consumes]` of the same contract, which would
            // silently share the first one's provider and settings. In the
            // process-wide hub (owner not a gear here) reuse it as before.
            if __hub.has_remote_proxy::<dyn #contract_path>() {
                if let ::std::option::Option::Some(__owner) = __hub.owner() {
                    return ::std::result::Result::Err(::anyhow::anyhow!(
                        "gear `{}` declares #[toolkit::consumes] for `{}` more than once \
                         (from = \"{}\"); a gear gets one client per contract",
                        __owner,
                        ::std::stringify!(#contract_path),
                        #from,
                    ));
                }
                return ::std::result::Result::Ok(#remote);
            }
            // Otherwise register the directory-resolving client. The tuning
            // is prepared by the runtime's proxy-wiring phase from the consumer's
            // `clients.consumes.<dep>` config and carries the process's platform-plane
            // credential source, so platform-plane methods attach
            // `X-ToolKit-Internal-Token` (`cpt-cf-adr-two-plane-auth`).
            let __client = #resolving_client_path::new(
                __resolver,
                #from,
                __tuning,
            );
            __hub.register_remote_proxy::<dyn #contract_path>(::std::sync::Arc::new(__client));
            ::std::result::Result::Ok(#remote)
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

    /// `owner_gear` is a config key (`gears.<owner_gear>.clients.consumes.<dep>`),
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

    /// With `#[toolkit::gear]` written below, its declared `name` is the key, even
    /// when it is not the kebab-case of the struct (`CredStoreGear` / `credstore`).
    #[test]
    fn declared_gear_name_wins_over_the_struct_ident() {
        let attr: ConsumesAttr =
            syn::parse_quote!(contract = authz::AuthZResolverApi, from = "authz-resolver");
        let item: syn::ItemStruct = syn::parse_quote! {
            #[toolkit::gear(name = "credstore", deps = [tenant_resolver], capabilities = [system, db], lifecycle(entry = "serve"))]
            pub struct CredStoreGear;
        };
        let out = generate(&attr, &item).unwrap().to_string();
        assert!(out.contains(r#"owner_gear : "credstore""#), "got:\n{out}");
    }

    #[test]
    fn an_unqualified_gear_attribute_names_the_gear() {
        let out = expand(&parse_quote! {
            #[gear(name = "billing-core")]
            pub struct BillingGear;
        });
        assert!(
            out.contains(r#"owner_gear : "billing-core""#),
            "got:\n{out}"
        );
    }

    /// `lifecycle(...)` is a parenthesised group; consuming it whole is what lets
    /// a `name` written after it still be read.
    #[test]
    fn a_name_after_a_lifecycle_group_is_read() {
        let out = expand(&parse_quote! {
            #[toolkit::gear(deps = [tenant_resolver], lifecycle(entry = "serve"), name = "credstore")]
            pub struct CredStoreGear;
        });
        assert!(out.contains(r#"owner_gear : "credstore""#), "got:\n{out}");
    }

    /// An attribute this cannot parse, or one that only ends in `gear`, falls
    /// back to the struct ident rather than failing the expansion.
    #[test]
    fn an_unreadable_or_foreign_gear_attribute_falls_back_to_the_ident() {
        for item in [
            parse_quote! {
                #[toolkit::gear(name = 42)]
                pub struct OrdersGear;
            },
            parse_quote! {
                #[other::gear(name = "billing")]
                pub struct OrdersGear;
            },
        ] {
            let out = expand(&item);
            assert!(out.contains(r#"owner_gear : "orders-gear""#), "got:\n{out}");
        }
    }

    #[test]
    fn single_word_struct_ident_is_unchanged() {
        let out = expand(&parse_quote!(
            pub struct Orders;
        ));
        assert!(out.contains(r#"owner_gear : "orders""#), "got:\n{out}");
    }

    /// A second proxy for the same contract in a gear's view is a boot error;
    /// only the process-wide hub reuses an existing proxy.
    #[test]
    fn a_duplicate_in_a_gear_view_is_an_error() {
        let out = expand(&parse_quote!(
            pub struct OrdersGear;
        ));
        assert!(out.contains("__hub . owner ()"), "got:\n{out}");
        assert!(
            out.contains("stringify ! (billing_sdk :: BillingApi)"),
            "got:\n{out}"
        );
    }

    fn expand_with(attr: &str) -> syn::Result<String> {
        let attr: ConsumesAttr = syn::parse_str(attr)?;
        let item: syn::ItemStruct = parse_quote!(
            pub struct OrdersGear;
        );
        Ok(generate(&attr, &item)?.to_string())
    }

    /// The list form wires each contract and emits the struct once.
    #[test]
    fn a_contract_list_wires_each_contract() {
        let out = expand_with(
            r#"from = "registry", contract = [sdk::PlatformApi, sdk::TenantApi(from = "tenants")]"#,
        )
        .unwrap();
        assert_eq!(out.matches("pub struct OrdersGear").count(), 1, "{out}");
        assert_eq!(out.matches("inventory :: submit !").count(), 2, "{out}");
        assert!(
            out.contains("__toolkit_wire_platform_api_from_registry"),
            "{out}"
        );
        assert!(
            out.contains("__toolkit_wire_tenant_api_from_tenants"),
            "{out}"
        );
        assert!(out.contains("sdk :: TenantApiRestResolvingClient"), "{out}");
    }

    #[test]
    fn transport_grpc_wires_the_grpc_resolving_client() {
        let out =
            expand_with(r#"contract = sdk::Api, from = "billing", transport = grpc"#).unwrap();
        assert!(
            out.contains("sdk :: ApiGrpcResolvingClient :: new"),
            "{out}"
        );
        assert!(
            out.contains("WireOutcome :: RemoteGrpc { service : sdk :: ApiGrpcResolvingClient :: GRPC_SERVICE"),
            "{out}"
        );
        assert!(!out.contains("RestResolvingClient"), "{out}");
    }

    #[test]
    fn rest_is_the_default_transport() {
        let implicit = expand_with(r#"contract = sdk::Api, from = "billing""#).unwrap();
        let explicit =
            expand_with(r#"contract = sdk::Api, from = "billing", transport = rest"#).unwrap();
        assert_eq!(implicit, explicit);
        assert!(!implicit.contains("RemoteGrpc"), "{implicit}");
    }

    #[test]
    fn an_entry_sets_its_own_transport() {
        let out = expand_with(
            r#"from = "b", transport = grpc, contract = [sdk::Fast, sdk::Plain(transport = rest)]"#,
        )
        .unwrap();
        assert!(out.contains("sdk :: FastGrpcResolvingClient"), "{out}");
        assert!(out.contains("sdk :: PlainRestResolvingClient"), "{out}");
    }

    #[test]
    fn an_unknown_transport_is_an_error() {
        let err = expand_with(r#"contract = sdk::Api, from = "b", transport = soap"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`transport` is `rest` or `grpc`"), "{err}");
    }

    #[test]
    fn a_one_element_list_expands_like_the_single_form() {
        let single = expand_with(r#"contract = sdk::Api, from = "billing""#).unwrap();
        let list = expand_with(r#"contract = [sdk::Api], from = "billing""#).unwrap();
        assert_eq!(single, list);
    }

    #[test]
    fn an_entry_sets_its_own_resolving_client() {
        let out = expand_with(
            r#"from = "b", contract = [sdk::Api(resolving_client = sdk::rest::Client), sdk::Other]"#,
        )
        .unwrap();
        assert!(out.contains("sdk :: rest :: Client :: new"), "{out}");
        assert!(
            out.contains("sdk :: OtherRestResolvingClient :: new"),
            "{out}"
        );
    }

    /// A list and a separate attribute on one struct: each expands on its own,
    /// leaving the other attribute on the item for the compiler to expand next.
    #[test]
    fn a_list_mixes_with_a_separate_attribute() {
        let attr: ConsumesAttr =
            syn::parse_str(r#"from = "registry", contract = [sdk::PlatformApi, sdk::TenantApi]"#)
                .unwrap();
        let item: syn::ItemStruct = parse_quote! {
            #[toolkit::consumes(contract = billing_sdk::BillingApi, from = "billing")]
            pub struct OrdersGear;
        };
        let out = generate(&attr, &item).unwrap().to_string();
        assert!(
            out.contains("# [toolkit :: consumes (contract = billing_sdk :: BillingApi"),
            "{out}"
        );
        assert_eq!(out.matches("inventory :: submit !").count(), 2, "{out}");
    }

    #[test]
    fn a_contract_stacked_twice_is_an_error() {
        let attr: ConsumesAttr =
            syn::parse_str(r#"contract = billing_sdk::BillingApi, from = "a""#).unwrap();
        for item in [
            parse_quote! {
                #[toolkit::consumes(contract = billing_sdk::BillingApi, from = "b")]
                pub struct OrdersGear;
            },
            parse_quote! {
                #[consumes(from = "b", contract = [other::Api, billing_sdk::BillingApi])]
                pub struct OrdersGear;
            },
        ] {
            let err = generate(&attr, &item).unwrap_err().to_string();
            assert!(err.contains("consumed by another"), "{err}");
        }
    }

    #[test]
    fn a_misspelled_contract_key_is_reported_as_unknown() {
        let err = expand_with(r#"contrct = sdk::Api, from = "b""#).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown #[toolkit::consumes] arg `contrct`"),
            "{err}"
        );
    }

    #[test]
    fn list_errors() {
        for (attr, expected) in [
            (
                r#"from = "b", resolving_client = x::C, contract = [sdk::A, sdk::B]"#,
                "`resolving_client` is per contract",
            ),
            (
                r#"from = "b", contract = [sdk::A, sdk::A]"#,
                "listed more than once",
            ),
            (r#"from = "b", contract = []"#, "lists no contracts"),
            (
                r#"contract = [sdk::A(from = "a"), sdk::B]"#,
                "missing required arg `from",
            ),
            (
                r#"from = "b", contract = [sdk::A(bogus = 1)]"#,
                "unknown #[toolkit::consumes] arg `bogus`",
            ),
            (
                r#"from = "b", contract = [sdk::A(contract = x)]"#,
                "cannot be set on a list entry",
            ),
            (
                r#"from = "b", contract = [sdk::A], contract = sdk::B"#,
                "given more than once",
            ),
            (
                r#"from = "b", contract = sdk::A, contract = [sdk::B]"#,
                "given more than once",
            ),
        ] {
            let err = expand_with(attr).expect_err(attr);
            assert!(err.to_string().contains(expected), "{attr}: {err}");
        }
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
