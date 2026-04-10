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
//! multiple `#[toolkit::provides(...)]` attributes; each generates a
//! distinct `wire_<contract_snake>` method.

use heck::ToSnakeCase;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Expr, Ident, ItemStruct, Path, PathArguments, PathSegment, Result as SynResult, Token,
};

/// Parsed `#[toolkit::provides(...)]` attribute.
pub struct ProvidesAttr {
    pub contract: Path,
    pub local: Path,
    pub transports: Vec<Transport>,
    pub rest_client: Option<Path>,
    pub grpc_client: Option<Path>,
    pub ir_fn: Option<Path>,
    pub rest_binding_fn: Option<Path>,
    pub grpc_binding_fn: Option<Path>,
    pub config_key: Option<String>,
    pub policies: Option<Vec<Expr>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Local,
    Rest,
    Grpc,
}

impl Parse for ProvidesAttr {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let mut contract: Option<Path> = None;
        let mut local: Option<Path> = None;
        let mut transports: Option<Vec<Transport>> = None;
        let mut rest_client: Option<Path> = None;
        let mut grpc_client: Option<Path> = None;
        let mut ir_fn: Option<Path> = None;
        let mut rest_binding_fn: Option<Path> = None;
        let mut grpc_binding_fn: Option<Path> = None;
        let mut config_key: Option<String> = None;
        let mut policies: Option<Vec<Expr>> = None;

        let items: Punctuated<KeyValue, Token![,]> =
            Punctuated::parse_terminated(input)?;
        for kv in items {
            let KeyValue { key, value } = kv;
            let name = key.to_string();
            match name.as_str() {
                "contract" => contract = Some(value.into_path(&key)?),
                "local" => local = Some(value.into_path(&key)?),
                "rest_client" => rest_client = Some(value.into_path(&key)?),
                "grpc_client" => grpc_client = Some(value.into_path(&key)?),
                "ir_fn" => ir_fn = Some(value.into_path(&key)?),
                "rest_binding_fn" => rest_binding_fn = Some(value.into_path(&key)?),
                "grpc_binding_fn" => grpc_binding_fn = Some(value.into_path(&key)?),
                "config_key" => config_key = Some(value.into_string(&key)?),
                "transports" => transports = Some(value.into_transports(&key)?),
                "policies" => policies = Some(value.into_exprs(&key)?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown #[toolkit::provides] arg `{other}`"),
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
        let local = local.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "missing required arg `local = path::to::factory_fn`",
            )
        })?;

        Ok(Self {
            contract,
            local,
            transports: transports.unwrap_or_else(|| vec![Transport::Local]),
            rest_client,
            grpc_client,
            ir_fn,
            rest_binding_fn,
            grpc_binding_fn,
            config_key,
            policies,
        })
    }
}

/// `name = value` pair where `value` is one of: path, string literal, or
/// bracketed list. We accept all three and disambiguate by the key.
struct KeyValue {
    key: Ident,
    value: AttrValue,
}

enum AttrValue {
    Expr(Expr),
}

impl AttrValue {
    fn into_path(self, key: &Ident) -> SynResult<Path> {
        match self {
            AttrValue::Expr(Expr::Path(p)) => Ok(p.path),
            AttrValue::Expr(other) => Err(syn::Error::new_spanned(
                other,
                format!("`{key}` expects a path"),
            )),
        }
    }

    fn into_string(self, key: &Ident) -> SynResult<String> {
        match self {
            AttrValue::Expr(Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            })) => Ok(s.value()),
            AttrValue::Expr(other) => Err(syn::Error::new_spanned(
                other,
                format!("`{key}` expects a string literal"),
            )),
        }
    }

    fn into_exprs(self, key: &Ident) -> SynResult<Vec<Expr>> {
        match self {
            AttrValue::Expr(Expr::Array(arr)) => Ok(arr.elems.into_iter().collect()),
            AttrValue::Expr(other) => Err(syn::Error::new_spanned(
                other,
                format!("`{key}` expects `[...]`"),
            )),
        }
    }

    fn into_transports(self, key: &Ident) -> SynResult<Vec<Transport>> {
        let exprs = self.into_exprs(key)?;
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
}

impl Parse for KeyValue {
    fn parse(input: ParseStream) -> SynResult<Self> {
        let key: Ident = input.parse()?;
        let _: Token![=] = input.parse()?;
        let expr: Expr = input.parse()?;
        Ok(Self {
            key,
            value: AttrValue::Expr(expr),
        })
    }
}

/// Top-level entry point invoked by the proc-macro shim in `lib.rs`.
pub fn generate(attr: &ProvidesAttr, item: &ItemStruct) -> SynResult<TokenStream> {
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
        append_segment(
            &parent_mod,
            &format_ident!("{}RestClient", contract_ident),
        )
    });
    let grpc_client_path = attr.grpc_client.clone().unwrap_or_else(|| {
        append_segment(
            &parent_mod,
            &format_ident!("{}GrpcClient", contract_ident),
        )
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

    let local_path = &attr.local;
    let transports = &attr.transports;
    let enable_local = transports.contains(&Transport::Local);
    let enable_rest = transports.contains(&Transport::Rest);
    let enable_grpc = transports.contains(&Transport::Grpc);

    // Policy stack expression. Default: `default_policy_stack()`.
    // Override: list of bare policy constructors → wrap each in `Arc::new(...)`
    // and feed into `policy_stack_from`.
    let policy_stack_expr = match &attr.policies {
        None => quote! { ::toolkit::wiring::default_policy_stack() },
        Some(list) if list.is_empty() => quote! { ::std::sync::Arc::new(::toolkit_contract::policy::PolicyStack::new()) },
        Some(list) => {
            let items = list.iter().map(|e| quote!(::std::sync::Arc::new(#e) as ::std::sync::Arc<dyn ::toolkit_contract::policy::Policy>));
            quote! { ::toolkit::wiring::policy_stack_from(vec![#(#items),*]) }
        }
    };

    // Build the match arms based on enabled transports.
    let local_arm = if enable_local {
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
                ::std::sync::Arc::new(
                    #rest_client_path::new(tuning.apply_to(endpoint))
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
                ::std::sync::Arc::new(
                    #grpc_client_path::connect(tuning.apply_to(endpoint)).await
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
        #item

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

/// Take all segments of `path` except the last (the trait name itself).
/// `foo::bar::Baz` → `foo::bar`. Single-segment paths produce an empty
/// path so subsequent appends fall back to the call-site scope.
fn parent_module(path: &Path) -> SynResult<Path> {
    if path.segments.is_empty() {
        return Err(syn::Error::new_spanned(path, "`contract` path is empty"));
    }
    let mut segments: Punctuated<PathSegment, Token![::]> = Punctuated::new();
    let n = path.segments.len();
    for (i, seg) in path.segments.iter().enumerate() {
        if i + 1 < n {
            segments.push(seg.clone());
        }
    }
    Ok(Path {
        leading_colon: path.leading_colon,
        segments,
    })
}

/// `parent` + `ident` → `parent::ident`. Empty parent yields a bare ident
/// path resolved against the call-site scope.
fn append_segment(parent: &Path, ident: &Ident) -> Path {
    let mut out = parent.clone();
    out.segments.push(PathSegment {
        ident: ident.clone(),
        arguments: PathArguments::None,
    });
    out
}
