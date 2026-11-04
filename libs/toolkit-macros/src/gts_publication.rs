//! `gts_publication(...)` section of `#[toolkit::gear]`: the GTS publication capability.
//!
//! ```ignore
//! gts_publication(
//!     crates = [crate, my_gear_sdk],          // crate-local collectors
//!     entities = Self::configured_entities,   // optional, config-only
//!     publisher = path::to::adapter,          // optional; see below
//! )
//! ```
//!
//! The publisher adapter defaults to `::types_registry_sdk::publication`: the
//! publishing gear depends on `types-registry-sdk` under its library name, with
//! the `rest-client` feature. `publisher = ...` overrides it, e.g. for a test
//! adapter or a dependency under another name. Toolkit itself never depends on
//! the SDK; only this expansion, in the gear crate, names it.
//!
//! The expansion lives in the gear crate. It builds the
//! `toolkit::gts_publication::GtsPublicationDescriptor`, implements
//! `GtsPublicationCapability` and the wiring of the adapter's registry client
//! (`GtsPublicationDescriptor::wire`), which the runtime calls only for a
//! non-empty manifest. Publishing is not consuming: a gear that also calls the
//! registry for its own business declares `#[toolkit::consumes]` of the same
//! contract, wired and readiness-gated like any consumed contract; both wire
//! the same client, whichever comes first.
//!
//! The publication policy is configuration, not an attribute argument: the runtime
//! reads `gears.<gear>.config.gts_publication` and the expansion forwards it to the
//! adapter's `timeout`, `retry_backoff` and `batch_size` builders.
//!
//! The runtime publishes the manifest and waits for its admission before any
//! application `init`; a plugin binds its scoped clients in `init` from
//! `GearCtx::gts_publication_manifest()`. There is no admission callback.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Expr, Ident, LitStr, MetaList, Path};

/// Parsed `gts_publication(...)`.
pub struct GtsPublicationCfg {
    crates: Vec<Path>,
    publisher: Path,
    entities: Option<Path>,
}

fn expect_path(expr: Expr, what: &str) -> syn::Result<Path> {
    match expr {
        Expr::Path(p) => Ok(p.path),
        other => Err(syn::Error::new_spanned(
            other,
            format!("`{what}` must be a path"),
        )),
    }
}

/// Parse the `gts_publication(...)` list of `#[toolkit::gear]`. `publisher` defaults to
/// `::types_registry_sdk::publication`.
///
/// # Errors
/// An unknown, removed or duplicate argument, a value that is not a path or a
/// list of paths, a crate listed twice, or neither `crates` nor `entities`.
pub fn parse(list: &MetaList) -> syn::Result<GtsPublicationCfg> {
    let mut crates: Option<Vec<Path>> = None;
    let mut publisher: Option<Path> = None;
    let mut entities: Option<Path> = None;

    list.parse_nested_meta(|meta| {
        let key = meta
            .path
            .get_ident()
            .map(ToString::to_string)
            .unwrap_or_default();
        let duplicate = |seen: bool| -> syn::Result<()> {
            if seen {
                Err(meta.error(format!("duplicate `{key}` in `gts_publication(...)`")))
            } else {
                Ok(())
            }
        };
        match key.as_str() {
            "crates" => {
                duplicate(crates.is_some())?;
                let Expr::Array(array) = meta.value()?.parse::<Expr>()? else {
                    return Err(
                        meta.error("`crates` must be a list, e.g. crates = [crate, my_sdk]")
                    );
                };
                let mut paths = Vec::new();
                for elem in array.elems {
                    let path = expect_path(elem, "crates")?;
                    if paths.contains(&path) {
                        return Err(syn::Error::new_spanned(
                            path,
                            "crate listed twice in `crates`",
                        ));
                    }
                    paths.push(path);
                }
                crates = Some(paths);
            }
            "publisher" => {
                duplicate(publisher.is_some())?;
                publisher = Some(expect_path(meta.value()?.parse()?, "publisher")?);
            }
            "entities" => {
                duplicate(entities.is_some())?;
                entities = Some(expect_path(meta.value()?.parse()?, "entities")?);
            }
            "on_admitted" => {
                return Err(meta.error(
                    "`on_admitted` is removed: the runtime admits the manifest before `init`; \
                     bind plugin clients in `init` from `GearCtx::gts_publication_manifest()`",
                ));
            }
            _ => {
                return Err(meta
                    .error("unknown `gts_publication(...)` argument; expected crates, publisher or entities"));
            }
        }
        Ok(())
    })?;

    let publisher =
        publisher.unwrap_or_else(|| syn::parse_quote!(::types_registry_sdk::publication));
    let crates = crates.unwrap_or_default();
    if crates.is_empty() && entities.is_none() {
        return Err(syn::Error::new_spanned(
            &list.path,
            "`gts_publication(...)` publishes nothing: give `crates = [...]`, `entities = ...` or both",
        ));
    }
    Ok(GtsPublicationCfg {
        crates,
        publisher,
        entities,
    })
}

