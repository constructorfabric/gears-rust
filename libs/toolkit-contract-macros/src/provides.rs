//! `#[toolkit::provides]` — auto-wire a contract client into a module.
//!
//! Applied on the **gear struct** in the provider crate. Generates an
//! inherent async method `wire_<contract_snake>(&self, &GearCtx)` that:
//!   1. validates the contract IR (fail-fast at startup),
//!   2. reads typed wiring config from
//!      `gears.<gear>.config.client_wiring.<contract_snake>`,
//!   3. instantiates the local/REST/gRPC client per the wiring,
//!   4. registers the resulting `Arc<dyn Trait>` in the
//!      [`ClientHub`](toolkit::ClientHub).
//!
//! The struct itself is **not** modified — no fields added, no derives
//! injected — so the attribute composes freely with `#[toolkit::gear]`
//! and any other attribute macros. Multi-provide is supported by stacking
//! multiple `#[toolkit::provides(...)]` attributes, or by listing contracts in
//! one: `contract = [sdk::Api(local = ...), sdk::OtherApi(local = ...)]`.
//! `transports` and `policies` given at the top level apply to every entry
//! unless the entry sets its own; the other keys name one contract, so with
//! several contracts they go on the entry. Each contract generates a distinct
//! `wire_<contract_snake>` method.

use heck::ToSnakeCase;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, ItemStruct, Path, Result as SynResult};

use crate::support::{
    ContractArgs, KeyValue, append_segment, expr_into_path, expr_into_string, parent_module,
};

/// Parsed `#[toolkit::provides(...)]` attribute: one entry per contract.
pub struct ProvidesAttr {
    /// Provided contracts in declaration order; never empty.
    pub contracts: Vec<ProvidedContract>,
}

/// One provided contract, with the top-level defaults applied.
pub struct ProvidedContract {
    /// Path of the contract trait being provided.
    pub contract: Path,
    /// In-process factory. Required when `transports` includes `local`; a
    /// provider served only remotely (its in-process access goes through
    /// another seam) omits it.
    pub local: Option<Path>,
    /// Transports the contract is exposed over; defaults to local only.
    pub transports: Vec<Transport>,
    /// Optional override for the generated REST client type path.
    pub rest_client: Option<Path>,
    /// Optional override for the generated gRPC client type path.
    pub grpc_client: Option<Path>,
    /// Optional override for the path of the function returning the contract IR.
    pub ir_fn: Option<Path>,
    /// Optional override for the path of the REST binding function.
    pub rest_binding_fn: Option<Path>,
    /// Optional override for the path of the gRPC binding function.
    pub grpc_binding_fn: Option<Path>,
    /// Optional config key under which the provider's transport settings are read.
    pub config_key: Option<String>,
    /// Optional list of policy expressions applied to the provided contract.
    pub policies: Option<Vec<Expr>>,
}

/// Transport over which a provided contract can be reached.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// In-process call through the local factory.
    Local,
    /// Remote call over REST.
    Rest,
    /// Remote call over gRPC.
    Grpc,
}

/// Optional keys of one contract, before defaults are applied.
#[derive(Clone, Default)]
struct Settings {
    local: Option<Path>,
    transports: Option<Vec<Transport>>,
    rest_client: Option<Path>,
    grpc_client: Option<Path>,
    ir_fn: Option<Path>,
    rest_binding_fn: Option<Path>,
    grpc_binding_fn: Option<Path>,
    config_key: Option<String>,
    policies: Option<Vec<Expr>>,
}

