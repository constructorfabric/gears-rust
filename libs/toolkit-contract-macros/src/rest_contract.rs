//! Code generation for `#[toolkit::rest_contract]`.
//!
//! Emits two artifacts:
//! 1. The original projection trait, with HTTP/marker attributes stripped from
//!    every method so it compiles unchanged outside the macro.
//! 2. A free function `<trait_snake_case>_http_binding() -> HttpBindingIr`
//!    that materializes the binding IR derived from the trait declaration.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Ident, TraitItem, Type};

use crate::projection::{
    build_delegation_body, client_struct_ident, generate_projection_impl_for_client,
    is_security_context_type, rewrite_streaming_signature, strip_method_attrs, type_path_ends_with,
};
use crate::rest_contract_parse::{HttpVerb, RestContractModel, RestMethodModel, RestParam};
use crate::support::contract_support_path;

const HTTP_ATTRS: &[&str] = &[
    "get",
    "post",
    "put",
    "patch",
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
    let coverage_check = generate_full_coverage_check(model, &support);

    quote! {
        #cleaned_trait
        #binding_fn
        #client_struct
        #client_impl
        #resolving_struct
        #resolving_impl
        #projection_impl
        #server_registration
        #coverage_check
    }
}

/// When `require_full_coverage` (ADR-0003) is set, emit a generated test that
/// asserts the base contract and the REST projection cover exactly the same
/// method set (both directions), naming any offending method.
///
/// This runs under `cargo test`, is feature-independent (does not need
/// `rest-client`), and reuses the runtime coverage validator
/// [`validate_http_binding`]. It requires the base trait to be
/// `#[toolkit::contract]`-annotated (so it implements `Contract`). A purely
/// compile-time, feature-independent, method-named check is not achievable from
/// the projection macro alone, because it cannot see the base trait's method
/// set at macro-expansion time (the base lives behind an imported path); the
/// signature / no-extra-method direction is already enforced feature-
/// independently by the delegating default methods, and the missing-method
/// direction is also caught by the generated client `impl` (E0046) under
/// `rest-client`.
fn generate_full_coverage_check(model: &RestContractModel, support: &TokenStream) -> TokenStream {
    if !model.require_full_coverage {
        return TokenStream::new();
    }
    let base_trait = &model.base_trait;
    let trait_snake = to_snake_case(&model.trait_ident.to_string());
    let binding_fn = format_ident!("{}_http_binding", trait_snake);
    let test_fn = format_ident!("__{}_require_full_coverage", trait_snake);
    quote! {
        #[cfg(test)]
        #[test]
        fn #test_fn() {
            let __ir = <dyn #base_trait as #support::Contract>::contract_ir();
            let __binding = #binding_fn();
            if let ::std::result::Result::Err(__errs) =
                #support::ir::validate_http_binding(&__ir, &__binding)
            {
                ::std::panic!(
                    "require_full_coverage: base/projection method-set mismatch: {:?}",
                    __errs
                );
            }
        }
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

    let field_bindings = build_field_bindings(method, support);

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

/// Where a method parameter is carried in the HTTP request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldClass {
    Path,
    Body,
    Query,
}

/// Classify each non-`self`, non-security-context parameter of a method into its
/// HTTP carrier. This is the **single source of truth** for path/body/query
/// binding: the client IR ([`build_field_bindings`]) and the server route
/// generator ([`body_param_type`], [`generate_unary_handler`]) both consume it,
/// so they can never disagree on the wire shape (ADR-0003 single-IR intent).
///
/// Rule: a param whose name matches a `{param}` in the path template is `Path`;
/// otherwise the first remaining param on a body-carrying verb (POST/PUT) is the
/// `Body`; all others are `Query`.
fn classify_params(method: &RestMethodModel) -> Vec<(&RestParam, FieldClass)> {
    let path_params = extract_path_param_names(&method.path_template);
    let mut out = Vec::new();
    let mut body_assigned = false;
    for param in &method.params {
        if param.ident == "self" || is_security_context_type(&param.ty) {
            continue;
        }
        let name = param.ident.to_string();
        let class = if path_params.iter().any(|p| p == &name) {
            FieldClass::Path
        } else if method.http_method.allows_body() && !body_assigned {
            body_assigned = true;
            FieldClass::Body
        } else {
            FieldClass::Query
        };
        out.push((param, class));
    }
    out
}

