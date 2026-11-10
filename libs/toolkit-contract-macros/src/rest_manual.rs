//! Codegen for `#[client_manual]` methods of `#[toolkit::rest_contract]`
//! (ADR-0009).
//!
//! A projection with manual methods gets a sealed `{Projection}Manual` trait
//! holding exactly those methods, and its generated clients become generic
//! over the handler: `{Projection}Client<__Handler: ?Sized [= Default]>`
//! stores an `Arc<__Handler>`, dispatches ordinary methods itself and
//! delegates manual ones.
//! A projection without manual methods expands exactly as before.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::model::StreamOpen;
use crate::projection::client_struct_ident;
use crate::rest_contract::{
    capture_auth_credentials, client_span_ctor, error_type, render_method_params,
    render_method_return, render_method_signature, to_snake_case,
};
use crate::rest_contract_parse::{RestContractModel, RestMethodModel};

/// Generic parameters of the generated client types. Empty for a projection
/// without manual methods, so its expansion is unchanged.
pub struct ClientGenerics {
    /// On the struct declaration: `<H: ?Sized [= Default]>`.
    pub decl: TokenStream,
    /// On impl blocks: `<H: {Projection}Manual + ?Sized>`.
    pub impl_generics: TokenStream,
    /// As above plus `'static`, for types stored in the directory resolver.
    pub impl_static: TokenStream,
    /// On type paths: `<H>`.
    pub ty: TokenStream,
}