/// The `gts_declarations()` call and origin name of one `crates` entry.
fn declarations_of(path: &Path) -> (TokenStream, TokenStream) {
    if path.is_ident("crate") {
        (
            quote! { crate::gts_declarations() },
            quote! { ::core::env!("CARGO_PKG_NAME") },
        )
    } else if let Some(ident) = path.get_ident() {
        let origin = LitStr::new(&ident.to_string(), ident.span());
        (quote! { ::#ident::gts_declarations() }, quote! { #origin })
    } else {
        let origin = LitStr::new(
            &quote!(#path).to_string().replace(' ', ""),
            proc_macro2::Span::call_site(),
        );
        (quote! { #path::gts_declarations() }, quote! { #origin })
    }
}

/// Top-level items for the gear struct `ident` named `name`.
pub fn expand(cfg: &GtsPublicationCfg, ident: &Ident, name: &LitStr) -> TokenStream {
    let publisher = &cfg.publisher;
    let sources = cfg.crates.iter().map(|path| {
        let (call, origin) = declarations_of(path);
        quote! { ::toolkit::gts_publication::DeclarationSource::new(#origin, #call) }
    });
    let entities = cfg.entities.as_ref().map_or_else(
        || quote! { ::core::option::Option::None },
        |path| {
            quote! {
                ::core::option::Option::Some(#path as ::toolkit::gts_publication::GtsEntitiesFn)
            }
        },
    );
    let snake = heck::ToSnakeCase::to_snake_case(ident.to_string().as_str());
    let wire_fn = format_ident!("__toolkit_wire_gts_publisher_for_{}", snake);

    quote! {
        #[doc(hidden)]
        impl #ident {
            /// The GTS publication generated from `gts_publication(...)`.
            pub const __TOOLKIT_GTS_PUBLICATION: ::toolkit::gts_publication::GtsPublicationDescriptor =
                ::toolkit::gts_publication::GtsPublicationDescriptor {
                    gear: #name,
                    publisher_version: ::core::env!("CARGO_PKG_VERSION"),
                    declarations: Self::__toolkit_gts_declarations,
                    entities: #entities,
                    backend: Self::__toolkit_gts_backend,
                    provider: #publisher::PROVIDER,
                    wire: #wire_fn,
                };

            fn __toolkit_gts_declarations()
                -> ::std::vec::Vec<::toolkit::gts_publication::DeclarationSource>
            {
                ::std::vec![#(#sources),*]
            }

            fn __toolkit_gts_backend(
                hub: &::toolkit::ClientHub,
            ) -> ::anyhow::Result<::toolkit::gts_publication::GtsPublicationBackend> {
                let registry = hub.get::<#publisher::Registry>().map_err(|e| {
                    ::anyhow::anyhow!("{}: no registry client to publish GTS documents: {e}", #name)
                })?;
                ::core::result::Result::Ok(::toolkit::gts_publication::GtsPublicationBackend::new(
                    move |request: ::toolkit::gts_publication::GtsPublicationRequest| {
                        let registry = ::std::sync::Arc::clone(&registry);
                        async move {
                            let manifest = request.gts_manifest();
                            let options = request.options();
                            #publisher::Publication::from_manifest(
                                &*registry,
                                manifest.publisher_name(),
                                manifest.publisher_version(),
                                manifest.documents(),
                            )
                            .map_err(::anyhow::Error::from)?
                            .deadline(request.deadline())
                            .timeout(options.timeout())
                            .retry_backoff(options.retry_backoff(), options.retry_backoff_max())
                            .batch_size(options.batch_size())
                            .on_pending(|pending| {
                                request.report_pending(&::std::string::ToString::to_string(pending));
                            })
                            .publish(request.cancel())
                            .await
                            .map(|_| ())
                            .map_err(::anyhow::Error::from)
                        }
                    },
                ))
            }
        }

        impl ::toolkit::gts_publication::GtsPublicationCapability for #ident {
            fn descriptor(&self) -> &'static ::toolkit::gts_publication::GtsPublicationDescriptor {
                &Self::__TOOLKIT_GTS_PUBLICATION
            }
        }

        ::toolkit::inventory::submit! {
            ::toolkit::gts_publication::GtsPublicationRegistration(&#ident::__TOOLKIT_GTS_PUBLICATION)
        }

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #wire_fn(
            hub: &::toolkit::ClientHub,
            resolver: ::std::sync::Arc<dyn ::toolkit::discovery::EndpointResolver>,
            tuning: ::toolkit::contract_support::wiring::ClientTuning,
        ) -> ::anyhow::Result<::toolkit::discovery::WireOutcome> {
            // As `#[toolkit::consumes]`: a local provider wins, an existing
            // proxy is reused, otherwise the adapter's resolving client.
            if hub.try_get_local::<#publisher::Registry>().is_some() {
                return ::core::result::Result::Ok(::toolkit::discovery::WireOutcome::Local);
            }
            if hub.has_remote_proxy::<#publisher::Registry>() {
                return ::core::result::Result::Ok(::toolkit::discovery::WireOutcome::Remote);
            }
            let client = #publisher::ResolvingClient::new(resolver, #publisher::PROVIDER, tuning);
            hub.register_remote_proxy::<#publisher::Registry>(::std::sync::Arc::new(client));
            ::core::result::Result::Ok(::toolkit::discovery::WireOutcome::Remote)
        }

    }
}
