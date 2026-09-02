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
/// whose only wire parameter (after filtering out `self` and `SecurityContext`)
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
        // Parameters: skip `self`. SecurityContext-typed arguments don't
        // travel on the wire but still need a static assertion that the
        // detected type really impls `SecurityContextMarker`.
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
            // `#[secctx]` is consumed by this macro; without stripping it the
            // attribute reaches the compiler unresolved. The cluster contract
            // needs the explicit form, since the `ctx:`-name heuristic does not
            // match `PlatformSecurityContext`.
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

    quote! {
        #[cfg(feature = "grpc-client")]
        #[doc = #doc]
        pub struct #client_ident {
            inner: #stubs::#client_module::#client_type_ident<::tonic::transport::Channel>,
            config: #support::runtime::config::ClientConfig,
        }

        #[cfg(feature = "grpc-client")]
        impl #client_ident {
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
                // Honor `require_tls`: refuse to build the channel over a
                // plaintext (`http://`) scheme so the long-lived, process-scoped
                // platform-plane credential is never sent over cleartext h2c
                // (`cpt-cf-adr-two-plane-auth`). When it is unset, the platform's
                // deliberate in-mesh plaintext service-to-service convention
                // is preserved (see `ClientConfig::require_tls`).
                if config.require_tls
                    && !config.base_url.trim_start().starts_with("https://")
                {
                    return ::std::result::Result::Err(
                        #support::runtime::transport_error::TransportError::network(
                            ::std::format!(
                                "require_tls is set but the gRPC endpoint scheme is not https: {}",
                                config.base_url,
                            ),
                        ),
                    );
                }
                let endpoint = ::tonic::transport::Endpoint::from_shared(config.base_url.clone())
                    .map_err(|e| #support::runtime::transport_error::TransportError::network(e))?;
                let endpoint = endpoint.timeout(config.timeout);
                let channel = endpoint.connect().await
                    .map_err(|e| #support::runtime::transport_error::TransportError::network(e))?;
                Ok(Self::new(channel, config))
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

    quote! {
        #[cfg(feature = "grpc-client")]
        #[::async_trait::async_trait]
        impl #trait_path for #client_ident {
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
/// Assuming the second case unconditionally happens to work only while every
/// contract in the tree names its DTO after its method. The moment two methods
/// share a request DTO — the shape the cluster design specifies, where
/// `put`/`put_if_absent` share `PutRequest` and `renew`/`release` share `LeaseRef`
/// — the macro refers to a prost type protogen never emitted.
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
    // Computed to agree with protogen rather than guessed — see
    // `proto_request_ident`. Also anchors type inference through the `Arc<T>`
    // template in retryable bodies (where the chain `From → Arc::new →
    // Arc::clone → deref → Request::new` would otherwise leave T ambiguous).
    let request_ty_ident = proto_request_ident(method);
    let proto_request_ty = quote! { #stubs::#request_ty_ident };

    let sig_inputs = render_method_inputs(method.params.iter().map(|p| (&p.ident, &p.ty)));
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
    // A single value carries both the token-attachment ident and the plane
    // classification, so a method can never attach a tenant bearer token while
    // classifying itself as platform-plane (or vice versa): the two are no
    // longer a separable `(Option<ident>, bool)` pair the three generators
    // could receive inconsistently.
    let plane = auth_plane(method);

    if method.server_streaming {
        return generate_streaming_client_method(
            method,
            stubs,
            support,
            &rpc_method_ident,
            &sig_inputs,
            &return_ty,
            &body_ident,
            plane,
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
            plane,
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
        plane,
        ok_ty,
        &proto_request_ty,
        support,
        &err_convert,
    )
}

/// The authentication plane of a method, derived **once** from its single
/// security-context parameter. Collapsing the former `(Option<&Ident>, bool)`
/// pair into one value makes it impossible for the token-attachment ident and
/// the plane classification to be passed inconsistently to the code generators
/// (`cpt-cf-adr-two-plane-auth`).
#[derive(Clone, Copy)]
enum AuthPlane<'a> {
    /// No security-context parameter: no credential is attached.
    None,
    /// Tenant plane (`SecurityContext`): attach the caller's bearer token,
    /// sourced from the named argument.
    Tenant(&'a syn::Ident),
    /// Platform plane (`PlatformSecurityContext`): the off-wire marker (named by
    /// the ident) is consumed and the runtime internal token is attached — never
    /// from the argument.
    Platform(&'a syn::Ident),
}

/// Classify a method's authentication plane from its (at most one)
/// security-context parameter. `parse_params` rejects more than one such
/// parameter, so the selection is unambiguous.
fn auth_plane(method: &GrpcMethodModel) -> AuthPlane<'_> {
    match security_context_param(method) {
        Some(p) if is_platform_security_context_type(&p.ty) => AuthPlane::Platform(&p.ident),
        Some(p) => AuthPlane::Tenant(&p.ident),
        None => AuthPlane::None,
    }
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
    plane: AuthPlane<'_>,
    ok_ty: &Type,
    proto_request_ty: &TokenStream,
    support: &TokenStream,
    err_convert: &TokenStream,
) -> TokenStream {
    let method_ident = &method.ident;
    let rpc_name = method.ident.to_string();
    let attach_metadata = match plane {
        // Platform plane: source the credential from the runtime provider on
        // `self.config`, not from the `PlatformSecurityContext` argument (which
        // carries no secret). The marker stays off the wire; `let _ = &#ctx`
        // consumes it so it is not an unused parameter. Permissive when no
        // provider is configured.
        AuthPlane::Platform(ctx) => quote! {
            let _ = &#ctx;
            #support::grpc::attach_internal_token(
                __request.metadata_mut(),
                self.config.internal_token_provider.as_ref(),
                #rpc_name,
            )?;
        },
        AuthPlane::Tenant(ctx) => quote! {
            #support::grpc::attach_bearer(__request.metadata_mut(), &#ctx)?;
        },
        AuthPlane::None => quote! {},
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
                // Fallible conversion: a `via_string`-bearing response type has no
                // infallible `From<Proto>` (a malformed field would otherwise let a
                // peer take this process down with one bad response), so decode
                // through the fallible path.
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
    plane: AuthPlane<'_>,
    ok_ty: &Type,
    proto_request_ty: &TokenStream,
    support: &TokenStream,
    err_convert: &TokenStream,
) -> TokenStream {
    let method_ident = &method.ident;
    let rpc_name = method.ident.to_string();
    // Inside the per-attempt async block we hold a CLONE of the context
    // (cheap if the context wraps an `Arc`). The outer binding of `__ctx`
    // captures by reference so the FnMut closure can re-clone on each retry.
    let (ctx_outer, attempt_ctx_clone, attach_metadata) = match plane {
        // Platform plane: no per-attempt context clone — the credential comes
        // from the runtime provider on `self.config` (accessible inside the
        // per-attempt `async move`, which already borrows `self`). `let _ =
        // &#ctx` consumes the off-wire marker so it is not unused. Permissive
        // when no provider is configured.
        AuthPlane::Platform(ctx) => (
            quote! { let _ = &#ctx; },
            quote! {},
            quote! {
                #support::grpc::attach_internal_token(
                    __request.metadata_mut(),
                    self.config.internal_token_provider.as_ref(),
                    #rpc_name,
                )?;
            },
        ),
        AuthPlane::Tenant(ctx) => (
            quote! { let __ctx = #ctx; },
            quote! { let __ctx_attempt = __ctx.clone(); },
            quote! {
                #support::grpc::attach_bearer(__request.metadata_mut(), &__ctx_attempt)?;
            },
        ),
        AuthPlane::None => (quote! {}, quote! {}, quote! {}),
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

/// Identify the body parameter (the first non-`self`, non-SecurityContext
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

/// The method's single security-context parameter, if any. `parse_params`
/// (`grpc_contract_parse.rs`) rejects methods with more than one such
/// parameter, so `.find` here is unambiguous — callers MUST derive both the
/// token-attachment ident and the plane classification (via
/// [`is_platform_security_context_type`]) from this same param, to avoid
/// mixing tenant and platform auth.
fn security_context_param(method: &GrpcMethodModel) -> Option<&GrpcParam> {
    method
        .params
        .iter()
        .find(|p| is_security_context_type(&p.ty))
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
    plane: AuthPlane<'_>,
    ok_ty: &Type,
    err_ty: &Type,
) -> TokenStream {
    let method_ident = &method.ident;
    let rpc_name = method.ident.to_string();

    // The returned stream is `'static`, so it cannot borrow `self`; any credential
    // source must be cloned out *before* the `try_stream!` block. For the tenant
    // plane that is the context clone; for the platform plane it is the runtime
    // provider (`Option<InternalTokenProvider>`, cheap `Arc` clone), with the
    // off-wire marker consumed via `let _ = &#ctx`.
    let (ctx_clone, attach_metadata) = match plane {
        AuthPlane::Platform(ctx) => (
            quote! {
                let _ = &#ctx;
                let __internal_token_provider =
                    self.config.internal_token_provider.clone();
            },
            quote! {
                if let Err(__e) = #support::grpc::attach_internal_token(
                    __request.metadata_mut(),
                    __internal_token_provider.as_ref(),
                    #rpc_name,
                ) {
                    let __out_err: #err_ty = ::std::convert::From::from(__e);
                    Err(__out_err)?;
                }
            },
        ),
        AuthPlane::Tenant(ctx) => (
            quote! { let __ctx_clone = #ctx.clone(); },
            quote! {
                if let Err(__e) = #support::grpc::attach_bearer(__request.metadata_mut(), &__ctx_clone) {
                    let __out_err: #err_ty = ::std::convert::From::from(__e);
                    Err(__out_err)?;
                }
            },
        ),
        AuthPlane::None => (quote! {}, quote! {}),
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc_contract_parse::{self, GrpcContractAttr};
    use quote::quote;

    fn build_model(tokens: TokenStream) -> GrpcContractModel {
        let attr: GrpcContractAttr = syn::parse2(quote! {
            package = "test.v1",
            stubs_module = "crate::stubs"
        })
        .unwrap();
        let item: syn::ItemTrait = syn::parse2(tokens).unwrap();
        grpc_contract_parse::parse(attr, item).unwrap()
    }

    fn expand(tokens: TokenStream) -> String {
        generate(&build_model(tokens)).to_string()
    }

    /// Concatenate the token strings of every generated `fn` body whose ident
    /// matches `method`, across all `impl` blocks in the expansion. Parsing the
    /// expansion as a `syn::File` and locating the statement is robust against
    /// proc-macro2 pretty-printing whitespace (`& ctx` vs `&ctx`) and lets a
    /// per-method assertion detect cross-method plane leakage that a whole-file
    /// `contains` on a single-method trait cannot.
    fn method_bodies(tokens: TokenStream, method: &str) -> String {
        use quote::ToTokens as _;
        let file: syn::File =
            syn::parse2(generate(&build_model(tokens))).expect("expansion parses as a syn::File");
        let mut out = String::new();
        for item in &file.items {
            let syn::Item::Impl(imp) = item else { continue };
            for impl_item in &imp.items {
                if let syn::ImplItem::Fn(f) = impl_item
                    && f.sig.ident == method
                {
                    out.push_str(&f.block.to_token_stream().to_string());
                    out.push('\n');
                }
            }
        }
        assert!(!out.is_empty(), "no fn `{method}` found in expansion");
        out
    }

    #[test]
    fn tenant_plane_method_attaches_bearer_not_internal_token() {
        let body = method_bodies(
            quote! {
                pub trait FooApiGrpc: FooApi {
                    async fn get_thing(&self, ctx: SecurityContext, id: String) -> Result<Resp, Err>;
                }
            },
            "get_thing",
        );
        assert!(body.contains("attach_bearer"), "got:\n{body}");
        assert!(!body.contains("attach_internal_token"), "got:\n{body}");
    }

    #[test]
    fn platform_plane_method_attaches_internal_token_not_bearer() {
        let body = method_bodies(
            quote! {
                pub trait FooApiGrpc: FooApi {
                    async fn get_thing(&self, ctx: PlatformSecurityContext, id: String) -> Result<Resp, Err>;
                }
            },
            "get_thing",
        );
        assert!(body.contains("attach_internal_token"), "got:\n{body}");
        assert!(!body.contains("attach_bearer"), "got:\n{body}");
        // The credential is wired to the runtime provider on `self.config`, not
        // sourced from the (off-wire) argument.
        assert!(
            body.contains("internal_token_provider"),
            "attach must be wired to self.config.internal_token_provider; got:\n{body}"
        );
    }

    /// A trait mixing a tenant method and a platform method: each generated body
    /// must carry ONLY its own plane's attacher. This is the case a single-method
    /// trait cannot cover — it detects plane classification derived from the
    /// wrong method.
    #[test]
    fn mixed_trait_keeps_each_method_on_its_own_plane() {
        let tokens = quote! {
            pub trait FooApiGrpc: FooApi {
                async fn tenant_call(&self, ctx: SecurityContext, id: String) -> Result<Resp, Err>;
                async fn platform_call(&self, ctx: PlatformSecurityContext, id: String) -> Result<Resp, Err>;
            }
        };
        let tenant = method_bodies(tokens.clone(), "tenant_call");
        assert!(tenant.contains("attach_bearer"), "got:\n{tenant}");
        assert!(!tenant.contains("attach_internal_token"), "got:\n{tenant}");

        let platform = method_bodies(tokens, "platform_call");
        assert!(
            platform.contains("attach_internal_token"),
            "got:\n{platform}"
        );
        assert!(!platform.contains("attach_bearer"), "got:\n{platform}");
    }

    #[test]
    fn platform_plane_retryable_method_attaches_internal_token() {
        let out = expand(quote! {
            pub trait FooApiGrpc: FooApi {
                #[retryable]
                #[idempotency_level(Idempotent)]
                async fn get_thing(&self, ctx: PlatformSecurityContext, id: String) -> Result<Resp, Err>;
            }
        });
        assert!(out.contains("attach_internal_token"), "got:\n{out}");
        assert!(!out.contains("attach_bearer"), "got:\n{out}");
    }

    #[test]
    fn platform_plane_streaming_method_attaches_internal_token() {
        let out = expand(quote! {
            pub trait FooApiGrpc: FooApi {
                #[streaming]
                async fn watch_thing(&self, ctx: PlatformSecurityContext, id: String) -> Result<Resp, Err>;
            }
        });
        assert!(out.contains("attach_internal_token"), "got:\n{out}");
        assert!(!out.contains("attach_bearer"), "got:\n{out}");
    }

    #[test]
    fn tenant_plane_streaming_method_attaches_bearer_not_internal_token() {
        let out = expand(quote! {
            pub trait FooApiGrpc: FooApi {
                #[streaming]
                async fn watch_thing(&self, ctx: SecurityContext, id: String) -> Result<Resp, Err>;
            }
        });
        assert!(out.contains("attach_bearer"), "got:\n{out}");
        assert!(!out.contains("attach_internal_token"), "got:\n{out}");
    }

    #[test]
    fn method_with_no_wire_body_emits_compile_error() {
        let out = expand(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn get_thing(&self, ctx: SecurityContext) -> Result<Resp, Err>;
            }
        });
        assert!(out.contains("compile_error"), "got:\n{out}");
        assert!(out.contains("no wire-body parameter"), "got:\n{out}");
    }

    #[test]
    fn security_context_param_selects_the_only_secctx_param() {
        let model = build_model(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn get_thing(&self, ctx: PlatformSecurityContext, id: String) -> Result<Resp, Err>;
            }
        });
        let param = security_context_param(&model.methods[0]).expect("secctx present");
        assert_eq!(param.ident, "ctx");
        assert!(is_platform_security_context_type(&param.ty));
    }

    #[test]
    fn security_context_param_is_none_when_absent() {
        let model = build_model(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn get_thing(&self, id: String) -> Result<Resp, Err>;
            }
        });
        assert!(security_context_param(&model.methods[0]).is_none());
    }

    #[test]
    fn body_param_ident_skips_security_context() {
        let model = build_model(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn get_thing(&self, ctx: SecurityContext, id: String) -> Result<Resp, Err>;
            }
        });
        let body = body_param_ident(&model.methods[0]).expect("body param present");
        assert_eq!(body, "id");
    }

    #[test]
    fn body_param_ident_none_when_only_security_context() {
        let model = build_model(quote! {
            pub trait FooApiGrpc: FooApi {
                async fn get_thing(&self, ctx: SecurityContext) -> Result<Resp, Err>;
            }
        });
        assert!(body_param_ident(&model.methods[0]).is_none());
    }
}
