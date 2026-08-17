//! Code generation for `#[toolkit::grpc_contract]`.
//!
//! Emits four artifacts from a projection trait:
//! 1. The cleaned projection trait, with grpc/marker attributes stripped and
//!    every method given a `default` body that delegates to the base trait
//!    via fully-qualified syntax (PRD #1536 D3).
//! 2. A free function `<trait_snake>_grpc_binding() -> GrpcBindingIr` that
//!    materializes the gRPC binding metadata.
//! 3. A `{Trait}Client` struct (gated on the user's `grpc-client` feature)
//!    that wraps the tonic-generated `<Service>Client<Channel>`.
//! 4. An `impl <BaseTrait> for {Trait}Client` that calls the tonic stub via
//!    user-provided `From`/`Into` conversions between SDK DTOs and
//!    prost-generated message types.
//!
//! See `rest_contract.rs` for the parallel REST pipeline.

use heck::{ToSnakeCase as _, ToUpperCamelCase as _};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{TraitItem, Type};

use crate::grpc_contract_parse::{GrpcContractModel, GrpcIdempotency, GrpcMethodModel, GrpcParam};
use crate::projection::{
    build_delegation_body, client_struct_ident, generate_projection_impl_for_client,
    is_platform_security_context_type, is_security_context_type, render_method_inputs,
    render_method_return_ty, rewrite_streaming_signature, strip_method_attrs, strip_param_attrs,
};
use crate::support::contract_support_path;

const GRPC_ATTRS: &[&str] = &[
    "rpc",
    "idempotency_level",
    "streaming",
    "retryable",
    "optional",
];

pub fn generate(model: &GrpcContractModel) -> TokenStream {
    let support = contract_support_path();
    let cleaned_trait = generate_cleaned_trait(model);
    let binding_fn = generate_binding_fn(model, &support);
    let repr_guards = generate_repr_guards(model, &support);
    let synthesized_request_bridges = generate_synthesized_request_from_impls(model);
    let client_struct = generate_client_struct(model, &support);
    let client_impl = generate_client_impl(model, &support);
    let projection_impl = generate_projection_impl(model);

    quote! {
        #cleaned_trait
        #binding_fn
        #repr_guards
        #synthesized_request_bridges
        #client_struct
        #client_impl
        #projection_impl
    }
}

