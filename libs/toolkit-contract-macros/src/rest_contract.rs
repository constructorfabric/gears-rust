//! Code generation for `#[toolkit::rest_contract]`.
//!
//! Emits two artifacts:
//! 1. The original projection trait, with HTTP/marker attributes stripped from
//!    every method so it compiles unchanged outside the macro.
//! 2. A free function `<trait_snake_case>_http_binding() -> HttpBindingIr`
//!    that materializes the binding IR derived from the trait declaration.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{TraitItem, Type};

use crate::projection::{
    build_delegation_body, client_struct_ident, generate_projection_impl_for_client,
    rewrite_streaming_signature, strip_method_attrs, type_path_ends_with,
};
use crate::rest_contract_parse::{HttpVerb, RestContractModel, RestMethodModel, RestParam};
use crate::support::contract_support_path;

const HTTP_ATTRS: &[&str] = &[
    "get",
    "post",
    "put",
    "delete",
    "retryable",
    "streaming",
    "server_manual",
];

fn streaming_idents(method: &RestMethodModel) -> Option<(Type, Type)> {
    if method.streaming {
        method.result_types.clone()
    } else {
        None
    }
}

pub fn generate(model: &RestContractModel) -> TokenStream {
    let support = contract_support_path();
    let cleaned_trait = generate_cleaned_trait(model);
    let binding_fn = generate_binding_fn(model, &support);
    let client_struct = generate_client_struct(model, &support);
    let client_impl = generate_client_impl(model, &support);
    let resolving_struct = generate_resolving_client_struct(model, &support);
    let resolving_impl = generate_resolving_client_impl(model, &support);
    let projection_impl = generate_projection_impl(model);
    let server_registration = generate_server_registration(model);

    quote! {
        #cleaned_trait
        #binding_fn
        #client_struct
        #client_impl
        #resolving_struct
        #resolving_impl
        #projection_impl
        #server_registration
    }
}

fn generate_projection_impl(model: &RestContractModel) -> TokenStream {
    generate_projection_impl_for_client(
        &model.trait_ident,
        &client_struct_ident(&model.trait_ident),
        "rest-client",
    )
}

fn generate_cleaned_trait(model: &RestContractModel) -> TokenStream {
    let mut item = model.item.clone();
    let base_trait = &model.base_trait;

    let streaming_methods: std::collections::HashMap<String, (Type, Type)> = model
        .methods
        .iter()
        .filter_map(|m| streaming_idents(m).map(|t| (m.ident.to_string(), t)))
        .collect();
    let model_methods: std::collections::HashMap<String, &RestMethodModel> = model
        .methods
        .iter()
        .map(|m| (m.ident.to_string(), m))
        .collect();

    for trait_item in &mut item.items {
        if let TraitItem::Fn(method) = trait_item {
            strip_method_attrs(method, HTTP_ATTRS);
            if let Some((ok, err)) = streaming_methods.get(&method.sig.ident.to_string()) {
                rewrite_streaming_signature(method, ok, err);
            }
            // PRD #1536 D3: projection-trait methods become default fns
            // that delegate to the base trait via fully-qualified syntax.
            // The generated REST client implements the base trait; this
            // delegation lets `Arc<dyn ProjectionTrait>` work for free.
            if let Some(model_method) = model_methods.get(&method.sig.ident.to_string()) {
                let arg_idents: Vec<&syn::Ident> = model_method
                    .params
                    .iter()
                    .filter(|p| p.ident != "self")
                    .map(|p| &p.ident)
                    .collect();
                method.default = Some(build_delegation_body(
                    base_trait,
                    &model_method.ident,
                    arg_idents,
                    model_method.streaming,
                ));
            }
        }
    }

    quote! {
        #[::async_trait::async_trait]
        #item
    }
}

