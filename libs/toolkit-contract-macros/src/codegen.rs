use heck::{ToShoutySnakeCase, ToSnakeCase};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::model::{ContractKind, ContractModel, Idempotency, MethodKind, MethodModel, ParamRole};
use crate::support::contract_support_path;

pub fn generate(model: &ContractModel) -> TokenStream {
    let support = contract_support_path();
    let trait_def = generate_trait(model);
    let descriptor = generate_descriptor(model, &support);
    let ir_fn = generate_ir_function(model, &support);
    let contract_impl = generate_contract_impl(model, &support);

    quote! {
        #trait_def
        #descriptor
        #ir_fn
        #contract_impl
    }
}

fn generate_trait(model: &ContractModel) -> TokenStream {
    let vis = &model.vis;
    let name = &model.trait_name;
    let supertraits = if model.supertraits.is_empty() {
        quote!()
    } else {
        let bounds = &model.supertraits;
        quote!(: #bounds)
    };
    let attrs = &model.attrs;
    let methods: Vec<TokenStream> = model.methods.iter().map(generate_trait_method).collect();

    quote! {
        #(#attrs)*
        #[::async_trait::async_trait]
        #vis trait #name #supertraits {
            #(#methods)*
        }
    }
}

fn generate_trait_method(method: &MethodModel) -> TokenStream {
    let attrs = &method.attrs;
    let mut sig = method.sig.clone();

    match method.kind {
        MethodKind::Unary => {}
        MethodKind::ServerStreaming => {
            let output = &method.output_type;
            let error = &method.error_type;

            sig.output = syn::parse_quote! {
                -> ::std::pin::Pin<Box<
                    dyn ::futures_core::Stream<Item = Result<#output, #error>> + Send + 'static
                >>
            };
        }
    }

    quote! {
        #(#attrs)*
        #sig;
    }
}

fn generate_descriptor(model: &ContractModel, support: &TokenStream) -> TokenStream {
    let trait_name = &model.trait_name;
    let trait_name_str = trait_name.to_string();
    let descriptor_name = format_ident!("{}_DESCRIPTOR", trait_name_str.to_shouty_snake_case());
    let gear = &model.gear;
    let version = &model.version;
    let vis = &model.vis;

    let method_descriptors: Vec<TokenStream> = model
        .methods
        .iter()
        .map(|m| {
            let name_str = m.name.to_string();
            let kind = method_kind_tokens(m.kind, support);
            let idempotency = idempotency_tokens(m.idempotency, support);
            let input_type_str = m
                .params
                .last()
                .map(|p| type_name_str(&p.ty))
                .unwrap_or_default();
            let output_type_str = type_name_str(&m.output_type);

            quote! {
                #support::descriptor::MethodDescriptor {
                    name: #name_str,
                    kind: #kind,
                    idempotency: #idempotency,
                    input_type: #input_type_str,
                    output_type: #output_type_str,
                }
            }
        })
        .collect();

    let kind = contract_kind_tokens(model.kind, support);
    let trait_doc = format!("Static descriptor for [`{trait_name_str}`].");
    quote! {
        #[doc = #trait_doc]
        #vis static #descriptor_name: #support::descriptor::ContractDescriptor =
            #support::descriptor::ContractDescriptor {
                gear: #gear,
                contract: #trait_name_str,
                service: #trait_name_str,
                version: #version,
                kind: #kind,
                methods: &[
                    #(#method_descriptors),*
                ],
            };
    }
}

fn contract_kind_tokens(kind: ContractKind, support: &TokenStream) -> TokenStream {
    match kind {
        ContractKind::Api => quote!(#support::descriptor::ContractKind::Api),
        ContractKind::Embedded => quote!(#support::descriptor::ContractKind::Embedded),
        ContractKind::Backend => quote!(#support::descriptor::ContractKind::Backend),
        ContractKind::Extension => quote!(#support::descriptor::ContractKind::Extension),
    }
}

fn generate_ir_function(model: &ContractModel, support: &TokenStream) -> TokenStream {
    let trait_name = &model.trait_name;
    let trait_name_str = trait_name.to_string();
    let fn_name = format_ident!("{}_ir", trait_name_str.to_snake_case());
    let gear = &model.gear;
    let version = &model.version;
    let vis = &model.vis;
    let method_irs: Vec<TokenStream> = model
        .methods
        .iter()
        .map(|m| generate_method_ir(m, support))
        .collect();

    let fn_doc = format!("Build the Contract IR for [`{trait_name_str}`].");
    quote! {
        #[doc = #fn_doc]
        #[must_use]
        #vis fn #fn_name() -> #support::ir::contract::ContractIr {
            #support::ir::contract::ContractIr {
                name: #trait_name_str.to_owned(),
                gear: #gear.to_owned(),
                version: #version.to_owned(),
                methods: vec![
                    #(#method_irs),*
                ],
            }
        }
    }
}

fn generate_method_ir(method: &MethodModel, support: &TokenStream) -> TokenStream {
    let name_str = method.name.to_string();
    let kind = method_kind_tokens(method.kind, support);
    let idempotency = idempotency_tokens(method.idempotency, support);

    let fields: Vec<TokenStream> = method
        .params
        .iter()
        .map(|p| {
            let p_name = p.name.to_string();
            let ty_ref = type_to_typeref(&p.ty, support);
            let is_optional = is_option_type(&p.ty);
            let role = param_role_tokens(p.role, support);
            quote! {
                #support::ir::contract::FieldIr {
                    name: #p_name.to_owned(),
                    ty: #ty_ref,
                    optional: #is_optional,
                    role: #role,
                }
            }
        })
        .collect();

    let output_ref = type_to_typeref(&method.output_type, support);
    let error_ref = type_to_typeref(&method.error_type, support);
    let optional = method.optional;

    quote! {
        #support::ir::contract::MethodIr {
            name: #name_str.to_owned(),
            kind: #kind,
            input: #support::ir::contract::InputShape {
                fields: vec![
                    #(#fields),*
                ],
            },
            output: #output_ref,
            error: Some(#error_ref),
            idempotency: #idempotency,
            optional: #optional,
        }
    }
}