/// Auto-emit `impl From<#param_ty> for #stubs::#RequestType` for methods
/// whose only wire parameter (after filtering out `self` and the security context)
/// is a proto-direct primitive type (`String`, `bool`, fixed-width int/float).
/// Mirrors `toolkit-contract-protogen`'s synthesized-request convention so
/// authors don't have to hand-write trivial wrapper bridges.
fn generate_synthesized_request_from_impls(model: &GrpcContractModel) -> TokenStream {
    let stubs = &model.stubs_module;
    let impls: Vec<TokenStream> = model
        .methods
        .iter()
        .filter_map(|method| {
            let wire_params: Vec<&GrpcParam> = method
                .params
                .iter()
                .filter(|p| p.ident != "self" && !is_security_context_type(&p.ty))
                .collect();
            // Exactly one wire param of a proto-direct primitive type.
            let [param] = wire_params.as_slice() else {
                return None;
            };
            if !is_proto_direct_primitive(&param.ty) {
                return None;
            }
            let request_ty_ident =
                format_ident!("{}Request", method.ident.to_string().to_upper_camel_case());
            let field_ident = &param.ident;
            let param_ty = &param.ty;
            Some(quote! {
                #[cfg(feature = "grpc-client")]
                #[automatically_derived]
                impl ::std::convert::From<#param_ty> for #stubs::#request_ty_ident {
                    fn from(value: #param_ty) -> Self {
                        Self { #field_ident: value }
                    }
                }
            })
        })
        .collect();
    if impls.is_empty() {
        quote! {}
    } else {
        quote! { #(#impls)* }
    }
}

/// Returns `true` if `ty` is a Rust primitive that maps 1:1 onto a proto3
/// scalar of the same shape — i.e. the synthesized `<MethodName>Request {
/// field: T }` constructor needs no transformation. Limited to types whose
/// `From<DTO> for ProtoStub` impl is `Self { field: dto }` verbatim.
fn is_proto_direct_primitive(ty: &Type) -> bool {
    if let Type::Path(p) = ty
        && let Some(last) = p.path.segments.last()
    {
        let name = last.ident.to_string();
        matches!(
            name.as_str(),
            "String" | "bool" | "i32" | "i64" | "u32" | "u64" | "f32" | "f64"
        )
    } else {
        false
    }
}

/// Emit `const _: () = { … };` blocks that statically assert every method
/// parameter type and the `Ok` half of every `Result<T, E>` return type
/// implements [`toolkit::GrpcRepr`]. The guard fires at trait-definition time
/// — independent of any feature gate — so unsupportable types are caught
/// before users ever try to enable the `grpc-client` feature.
///
/// In addition, when the `grpc-client` feature is enabled, emits a
/// `SecurityContextMarker` assertion for every parameter detected as a
/// security context — so accidentally naming a wire DTO `SecurityContext`
/// (without implementing the marker) fails to compile.
fn generate_repr_guards(model: &GrpcContractModel, support: &TokenStream) -> TokenStream {
    let mut asserts = Vec::new();
    let mut secctx_asserts: Vec<TokenStream> = Vec::new();
    let mut seen_secctx_keys: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for method in &model.methods {
        // Parameters: skip `self`. Security-context arguments don't travel on
        // the wire but still need a static assertion that the detected type
        // really impls `SecurityContextMarker`. Both planes are recognised —
        // the tenant `SecurityContext` and the platform
        // `PlatformSecurityContext`.
        for param in &method.params {
            if param.ident == "self" {
                continue;
            }
            if is_security_context_type(&param.ty) {
                let ty = &param.ty;
                let key = quote!(#ty).to_string();
                if seen_secctx_keys.insert(key) {
                    secctx_asserts.push(quote! {
                        #support::grpc_repr::assert_security_context::<#ty>();
                    });
                }
                continue;
            }
            let ty = &param.ty;
            asserts.push(quote! {
                #support::grpc_repr::assert_grpc_repr::<#ty>();
            });
        }
        // Return type's success half. `Result<T, E>` was extracted by the
        // parser into `(Type, Type)`. We assert only on `T` — the error
        // type is conventionally a domain error and travels via gRPC
        // trailers, not a proto message.
        let (ok_ty, _err_ty) = &method.result_types;
        asserts.push(quote! {
            #support::grpc_repr::assert_grpc_repr::<#ok_ty>();
        });
    }

    let trait_ident = &model.trait_ident;
    let repr_guard = if asserts.is_empty() {
        quote! {}
    } else {
        let const_ident = quote::format_ident!("_GRPC_REPR_GUARD_{}", trait_ident);
        quote! {
            #[doc(hidden)]
            #[allow(non_upper_case_globals, dead_code)]
            const #const_ident: () = {
                #(#asserts)*
            };
        }
    };
    let secctx_guard = if secctx_asserts.is_empty() {
        quote! {}
    } else {
        let const_ident = quote::format_ident!("_GRPC_SECCTX_GUARD_{}", trait_ident);
        // `SecurityContextMarker` is always-on (the marker lives in
        // `toolkit_contract::grpc_repr`). The default impl for
        // `toolkit_security::SecurityContext` is gated on `grpc-client` —
        // users without that feature still get a useful compile error
        // pointing at the missing marker impl.
        quote! {
            #[doc(hidden)]
            #[allow(non_upper_case_globals, dead_code)]
            const #const_ident: () = {
                #(#secctx_asserts)*
            };
        }
    };
    quote! { #repr_guard #secctx_guard }
}

fn generate_cleaned_trait(model: &GrpcContractModel) -> TokenStream {
    let mut item = model.item.clone();
    let base_trait = &model.base_trait;

    let model_methods: std::collections::HashMap<String, &GrpcMethodModel> = model
        .methods
        .iter()
        .map(|m| (m.ident.to_string(), m))
        .collect();

    for trait_item in &mut item.items {
        if let TraitItem::Fn(method) = trait_item {
            strip_method_attrs(method, GRPC_ATTRS);
            strip_param_attrs(method);
            if let Some(model_method) = model_methods.get(&method.sig.ident.to_string()) {
                if model_method.server_streaming {
                    let (ok, err) = &model_method.result_types;
                    rewrite_streaming_signature(method, ok, err);
                }
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
                    model_method.server_streaming,
                ));
            }
        }
    }

    quote! {
        #[::async_trait::async_trait]
        #item
    }
}

fn generate_binding_fn(model: &GrpcContractModel, support: &TokenStream) -> TokenStream {
    // Naming convention: `<base_trait_snake>_grpc_binding`, e.g. `payment_api_grpc_binding()`
    // for projection trait `PaymentApiGrpc: PaymentApi`. Using the base trait
    // (not the projection trait) avoids the redundant `_grpc_grpc_binding`
    // suffix that arises from `to_snake_case("PaymentApiGrpc")`.
    let base_name = model
        .base_trait
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let fn_ident = format_ident!("{}_grpc_binding", base_name.to_snake_case());
    let trait_doc = format!("Build the gRPC binding IR for [`{}`].", model.trait_ident);
    let package = &model.package;
    let service = &model.service;

    let method_entries = model
        .methods
        .iter()
        .map(|m| build_method_binding(m, support));

    quote! {
        #[doc = #trait_doc]
        #[must_use]
        pub fn #fn_ident() -> #support::ir::grpc::GrpcBindingIr {
            #support::ir::grpc::GrpcBindingIr {
                package: #package.to_owned(),
                service: #service.to_owned(),
                methods: vec![ #(#method_entries),* ],
            }
        }
    }
}