fn generate_binding_fn(model: &RestContractModel, support: &TokenStream) -> TokenStream {
    let trait_name_snake = to_snake_case(&model.trait_ident.to_string());
    let fn_ident = format_ident!("{}_http_binding", trait_name_snake);
    let trait_doc = format!("Build the HTTP binding IR for [`{}`].", model.trait_ident);
    let base_path = &model.base_path;

    let method_entries = model
        .methods
        .iter()
        .map(|m| build_method_binding(m, support));

    quote! {
        #[doc = #trait_doc]
        #[must_use]
        pub fn #fn_ident() -> #support::ir::binding::HttpBindingIr {
            #support::ir::binding::HttpBindingIr {
                base_path: #base_path.to_owned(),
                methods: vec![
                    #(#method_entries),*
                ],
            }
        }
    }
}

fn build_method_binding(method: &RestMethodModel, support: &TokenStream) -> TokenStream {
    let method_name = method.ident.to_string();
    let path = &method.path_template;
    let http_method = http_method_tokens(method.http_method, support);
    let retryable = method.retryable;
    let streaming = method.streaming;
    let optional = method.optional;

    let path_param_names = extract_path_param_names(path);

    let field_bindings = build_field_bindings(method, &path_param_names, support);

    quote! {
        #support::ir::binding::HttpMethodBindingIr {
            method_name: #method_name.to_owned(),
            http_method: #http_method,
            path_template: #path.to_owned(),
            field_bindings: vec![ #(#field_bindings),* ],
            retryable: #retryable,
            streaming: #streaming,
            optional: #optional,
        }
    }
}

fn http_method_tokens(verb: HttpVerb, support: &TokenStream) -> TokenStream {
    let variant = syn::Ident::new(verb.ir_variant(), proc_macro2::Span::call_site());
    quote! { #support::ir::binding::HttpMethod::#variant }
}

fn build_field_bindings(
    method: &RestMethodModel,
    path_params: &[String],
    support: &TokenStream,
) -> Vec<TokenStream> {
    let mut bindings = Vec::new();
    let mut body_assigned = false;

    for param in &method.params {
        if is_skip_param(param) {
            continue;
        }
        let name = param.ident.to_string();

        if path_params.iter().any(|p| p == &name) {
            bindings.push(quote! {
                #support::ir::binding::HttpFieldBinding::Path {
                    field: #name.to_owned(),
                    param: #name.to_owned(),
                }
            });
            continue;
        }

        if method.http_method.allows_body() && !body_assigned {
            bindings.push(quote! { #support::ir::binding::HttpFieldBinding::Body });
            body_assigned = true;
            continue;
        }

        // GET/DELETE remaining parameters, or extra POST/PUT params after body.
        bindings.push(quote! {
            #support::ir::binding::HttpFieldBinding::Query {
                field: #name.to_owned(),
                param: #name.to_owned(),
            }
        });
    }

    bindings
}

fn is_skip_param(param: &RestParam) -> bool {
    let ident = param.ident.to_string();
    if ident == "self" {
        return true;
    }
    // Heuristic: parameters whose type ends in `SecurityContext` are not part
    // of the wire payload — they are populated by the server via Axum
    // extractors and by the client via per-request headers.
    type_path_ends_with(&param.ty, "SecurityContext")
}

fn extract_path_param_names(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        if let Some(end) = rest[start..].find('}') {
            let inner = &rest[start + 1..start + end];
            if !inner.is_empty() {
                names.push(inner.to_owned());
            }
            rest = &rest[start + end + 1..];
        } else {
            break;
        }
    }
    names
}