impl Settings {
    /// Apply one `key = value`. `per_contract` is called for keys that name a
    /// single contract, so the top level can refuse them in a list.
    fn apply(
        &mut self,
        kv: &KeyValue,
        per_contract: impl Fn(&Ident) -> SynResult<()>,
    ) -> SynResult<()> {
        let key = &kv.key;
        let value = kv.value.clone();
        match key.to_string().as_str() {
            "transports" => self.transports = Some(into_transports(value, key)?),
            "policies" => self.policies = Some(into_exprs(value, key)?),
            name => {
                let path_slot = match name {
                    "local" => Some(&mut self.local),
                    "rest_client" => Some(&mut self.rest_client),
                    "grpc_client" => Some(&mut self.grpc_client),
                    "ir_fn" => Some(&mut self.ir_fn),
                    "rest_binding_fn" => Some(&mut self.rest_binding_fn),
                    "grpc_binding_fn" => Some(&mut self.grpc_binding_fn),
                    _ => None,
                };
                if let Some(slot) = path_slot {
                    per_contract(key)?;
                    *slot = Some(expr_into_path(value, key)?);
                } else if name == "config_key" {
                    per_contract(key)?;
                    self.config_key = Some(expr_into_string(value, key)?);
                } else {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown #[toolkit::provides] arg `{name}`"),
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Parse for ProvidesAttr {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let args: ContractArgs = input.parse()?;

        let mut top = Settings::default();
        for kv in &args.rest {
            top.apply(kv, |key| args.check_per_contract_key(key))?;
        }
        args.require_contract()?;

        let mut contracts = Vec::with_capacity(args.contracts.len());
        for entry in &args.contracts {
            let mut settings = top.clone();
            for kv in &entry.opts {
                settings.apply(kv, |_| Ok(()))?;
            }
            let Settings {
                local,
                transports,
                rest_client,
                grpc_client,
                ir_fn,
                rest_binding_fn,
                grpc_binding_fn,
                config_key,
                policies,
            } = settings;
            let transports = transports.unwrap_or_else(|| vec![Transport::Local]);
            match (&local, transports.contains(&Transport::Local)) {
                (None, true) => {
                    return Err(args.error_at(
                        entry,
                        "missing required arg `local = path::to::factory_fn` \
                         (required when `transports` includes `local`)",
                    ));
                }
                (Some(_), false) => {
                    return Err(args.error_at(
                        entry,
                        "`local = ...` is given but `transports` excludes `local`",
                    ));
                }
                _ => {}
            }
            contracts.push(ProvidedContract {
                contract: entry.path.clone(),
                local,
                transports,
                rest_client,
                grpc_client,
                ir_fn,
                rest_binding_fn,
                grpc_binding_fn,
                config_key,
                policies,
            });
        }
        Ok(Self { contracts })
    }
}

fn into_exprs(value: Expr, key: &Ident) -> SynResult<Vec<Expr>> {
    match value {
        Expr::Array(arr) => Ok(arr.elems.into_iter().collect()),
        other => Err(syn::Error::new_spanned(
            other,
            format!("`{key}` expects `[...]`"),
        )),
    }
}

fn into_transports(value: Expr, key: &Ident) -> SynResult<Vec<Transport>> {
    let exprs = into_exprs(value, key)?;
    let mut out = Vec::with_capacity(exprs.len());
    for e in exprs {
        let Expr::Path(p) = &e else {
            return Err(syn::Error::new_spanned(
                e,
                "transports list expects bare idents: `local`, `rest`, `grpc`",
            ));
        };
        let Some(seg) = p.path.segments.last() else {
            return Err(syn::Error::new_spanned(p, "empty transport name"));
        };
        let name = seg.ident.to_string();
        match name.as_str() {
            "local" => out.push(Transport::Local),
            "rest" => out.push(Transport::Rest),
            "grpc" => out.push(Transport::Grpc),
            other => {
                return Err(syn::Error::new_spanned(
                    &seg.ident,
                    format!("unknown transport `{other}`; expected local | rest | grpc"),
                ));
            }
        }
    }
    Ok(out)
}

/// Top-level entry point invoked by the proc-macro shim in `lib.rs`.
pub fn generate(attr: &ProvidesAttr, item: &ItemStruct) -> SynResult<TokenStream> {
    let impls = attr
        .contracts
        .iter()
        .map(|c| generate_one(c, item))
        .collect::<SynResult<Vec<_>>>()?;
    Ok(quote! {
        #item
        #(#impls)*
    })
}

/// The `wire_<contract_snake>` method for one provided contract.
fn generate_one(attr: &ProvidedContract, item: &ItemStruct) -> SynResult<TokenStream> {
    let struct_ident = &item.ident;
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();

    let contract_path = &attr.contract;
    let contract_ident = contract_path
        .segments
        .last()
        .map(|s| s.ident.clone())
        .ok_or_else(|| syn::Error::new_spanned(contract_path, "`contract` path is empty"))?;
    let contract_snake = contract_ident.to_string().to_snake_case();
    let wire_method = format_ident!("wire_{contract_snake}");
    let config_key = attr
        .config_key
        .clone()
        .unwrap_or_else(|| contract_snake.clone());

    let parent_mod = parent_module(contract_path)?;

    let ir_fn_path = attr
        .ir_fn
        .clone()
        .unwrap_or_else(|| append_segment(&parent_mod, &format_ident!("{}_ir", contract_snake)));

    let rest_client_path = attr.rest_client.clone().unwrap_or_else(|| {
        append_segment(&parent_mod, &format_ident!("{}RestClient", contract_ident))
    });
    let grpc_client_path = attr.grpc_client.clone().unwrap_or_else(|| {
        append_segment(&parent_mod, &format_ident!("{}GrpcClient", contract_ident))
    });
    let rest_binding_fn_path = attr.rest_binding_fn.clone().unwrap_or_else(|| {
        append_segment(
            &parent_mod,
            &format_ident!("{}_rest_http_binding", contract_snake),
        )
    });
    let _grpc_binding_fn_path = attr.grpc_binding_fn.clone().unwrap_or_else(|| {
        append_segment(
            &parent_mod,
            &format_ident!("{}_grpc_binding", contract_snake),
        )
    });

    let local_path = attr.local.as_ref();
    let transports = &attr.transports;
    let enable_local = transports.contains(&Transport::Local);
    let enable_rest = transports.contains(&Transport::Rest);
    let enable_grpc = transports.contains(&Transport::Grpc);

    // Policy stack expression. Default: `default_policy_stack()`.
    // Override: list of bare policy constructors → wrap each in `Arc::new(...)`
    // and feed into `policy_stack_from`.
    let policy_stack_expr = match &attr.policies {
        None => quote! { ::toolkit::wiring::default_policy_stack() },
        Some(list) if list.is_empty() => {
            quote! { ::std::sync::Arc::new(::toolkit_contract::policy::PolicyStack::new()) }
        }
        Some(list) => {
            let items = list.iter().map(|e| quote!(::std::sync::Arc::new(#e) as ::std::sync::Arc<dyn ::toolkit_contract::policy::Policy>));
            quote! { ::toolkit::wiring::policy_stack_from(vec![#(#items),*]) }
        }
    };

    // The policy stack is a local-transport construct: it wraps the in-process
    // call, and the generated REST/gRPC clients have no hook to run it through
    // (their `new(config)` takes no policies). A `compile_error!` would be
    // wrong — `transports = [local, rest]` with policies is legitimate, since
    // the transport is chosen at runtime from config and the policies do apply
    // on the local path. So warn at wiring time when the choice lands on a
    // transport that will ignore them, rather than dropping them silently.
    let policies_declared = attr.policies.is_some();
    let warn_policies_ignored = |transport: &str| {
        if policies_declared {
            let msg = format!(
                "contract `{contract_ident}`: `policies = [...]` is declared but the configured \
                 `{transport}` transport does not run them - the policy stack applies to the \
                 local (in-process) transport only"
            );
            quote! { ::tracing::warn!(gear = ctx.gear_name(), #msg); }
        } else {
            quote! {}
        }
    };
    let rest_policy_warning = warn_policies_ignored("rest");
    let grpc_policy_warning = warn_policies_ignored("grpc");

    // gRPC reads only `endpoint`, `timeout` and `require_tls`; the REST-only
    // pool/concurrency tuning knobs on a `transport: grpc` wiring parse but go
    // nowhere. Warn at wiring time rather than dropping them silently. The
    // message is a static literal (built here) and the offending keys ride along
    // as the structured `ignored` field, mirroring `warn_policies_ignored`.
    let grpc_rest_only_warning_msg = format!(
        "contract `{contract_ident}`: gRPC transport ignores REST-only client tuning knobs \
         (pool_max_idle_per_host / pool_idle_timeout / max_concurrent_requests); they apply \
         to the REST transport only"
    );
    let grpc_rest_only_warning = quote! {
        let __ignored = tuning.rest_only_knobs_set();
        if !__ignored.is_empty() {
            ::tracing::warn!(gear = ctx.gear_name(), ignored = ?__ignored, #grpc_rest_only_warning_msg);
        }
    };

    // Build the match arms based on enabled transports.
    let local_arm = if let (true, Some(local_path)) = (enable_local, local_path) {
        quote! {
            ::toolkit_contract::wiring::ClientWiring::Local => {
                let __policies = #policy_stack_expr;
                #local_path(ctx, __policies)?
            }
        }
    } else {
        quote! {
            ::toolkit_contract::wiring::ClientWiring::Local => {
                ::anyhow::bail!(
                    concat!(
                        "contract `", stringify!(#contract_ident),
                        "`: local transport is not enabled for gear `{}` ",
                        "(provider was declared with `transports = [...]` excluding `local`)"
                    ),
                    ctx.gear_name()
                )
            }
        }
    };

    let rest_arm = if enable_rest {
        quote! {
            ::toolkit_contract::wiring::ClientWiring::Rest { endpoint, tuning } => {
                #rest_policy_warning
                // Thread the process's platform-plane credential source onto the
                // client config so platform-plane (`PlatformSecurityContext`)
                // methods attach `X-ToolKit-Internal-Token`. `None` (Profile 1 /
                // no credential) attaches nothing (cpt-cf-adr-two-plane-auth).
                let __cfg = tuning.apply_to(endpoint)
                    .with_internal_token_provider(ctx.internal_token_provider());
                ::std::sync::Arc::new(
                    #rest_client_path::new(__cfg)
                        .map_err(|e| ::anyhow::anyhow!(
                            concat!("building REST client for `", stringify!(#contract_ident), "`: {}"), e
                        ))?
                ) as ::std::sync::Arc<dyn #contract_path>
            }
        }
    } else {
        quote! {
            ::toolkit_contract::wiring::ClientWiring::Rest { .. } => {
                ::anyhow::bail!(
                    concat!(
                        "contract `", stringify!(#contract_ident),
                        "`: REST transport requested in config but provider was declared without `rest` in `transports = [...]`"
                    )
                )
            }
        }
    };

    let grpc_arm = if enable_grpc {
        quote! {
            ::toolkit_contract::wiring::ClientWiring::Grpc { endpoint, tuning } => {
                #grpc_policy_warning
                #grpc_rest_only_warning
                // See the REST arm: platform-plane methods attach the runtime
                // internal credential as `x-toolkit-internal-token` metadata.
                let __cfg = tuning.apply_to(endpoint)
                    .with_internal_token_provider(ctx.internal_token_provider());
                ::std::sync::Arc::new(
                    #grpc_client_path::connect(__cfg).await
                        .map_err(|e| ::anyhow::anyhow!(
                            concat!("connecting gRPC client for `", stringify!(#contract_ident), "`: {}"), e
                        ))?
                ) as ::std::sync::Arc<dyn #contract_path>
            }
        }
    } else {
        quote! {
            ::toolkit_contract::wiring::ClientWiring::Grpc { .. } => {
                ::anyhow::bail!(
                    concat!(
                        "contract `", stringify!(#contract_ident),
                        "`: gRPC transport requested in config but provider was declared without `grpc` in `transports = [...]`"
                    )
                )
            }
        }
    };

    // REST binding validation is only meaningful if REST is enabled.
    let rest_binding_check = if enable_rest {
        quote! {
            ::toolkit_contract::ir::validation::validate_http_binding(&__ir, &#rest_binding_fn_path())
                .map_err(|errs| ::anyhow::anyhow!(
                    concat!("HTTP binding IR for `", stringify!(#contract_ident), "` failed validation: {:?}"),
                    errs
                ))?;
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        impl #impl_generics #struct_ident #ty_generics #where_clause {
            /// Auto-generated by `#[toolkit::provides]`. Validates the
            /// contract IR, reads wiring config, builds the requested
            /// transport, and registers the resulting client in the
            /// `ClientHub`.
            #[allow(clippy::needless_return, dead_code, unused_imports, unused_variables)]
            pub async fn #wire_method(
                &self,
                ctx: &::toolkit::GearCtx,
            ) -> ::anyhow::Result<()> {
                // (1) IR fail-fast at startup.
                let __ir = #ir_fn_path();
                ::toolkit_contract::ir::validation::validate_contract(&__ir)
                    .map_err(|errs| ::anyhow::anyhow!(
                        concat!("contract IR for `", stringify!(#contract_ident), "` failed validation: {:?}"),
                        errs
                    ))?;
                #rest_binding_check

                // (2) Read wiring config (or default to Local).
                let __wiring = ::toolkit::wiring::read_wiring(ctx, #config_key)?;

                // (3) Build per-transport client.
                let __client: ::std::sync::Arc<dyn #contract_path> = match __wiring {
                    #local_arm
                    #rest_arm
                    #grpc_arm
                };

                // (4) Publish in ClientHub.
                ctx.client_hub().register::<dyn #contract_path>(__client);
                Ok(())
            }
        }
    };

    Ok(expanded)
}

// `parent_module` / `append_segment` now live in `crate::support` (shared with
// `consumes.rs`).

#[cfg(test)]
mod tests {
    use super::{ProvidedContract, ProvidesAttr, Transport};

    fn parse_all(args: &str) -> syn::Result<ProvidesAttr> {
        syn::parse_str(args)
    }

    /// The single contract of an attribute.
    fn parse(args: &str) -> syn::Result<ProvidedContract> {
        let mut attr = parse_all(args)?;
        assert_eq!(attr.contracts.len(), 1);
        Ok(attr.contracts.remove(0))
    }

    #[test]
    fn local_is_required_when_transports_include_it() {
        let err = parse("contract = sdk::Api")
            .err()
            .expect("no local factory");
        assert!(
            err.to_string().contains("missing required arg `local"),
            "{err}"
        );
        let err = parse("contract = sdk::Api, transports = [local, grpc]")
            .err()
            .expect("local listed, no factory");
        assert!(
            err.to_string().contains("missing required arg `local"),
            "{err}"
        );
    }

    #[test]
    fn local_is_refused_when_transports_exclude_it() {
        let err = parse("contract = sdk::Api, local = make, transports = [grpc]")
            .err()
            .expect("a factory nothing calls");
        assert!(err.to_string().contains("excludes `local`"), "{err}");
    }

    #[test]
    fn a_contract_list_applies_shared_keys_and_entry_overrides() {
        let attr = parse_all(
            "transports = [grpc], contract = [\
                sdk::CacheApi(grpc_client = sdk::grpc::CacheClient), \
                sdk::LockApi(transports = [local, rest], local = make_lock, config_key = \"lock\"), \
            ]",
        )
        .expect("parses");
        let [cache, lock] = attr.contracts.as_slice() else {
            panic!("two contracts");
        };
        assert!(cache.transports == [Transport::Grpc]);
        assert!(cache.grpc_client.is_some() && cache.local.is_none());
        assert!(lock.transports == [Transport::Local, Transport::Rest]);
        assert!(lock.local.is_some() && lock.grpc_client.is_none());
        assert_eq!(lock.config_key.as_deref(), Some("lock"));
    }

    #[test]
    fn a_contract_list_generates_one_struct_and_a_method_per_contract() {
        let attr = parse_all("transports = [grpc], contract = [sdk::CacheApi, sdk::LockApi]")
            .expect("parses");
        let item: syn::ItemStruct = syn::parse_quote!(
            struct ClusterGear;
        );
        let out = super::generate(&attr, &item).unwrap().to_string();
        assert_eq!(out.matches("struct ClusterGear").count(), 1, "{out}");
        assert!(out.contains("fn wire_cache_api"), "{out}");
        assert!(out.contains("fn wire_lock_api"), "{out}");
    }

    #[test]
    fn a_one_element_list_expands_like_the_single_form() {
        let item: syn::ItemStruct = syn::parse_quote!(
            struct Gear;
        );
        let single = parse_all("contract = sdk::Api, local = make, transports = [local, rest]")
            .expect("parses");
        let list = parse_all("contract = [sdk::Api(local = make)], transports = [local, rest]")
            .expect("parses");
        assert_eq!(
            super::generate(&single, &item).unwrap().to_string(),
            super::generate(&list, &item).unwrap().to_string()
        );
    }

    #[test]
    fn list_errors() {
        for (args, expected) in [
            (
                "local = make, contract = [sdk::A, sdk::B]",
                "`local` is per contract",
            ),
            (
                "config_key = \"k\", transports = [grpc], contract = [sdk::A, sdk::B]",
                "`config_key` is per contract",
            ),
            (
                "contract = [sdk::A(local = a), sdk::B]",
                "missing required arg `local",
            ),
            ("transports = [grpc], contract = []", "lists no contracts"),
            (
                "transports = [grpc], contract = [sdk::A, sdk::A]",
                "listed more than once",
            ),
            (
                "contract = [sdk::A(bogus = 1)]",
                "unknown #[toolkit::provides] arg `bogus`",
            ),
            (
                "contrct = sdk::A, transports = [grpc]",
                "unknown #[toolkit::provides] arg `contrct`",
            ),
        ] {
            let err = parse_all(args).err().expect(args);
            assert!(err.to_string().contains(expected), "{args}: {err}");
        }
    }

    #[test]
    fn a_remote_only_provider_needs_no_local_factory() {
        let attr = parse("contract = sdk::Api, transports = [grpc]").expect("parses");
        assert!(attr.local.is_none());
        assert!(attr.transports == [Transport::Grpc]);
    }
}