fn build_method_binding(method: &GrpcMethodModel, support: &TokenStream) -> TokenStream {
    let method_name = method.ident.to_string();
    let rpc_name = &method.rpc_name;
    let server_streaming = method.server_streaming;
    let retryable = method.retryable;
    let optional = method.optional;
    let idempotency = idempotency_tokens(method.idempotency, support);

    quote! {
        #support::ir::grpc::GrpcMethodBindingIr {
            method_name: #method_name.to_owned(),
            rpc_name: #rpc_name.to_owned(),
            client_streaming: false,
            server_streaming: #server_streaming,
            idempotency_level: #idempotency,
            retryable: #retryable,
            optional: #optional,
        }
    }
}

fn idempotency_tokens(idem: GrpcIdempotency, support: &TokenStream) -> TokenStream {
    let variant = syn::Ident::new(idem.ir_variant(), proc_macro2::Span::call_site());
    quote! { #support::ir::grpc::GrpcIdempotency::#variant }
}

/// The bounds tonic's generated client requires of its transport, plus what
/// `#[async_trait]` needs to make the generated futures `Send`.
///
/// Spelled through `tonic::codegen`'s re-exports (`StdError`, `Body`, `Bytes`)
/// rather than through `bytes` / `http-body` directly, so a crate consuming this
/// macro needs no dependency it did not already have for tonic.
fn transport_bounds() -> TokenStream {
    quote! {
        T: ::tonic::client::GrpcService<::tonic::body::Body>
            + ::std::clone::Clone
            + ::std::marker::Send
            + ::std::marker::Sync
            + 'static,
        T::Error: ::std::convert::Into<::tonic::codegen::StdError>,
        T::ResponseBody: ::tonic::codegen::Body<Data = ::tonic::codegen::Bytes>
            + ::std::marker::Send
            + 'static,
        <T::ResponseBody as ::tonic::codegen::Body>::Error:
            ::std::convert::Into<::tonic::codegen::StdError> + ::std::marker::Send,
        T::Future: ::std::marker::Send,
    }
}

fn generate_client_struct(model: &GrpcContractModel, support: &TokenStream) -> TokenStream {
    let client_ident = client_struct_ident(&model.trait_ident);
    let stubs = &model.stubs_module;
    // tonic-prost-build emits `pub mod <service_snake>_client { pub struct <Service>Client<C> {...} }`.
    let client_module = format_ident!("{}_client", model.service.to_snake_case());
    let client_type_ident = format_ident!("{}Client", model.service);
    let doc = format!(
        "Generated gRPC client for [`{}`] (wraps `{}::{}::{}`).",
        model.trait_ident,
        quote!(#stubs),
        client_module,
        client_type_ident,
    );

    let bounds = transport_bounds();

    quote! {
        #[cfg(feature = "grpc-client")]
        #[doc = #doc]
        ///
        /// Generic over its transport so a platform-plane caller can supply a
        /// channel wrapped with an interceptor. `T` defaults to
        /// `tonic::transport::Channel`, so `#client_ident` on its own means
        /// exactly what it meant before this parameter existed.
        pub struct #client_ident<T = ::tonic::transport::Channel> {
            inner: #stubs::#client_module::#client_type_ident<T>,
            config: #support::runtime::config::ClientConfig,
        }

        #[cfg(feature = "grpc-client")]
        impl #client_ident<::tonic::transport::Channel> {
            /// Build a new client wrapping the supplied tonic Channel.
            #[must_use]
            pub fn new(
                channel: ::tonic::transport::Channel,
                config: #support::runtime::config::ClientConfig,
            ) -> Self {
                Self {
                    inner: #stubs::#client_module::#client_type_ident::new(channel),
                    config,
                }
            }

            /// Connect to a base URL and build a client.
            ///
            /// # Errors
            ///
            /// Returns a [`#support::runtime::transport_error::TransportError`]
            /// when the channel cannot be established.
            pub async fn connect(
                config: #support::runtime::config::ClientConfig,
            ) -> ::std::result::Result<
                Self,
                #support::runtime::transport_error::TransportError,
            > {
                let channel = Self::channel(&config).await?;
                Ok(Self::new(channel, config))
            }

            /// Build a client whose every outbound call passes through
            /// `interceptor`.
            ///
            /// This is the **platform-plane** entry point. A
            /// `PlatformSecurityContext` parameter carries no credential — it holds
            /// a validated identity, which is the *result* of inbound
            /// authentication — so the generated method bodies attach nothing from
            /// it. The credential is process-level and belongs here, on the
            /// channel: pass a
            /// `toolkit_transport_grpc::InternalAuthInterceptor` built from a
            /// rotating service-account token or a shared secret, and it attaches
            /// `x-toolkit-internal-token` to every request.
            ///
            /// Mirrors tonic's own `with_interceptor`, and the shape the
            /// hand-written `DirectoryGrpcClient` uses.
            #[must_use]
            pub fn with_interceptor<I>(
                channel: ::tonic::transport::Channel,
                interceptor: I,
                config: #support::runtime::config::ClientConfig,
            ) -> #client_ident<
                ::tonic::service::interceptor::InterceptedService<
                    ::tonic::transport::Channel,
                    I,
                >,
            >
            where
                I: ::tonic::service::Interceptor,
            {
                #client_ident {
                    inner: #stubs::#client_module::#client_type_ident::with_interceptor(
                        channel,
                        interceptor,
                    ),
                    config,
                }
            }

            /// Connect to a base URL and build an intercepted client.
            ///
            /// The platform-plane counterpart of [`connect`](Self::connect); see
            /// [`with_interceptor`](Self::with_interceptor).
            ///
            /// # Errors
            ///
            /// Returns a [`#support::runtime::transport_error::TransportError`]
            /// when the channel cannot be established.
            pub async fn connect_with_interceptor<I>(
                config: #support::runtime::config::ClientConfig,
                interceptor: I,
            ) -> ::std::result::Result<
                #client_ident<
                    ::tonic::service::interceptor::InterceptedService<
                        ::tonic::transport::Channel,
                        I,
                    >,
                >,
                #support::runtime::transport_error::TransportError,
            >
            where
                I: ::tonic::service::Interceptor,
            {
                let channel = Self::channel(&config).await?;
                Ok(Self::with_interceptor(channel, interceptor, config))
            }

            async fn channel(
                config: &#support::runtime::config::ClientConfig,
            ) -> ::std::result::Result<
                ::tonic::transport::Channel,
                #support::runtime::transport_error::TransportError,
            > {
                let endpoint = ::tonic::transport::Endpoint::from_shared(config.base_url.clone())
                    .map_err(|e| #support::runtime::transport_error::TransportError::network(e))?;
                let endpoint = endpoint.timeout(config.timeout);
                endpoint.connect().await
                    .map_err(|e| #support::runtime::transport_error::TransportError::network(e))
            }
        }

        #[cfg(feature = "grpc-client")]
        impl<T> #client_ident<T>
        where
            #bounds
        {
            /// Build a client over an arbitrary tonic transport — an
            /// `InterceptedService`, a test double, or a custom tower stack.
            #[must_use]
            pub fn with_transport(
                transport: T,
                config: #support::runtime::config::ClientConfig,
            ) -> Self {
                Self {
                    inner: #stubs::#client_module::#client_type_ident::new(transport),
                    config,
                }
            }
        }
    }
}