fn generate_client_struct(model: &RestContractModel, support: &TokenStream) -> TokenStream {
    let client_ident = client_struct_ident(&model.trait_ident);
    let doc = format!(
        "Generated REST client for [`{}`].\n\nProduced by `#[toolkit::rest_contract]`.",
        model.trait_ident
    );

    quote! {
        #[cfg(feature = "rest-client")]
        #[doc = #doc]
        pub struct #client_ident {
            http: ::toolkit_http::HttpClient,
            config: #support::runtime::config::ClientConfig,
        }

        #[cfg(feature = "rest-client")]
        impl #client_ident {
            /// Build a new client with a default `toolkit-http` HTTP client.
            ///
            /// Fallible because the underlying `toolkit-http` builder can fail
            /// under non-default cryptographic backends (FIPS, custom TLS).
            /// The previous infallible `new` panicked in those configurations;
            /// callers must now `?` the error or pass it up. For
            /// caller-controlled HTTP client construction, use
            /// [`Self::with_http_client`].
            ///
            /// # Errors
            /// Returns whatever `toolkit_http::HttpClient::builder().build()` returned.
            pub fn new(
                config: #support::runtime::config::ClientConfig,
            ) -> ::std::result::Result<Self, ::toolkit_http::HttpError> {
                let http = Self::build_default_http_client()?;
                Ok(Self { http, config })
            }

            /// Build the default `toolkit-http` HttpClient used by `new`/`try_new`.
            ///
            /// - Retry is **disabled** at the transport layer: this SDK
            ///   consults [`ClientConfig::retry`] and runs its own retry loop
            ///   in `runtime::retry`; double-retry would amplify request rate
            ///   under failure.
            /// - Plain `http://` is **allowed**: internal service-to-service
            ///   traffic in dev / behind a mesh sidecar typically uses
            ///   plaintext. Callers needing TLS-only enforcement use
            ///   [`Self::with_http_client`] with a stricter `HttpClient`.
            /// - OpenTelemetry is **enabled** (`with_otel`): outbound calls get
            ///   a span and W3C trace-context propagation so a contract hop
            ///   joins the distributed trace. Inert unless the build enables
            ///   `toolkit-http/otel`; callers wanting a different observability
            ///   stance use [`Self::with_http_client`].
            fn build_default_http_client() -> ::std::result::Result<
                ::toolkit_http::HttpClient,
                ::toolkit_http::HttpError,
            > {
                ::toolkit_http::HttpClient::builder()
                    .retry(::std::option::Option::None)
                    .transport(::toolkit_http::TransportSecurity::AllowInsecureHttp)
                    .with_otel()
                    .build()
            }

            /// Build a new client with a caller-supplied `toolkit-http`
            /// HTTP client.
            #[must_use]
            pub fn with_http_client(
                http: ::toolkit_http::HttpClient,
                config: #support::runtime::config::ClientConfig,
            ) -> Self {
                Self { http, config }
            }
        }
    }
}

fn generate_client_impl(model: &RestContractModel, support: &TokenStream) -> TokenStream {
    let client_ident = client_struct_ident(&model.trait_ident);
    let trait_path = &model.base_trait;

    let methods = model
        .methods
        .iter()
        .map(|m| generate_client_method(m, &model.trait_ident, support));

    quote! {
        #[cfg(feature = "rest-client")]
        #[::async_trait::async_trait]
        impl #trait_path for #client_ident {
            #(#methods)*
        }
    }
}

/// Generate the directory-resolving wrapper struct + its constructor.
///
/// The wrapper holds an `Arc<DirectoryResolvingClient<XxxRestClient>>` and
/// delegates each base-trait method through it (see
/// [`generate_resolving_client_impl`]). Gated behind `directory-rest-client`
/// (which enables `rest-client`, so `XxxRestClient` exists).
fn generate_resolving_client_struct(model: &RestContractModel, support: &TokenStream) -> TokenStream {
    let resolving_ident = resolving_struct_ident(&model.trait_ident);
    let client_ident = client_struct_ident(&model.trait_ident);
    let doc = format!(
        "Directory-resolving REST client for [`{}`].\n\nProduced by `#[toolkit::rest_contract]`. \
         Resolves the provider endpoint from the service directory on every call and rebuilds \
         the inner [`{client_ident}`] when the endpoint changes, so consumers tolerate eventual \
         readiness and provider churn.",
        model.trait_ident
    );

    quote! {
        #[cfg(feature = "directory-rest-client")]
        #[doc = #doc]
        pub struct #resolving_ident {
            __inner: ::std::sync::Arc<
                #support::runtime::resolving::DirectoryResolvingClient<#client_ident>,
            >,
        }

        #[cfg(feature = "directory-rest-client")]
        impl #resolving_ident {
            /// Build a resolving client that discovers `from_gear` via `resolver`.
            ///
            /// The inner REST client is (re)built from the resolved endpoint via
            /// the generated [`new`](#client_ident::new); `tuning` applies the
            /// per-call timeout / retry / reconnect overrides.
            #[must_use]
            pub fn new(
                resolver: ::std::sync::Arc<dyn #support::runtime::resolving::EndpointResolver>,
                from_gear: impl ::std::convert::Into<::std::string::String>,
                tuning: #support::wiring::ClientTuning,
            ) -> Self {
                let __inner = #support::runtime::resolving::DirectoryResolvingClient::new(
                    resolver,
                    from_gear,
                    tuning,
                    |__cfg| {
                        <#client_ident>::new(__cfg).map_err(|__e| {
                            #support::runtime::transport_error::TransportError::network(__e)
                        })
                    },
                );
                Self { __inner: ::std::sync::Arc::new(__inner) }
            }
        }
    }
}