fn build_field_bindings(method: &RestMethodModel, support: &TokenStream) -> Vec<TokenStream> {
    classify_params(method)
        .into_iter()
        .map(|(param, class)| {
            let name = param.ident.to_string();
            match class {
                FieldClass::Path => quote! {
                    #support::ir::binding::HttpFieldBinding::Path {
                        field: #name.to_owned(),
                        param: #name.to_owned(),
                    }
                },
                FieldClass::Body => {
                    quote! { #support::ir::binding::HttpFieldBinding::Body }
                }
                FieldClass::Query => quote! {
                    #support::ir::binding::HttpFieldBinding::Query {
                        field: #name.to_owned(),
                        param: #name.to_owned(),
                    }
                },
            }
        })
        .collect()
}

/// Strip leading `&`/`&mut` from a type, returning the owned inner type. Axum
/// extension extractors own their value, so the server handler extracts the
/// owned context type even when the trait method takes it by reference.
fn strip_reference(ty: &Type) -> &Type {
    match ty {
        Type::Reference(r) => strip_reference(&r.elem),
        other => other,
    }
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
    // `client_type` label for the (otel-gated) `toolkit-http` client metrics.
    let metrics_label = model.trait_ident.to_string();

    quote! {
        #[cfg(feature = "rest-client")]
        #[doc = #doc]
        pub struct #client_ident {
            http: ::toolkit_http::HttpClient,
            config: #support::runtime::config::ClientConfig,
        }

        #[cfg(feature = "rest-client")]
        impl #client_ident {
            /// Build a new client with the default `toolkit-http` HTTP client.
            ///
            /// The transport is built by
            /// [`build_default_http_client`](#support::runtime::client::build_default_http_client):
            /// transport-layer retry disabled (the SDK retries itself), plaintext
            /// `http://` allowed unless `config.require_tls` is set, and — when
            /// the `otel` feature is enabled — W3C `traceparent` propagation plus
            /// RED client metrics labeled with this projection trait name. For
            /// caller-controlled HTTP client construction use
            /// [`Self::with_http_client`].
            ///
            /// Fallible because the underlying `toolkit-http` builder can fail
            /// under non-default cryptographic backends (FIPS, custom TLS).
            ///
            /// # Errors
            /// Returns whatever `toolkit_http::HttpClient::builder().build()` returned.
            pub fn new(
                config: #support::runtime::config::ClientConfig,
            ) -> ::std::result::Result<Self, ::toolkit_http::HttpError> {
                let http = #support::runtime::client::build_default_http_client(
                    #metrics_label,
                    config.require_tls,
                )?;
                Ok(Self { http, config })
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
fn generate_resolving_client_struct(
    model: &RestContractModel,
    support: &TokenStream,
) -> TokenStream {
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

/// Build the `info_span!(...)` expression emitted inside every generated
/// client method. The span name and all attribute values are baked as string
/// literals at macro-expansion time. `otel.kind = "client"` makes this the
/// parent of the `toolkit-http` `outgoing_http` span, so W3C `traceparent`
/// (`trace_id` / `span_id`) propagates downstream with no manual threading.
///
/// Routed through `#support::__tracing` (a `toolkit-contract` re-export) so SDK
/// crates need no direct `tracing` dependency.
fn client_span_ctor(
    service: &str,
    method_name: &str,
    method: &RestMethodModel,
    support: &TokenStream,
) -> TokenStream {
    let span_name = format!("{service}.{method_name}");
    let http_method_str = method.http_method.ir_variant().to_uppercase();
    let route_str = method.path_template.clone();
    quote! {
        #support::__tracing::info_span!(
            #span_name,
            otel.kind = "client",
            rpc.system = "rest",
            rpc.service = #service,
            rpc.method = #method_name,
            http.method = #http_method_str,
            http.route = #route_str,
            error = #support::__tracing::field::Empty,
        )
    }
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

    // Per-method client span (baked-in telemetry) — see `client_span_ctor`.
    let span_ctor = client_span_ctor(&trait_ident.to_string(), &method_name_str, method, support);

    if method.streaming {
        return generate_streaming_method_body(
            method,
            &sig,
            &binding_fn,
            &method_name_str,
            &fields_init,
            &bearer_capture,
            &span_ctor,
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
            let __span = #span_ctor;
            // Enter the span across the awaited dispatch so `toolkit-http`'s
            // OtelLayer sees it as `Context::current()` and injects this span's
            // W3C `traceparent` on the outbound request.
            #support::__tracing::Instrument::instrument(async move {
                let __binding = #binding_fn();
                // The binding is generated from the same trait model, so this is
                // unreachable in practice — but return a typed error rather than
                // panicking inside generated library code (no panic paths).
                let __m = match __binding.find_method(#method_name_str) {
                    ::std::option::Option::Some(__m) => __m,
                    ::std::option::Option::None => {
                        return ::std::result::Result::Err((#convert_err)(
                            #support::runtime::transport_error::TransportError::UrlBuild(
                                concat!(
                                    "missing HTTP binding for method '",
                                    #method_name_str,
                                    "'",
                                )
                                .to_owned(),
                            ),
                        ));
                    }
                };

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
                    // Tenant bearer forwarded via a sensitive `Authorization`
                    // header (never logged); see `RequestBuilder::bearer_auth`.
                    let mut __builder = self.http.#verb_call(&__url);
                    if let Some(ref __t) = __bearer {
                        __builder = __builder.bearer_auth(__t);
                    }
                    #body_apply
                    let __build_result: ::std::result::Result<
                        ::toolkit_http::RequestBuilder,
                        #support::runtime::transport_error::TransportError,
                    > = ::std::result::Result::Ok(__builder);
                    // Per-attempt deadline from `ClientConfig::timeout` (mirrors
                    // the streaming path); elapse → transient `Timeout`.
                    #support::runtime::client::send_unary::<_, #response_ty>(
                        || __build_result,
                        ::std::option::Option::Some(self.config.timeout),
                    ).await
                };

                let __result: ::std::result::Result<#response_ty, #support::runtime::transport_error::TransportError> =
                    #retry_call;
                if __result.is_err() {
                    #support::__tracing::Span::current().record("error", true);
                }
                __result.map_err(#convert_err)
            }, __span).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_streaming_method_body(
    method: &RestMethodModel,
    sig: &TokenStream,
    binding_fn: &syn::Ident,
    method_name: &str,
    fields_init: &TokenStream,
    bearer_capture: &TokenStream,
    span_ctor: &TokenStream,
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

            // Per-method client span (baked-in telemetry). Entered per yielded
            // item so SSE event processing runs under the contract span; the
            // per-attempt HTTP send is traced by `toolkit-http`'s OtelLayer.
            let __span = #span_ctor;

            let __binding = #binding_fn();
            // The binding is generated from the same trait model, so this is
            // unreachable in practice — but return a typed error rather than
            // panicking inside generated library code (mirrors the unary path).
            let __m = match __binding.find_method(#method_name) {
                ::std::option::Option::Some(__m) => __m.clone(),
                ::std::option::Option::None => {
                    let __err = (#convert_err)(
                        #support::runtime::transport_error::TransportError::UrlBuild(
                            concat!(
                                "missing HTTP binding for method '",
                                #method_name,
                                "'",
                            )
                            .to_owned(),
                        ),
                    );
                    return ::std::boxed::Box::pin(::futures_util::stream::once(async move {
                        ::std::result::Result::Err(__err)
                    }));
                }
            };
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
            let __factory = move |last: ::std::option::Option<&str>|
                -> ::std::result::Result<
                    ::toolkit_http::RequestBuilder,
                    #support::runtime::transport_error::TransportError,
                >
            {
                // SSE clients MUST advertise the event-stream media type
                // (PRD §5.6) so content-negotiating servers/gateways return the
                // stream rather than JSON.
                let mut __builder = __http
                    .#verb_call(&__url)
                    .header("accept", "text/event-stream");
                // Tenant bearer via a sensitive `Authorization` header.
                if let Some(ref __t) = __bearer {
                    __builder = __builder.bearer_auth(__t);
                }
                if let Some(__id) = last {
                    __builder = __builder.header("Last-Event-ID", __id);
                }
                ::std::result::Result::Ok(__builder)
            };

            // SSE uses the per-event idle deadline (NOT the unary `timeout`),
            // so a healthy slow stream is not killed between events.
            let __timeout = ::std::option::Option::Some(self.config.sse_idle_timeout);
            let __stream = #support::runtime::client::send_streaming::<_, #item_ty>(
                __factory, __reconnect, __timeout,
            );
            ::std::boxed::Box::pin(__stream.map(move |r| {
                let __enter = __span.enter();
                let __mapped = r.map_err(|e| __convert(e));
                if __mapped.is_err() {
                    #support::__tracing::Span::current().record("error", true);
                }
                ::std::mem::drop(__enter);
                __mapped
            }))
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
        HttpVerb::Patch => format_ident!("patch"),
        HttpVerb::Delete => format_ident!("delete"),
    }
}