fn generate_client_impl(model: &GrpcContractModel, support: &TokenStream) -> TokenStream {
    let client_ident = client_struct_ident(&model.trait_ident);
    let trait_path = &model.base_trait;

    let methods = model
        .methods
        .iter()
        .map(|m| generate_client_method(m, model, support));

    let bounds = transport_bounds();

    quote! {
        #[cfg(feature = "grpc-client")]
        #[::async_trait::async_trait]
        impl<T> #trait_path for #client_ident<T>
        where
            #bounds
        {
            #(#methods)*
        }
    }
}

/// The prost type of a method's request message, computed the way
/// `toolkit-contract-protogen` computes it rather than guessed.
///
/// protogen has two cases, and only the second yields `<Method>Request`:
///
/// - **exactly one wire parameter of a named (non-primitive) type** — the message
///   *is* that type, reused. `put_if_absent(req: PutRequest)` therefore has input
///   `PutRequest`, and `renew(req: LeaseRef)` has input `LeaseRef`;
/// - **anything else** — protogen synthesizes `<UpperCamelCase(method)>Request`
///   from the wire fields, which is the case a single primitive parameter or a
///   multi-parameter method falls into.
///
/// This used to assume the second case unconditionally, which happened to work only
/// while every contract in the tree named its DTO after its method. The moment two
/// methods shared a request DTO — the shape §12.1 of the cluster design specifies,
/// where `put`/`put_if_absent` share `PutRequest` and `renew`/`release` share
/// `LeaseRef` — the macro referred to a prost type protogen had never emitted.
///
/// The two must agree by construction, not by naming discipline: they are two
/// halves of one pipeline, and a mismatch is a compile error in generated code
/// pointing at the macro invocation rather than at the cause.
fn proto_request_ident(method: &GrpcMethodModel) -> syn::Ident {
    let wire_params: Vec<&GrpcParam> = method
        .params
        .iter()
        .filter(|p| p.ident != "self" && !is_security_context_type(&p.ty))
        .collect();

    if let [param] = wire_params.as_slice()
        && !is_proto_direct_primitive(&param.ty)
        && let Some(named) = named_type_ident(&param.ty)
    {
        return named;
    }

    format_ident!("{}Request", method.ident.to_string().to_upper_camel_case())
}