fn generate_contract_impl(model: &ContractModel, support: &TokenStream) -> TokenStream {
    let trait_name = &model.trait_name;
    let trait_name_str = trait_name.to_string();
    let descriptor_name = format_ident!("{}_DESCRIPTOR", trait_name_str.to_shouty_snake_case());
    let fn_name = format_ident!("{}_ir", trait_name_str.to_snake_case());

    quote! {
        impl #support::contract::Contract for dyn #trait_name {
            fn descriptor() -> &'static #support::descriptor::ContractDescriptor {
                &#descriptor_name
            }

            fn contract_ir() -> #support::ir::contract::ContractIr {
                #fn_name()
            }
        }
    }
}

fn method_kind_tokens(kind: MethodKind, support: &TokenStream) -> TokenStream {
    match kind {
        MethodKind::Unary => quote!(#support::ir::contract::MethodKind::Unary),
        MethodKind::ServerStreaming => quote!(#support::ir::contract::MethodKind::ServerStreaming),
    }
}

fn param_role_tokens(role: ParamRole, support: &TokenStream) -> TokenStream {
    match role {
        ParamRole::Wire => quote!(#support::ir::contract::FieldRole::Wire),
        ParamRole::SecurityContext => {
            quote!(#support::ir::contract::FieldRole::SecurityContext)
        }
    }
}

fn idempotency_tokens(idempotency: Idempotency, support: &TokenStream) -> TokenStream {
    match idempotency {
        Idempotency::SafeRead => quote!(#support::ir::contract::Idempotency::SafeRead),
        Idempotency::IdempotentWrite => {
            quote!(#support::ir::contract::Idempotency::IdempotentWrite)
        }
        Idempotency::NonIdempotentWrite => {
            quote!(#support::ir::contract::Idempotency::NonIdempotentWrite)
        }
    }
}

fn type_to_typeref(ty: &syn::Type, support: &TokenStream) -> TokenStream {
    if let syn::Type::Path(type_path) = ty
        && let Some(last_seg) = type_path.path.segments.last()
    {
        let ident_str = last_seg.ident.to_string();
        match ident_str.as_str() {
            "String" => {
                return quote!(#support::ir::contract::TypeRef::Primitive(#support::ir::contract::PrimitiveType::String));
            }
            "i32" => {
                return quote!(#support::ir::contract::TypeRef::Primitive(#support::ir::contract::PrimitiveType::I32));
            }
            "i64" => {
                return quote!(#support::ir::contract::TypeRef::Primitive(#support::ir::contract::PrimitiveType::I64));
            }
            "u64" => {
                return quote!(#support::ir::contract::TypeRef::Primitive(#support::ir::contract::PrimitiveType::U64));
            }
            "f64" => {
                return quote!(#support::ir::contract::TypeRef::Primitive(#support::ir::contract::PrimitiveType::F64));
            }
            "bool" => {
                return quote!(#support::ir::contract::TypeRef::Primitive(#support::ir::contract::PrimitiveType::Bool));
            }
            "Uuid" => {
                return quote!(#support::ir::contract::TypeRef::Primitive(#support::ir::contract::PrimitiveType::Uuid));
            }
            "Option" => {
                if let Some(inner) = extract_single_generic_arg(last_seg) {
                    let inner_ref = type_to_typeref(inner, support);
                    return quote!(#support::ir::contract::TypeRef::Optional(Box::new(#inner_ref)));
                }
            }
            "Vec" => {
                if let Some(inner) = extract_single_generic_arg(last_seg) {
                    let inner_ref = type_to_typeref(inner, support);
                    return quote!(#support::ir::contract::TypeRef::List(Box::new(#inner_ref)));
                }
            }
            other => {
                let name = (*other).to_owned();
                return quote!(#support::ir::contract::TypeRef::Named(#name.to_owned()));
            }
        }
    }

    let name = quote!(#ty).to_string();
    quote!(#support::ir::contract::TypeRef::Named(#name.to_owned()))
}

fn extract_single_generic_arg(seg: &syn::PathSegment) -> Option<&syn::Type> {
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let first = args.args.first()?;
    let syn::GenericArgument::Type(ty) = first else {
        return None;
    };
    Some(ty)
}

fn is_option_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(type_path) = ty
        && let Some(last_seg) = type_path.path.segments.last()
    {
        return last_seg.ident == "Option";
    }
    false
}

fn type_name_str(ty: &syn::Type) -> String {
    if let syn::Type::Path(type_path) = ty
        && let Some(last_seg) = type_path.path.segments.last()
    {
        return last_seg.ident.to_string();
    }
    quote!(#ty).to_string()
}