impl ClientGenerics {
    /// The generics for `model`'s clients.
    pub fn of(model: &RestContractModel) -> Self {
        if !model.has_client_manual() {
            return Self {
                decl: TokenStream::new(),
                impl_generics: TokenStream::new(),
                impl_static: TokenStream::new(),
                ty: TokenStream::new(),
            };
        }
        let manual = manual_trait_ident(model);
        let handler = handler_param();
        let default = model.manual_handler.as_ref().map(|path| quote! { = #path });
        Self {
            decl: quote! { <#handler: ?::std::marker::Sized #default> },
            impl_generics: quote! { <#handler: #manual + ?::std::marker::Sized> },
            impl_static: quote! { <#handler: #manual + ?::std::marker::Sized + 'static> },
            ty: quote! { <#handler> },
        }
    }
}

/// The handler type parameter. Generated, so it cannot shadow an SDK type
/// that the contract's signatures name (`H` could).
fn handler_param() -> syn::Ident {
    format_ident!("__Handler")
}

/// `{Projection}Manual`: the handler trait's name.
pub fn manual_trait_ident(model: &RestContractModel) -> syn::Ident {
    format_ident!("{}Manual", model.trait_ident)
}

fn sealed_mod_ident(model: &RestContractModel) -> syn::Ident {
    format_ident!(
        "{}_manual_sealed",
        to_snake_case(&model.trait_ident.to_string())
    )
}

/// The sealing module and the `{Projection}Manual` trait.
pub fn generate_manual_trait(model: &RestContractModel, support: &TokenStream) -> TokenStream {
    if !model.has_client_manual() {
        return TokenStream::new();
    }
    let manual = manual_trait_ident(model);
    let sealed = sealed_mod_ident(model);
    let client = client_struct_ident(&model.trait_ident);
    let methods = model.methods.iter().filter(|m| m.client_manual).map(|m| {
        let ident = &m.ident;
        let sig = manual_signature(m, support);
        let asyncness = manual_asyncness(m);
        quote! { #asyncness fn #ident #sig; }
    });
    let trait_doc = format!(
        "Hand-written `#[client_manual]` methods of [`{}`], called by [`{client}`].\n\n\
         Sealed: only the SDK that declares the projection implements it, by also \
         implementing `{sealed}::Sealed` for its handler type. Each method receives the \
         base method's arguments plus a `ManualCallContext` that sends with the client's \
         endpoint, credentials and transport.",
        model.trait_ident,
    );
    quote! {
        #[cfg(feature = "rest-client")]
        #[doc = "Seals the manual-handler trait of this projection to its SDK crate."]
        #[allow(clippy::redundant_pub_crate)]
        pub(crate) mod #sealed {
            /// Implemented by the SDK's handler types next to the manual trait.
            pub trait Sealed {}
        }

        #[cfg(feature = "rest-client")]
        #[doc = #trait_doc]
        #[::async_trait::async_trait]
        pub trait #manual: #sealed::Sealed + ::std::marker::Send + ::std::marker::Sync {
            #(#methods)*
        }
    }
}

/// The base method's signature with `call: &ManualCallContext<'_>` after
/// `&self`.
fn manual_signature(method: &RestMethodModel, support: &TokenStream) -> TokenStream {
    let params = render_method_params(method);
    let ret = render_method_return(method);
    quote! {
        (&self, call: &#support::runtime::manual::ManualCallContext<'_>, #(#params),*) #ret
    }
}

fn manual_asyncness(method: &RestMethodModel) -> TokenStream {
    if method.shape.stream_open() == Some(StreamOpen::Immediate) {
        TokenStream::new()
    } else {
        quote! { async }
    }
}

/// The generic client struct and its constructors.
pub fn generate_mixed_client_struct(
    model: &RestContractModel,
    support: &TokenStream,
) -> TokenStream {
    let client = client_struct_ident(&model.trait_ident);
    let manual = manual_trait_ident(model);
    let generics = ClientGenerics::of(model);
    let decl = &generics.decl;
    let impl_generics = &generics.impl_generics;
    let handler = handler_param();
    let metrics_label = model.trait_ident.to_string();
    let doc = format!(
        "Generated REST client for [`{}`].\n\nProduced by `#[toolkit::rest_contract]`. \
         Ordinary methods are generated; `#[client_manual]` methods are delegated to the \
         handler `__Handler` ([`{manual}`]).",
        model.trait_ident
    );

    let default_ctors = model.manual_handler.as_ref().map(|default| {
        quote! {
            #[cfg(feature = "rest-client")]
            impl #client<#default> {
                /// Build a client with the default `toolkit-http` HTTP client and
                /// the SDK's default handler. See [`Self::with_manual`].
                ///
                /// # Errors
                /// Returns whatever `toolkit_http::HttpClient::builder().build()` returned.
                pub fn new(
                    config: #support::runtime::config::ClientConfig,
                ) -> ::std::result::Result<Self, ::toolkit_http::HttpError> {
                    Self::with_manual(
                        config,
                        ::std::sync::Arc::new(<#default as ::std::default::Default>::default()),
                    )
                }

                /// Build a client with a caller-supplied HTTP client and the SDK's
                /// default handler. See [`Self::with_http_client_and_manual`].
                #[must_use]
                pub fn with_http_client(
                    http: ::toolkit_http::HttpClient,
                    config: #support::runtime::config::ClientConfig,
                ) -> Self {
                    Self::with_http_client_and_manual(
                        http,
                        config,
                        ::std::sync::Arc::new(<#default as ::std::default::Default>::default()),
                    )
                }
            }
        }
    });

    quote! {
        #[cfg(feature = "rest-client")]
        #[doc = #doc]
        pub struct #client #decl {
            http: ::toolkit_http::HttpClient,
            config: #support::runtime::config::ClientConfig,
            manual_transport: #support::runtime::manual::ManualTransport,
            manual: ::std::sync::Arc<#handler>,
        }

        #default_ctors

        #[cfg(feature = "rest-client")]
        impl #impl_generics #client<#handler> {
            /// Build a client with the default `toolkit-http` HTTP client (see
            /// [`build_default_http_client`](#support::runtime::client::build_default_http_client))
            /// that delegates `#[client_manual]` methods to `manual`.
            ///
            /// # Errors
            /// Returns whatever `toolkit_http::HttpClient::builder().build()` returned.
            pub fn with_manual(
                config: #support::runtime::config::ClientConfig,
                manual: ::std::sync::Arc<#handler>,
            ) -> ::std::result::Result<Self, ::toolkit_http::HttpError> {
                // One `max_concurrent_requests` budget for generated and raw calls.
                let (http, manual_transport) =
                    #support::runtime::manual::ManualTransport::build(#metrics_label, &config)?;
                ::std::result::Result::Ok(Self {
                    http,
                    config,
                    manual_transport,
                    manual,
                })
            }

            /// Build a client with a caller-supplied `toolkit-http` HTTP client
            /// that delegates `#[client_manual]` methods to `manual`. As with the
            /// plain `with_http_client`, the transport-construction fields of
            /// `config` must already be baked into `http`. Raw passthrough calls
            /// do not go through `http`: they take TLS settings and their own
            /// `max_concurrent_requests` budget from `config`.
            #[must_use]
            pub fn with_http_client_and_manual(
                http: ::toolkit_http::HttpClient,
                config: #support::runtime::config::ClientConfig,
                manual: ::std::sync::Arc<#handler>,
            ) -> Self {
                let manual_transport =
                    #support::runtime::manual::ManualTransport::new(#metrics_label, &config);
                Self {
                    http,
                    config,
                    manual_transport,
                    manual,
                }
            }
        }
    }
}

/// Base-trait method of the generic client that delegates to the handler:
/// same span and credential capture as a generated method, then the handler
/// sends through the [`ManualCallContext`].
pub fn generate_manual_client_method(
    method: &RestMethodModel,
    model: &RestContractModel,
    support: &TokenStream,
) -> TokenStream {
    let trait_ident = &model.trait_ident;
    let manual = manual_trait_ident(model);
    let handler = handler_param();
    let binding_fn = format_ident!("__{}_http_binding", to_snake_case(&trait_ident.to_string()));
    let method_name = method.ident.to_string();
    let method_ident = &method.ident;
    let sig = render_method_signature(method);
    let span_ctor = client_span_ctor(&trait_ident.to_string(), &method_name, method, support);
    let auth_capture = capture_auth_credentials(method, support);
    let err_ty = error_type(method);
    let args: Vec<&syn::Ident> = method
        .params
        .iter()
        .filter(|p| p.ident != "self")
        .map(|p| &p.ident)
        .collect();
    let missing_binding = quote! {
        <#err_ty as ::std::convert::From<#support::runtime::transport_error::TransportError>>::from(
            #support::runtime::transport_error::TransportError::UrlBuild(
                concat!("missing HTTP binding for method '", #method_name, "'").to_owned(),
            ),
        )
    };
    let context = quote! {
        let __binding = #binding_fn();
        let __m = __binding.find_method(#method_name);
    };
    let call = quote! {
        #auth_capture
        let __call = #support::runtime::manual::ManualCallContext::new(
            &self.http,
            &self.config,
            __binding,
            __m,
            __bearer,
            __internal_token_provider,
            &self.manual_transport,
        );
    };

    if method.shape.stream_open() == Some(StreamOpen::Immediate) {
        let ret = render_method_return(method);
        // As for generated streams, the span is entered per yielded item and
        // marks failed items.
        return quote! {
            fn #method_ident #sig {
                use ::futures_util::StreamExt as _;
                let __span = #span_ctor;
                let __stream = __span.in_scope(|| #ret {
                    #context
                    let ::std::option::Option::Some(__m) = __m else {
                        let __err = #missing_binding;
                        return ::std::boxed::Box::pin(::futures_util::stream::once(async move {
                            ::std::result::Result::Err(__err)
                        }));
                    };
                    #call
                    <#handler as #manual>::#method_ident(&*self.manual, &__call, #(#args),*)
                });
                ::std::boxed::Box::pin(__stream.map(move |__item| {
                    let __enter = __span.enter();
                    if __item.is_err() {
                        #support::__tracing::Span::current().record("error", true);
                    }
                    ::std::mem::drop(__enter);
                    __item
                }))
            }
        };
    }

    // A fallible open keeps the span for the items of the stream it returns.
    let wrap_stream = method
        .result_types
        .as_ref()
        .filter(|_| method.shape.is_streaming())
        .map(|(item_ty, err_ty)| {
            quote! {
                let __result = __result.map(|__stream| {
                    use ::futures_util::StreamExt as _;
                    let __boxed: ::std::pin::Pin<::std::boxed::Box<
                        dyn ::futures_core::Stream<
                            Item = ::std::result::Result<#item_ty, #err_ty>,
                        > + ::std::marker::Send + 'static,
                    >> = ::std::boxed::Box::pin(__stream.map(move |__item| {
                        let __enter = __item_span.enter();
                        if __item.is_err() {
                            #support::__tracing::Span::current().record("error", true);
                        }
                        ::std::mem::drop(__enter);
                        __item
                    }));
                    __boxed
                });
            }
        });

    quote! {
        async fn #method_ident #sig {
            let __span = #span_ctor;
            let __item_span = __span.clone();
            #support::__tracing::Instrument::instrument(async move {
                #context
                let ::std::option::Option::Some(__m) = __m else {
                    return ::std::result::Result::Err(#missing_binding);
                };
                #call
                let __result =
                    <#handler as #manual>::#method_ident(&*self.manual, &__call, #(#args),*).await;
                if __result.is_err() {
                    #support::__tracing::Span::current().record("error", true);
                }
                #wrap_stream
                __result
            }, __span).await
        }
    }
}