/// Generate the base-trait impl for the resolving wrapper. Each method resolves
/// the live client and delegates; unresolved providers surface as
/// `TransportError::Unresolved` (mapped into the trait's error type).
fn generate_resolving_client_impl(model: &RestContractModel, support: &TokenStream) -> TokenStream {
    let resolving_ident = resolving_struct_ident(&model.trait_ident);
    let client_ident = client_struct_ident(&model.trait_ident);
    let trait_path = &model.base_trait;

    let methods = model
        .methods
        .iter()
        .map(|m| generate_resolving_method(m, &client_ident, trait_path, support));

    quote! {
        #[cfg(feature = "directory-rest-client")]
        #[::async_trait::async_trait]
        impl #trait_path for #resolving_ident {
            #(#methods)*
        }
    }
}

/// One delegating method body for the resolving wrapper.
fn generate_resolving_method(
    method: &RestMethodModel,
    client_ident: &syn::Ident,
    trait_path: &syn::Path,
    support: &TokenStream,
) -> TokenStream {
    let method_ident = &method.ident;
    let sig = render_method_signature(method);
    let arg_idents: Vec<&syn::Ident> = method
        .params
        .iter()
        .filter(|p| p.ident != "self")
        .map(|p| &p.ident)
        .collect();

    if method.streaming {
        let item_ty = streaming_item_type(method);
        let err_ty = error_type(method);
        return quote! {
            fn #method_ident #sig {
                use ::futures_util::StreamExt as _;
                let __inner = ::std::sync::Arc::clone(&self.__inner);
                let __fut = async move {
                    let __stream: ::std::pin::Pin<::std::boxed::Box<
                        dyn ::futures_core::Stream<Item = ::std::result::Result<#item_ty, #err_ty>>
                            + ::std::marker::Send + 'static,
                    >> = match __inner.resolved().await {
                        ::std::result::Result::Ok(__c) => {
                            <#client_ident as #trait_path>::#method_ident(&*__c, #(#arg_idents),*)
                        }
                        ::std::result::Result::Err(__e) => ::std::boxed::Box::pin(
                            ::futures_util::stream::once(async move {
                                ::std::result::Result::Err(
                                    <#err_ty as ::std::convert::From<
                                        #support::runtime::transport_error::TransportError,
                                    >>::from(__e),
                                )
                            }),
                        ),
                    };
                    __stream
                };
                ::std::boxed::Box::pin(::futures_util::stream::once(__fut).flatten())
            }
        };
    }

    let err_ty = error_type(method);
    let convert_err = quote! {
        |__e| <#err_ty as ::std::convert::From<#support::runtime::transport_error::TransportError>>::from(__e)
    };
    quote! {
        async fn #method_ident #sig {
            let __c = self.__inner.resolved().await.map_err(#convert_err)?;
            <#client_ident as #trait_path>::#method_ident(&*__c, #(#arg_idents),*).await
        }
    }
}

fn resolving_struct_ident(trait_ident: &syn::Ident) -> syn::Ident {
    format_ident!("{}ResolvingClient", trait_ident)
}