/// The last path segment of a type, when it is a plain path that protogen would
/// render as `TypeRef::Named` — so not a container, whose element type protogen
/// projects as `repeated` / `optional` rather than as a message of its own.
fn named_type_ident(ty: &Type) -> Option<syn::Ident> {
    let Type::Path(path) = ty else {
        return None;
    };
    let last = path.path.segments.last()?;
    if matches!(
        last.ident.to_string().as_str(),
        "Option" | "Vec" | "HashMap" | "BTreeMap"
    ) {
        return None;
    }
    Some(last.ident.clone())
}

fn generate_client_method(
    method: &GrpcMethodModel,
    model: &GrpcContractModel,
    support: &TokenStream,
) -> TokenStream {
    let rpc_method_ident = format_ident!("{}", method.rpc_name.to_snake_case());
    let stubs = &model.stubs_module;
    let request_ty_ident = proto_request_ident(method);
    let proto_request_ty = quote! { #stubs::#request_ty_ident };

    // A platform-plane context is unused *in the client*: it carries no credential,
    // so nothing is attached from it. Rename it to `_ctx` in the generated
    // signature rather than leaving an `unused_variables` warning in every
    // consumer's build — parameter names need not match the trait, and the
    // underscore is the accurate statement about this client.
    let renamed: Vec<(syn::Ident, syn::Type)> = method
        .params
        .iter()
        .map(|p| {
            let unused_here = is_platform_security_context_type(&p.ty);
            let ident = if unused_here {
                format_ident!("_{}", p.ident)
            } else {
                p.ident.clone()
            };
            (ident, p.ty.clone())
        })
        .collect();
    let sig_inputs = render_method_inputs(renamed.iter().map(|(i, t)| (i, t)));
    let (ok_ty, err_ty) = &method.result_types;
    let return_ty = render_method_return_ty(ok_ty, err_ty, method.server_streaming);
    let err_convert = quote! {
        |__e| <#err_ty as ::std::convert::From<#support::runtime::transport_error::TransportError>>::from(__e)
    };

    let Some(body_ident) = body_param_ident(method) else {
        let span = method.ident.span();
        let msg = format!(
            "#[grpc_contract] method `{}` has no wire-body parameter (after \
             filtering out `self` and the SecurityContext-typed argument). \
             Add a single payload parameter — typically a Named DTO — or \
             a primitive (String, i64, ...) for which a synthesized request \
             type is generated.",
            method.ident
        );
        return quote::quote_spanned! { span => compile_error!(#msg); };
    };
    // A platform-plane context carries no credential, so for *attachment*
    // purposes it is absent — see `bearer_context_param`. The parameter is still
    // in `sig_inputs`, which is built from `method.params`.
    let ctx_ident = bearer_context_param(method);

    if method.server_streaming {
        return generate_streaming_client_method(
            method,
            stubs,
            support,
            &rpc_method_ident,
            &sig_inputs,
            &return_ty,
            &body_ident,
            ctx_ident.as_ref(),
            ok_ty,
            err_ty,
        );
    }

    if method.retryable {
        return generate_retryable_unary_method(
            method,
            &rpc_method_ident,
            &sig_inputs,
            &return_ty,
            &body_ident,
            ctx_ident.as_ref(),
            ok_ty,
            &proto_request_ty,
            support,
            &err_convert,
        );
    }

    generate_one_shot_unary_method(
        method,
        &rpc_method_ident,
        &sig_inputs,
        &return_ty,
        &body_ident,
        ctx_ident.as_ref(),
        ok_ty,
        &proto_request_ty,
        support,
        &err_convert,
    )
}

/// Emit a non-retryable unary method body. Converts the DTO to the proto
/// stub exactly once and issues a single RPC — no Arc, no template clone.
#[allow(clippy::too_many_arguments)]
fn generate_one_shot_unary_method(
    method: &GrpcMethodModel,
    rpc_method_ident: &syn::Ident,
    sig_inputs: &TokenStream,
    return_ty: &TokenStream,
    body_ident: &syn::Ident,
    ctx_ident: Option<&syn::Ident>,
    ok_ty: &Type,
    proto_request_ty: &TokenStream,
    support: &TokenStream,
    err_convert: &TokenStream,
) -> TokenStream {
    let method_ident = &method.ident;
    let attach_metadata = match ctx_ident {
        Some(ctx) => quote! {
            #support::grpc::attach_bearer(__request.metadata_mut(), &#ctx)?;
        },
        None => quote! {},
    };

    // Wrap the body in an inner closure that yields
    // `Result<#ok_ty, TransportError>` so `?` works with attach_bearer's
    // `TransportError`. The outer fn maps through `err_convert`.
    quote! {
        async fn #method_ident #sig_inputs #return_ty {
            let __inner = || async {
                let __proto: #proto_request_ty = ::std::convert::From::from(#body_ident);
                #[allow(unused_mut)]
                let mut __request = ::tonic::Request::new(__proto);
                #attach_metadata
                let mut __client = self.inner.clone();
                let __response = __client
                    .#rpc_method_ident(__request)
                    .await
                    .map_err(|__s| #support::grpc::map_tonic_status(&__s))?;
                // Fallible conversion: the infallible `From<Proto>` panics on a
                // malformed `via_string` field, which would let a peer take this
                // process down with one bad response.
                let __decoded = <#ok_ty as #support::grpc_repr::TryFromProto<_>>::try_from_proto_wire(
                    __response.into_inner(),
                )
                .map_err(#support::runtime::transport_error::TransportError::serialization)?;
                ::std::result::Result::<#ok_ty, #support::runtime::transport_error::TransportError>::Ok(
                    __decoded,
                )
            };
            let __result = __inner().await;
            __result.map_err(#err_convert)
        }
    }
}

/// Emit a retryable unary method body. The DTO is converted to a proto
/// template *once*, then shared between attempts via `Arc<T>` so each
/// retry only clones the (typically smaller) proto instead of re-running
/// the user-defined `From<DTO>` conversion.
#[allow(clippy::too_many_arguments)]
fn generate_retryable_unary_method(
    method: &GrpcMethodModel,
    rpc_method_ident: &syn::Ident,
    sig_inputs: &TokenStream,
    return_ty: &TokenStream,
    body_ident: &syn::Ident,
    ctx_ident: Option<&syn::Ident>,
    ok_ty: &Type,
    proto_request_ty: &TokenStream,
    support: &TokenStream,
    err_convert: &TokenStream,
) -> TokenStream {
    let method_ident = &method.ident;
    // Inside the per-attempt async block we hold a CLONE of the context
    // (cheap if the context wraps an `Arc`). The outer binding of `__ctx`
    // captures by reference so the FnMut closure can re-clone on each retry.
    let (ctx_outer, attempt_ctx_clone, attach_metadata) = match ctx_ident {
        Some(ctx) => (
            quote! { let __ctx = #ctx; },
            quote! { let __ctx_attempt = __ctx.clone(); },
            quote! {
                #support::grpc::attach_bearer(__request.metadata_mut(), &__ctx_attempt)?;
            },
        ),
        None => (quote! {}, quote! {}, quote! {}),
    };

    quote! {
        async fn #method_ident #sig_inputs #return_ty {
            // One conversion up-front; retries clone the proto, not the DTO.
            let __proto_template: #proto_request_ty = ::std::convert::From::from(#body_ident);
            let __proto_arc: ::std::sync::Arc<#proto_request_ty> =
                ::std::sync::Arc::new(__proto_template);
            #ctx_outer

            let __attempt = || {
                let __proto: ::std::sync::Arc<#proto_request_ty> =
                    ::std::sync::Arc::clone(&__proto_arc);
                #attempt_ctx_clone
                async move {
                    let mut __client = self.inner.clone();
                    #[allow(unused_mut)]
                    let mut __request = ::tonic::Request::new((*__proto).clone());
                    #attach_metadata
                    let __response = __client
                        .#rpc_method_ident(__request)
                        .await
                        .map_err(|__s| #support::grpc::map_tonic_status(&__s))?;
                    // See the one-shot arm: a panicking decode here would blow
                    // up inside the retry loop rather than returning an error.
                    let __decoded = <#ok_ty as #support::grpc_repr::TryFromProto<_>>::try_from_proto_wire(
                        __response.into_inner(),
                    )
                    .map_err(#support::runtime::transport_error::TransportError::serialization)?;
                    ::std::result::Result::<#ok_ty, #support::runtime::transport_error::TransportError>::Ok(
                        __decoded,
                    )
                }
            };

            let __result: ::std::result::Result<#ok_ty, #support::runtime::transport_error::TransportError> =
                #support::runtime::retry::retry_with_backoff(&self.config.retry, __attempt).await;
            __result.map_err(#err_convert)
        }
    }
}

/// Identify the body parameter (the first non-`self`, non-security-context
/// param). Returns `None` when the method has no wire payload — in which
/// case the macro emits a `compile_error!` pointing at the method ident,
/// rather than producing generated code that fails downstream with an
/// opaque "undefined variable `__missing_body`" diagnostic.
fn body_param_ident(method: &GrpcMethodModel) -> Option<syn::Ident> {
    method
        .params
        .iter()
        .find(|p| p.ident != "self" && !is_security_context_type(&p.ty))
        .map(|p| p.ident.clone())
}

/// The security-context parameter **only when it carries a credential** — that
/// is, only on the tenant plane.
///
/// The two planes source their outbound credential differently, and this is the
/// distinction the generated client turns on:
///
/// - **tenant** — the credential is *inside* the context. `SecurityContext`
///   carries the bearer token it was built from, so the client attaches
///   `authorization: Bearer …` per request via `attach_bearer`.
/// - **platform** — the context carries **no credential at all**.
///   `PlatformSecurityContext` holds a validated `PlatformIdentity`, which is the
///   *result* of inbound authentication. The outbound credential is process-level
///   (a projected service-account token or a shared secret) and is attached at the
///   channel by `InternalAuthInterceptor` as `x-toolkit-internal-token` — see
///   [`with_interceptor`] on the generated client.
///
/// So a platform-plane context contributes nothing to the request, and the
/// generated body attaches nothing from it. The parameter stays in the signature,
/// where `cpt-cf-binding-constraint-security-context` requires it and where the
/// server-side handler is what consumes the plane marker.
///
/// [`with_interceptor`]: generate_client_struct
fn bearer_context_param(method: &GrpcMethodModel) -> Option<syn::Ident> {
    method
        .params
        .iter()
        .find(|p| is_security_context_type(&p.ty))
        .filter(|p| !is_platform_security_context_type(&p.ty))
        .map(|p| p.ident.clone())
}

#[allow(clippy::too_many_arguments)]
fn generate_streaming_client_method(
    method: &GrpcMethodModel,
    _stubs: &syn::Path,
    support: &TokenStream,
    rpc_method_ident: &syn::Ident,
    sig_inputs: &TokenStream,
    return_ty: &TokenStream,
    body_ident: &syn::Ident,
    ctx_ident: Option<&syn::Ident>,
    ok_ty: &Type,
    err_ty: &Type,
) -> TokenStream {
    let method_ident = &method.ident;

    let attach_metadata = match ctx_ident {
        Some(_) => quote! {
            if let Err(__e) = #support::grpc::attach_bearer(__request.metadata_mut(), &__ctx_clone) {
                let __out_err: #err_ty = ::std::convert::From::from(__e);
                Err(__out_err)?;
            }
        },
        None => quote! {},
    };

    let ctx_clone = match ctx_ident {
        Some(ctx) => quote! { let __ctx_clone = #ctx.clone(); },
        None => quote! {},
    };

    quote! {
        fn #method_ident #sig_inputs #return_ty {
            use ::futures_util::StreamExt as _;
            let __body_owned = #body_ident;
            let __client_arc = self.inner.clone();
            #ctx_clone

            ::std::boxed::Box::pin(::async_stream::try_stream! {
                let mut __client = __client_arc;
                let __proto: _ = ::std::convert::From::from(__body_owned);
                #[allow(unused_mut)]
                let mut __request = ::tonic::Request::new(__proto);
                #attach_metadata
                let __response = __client
                    .#rpc_method_ident(__request)
                    .await
                    .map_err(|__s| -> #err_ty {
                        ::std::convert::From::from(#support::grpc::map_tonic_status(&__s))
                    })?;
                let mut __stream = __response.into_inner();
                while let Some(__item) = __stream.next().await {
                    let __proto_item = __item.map_err(|__s| -> #err_ty {
                        ::std::convert::From::from(#support::grpc::map_tonic_status(&__s))
                    })?;
                    // Fallible decode per item: a malformed `via_string` in one
                    // frame ends the stream with an error instead of panicking
                    // through whatever task is polling it.
                    let __out: #ok_ty =
                        <#ok_ty as #support::grpc_repr::TryFromProto<_>>::try_from_proto_wire(
                            __proto_item,
                        )
                        .map_err(|__e| -> #err_ty {
                            ::std::convert::From::from(
                                #support::runtime::transport_error::TransportError::serialization(__e),
                            )
                        })?;
                    yield __out;
                }
            })
        }
    }
}

fn generate_projection_impl(model: &GrpcContractModel) -> TokenStream {
    generate_projection_impl_for_client(
        &model.trait_ident,
        &client_struct_ident(&model.trait_ident),
        "grpc-client",
        Some(&transport_bounds()),
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::generate;
    use crate::grpc_contract_parse::{GrpcContractAttr, parse};

    /// Expand a projection trait and return the emitted tokens as text.
    ///
    /// Asserting on the *emitted body* rather than on whether it compiles is
    /// deliberate: "a platform-plane context contributes nothing to the request"
    /// is invisible to the type system. A regression that reintroduced
    /// `attach_bearer` for the platform plane would fail to compile only because
    /// `PlatformSecurityContext` lacks `BearerContext` — and the moment someone
    /// "helpfully" implemented that, the failure would become a runtime one with
    /// requests carrying an empty credential.
    fn expand(item: &str) -> String {
        let attr: GrpcContractAttr = syn::parse_str(
            r#"package = "demo.v1", service = "DemoApi", stubs_module = "crate::stubs""#,
        )
        .expect("attr parses");
        let item: syn::ItemTrait = syn::parse_str(item).expect("trait parses");
        let model = parse(attr, item).expect("model parses");
        generate(&model).to_string()
    }

    const TENANT: &str = r#"
        pub trait DemoApiGrpc: DemoApi {
            #[rpc(name = "Ping")]
            async fn ping(
                &self,
                ctx: SecurityContext,
                body: String,
            ) -> Result<u32, CanonicalError>;
        }
    "#;

    const PLATFORM: &str = r#"
        pub trait DemoApiGrpc: DemoApi {
            #[rpc(name = "Ping")]
            async fn ping(
                &self,
                ctx: PlatformSecurityContext,
                body: String,
            ) -> Result<u32, CanonicalError>;
        }
    "#;

    #[test]
    fn tenant_client_attaches_the_bearer_from_its_context() {
        assert!(
            expand(TENANT).contains("attach_bearer"),
            "the tenant context carries the credential, so the client must attach it"
        );
    }

    /// The platform plane's credential is process-level and is attached at the
    /// channel by `InternalAuthInterceptor`. `PlatformSecurityContext` holds a
    /// validated identity and no token, so there is nothing here to attach.
    #[test]
    fn platform_client_attaches_no_bearer() {
        let expanded = expand(PLATFORM);
        assert!(
            !expanded.contains("attach_bearer"),
            "a platform-plane context has no bearer to attach:\n{expanded}"
        );
    }

    /// Both planes must classify the context out of the payload, so the *body*
    /// parameter is the one after it. Misclassify the platform form and
    /// `body_param_ident` picks `ctx`, silently making the context the wire body.
    #[test]
    fn both_planes_identify_the_second_parameter_as_the_wire_body() {
        for src in [TENANT, PLATFORM] {
            let expanded = expand(src);
            assert!(
                expanded.contains("From :: from (body)"),
                "the wire body must be `body`, not the context:\n{expanded}"
            );
        }
    }

    /// The explicit attribute must not reach the compiler: a proc-macro attribute
    /// cannot declare helper attributes, so the macro removing it is the only thing
    /// that makes `#[secctx]` usable on a projection trait — and the attribute is
    /// how a platform-plane contract has to be written, since the `ctx:`-name
    /// heuristic does not recognise `PlatformSecurityContext`.
    #[test]
    fn the_secctx_attribute_is_stripped_from_the_emitted_trait() {
        let expanded = expand(
            r#"
            pub trait DemoApiGrpc: DemoApi {
                #[rpc(name = "Ping")]
                async fn ping(
                    &self,
                    #[secctx] auth: PlatformSecurityContext,
                    body: String,
                ) -> Result<u32, CanonicalError>;
            }
        "#,
        );
        assert!(
            !expanded.contains("secctx"),
            "`#[secctx]` leaked into the emitted trait:\n{expanded}"
        );
    }

    /// An explicitly-marked context is classified by the attribute even when its
    /// type name gives no hint — which is the whole point of the attribute.
    #[test]
    fn an_attribute_marked_context_is_kept_off_the_wire() {
        let expanded = expand(
            r#"
            pub trait DemoApiGrpc: DemoApi {
                #[rpc(name = "Ping")]
                async fn ping(
                    &self,
                    #[secctx] auth: PlatformSecurityContext,
                    body: String,
                ) -> Result<u32, CanonicalError>;
                }
            "#,
        );
        assert!(
            expanded.contains("From :: from (body)"),
            "the wire body must be `body`, not the marked context:\n{expanded}"
        );
    }

    /// The generated client is generic over its transport with a `Channel`
    /// default, which is what lets a platform-plane caller pass an
    /// `InterceptedService` while every existing caller keeps writing `FooClient`.
    #[test]
    fn the_client_is_generic_over_its_transport_and_defaults_to_channel() {
        let expanded = expand(TENANT);
        assert!(
            expanded.contains("struct DemoApiGrpcClient < T = :: tonic :: transport :: Channel >"),
            "expected a defaulted transport parameter:\n{expanded}"
        );
        assert!(
            expanded.contains("with_interceptor"),
            "expected a platform-plane constructor:\n{expanded}"
        );
    }
}