fn build_fields_json(method: &RestMethodModel, support: &TokenStream) -> TokenStream {
    // Only path/query fields go into `__fields` (consumed by URL building); the
    // body param is applied separately via `with_json_body`, so excluding it
    // here avoids serializing it twice. Uses the shared `classify_params`.
    let entries = classify_params(method)
        .into_iter()
        .filter_map(|(p, class)| {
            if class == FieldClass::Body {
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
            if is_security_context_type(&p.ty) {
                return false;
            }
            !path_params.iter().any(|pp| p.ident == pp)
        })
        .map(|p| p.ident.clone())
}

fn generate_server_registration(model: &RestContractModel) -> TokenStream {
    let fn_name = format_ident!(
        "register_{}_routes",
        to_snake_case(&model.trait_ident.to_string())
    );
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
        HttpVerb::Patch => quote! { patch },
        HttpVerb::Delete => quote! { delete },
    };

    // axum permits a single `Query` extractor per handler, so >1 query-classified
    // param cannot be aggregated automatically (their fields would have to be
    // merged into one type). Reject it loudly here — a silent extractor mismatch
    // (server decodes only one, client sends several) is far worse. Authors
    // combine query params into one flat struct (ADR-0002 flat-struct form).
    let query_count = classify_params(method)
        .into_iter()
        .filter(|(_, class)| *class == FieldClass::Query)
        .count();
    if query_count > 1 {
        let msg = format!(
            "rest_contract: method `{method_name}` has {query_count} query parameters; the \
             generated server route supports at most one (axum allows a single `Query` \
             extractor). Combine them into one flat struct query parameter, or mark the \
             method `#[server_manual]` and register it by hand."
        );
        return quote! { ::std::compile_error!(#msg); };
    }

    // OpenAPI path parameters — one per `{param}` in the template.
    let path_param_registrations = classify_params(method)
        .into_iter()
        .filter(|(_, class)| *class == FieldClass::Path)
        .map(|(param, _)| {
            let name = param.ident.to_string();
            quote! { .path_param(#name, "") }
        });

    // OpenAPI query parameter — registered only when the (single, per the
    // `query_count > 1` guard above) query param is a scalar type, so its name
    // and required-ness in the spec genuinely match the wire. A flat-struct
    // query param's individual fields aren't visible at macro-expansion time
    // (the macro doesn't parse the struct definition), so registering it under
    // the Rust parameter's own name would document a parameter shape the
    // client never sends — worse than omitting it. That case remains a known,
    // documented gap; authors needing an accurate spec use `#[server_manual]`.
    let query_param_registration = classify_params(method)
        .into_iter()
        .find(|(_, class)| *class == FieldClass::Query)
        .and_then(|(param, _)| {
            let (openapi_ty, required) = scalar_openapi_type(&param.ty)?;
            let name = param.ident.to_string();
            Some(quote! { .query_param_typed(#name, #required, "", #openapi_ty) })
        })
        .unwrap_or_default();

    // Request-body param type (for OpenAPI request schema) — from the shared
    // classifier, so it matches the client IR and the handler below.
    let body_ty = body_param_type(method);
    let request_registration = match body_ty {
        Some(ty) => quote! { .json_request::<#ty>(openapi, "") },
        None => quote! {},
    };

    let handler = generate_unary_handler(method);

    // Response schema: derive from the `Ok` type of `Result<Ok, Err>`. A unit
    // `Ok` type (`Result<(), E>`) has no schema and is not a `ResponseApiDto`,
    // so it uses the schema-less `.json_response(..)` — matching the generated
    // handler, which wraps the unit value in `Json(())` (200 + `null` body).
    let response_registration = match &method.result_types {
        Some((ok_ty, _)) if !is_unit_type(ok_ty) => quote! {
            .json_response_with_schema::<#ok_ty>(openapi, ::axum::http::StatusCode::OK, "")
        },
        _ => quote! {
            .json_response(::axum::http::StatusCode::OK, "")
        },
    };

    quote! {
        router = ::toolkit::api::OperationBuilder::#http_verb_method(#full_path)
            .operation_id(#operation_id)
            .authenticated()
            .no_license_required()
            #(#path_param_registrations)*
            #query_param_registration
            #request_registration
            .handler(#handler)
            #response_registration
            .standard_errors(openapi)
            .register(router, openapi);
    }
}

/// Recognizes a "scalar-like" Rust type usable as a single `OpenAPI` query
/// parameter — `String`/`&str`/numeric/`bool`, or `Option<...>` of one of
/// those (peeled once). Returns `(openapi_type, required)`. Returns `None` for
/// anything else (flat-struct query params), since the macro cannot see a
/// struct's field definitions at expansion time and registering it under the
/// Rust parameter's own name would document a parameter shape that does not
/// match what actually goes on the wire.
fn scalar_openapi_type(ty: &Type) -> Option<(&'static str, bool)> {
    fn leaf_ident(ty: &Type) -> Option<&syn::Ident> {
        match ty {
            Type::Reference(r) => leaf_ident(&r.elem),
            Type::Path(p) => p.path.segments.last().map(|s| &s.ident),
            _ => None,
        }
    }
    fn openapi_type_for(ident: &syn::Ident) -> Option<&'static str> {
        match ident.to_string().as_str() {
            "String" | "str" => Some("string"),
            "bool" => Some("boolean"),
            "f32" | "f64" => Some("number"),
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
            | "u128" | "usize" => Some("integer"),
            _ => None,
        }
    }

    // Peel one layer of `Option<...>` (by reference, so `&Option<T>` also
    // works) to determine both the leaf type and required-ness.
    if let Type::Path(p) = strip_reference(ty)
        && let Some(seg) = p.path.segments.last()
        && seg.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return leaf_ident(inner)
            .and_then(openapi_type_for)
            .map(|t| (t, false));
    }

    leaf_ident(ty).and_then(openapi_type_for).map(|t| (t, true))
}

/// True for the unit type `()`. Server response codegen uses this to pick the
/// schema-less `.json_response(..)` (the unit type is not a `ResponseApiDto`).
fn is_unit_type(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

/// Returns the type of the request-body parameter (the `Body`-classified param),
/// via the shared [`classify_params`]. `None` for GET/DELETE or bodyless methods.
fn body_param_type(method: &RestMethodModel) -> Option<Type> {
    classify_params(method)
        .into_iter()
        .find(|(_, class)| *class == FieldClass::Body)
        .map(|(param, _)| param.ty.clone())
}

fn generate_unary_handler(method: &RestMethodModel) -> TokenStream {
    let method_ident = &method.ident;

    // The security-plane context is always first (populated by gateway auth
    // middleware into Axum extensions). Drive the extractor's type from the
    // actual context parameter (`SecurityContext` / `PlatformSecurityContext`,
    // value or reference) rather than hardcoding it, and forward it to the
    // service call by value or by reference to match the trait signature.
    let ctx_param = method
        .params
        .iter()
        .find(|p| p.ident != "self" && is_security_context_type(&p.ty));

    let (ctx_extractor, ctx_call_arg) = match ctx_param {
        Some(p) => {
            let owned_ty = strip_reference(&p.ty);
            let is_ref = matches!(&p.ty, Type::Reference(_));
            let call = if is_ref {
                quote! { &ctx }
            } else {
                quote! { ctx }
            };
            (
                quote! { ::axum::Extension(ctx): ::axum::Extension<#owned_ty> },
                call,
            )
        }
        // Enforced upstream (`parse_method` rejects a remote method without a
        // plane context), but keep a sane fallback for robustness.
        None => (
            quote! { ::axum::Extension(ctx): ::axum::Extension<::toolkit_security::SecurityContext> },
            quote! { ctx },
        ),
    };

    // Collect params by carrier, preserving the trait's declaration order for
    // `call_args` (the service is invoked as `svc.method(ctx, a, b, ..)`).
    // Classification is shared with the client IR via `classify_params`.
    let mut call_args = vec![ctx_call_arg];
    let mut path_idents: Vec<&Ident> = Vec::new();
    let mut path_tys: Vec<&Type> = Vec::new();
    let mut query_extractor: Option<TokenStream> = None;
    let mut body_extractor: Option<TokenStream> = None;

    for (param, class) in classify_params(method) {
        let param_name = &param.ident;
        let param_ty = &param.ty;
        match class {
            FieldClass::Path => {
                path_idents.push(param_name);
                path_tys.push(param_ty);
            }
            FieldClass::Body => {
                body_extractor = Some(quote! {
                    ::axum::Json(#param_name): ::axum::Json<#param_ty>
                });
            }
            // At most one query param reaches here — `generate_method_route`
            // emits a `compile_error!` for >1 (axum allows a single `Query`
            // extractor; multiple scalar query params must be combined into one
            // flat struct). A single flat-struct/scalar query param works.
            FieldClass::Query => {
                query_extractor = Some(quote! {
                    ::axum::extract::Query(#param_name): ::axum::extract::Query<#param_ty>
                });
            }
        }
        call_args.push(quote! { #param_name });
    }

    // Aggregate path params into a SINGLE extractor: axum deserializes ALL path
    // segments through one `Path<_>`, so multiple path params must be one
    // `Path<(T0, T1, ..)>` tuple (individual `Path<T>` extractors each try to
    // decode the whole tuple and fail at runtime — the bug this fixes).
    let path_extractor: Option<TokenStream> = match path_idents.len() {
        0 => None,
        1 => {
            let i = path_idents[0];
            let t = path_tys[0];
            Some(quote! { ::axum::extract::Path(#i): ::axum::extract::Path<#t> })
        }
        _ => Some(quote! {
            ::axum::extract::Path((#(#path_idents),*)): ::axum::extract::Path<(#(#path_tys),*)>
        }),
    };

    // Order: ctx (Extension) → path → query → body. The body-consuming `Json`
    // extractor MUST be last (axum requires the single `FromRequest` extractor to
    // be the final handler argument); `Extension`/`Path`/`Query` are
    // `FromRequestParts` and may precede it in any order.
    let mut extractors = vec![ctx_extractor];
    if let Some(p) = path_extractor {
        extractors.push(p);
    }
    if let Some(q) = query_extractor {
        extractors.push(q);
    }
    if let Some(body) = body_extractor {
        extractors.push(body);
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

/// Snake-case a trait ident. Delegates to `heck::ToSnakeCase` — the SAME
/// converter used by `#[toolkit::provides]` (`provides.rs`) and `codegen.rs` —
/// so the generated `<trait_snake>_http_binding` symbol name always matches the
/// name `provides` reconstructs, including for acronym/adjacent-capital trait
/// names (e.g. `HttpGatewayApi` → `http_gateway_api`, not `h_t_t_p_...`). A
/// hand-rolled converter here previously diverged on such names and broke the
/// `provides`↔REST wiring with an `E0425 cannot find function`.
fn to_snake_case(s: &str) -> String {
    use heck::ToSnakeCase as _;
    s.to_snake_case()
}

#[cfg(test)]
mod scalar_openapi_type_tests {
    use super::scalar_openapi_type;
    use syn::parse_quote;

    #[test]
    fn recognizes_scalar_types_as_required() {
        assert_eq!(
            scalar_openapi_type(&parse_quote!(String)),
            Some(("string", true))
        );
        assert_eq!(
            scalar_openapi_type(&parse_quote!(&str)),
            Some(("string", true))
        );
        assert_eq!(
            scalar_openapi_type(&parse_quote!(bool)),
            Some(("boolean", true))
        );
        assert_eq!(
            scalar_openapi_type(&parse_quote!(u64)),
            Some(("integer", true))
        );
        assert_eq!(
            scalar_openapi_type(&parse_quote!(i32)),
            Some(("integer", true))
        );
        assert_eq!(
            scalar_openapi_type(&parse_quote!(f64)),
            Some(("number", true))
        );
    }

    #[test]
    fn recognizes_optional_scalars_as_not_required() {
        assert_eq!(
            scalar_openapi_type(&parse_quote!(Option<String>)),
            Some(("string", false))
        );
        assert_eq!(
            scalar_openapi_type(&parse_quote!(Option<u32>)),
            Some(("integer", false))
        );
    }

    #[test]
    fn rejects_struct_types() {
        // A flat-struct query param's fields aren't visible at macro-expansion
        // time, so it must NOT be registered as a scalar (that would document a
        // wrong parameter shape) — see the `#11` fix rationale.
        assert_eq!(scalar_openapi_type(&parse_quote!(ListPaymentsFilter)), None);
        assert_eq!(
            scalar_openapi_type(&parse_quote!(Option<ListPaymentsFilter>)),
            None
        );
    }
}