fn generate_client_method(
    method: &RestMethodModel,
    trait_ident: &syn::Ident,
    support: &TokenStream,
) -> TokenStream {
    let trait_snake = to_snake_case(&trait_ident.to_string());
    let binding_fn = format_ident!("{}_http_binding", trait_snake);
    let method_name_str = method.ident.to_string();
    let method_ident = &method.ident;

    let sig = render_method_signature(method);
    let fields_init = build_fields_json(method, support);
    let bearer_capture = capture_bearer_token(method);
    let body_capture = capture_body_param(method);

    if method.streaming {
        return generate_streaming_method_body(
            method,
            &sig,
            &binding_fn,
            &method_name_str,
            &fields_init,
            &bearer_capture,
            support,
        );
    }

    let verb = method.http_method;
    let verb_call = http_verb_call(verb);
    let retry_call = if method.retryable {
        quote! {
            #support::runtime::retry::retry_with_backoff(&self.config.retry, __attempt).await
        }
    } else {
        quote! { __attempt().await }
    };

    // `toolkit-http`'s `.json()` is fallible (returns `Result<RequestBuilder,
    // HttpError>`) — funnel through `with_json_body` which wraps the error
    // into `TransportError::Serialization` so the macro emit path stays
    // uniform. Without `body_capture` the closure threads the builder through
    // unchanged.
    let body_apply = if let Some(body_ident) = &body_capture {
        quote! {
            let __builder = #support::runtime::client::with_json_body(__builder, &#body_ident)?;
        }
    } else {
        quote! {}
    };

    let response_ty = response_type(method);
    let err_ty = error_type(method);
    let convert_err = quote! {
        |__e| <#err_ty as ::std::convert::From<#support::runtime::transport_error::TransportError>>::from(__e)
    };

    quote! {
        async fn #method_ident #sig {
            let __binding = #binding_fn();
            let __m = __binding
                .find_method(#method_name_str)
                .expect(concat!("missing HTTP binding for method '", #method_name_str, "'"));

            #fields_init
            let __fields = __fields_result.map_err(#convert_err)?;
            let __url = #support::runtime::http::build_request_url(
                &self.config.base_url,
                &__binding.base_path,
                __m,
                &__fields,
            )
            .map_err(#convert_err)?;

            #bearer_capture

            let __attempt = || async {
                // `toolkit-http` has no `.bearer_auth()` helper — use the
                // `authorization` header directly.
                let mut __builder = self.http.#verb_call(&__url);
                if let Some(ref __t) = __bearer {
                    __builder = __builder.header(
                        "authorization",
                        &::std::format!("Bearer {}", __t),
                    );
                }
                #body_apply
                let __build_result: ::std::result::Result<
                    ::toolkit_http::RequestBuilder,
                    #support::runtime::transport_error::TransportError,
                > = ::std::result::Result::Ok(__builder);
                #support::runtime::client::send_unary::<_, #response_ty>(|| __build_result).await
            };

            let __result: ::std::result::Result<#response_ty, #support::runtime::transport_error::TransportError> =
                #retry_call;
            __result.map_err(#convert_err)
        }
    }
}

fn generate_streaming_method_body(
    method: &RestMethodModel,
    sig: &TokenStream,
    binding_fn: &syn::Ident,
    method_name: &str,
    fields_init: &TokenStream,
    bearer_capture: &TokenStream,
    support: &TokenStream,
) -> TokenStream {
    let method_ident = &method.ident;
    let item_ty = streaming_item_type(method);
    let err_ty = error_type(method);
    let verb_call = http_verb_call(method.http_method);
    let convert_err = quote! {
        |__e| <#err_ty as ::std::convert::From<#support::runtime::transport_error::TransportError>>::from(__e)
    };

    quote! {
        fn #method_ident #sig {
            use ::futures_util::StreamExt as _;

            let __binding = #binding_fn();
            let __m = __binding
                .find_method(#method_name)
                .expect(concat!("missing HTTP binding for method '", #method_name, "'"))
                .clone();
            let __base_path = __binding.base_path.clone();
            let __base_url = self.config.base_url.clone();
            let __http = self.http.clone();

            #fields_init
            #bearer_capture

            // Bind the convert closure once so we can both call it
            // imperatively (URL-build error path) and pass it to the map_err
            // tail below. Boxed because closures don't impl `Copy`.
            let __convert: ::std::boxed::Box<
                dyn Fn(#support::runtime::transport_error::TransportError) -> #err_ty + Send,
            > = ::std::boxed::Box::new(#convert_err);
            let __fields = match __fields_result {
                Ok(v) => v,
                Err(e) => {
                    let __err = __convert(e);
                    return ::std::boxed::Box::pin(::futures_util::stream::once(async move {
                        ::std::result::Result::Err(__err)
                    }));
                }
            };
            // Compute the URL once; reconnect attempts re-use it.
            let __url_result = #support::runtime::http::build_request_url(
                &__base_url, &__base_path, &__m, &__fields,
            );
            let __url = match __url_result {
                Ok(u) => u,
                Err(e) => {
                    let __err = __convert(e);
                    return ::std::boxed::Box::pin(::futures_util::stream::once(async move {
                        ::std::result::Result::Err(__err)
                    }));
                }
            };
            let __reconnect = self.config.sse_reconnect.clone();
            // Factory: invoked once per attempt with the latest seen
            // `Last-Event-ID`. On the first attempt `last` is `None`.
            // `toolkit-http` has no `.bearer_auth()` helper — use the
            // `authorization` header directly.
            let __factory = move |last: ::std::option::Option<&str>|
                -> ::std::result::Result<
                    ::toolkit_http::RequestBuilder,
                    #support::runtime::transport_error::TransportError,
                >
            {
                let mut __builder = __http.#verb_call(&__url);
                if let Some(ref __t) = __bearer {
                    __builder = __builder.header(
                        "authorization",
                        &::std::format!("Bearer {}", __t),
                    );
                }
                if let Some(__id) = last {
                    __builder = __builder.header("Last-Event-ID", __id);
                }
                ::std::result::Result::Ok(__builder)
            };

            let __timeout = ::std::option::Option::Some(self.config.timeout);
            let __stream = #support::runtime::client::send_streaming::<_, #item_ty>(
                __factory, __reconnect, __timeout,
            );
            ::std::boxed::Box::pin(__stream.map(move |r| r.map_err(|e| __convert(e))))
        }
    }
}

fn render_method_signature(method: &RestMethodModel) -> TokenStream {
    let params = method.params.iter().map(|p| {
        let ident = &p.ident;
        let ty = &p.ty;
        if ident == "self" {
            return quote! { &self };
        }
        quote! { #ident: #ty }
    });

    let return_ty = match &method.result_types {
        Some((ok, err)) if !method.streaming => quote! { -> ::std::result::Result<#ok, #err> },
        _ => streaming_signature_return(method),
    };

    quote! {
        ( &self, #(#params),* ) #return_ty
    }
}

fn streaming_signature_return(method: &RestMethodModel) -> TokenStream {
    // For streaming methods we mirror the original trait return type. The
    // parser recorded it as the function output; we re-emit the same tokens
    // here by re-using the generic stream signature.
    if let Some((ok, err)) = &method.result_types {
        return quote! {
            -> ::std::pin::Pin<::std::boxed::Box<dyn ::futures_core::Stream<Item = ::std::result::Result<#ok, #err>> + ::std::marker::Send + 'static>>
        };
    }
    quote! { -> ::std::pin::Pin<::std::boxed::Box<dyn ::futures_core::Stream<Item = ()> + ::std::marker::Send + 'static>> }
}

fn streaming_item_type(method: &RestMethodModel) -> TokenStream {
    if let Some((ok, _)) = &method.result_types {
        return quote! { #ok };
    }
    quote! { () }
}

fn response_type(method: &RestMethodModel) -> TokenStream {
    if let Some((ok, _)) = &method.result_types {
        return quote! { #ok };
    }
    quote! { () }
}

fn error_type(method: &RestMethodModel) -> TokenStream {
    if let Some((_, err)) = &method.result_types {
        return quote! { #err };
    }
    quote! { () }
}

fn http_verb_call(verb: HttpVerb) -> syn::Ident {
    match verb {
        HttpVerb::Get => format_ident!("get"),
        HttpVerb::Post => format_ident!("post"),
        HttpVerb::Put => format_ident!("put"),
        HttpVerb::Delete => format_ident!("delete"),
    }
}

fn build_fields_json(method: &RestMethodModel, support: &TokenStream) -> TokenStream {
    let entries = method.params.iter().filter_map(|p| {
        if p.ident == "self" {
            return None;
        }
        if type_path_ends_with(&p.ty, "SecurityContext") {
            return None;
        }
        let key = p.ident.to_string();
        let ident = &p.ident;
        Some(quote! {
            __obj.insert(
                #key.to_owned(),
                match ::serde_json::to_value(&#ident) {
                    ::std::result::Result::Ok(__v) => __v,
                    ::std::result::Result::Err(__e) => return ::std::result::Result::Err(
                        #support::runtime::transport_error::TransportError::serialization(__e),
                    ),
                },
            );
        })
    });

    quote! {
        let __fields_result: ::std::result::Result<
            ::serde_json::Value,
            #support::runtime::transport_error::TransportError,
        > = (|| {
            let mut __obj = ::serde_json::Map::new();
            #(#entries)*
            ::std::result::Result::Ok(::serde_json::Value::Object(__obj))
        })();
    }
}

fn capture_bearer_token(method: &RestMethodModel) -> TokenStream {
    let ctx_ident = method.params.iter().find_map(|p| {
        if type_path_ends_with(&p.ty, "SecurityContext") {
            Some(p.ident.clone())
        } else {
            None
        }
    });

    if let Some(ident) = ctx_ident {
        quote! {
            let __bearer: ::std::option::Option<::std::string::String> = #ident
                .bearer_token()
                .map(|__t| {
                    use ::secrecy::ExposeSecret as _;
                    __t.expose_secret().to_owned()
                });
        }
    } else {
        quote! {
            let __bearer: ::std::option::Option<::std::string::String> = ::std::option::Option::None;
        }
    }
}

fn capture_body_param(method: &RestMethodModel) -> Option<syn::Ident> {
    if !method.http_method.allows_body() {
        return None;
    }
    let path_params = extract_path_param_names(&method.path_template);
    method
        .params
        .iter()
        .find(|p| {
            if p.ident == "self" {
                return false;
            }
            if type_path_ends_with(&p.ty, "SecurityContext") {
                return false;
            }
            !path_params.iter().any(|pp| p.ident == pp)
        })
        .map(|p| p.ident.clone())
}

fn generate_server_registration(model: &RestContractModel) -> TokenStream {
    let fn_name = format_ident!("register_{}_routes", to_snake_case(&model.trait_ident.to_string()));
    let base_trait = &model.base_trait;
    let doc = format!(
        "Register the macro-generated REST routes for [`{}`] on the given router.\n\n\
         Methods marked `#[server_manual]` are SKIPPED — register them by hand via \
         `OperationBuilder` on the returned router. This function is additive and \
         composable: the returned router can be chained into further manual \
         `OperationBuilder::verb(..).register(router, openapi)` calls.",
        model.trait_ident
    );

    // Methods marked `#[server_manual]` are excluded from generation so the
    // author can register them by hand. They remain in the client + IR.
    let method_routes = model
        .methods
        .iter()
        .filter(|method| !method.server_manual)
        .map(|method| generate_method_route(method, model));

    quote! {
        #[cfg(feature = "rest-server")]
        #[doc = #doc]
        pub fn #fn_name(
            mut router: ::axum::Router,
            openapi: &dyn ::toolkit::api::OpenApiRegistry,
            svc: ::std::sync::Arc<dyn #base_trait>,
        ) -> ::axum::Router {
            #(#method_routes)*
            router
        }
    }
}

fn generate_method_route(method: &RestMethodModel, model: &RestContractModel) -> TokenStream {
    // Streaming server-side codegen (SSE) is not yet implemented. Such methods
    // must opt out with `#[server_manual]` and be registered by hand via
    // `OperationBuilder`. (server_manual methods are filtered out before
    // reaching this function, so a streaming method here is an un-opted-out one.)
    if method.streaming {
        let ident = &method.ident;
        let msg = format!(
            "rest_contract: streaming method `{ident}` cannot be auto-registered on the server yet. \
             Mark it `#[server_manual]` and register it by hand via OperationBuilder."
        );
        return quote! { ::std::compile_error!(#msg); };
    }

    let base_path = &model.base_path;
    let method_name = &method.ident;
    let path = &method.path_template;
    let full_path = format!("{base_path}{path}");
    let operation_id = format!(
        "{}_{method_name}",
        to_snake_case(&model.trait_ident.to_string()),
    );

    let http_verb_method = match method.http_method {
        HttpVerb::Get => quote! { get },
        HttpVerb::Post => quote! { post },
        HttpVerb::Put => quote! { put },
        HttpVerb::Delete => quote! { delete },
    };

    let path_param_names = extract_path_param_names(&method.path_template);

    // Detect the request-body parameter type (for OpenAPI request schema).
    let body_ty = body_param_type(method, &path_param_names);
    let request_registration = match body_ty {
        Some(ty) => quote! { .json_request::<#ty>(openapi, "") },
        None => quote! {},
    };

    let handler = generate_unary_handler(method, &path_param_names);

    // Response schema: derive from the `Ok` type of `Result<Ok, Err>`.
    let response_registration = if let Some((ok_ty, _)) = &method.result_types {
        quote! {
            .json_response_with_schema::<#ok_ty>(openapi, ::axum::http::StatusCode::OK, "")
        }
    } else {
        quote! {
            .json_response(::axum::http::StatusCode::OK, "")
        }
    };

    quote! {
        router = ::toolkit::api::OperationBuilder::#http_verb_method(#full_path)
            .operation_id(#operation_id)
            .authenticated()
            .no_license_required()
            #request_registration
            .handler(#handler)
            #response_registration
            .standard_errors(openapi)
            .register(router, openapi);
    }
}

/// Returns the type of the request-body parameter for body-carrying verbs
/// (POST/PUT): the first parameter that is not `self`, not `SecurityContext`,
/// and not a path parameter. `None` for GET/DELETE or bodyless methods.
fn body_param_type(method: &RestMethodModel, path_param_names: &[String]) -> Option<Type> {
    if !method.http_method.allows_body() {
        return None;
    }
    for param in &method.params {
        let name = param.ident.to_string();
        if name == "self" || type_path_ends_with(&param.ty, "SecurityContext") {
            continue;
        }
        if path_param_names.contains(&name) {
            continue;
        }
        return Some(param.ty.clone());
    }
    None
}

fn generate_unary_handler(method: &RestMethodModel, path_param_names: &[String]) -> TokenStream {
    let method_ident = &method.ident;

    // Build parameter extractors and the corresponding service-call arguments.
    // SecurityContext is always first (populated by gateway auth middleware
    // into Axum extensions), then path params, then body, then query params.
    let mut extractors = vec![quote! {
        ::axum::Extension(ctx): ::axum::Extension<::toolkit_security::SecurityContext>
    }];
    let mut call_args = vec![quote! { ctx }];
    let mut body_taken = false;

    for param in &method.params {
        let param_name = &param.ident;
        if param_name == "self" || type_path_ends_with(&param.ty, "SecurityContext") {
            continue;
        }
        let param_ty = &param.ty;

        if path_param_names.contains(&param_name.to_string()) {
            extractors.push(quote! {
                ::axum::extract::Path(#param_name): ::axum::extract::Path<#param_ty>
            });
        } else if method.http_method.allows_body() && !body_taken {
            extractors.push(quote! {
                ::axum::Json(#param_name): ::axum::Json<#param_ty>
            });
            body_taken = true;
        } else {
            extractors.push(quote! {
                ::axum::extract::Query(#param_name): ::axum::extract::Query<#param_ty>
            });
        }
        call_args.push(quote! { #param_name });
    }

    let extractor_list = quote! { #(#extractors),* };

    // The handler mirrors the hand-written pattern: call the domain method and
    // wrap the `Ok` value in `Json`. The error type (`CanonicalError` in the
    // common case) implements `IntoResponse`, so `?`/`map`-style propagation
    // renders the RFC 9457 `Problem` envelope at the framework boundary.
    quote! {
        {
            let svc = ::std::sync::Arc::clone(&svc);
            move |#extractor_list| {
                let svc = ::std::sync::Arc::clone(&svc);
                async move {
                    svc.#method_ident(#(#call_args),*).await.map(::axum::Json)
                }
            }
        }
    }
}

fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