/// The generic directory-resolving client: every endpoint rebuild constructs
/// a new inner client around the same `Arc<H>`, so handler state survives
/// provider churn.
pub fn generate_mixed_resolving_struct(
    model: &RestContractModel,
    support: &TokenStream,
) -> TokenStream {
    let resolving = crate::rest_contract::resolving_struct_ident(&model.trait_ident);
    let client = client_struct_ident(&model.trait_ident);
    let generics = ClientGenerics::of(model);
    let decl = &generics.decl;
    let impl_static = &generics.impl_static;
    let handler = handler_param();
    let doc = format!(
        "Directory-resolving REST client for [`{}`].\n\nProduced by `#[toolkit::rest_contract]`. \
         Resolves the provider endpoint from the service directory on every call and rebuilds \
         the inner [`{client}`] when the endpoint changes, keeping the same handler.",
        model.trait_ident
    );

    let default_ctor = model.manual_handler.as_ref().map(|default| {
        quote! {
            #[cfg(feature = "rest-client")]
            impl #resolving<#default> {
                /// Build a resolving client that discovers `from_gear` via
                /// `resolver`, with the SDK's default handler. See
                /// [`Self::with_manual`].
                #[must_use]
                pub fn new(
                    resolver: ::std::sync::Arc<dyn #support::runtime::resolving::EndpointResolver>,
                    from_gear: impl ::std::convert::Into<::std::string::String>,
                    tuning: #support::wiring::ClientTuning,
                ) -> Self {
                    Self::with_manual(
                        resolver,
                        from_gear,
                        tuning,
                        ::std::sync::Arc::new(<#default as ::std::default::Default>::default()),
                    )
                }
            }
        }
    });

    quote! {
        #[cfg(feature = "rest-client")]
        #[doc = #doc]
        pub struct #resolving #decl {
            __inner: ::std::sync::Arc<
                #support::runtime::resolving::DirectoryResolvingClient<#client<#handler>>,
            >,
        }

        #default_ctor

        #[cfg(feature = "rest-client")]
        impl #impl_static #resolving<#handler> {
            /// Build a resolving client that discovers `from_gear` via `resolver`
            /// and delegates `#[client_manual]` methods to `manual`.
            ///
            /// The inner REST client is (re)built from each resolved endpoint with
            /// `with_manual`, sharing `manual`; `tuning` applies the per-call
            /// timeout / retry / reconnect overrides.
            #[must_use]
            pub fn with_manual(
                resolver: ::std::sync::Arc<dyn #support::runtime::resolving::EndpointResolver>,
                from_gear: impl ::std::convert::Into<::std::string::String>,
                tuning: #support::wiring::ClientTuning,
                manual: ::std::sync::Arc<#handler>,
            ) -> Self {
                let __inner = #support::runtime::resolving::DirectoryResolvingClient::new(
                    resolver,
                    from_gear,
                    tuning,
                    move |__cfg| {
                        #client::<#handler>::with_manual(__cfg, ::std::sync::Arc::clone(&manual)).map_err(
                            |__e| #support::runtime::transport_error::TransportError::network(__e),
                        )
                    },
                );
                Self { __inner: ::std::sync::Arc::new(__inner) }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use proc_macro2::TokenStream;
    use quote::quote;

    use crate::rest_contract_parse::{RestContractAttr, parse};

    /// Expansion with whitespace removed, so assertions read like source.
    fn expand(attr: TokenStream, item: TokenStream) -> String {
        let attr: RestContractAttr = syn::parse2(attr).unwrap();
        let model = parse(attr, syn::parse2(item).unwrap()).unwrap();
        crate::rest_contract::generate(&model)
            .to_string()
            .replace(' ', "")
    }

    fn parse_err(attr: TokenStream, item: TokenStream) -> String {
        let attr: RestContractAttr = syn::parse2(attr).unwrap();
        match parse(attr, syn::parse2(item).unwrap()) {
            Ok(_) => panic!("expected a parse error"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn projection_without_manual_methods_expands_as_before() {
        let out = expand(
            quote! { base_path = "/demo/v1" },
            quote! {
                pub trait DemoApiRest: DemoApi {
                    #[get("/one/{id}")]
                    async fn one(&self, ctx: SecurityContext, id: String) -> Result<u32, E>;
                }
            },
        );
        assert!(out.contains("pubstructDemoApiRestClient{"), "{out}");
        assert!(
            out.contains("pubstructDemoApiRestResolvingClient{"),
            "{out}"
        );
        assert!(!out.contains("Manual"), "{out}");
    }

    #[test]
    fn manual_projection_is_generic_over_a_sealed_handler() {
        let out = expand(
            quote! { base_path = "/demo/v1", manual_handler = crate::Handler },
            quote! {
                pub trait DemoApiRest: DemoApi {
                    #[get("/one/{id}")]
                    async fn one(&self, ctx: SecurityContext, id: String) -> Result<u32, E>;
                    #[get("/two")]
                    #[client_manual]
                    async fn two(&self, ctx: SecurityContext) -> Result<u32, E>;
                }
            },
        );
        assert!(
            out.contains(
                "pubstructDemoApiRestClient<__Handler:?::std::marker::Sized=crate::Handler>"
            ),
            "{out}"
        );
        assert!(
            out.contains("pub(crate)moddemo_api_rest_manual_sealed"),
            "{out}"
        );
        assert!(
            out.contains("pubtraitDemoApiRestManual:demo_api_rest_manual_sealed::Sealed"),
            "{out}"
        );
        // Only the manual method is in the handler trait.
        assert!(out.contains("asyncfntwo(&self,call:&"), "{out}");
        assert!(!out.contains("asyncfnone(&self,call:&"), "{out}");
        assert!(
            out.contains("implDemoApiRestClient<crate::Handler>{"),
            "{out}"
        );
        assert!(
            out.contains("<__HandlerasDemoApiRestManual>::two(&*self.manual"),
            "{out}"
        );
    }

    #[test]
    fn passthrough_binding_is_honest_in_the_ir() {
        let out = expand(
            quote! { base_path = "/demo/v1" },
            quote! {
                pub trait DemoApiRest: DemoApi {
                    #[passthrough("/proxy/{*path}", methods = [get, head])]
                    #[client_manual]
                    #[server_manual]
                    async fn proxy(&self, ctx: SecurityContext, req: Req) -> Result<Resp, E>;
                }
            },
        );
        assert!(out.contains("http_method:"), "{out}");
        assert!(out.contains("HttpMethod::Passthrough,"), "{out}");
        assert!(
            out.contains(
                "HttpFieldBinding::Passthrough{field:\"req\".to_owned(),param:\"path\".to_owned()"
            ),
            "{out}"
        );
        assert!(out.contains("HttpMethod::Get,"), "{out}");
        assert!(out.contains("HttpMethod::Head]"), "{out}");
    }

    #[test]
    fn passthrough_rejects_extra_arguments_and_bad_templates() {
        let extra = parse_err(
            quote! { base_path = "/demo/v1" },
            quote! {
                pub trait DemoApiRest: DemoApi {
                    #[passthrough("/proxy/{*path}", methods = [get])]
                    #[client_manual]
                    #[server_manual]
                    async fn proxy(&self, ctx: SecurityContext, a: Req, b: u32) -> Result<R, E>;
                }
            },
        );
        assert!(extra.contains("exactly one argument"), "{extra}");

        let middle = parse_err(
            quote! { base_path = "/demo/v1" },
            quote! {
                pub trait DemoApiRest: DemoApi {
                    #[passthrough("/proxy/{*path}/tail", methods = [get])]
                    #[client_manual]
                    #[server_manual]
                    async fn proxy(&self, ctx: SecurityContext, req: Req) -> Result<R, E>;
                }
            },
        );
        assert!(middle.contains("must end with exactly one"), "{middle}");

        let glued = parse_err(
            quote! { base_path = "/demo/v1" },
            quote! {
                pub trait DemoApiRest: DemoApi {
                    #[passthrough("/proxy{*path}", methods = [get])]
                    #[client_manual]
                    #[server_manual]
                    async fn proxy(&self, ctx: SecurityContext, req: Req) -> Result<R, E>;
                }
            },
        );
        assert!(glued.contains("`/{*name}` wildcard segment"), "{glued}");

        let duplicate = parse_err(
            quote! { base_path = "/demo/v1" },
            quote! {
                pub trait DemoApiRest: DemoApi {
                    #[passthrough("/proxy/{*path}", methods = [get, get])]
                    #[client_manual]
                    #[server_manual]
                    async fn proxy(&self, ctx: SecurityContext, req: Req) -> Result<R, E>;
                }
            },
        );
        assert!(
            duplicate.contains("duplicate passthrough method"),
            "{duplicate}"
        );
    }

    #[test]
    fn rejects_malformed_manual_attributes() {
        let passthrough = |args: TokenStream, extra: TokenStream| {
            parse_err(
                quote! { base_path = "/demo/v1" },
                quote! {
                    pub trait DemoApiRest: DemoApi {
                        #[passthrough(#args)]
                        #[client_manual]
                        #[server_manual]
                        #extra
                        async fn proxy(&self, ctx: SecurityContext, req: Req) -> Result<R, E>;
                    }
                },
            )
        };
        let unknown = passthrough(quote! { "/p/{*path}", methods = [get, trace] }, quote! {});
        assert!(
            unknown.contains("unknown passthrough method `trace`"),
            "{unknown}"
        );
        let empty = passthrough(quote! { "/p/{*path}", methods = [] }, quote! {});
        assert!(empty.contains("at least one method"), "{empty}");
        let streaming = passthrough(
            quote! { "/p/{*path}", methods = [get] },
            quote! { #[streaming(open = fallible)] },
        );
        assert!(
            streaming.contains("cannot also be `#[streaming]`"),
            "{streaming}"
        );

        let attr: Result<RestContractAttr, _> =
            syn::parse2(quote! { base_path = "/demo/v1", manual_handler = A, manual_handler = B });
        let duplicate = attr.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            duplicate.contains("duplicate `manual_handler`"),
            "{duplicate}"
        );
    }

    #[test]
    fn passthrough_accepts_a_trailing_comma() {
        let out = expand(
            quote! { base_path = "/demo/v1" },
            quote! {
                pub trait DemoApiRest: DemoApi {
                    #[passthrough("/p/{*path}", methods = [get],)]
                    #[client_manual]
                    #[server_manual]
                    async fn proxy(&self, ctx: SecurityContext, req: Req) -> Result<R, E>;
                }
            },
        );
        assert!(out.contains("HttpMethod::Passthrough,"), "{out}");
    }
}
