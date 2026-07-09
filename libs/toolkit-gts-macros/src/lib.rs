#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
// Proc macros run at compile time, so panics become compile errors.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Proc-macros for the `toolkit-gts` crate.
//!
//! Thin wrappers around the upstream `gts-macros` crate. Each wrapper
//! delegates the full GTS construction / validation to its upstream
//! counterpart and adds exactly one extra emission: an `inventory::submit!`
//! block that registers the GTS Type Schema or Instance into the process-wide
//! `toolkit-gts` collectors. Every other concern — id validation, prefix
//! const-asserts, `id`-field rewriting, `pub static` binding emission —
//! belongs to upstream.
//!
//! - **`#[gts_type_schema(...)]`** — attribute macro applied to a struct.
//!   Forwards all attrs verbatim to `gts_macros::struct_to_gts_schema` and
//!   submits an `InventoryTypeSchema` entry (Type Schema record).
//! - **`gts_instance! { ... }`** — typed Instance. Forwards verbatim to
//!   `gts_macros::gts_instance!` and submits an `InventoryInstance`.
//! - **`gts_instance_raw! { ... }`** — raw-JSON Instance. Forwards verbatim
//!   to `gts_macros::gts_instance_raw!` and submits an `InventoryInstance`.

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, TokenStream as TokenStream2, TokenTree};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, ExprStruct, Ident, ItemStruct, LitStr, parse_macro_input, parse2};

const TOOLKIT_GTS_PKG: &str = "cf-gears-toolkit-gts";
const TOOLKIT_GTS_LIB: &str = "toolkit_gts";

/// Resolves the path to the `toolkit_gts` crate at the expansion site.
///
/// Mirrors the `proc-macro-crate` dance used elsewhere in the workspace:
/// inside the `toolkit_gts` crate itself (integration tests), returns the
/// lib name; otherwise delegates to `proc_macro_crate`.
fn resolve_crate_path() -> syn::Result<TokenStream2> {
    let in_self = std::env::var("CARGO_PKG_NAME").is_ok_and(|p| p == TOOLKIT_GTS_PKG);
    if in_self {
        let is_lib = std::env::var("CARGO_CRATE_NAME").is_ok_and(|c| c == TOOLKIT_GTS_LIB);
        if is_lib {
            return Ok(quote!(crate));
        }
        let ident = Ident::new(TOOLKIT_GTS_LIB, proc_macro2::Span::call_site());
        return Ok(quote!(::#ident));
    }

    match proc_macro_crate::crate_name(TOOLKIT_GTS_PKG) {
        Ok(proc_macro_crate::FoundCrate::Itself) => Ok(quote!(crate)),
        Ok(proc_macro_crate::FoundCrate::Name(n)) => {
            let pkg_normalized = TOOLKIT_GTS_PKG.replace('-', "_");
            let effective = if n == pkg_normalized {
                TOOLKIT_GTS_LIB
            } else {
                &n
            };
            let ident = Ident::new(effective, proc_macro2::Span::call_site());
            Ok(quote!(::#ident))
        }
        Err(_) => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "cf-gears-toolkit-gts must be a direct dependency",
        )),
    }
}

/// Slice the type-id prefix (everything up to and including the last `~`)
/// from a full instance-id literal. Best-effort — upstream does the real
/// validation; the wrapper only needs the slice to populate
/// `InventoryInstance::type_id`.
fn instance_id_prefix(instance_id: &LitStr) -> LitStr {
    let raw = instance_id.value();
    let prefix = match raw.rfind('~') {
        Some(pos) => &raw[..=pos],
        None => "",
    };
    LitStr::new(prefix, instance_id.span())
}

// =====================================================================
//                          #[gts_type_schema(...)]
// =====================================================================

/// Walk the attribute token stream and pull out the `type_id = "..."`
/// pair. Used to populate `InventoryTypeSchema::type_id` — the only piece
/// of information the wrapper needs from the attribute. Everything else
/// is forwarded verbatim and parsed by upstream.
fn extract_type_id(attr: &TokenStream2) -> syn::Result<LitStr> {
    let mut iter = attr.clone().into_iter().peekable();
    while let Some(tt) = iter.next() {
        if let TokenTree::Ident(ident) = &tt
            && ident == "type_id"
        {
            let Some(TokenTree::Punct(p)) = iter.next() else {
                return Err(syn::Error::new_spanned(&tt, "expected `=` after `type_id`"));
            };
            if p.as_char() != '=' {
                return Err(syn::Error::new_spanned(&tt, "expected `=` after `type_id`"));
            }
            let Some(TokenTree::Literal(lit)) = iter.next() else {
                return Err(syn::Error::new_spanned(
                    &tt,
                    "`type_id = ...` must be a string literal",
                ));
            };
            let lit_ts: TokenStream2 = TokenTree::Literal(lit).into();
            return parse2::<LitStr>(lit_ts);
        }
    }
    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "missing `type_id = \"...\"` attribute",
    ))
}

/// Thin wrapper around `gts_macros::struct_to_gts_schema`. Forwards every
/// attribute verbatim and additionally submits an `InventoryTypeSchema` entry
/// pointing at the macro-generated `gts_schema_with_refs_as_string()`
/// accessor.
///
/// The wrapper takes no opinions on the upstream attrs: `dir_path`,
/// `type_id`, `description`, `properties`, and `base` are all required
/// by upstream and are not defaulted here.
///
/// ```ignore
/// #[toolkit_gts::gts_type_schema(
///     dir_path = "schemas",
///     type_id = "gts.cf.toolkit.plugins.plugin.v1~",
///     description = "Base toolkit plugin schema",
///     properties = "id,vendor,priority,properties",
///     base = true,
/// )]
/// pub struct PluginV1<P: gts::GtsSchema> { /* ... */ }
/// ```
#[proc_macro_attribute]
pub fn gts_type_schema(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_ts: TokenStream2 = attr.into();
    let input = parse_macro_input!(item as ItemStruct);
    match expand_gts_type_schema(&attr_ts, &input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_gts_type_schema(attr: &TokenStream2, input: &ItemStruct) -> syn::Result<TokenStream2> {
    let crate_path = resolve_crate_path()?;
    let type_id_lit = extract_type_id(attr)?;
    let struct_name = &input.ident;

    // Generic structs need turbofish on the schema-fn call. Upstream's
    // `gts_schema_with_refs_as_string` is a static method; for a generic
    // carrier `Foo<P>` we always materialise it as `Foo::<()>` since the
    // schema text is invariant in `P`.
    //
    // Reject generic shapes the wrapper can't safely materialise: the
    // turbofish below only fills exactly one type parameter, so multiple
    // type params, lifetimes, or const generics would expand to invalid
    // Rust. All current callsites are zero- or one-parameter; the guard
    // is here to fail loudly if that ever changes.
    let type_param_count = input.generics.type_params().count();
    if input.generics.lifetimes().next().is_some()
        || input.generics.const_params().next().is_some()
        || type_param_count > 1
    {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[gts_type_schema]` supports only structs with zero or one type parameter; \
             lifetimes, const generics, and multiple type parameters are not supported",
        ));
    }
    let has_generics = type_param_count == 1;
    let schema_fn_body = if has_generics {
        quote! { <#struct_name::<()>>::gts_schema_with_refs_as_string() }
    } else {
        quote! { <#struct_name>::gts_schema_with_refs_as_string() }
    };

    Ok(quote! {
        #[#crate_path::__private::upstream_struct_to_gts_schema(#attr)]
        #input

        #crate_path::inventory::submit! {
            #crate_path::InventoryTypeSchema {
                type_id: #type_id_lit,
                schema_fn: || #schema_fn_body,
            }
        }
    })
}

// =====================================================================
//             gts_instance! / gts_instance_raw!
// =====================================================================

/// Parsed shape of `gts_instance!` input — same as upstream:
/// `[#[gts_static(NAME)]]? StructPath { id: "...", ...other fields }`.
///
/// The wrapper parses just enough to extract the `id` literal (for the
/// `InventoryInstance` fields) and to know whether `#[gts_static(...)]`
/// was given (so the additional `pub static` upstream call can be emitted
/// alongside the inventory submission). The struct literal itself and any
/// validation errors are owned by upstream — we forward unchanged.
struct InstanceInput {
    /// Outer attrs as the user wrote them (`#[gts_static(NAME)]` is the
    /// only one upstream accepts; other attrs are upstream-rejected).
    attrs: Vec<Attribute>,
    /// The user's struct literal — must contain an `id` / `gts_id` /
    /// `gtsId` field with a string literal value.
    instance: ExprStruct,
}

impl Parse for InstanceInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let instance: ExprStruct = input.parse().map_err(|e| {
            syn::Error::new(
                e.span(),
                "expected a struct literal: `StructPath { id: \"gts...\", ...other fields }`",
            )
        })?;
        if !input.is_empty() {
            return Err(input.error(
                "unexpected tokens after struct literal; gts_instance! takes a single struct literal optionally preceded by `#[gts_static(...)]`",
            ));
        }
        Ok(Self { attrs, instance })
    }
}

/// Reserved field names for the GTS instance-id slot (mirrors upstream).
const ID_FIELD_NAMES: &[&str] = &["id", "gts_id", "gtsId"];

/// Locate the GTS id field's string literal in a struct expression.
fn extract_id_literal(instance: &ExprStruct) -> syn::Result<LitStr> {
    let mut found: Option<LitStr> = None;
    for field in &instance.fields {
        let syn::Member::Named(ident) = &field.member else {
            continue;
        };
        if !ID_FIELD_NAMES.contains(&ident.to_string().as_str()) {
            continue;
        }
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(lit_str),
            ..
        }) = &field.expr
        else {
            return Err(syn::Error::new_spanned(
                &field.expr,
                "GTS id field must be a string literal containing the full instance id (e.g. \"gts.acme.core.events.topic.v1~vendor.app.x.v1\")",
            ));
        };
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                field,
                "ambiguous id field: only one of `id`, `gts_id`, `gtsId` may be set",
            ));
        }
        found = Some(lit_str.clone());
    }
    found.ok_or_else(|| {
        syn::Error::new_spanned(
            &instance.path,
            "missing GTS id field; the struct literal must contain one of: id, gts_id, gtsId",
        )
    })
}

/// Typed GTS instance. Forwards verbatim to `gts_macros::gts_instance!`
/// and additionally submits an `InventoryInstance` entry. The optional
/// `#[gts_static(NAME)]` attribute (item form: emits `pub static NAME:
/// LazyLock<T>`) is recognised by upstream — pass it through unchanged.
///
/// ```ignore
/// toolkit_gts::gts_instance! {
///     AuthzPermissionV1 {
///         id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read.v1",
///         resource_type: "...".to_owned(),
///         action: "read".to_owned(),
///         display_name: "Read chat".to_owned(),
///     }
/// }
/// ```
///
/// With a typed runtime accessor:
///
/// ```ignore
/// toolkit_gts::gts_instance! {
///     #[gts_static(CHAT_READ_PERM)]
///     AuthzPermissionV1 { id: "gts...", /* ... */ }
/// }
///
/// let p: &AuthzPermissionV1 = &CHAT_READ_PERM;
/// ```
#[proc_macro]
pub fn gts_instance(input: TokenStream) -> TokenStream {
    match expand_gts_instance(input.into()) {
        Ok(t) => t.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_gts_instance(input: TokenStream2) -> syn::Result<TokenStream2> {
    let parsed: InstanceInput = parse2(input)?;
    let crate_path = resolve_crate_path()?;
    let id_lit = extract_id_literal(&parsed.instance)?;
    let type_id_lit = instance_id_prefix(&id_lit);
    let instance_struct = &parsed.instance;
    let attrs = &parsed.attrs;

    // payload_fn always uses upstream's expression form — the `#[gts_static]`
    // attribute is item-position only and would clash with returning a
    // value from the closure. The optional static binding is a *separate*
    // upstream call alongside the inventory submission.
    let payload_call = quote! {
        #crate_path::__private::upstream_gts_instance!(#instance_struct)
    };

    let submit_block = quote! {
        #crate_path::inventory::submit! {
            #crate_path::InventoryInstance {
                type_id: #type_id_lit,
                instance_id: #id_lit,
                payload_fn: || ::serde_json::to_value(&#payload_call)
                    .expect("GTS instance must serialize cleanly"),
            }
        }
    };

    if attrs.is_empty() {
        Ok(submit_block)
    } else {
        // Re-emit the original input (attrs + struct literal) for upstream
        // — that's the call that produces `pub static NAME: LazyLock<T>`.
        let static_call = quote! {
            #crate_path::__private::upstream_gts_instance! {
                #(#attrs)*
                #instance_struct
            }
        };
        Ok(quote! {
            #submit_block
            #static_call
        })
    }
}

/// Raw-JSON GTS instance. Forwards verbatim to
/// `gts_macros::gts_instance_raw!` and additionally submits an
/// `InventoryInstance` entry.
///
/// ```ignore
/// toolkit_gts::gts_instance_raw!({
///     "id": "gts.cf.core.events.topic.v1~cf.core._.audit.v1",
///     "name": "audit",
///     "description": "Audit log events",
/// });
/// ```
#[proc_macro]
pub fn gts_instance_raw(input: TokenStream) -> TokenStream {
    match expand_gts_instance_raw(input.into()) {
        Ok(t) => t.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Walk a brace-delimited JSON object literal and locate the top-level
/// `"id"` key's string-literal value. Mirrors upstream's check; the
/// wrapper needs the value for the `InventoryInstance` fields.
fn extract_raw_id_literal(body: &TokenStream2) -> syn::Result<LitStr> {
    let mut iter = body.clone().into_iter().peekable();
    while let Some(tt) = iter.next() {
        // Top-level key must be a string literal.
        let TokenTree::Literal(lit) = &tt else {
            // Skip non-literal top-level tokens (commas, etc.)
            continue;
        };
        let lit_ts: TokenStream2 = TokenTree::Literal(lit.clone()).into();
        let key: LitStr = match parse2::<LitStr>(lit_ts) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if key.value() != "id" {
            // Skip past the value (until next top-level `,`).
            skip_until_comma(&mut iter);
            continue;
        }
        let Some(TokenTree::Punct(p)) = iter.next() else {
            return Err(syn::Error::new_spanned(
                tt,
                "expected `:` after `\"id\"` key",
            ));
        };
        if p.as_char() != ':' {
            return Err(syn::Error::new_spanned(
                tt,
                "expected `:` after `\"id\"` key",
            ));
        }
        let Some(TokenTree::Literal(value_lit)) = iter.next() else {
            return Err(syn::Error::new_spanned(
                tt,
                "`\"id\"` must be a string literal containing the full GTS instance id",
            ));
        };
        let v_ts: TokenStream2 = TokenTree::Literal(value_lit).into();
        return parse2::<LitStr>(v_ts);
    }
    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "missing top-level `\"id\"` key in gts_instance_raw! body",
    ))
}

/// Advance the iterator past the next top-level `,` (or to end of stream).
/// Group token trees are atomic, so commas inside `{...}` / `[...]` are
/// invisible at this level.
fn skip_until_comma(iter: &mut std::iter::Peekable<proc_macro2::token_stream::IntoIter>) {
    while let Some(tt) = iter.peek() {
        if let TokenTree::Punct(p) = tt
            && p.as_char() == ','
        {
            iter.next();
            return;
        }
        iter.next();
    }
}

fn expand_gts_instance_raw(input: TokenStream2) -> syn::Result<TokenStream2> {
    let crate_path = resolve_crate_path()?;

    // Upstream takes `{ ... }` — possibly wrapped in `(...)`. Strip an
    // outer `(...)` group if the user wrote the call-style form, then
    // expect a single brace group.
    let mut iter = input.into_iter();
    let first = iter.next().ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "gts_instance_raw! takes a single brace-delimited JSON object literal",
        )
    })?;
    let body_group = match first {
        TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => g,
        TokenTree::Group(g) if g.delimiter() == Delimiter::Parenthesis => {
            // call-style: (...) wrapping a single { ... }
            let mut inner = g.stream().into_iter();
            match (inner.next(), inner.next()) {
                (Some(TokenTree::Group(inner_g)), None)
                    if inner_g.delimiter() == Delimiter::Brace =>
                {
                    inner_g
                }
                _ => {
                    return Err(syn::Error::new(
                        g.span(),
                        "gts_instance_raw! takes a single brace-delimited JSON object literal",
                    ));
                }
            }
        }
        other => {
            return Err(syn::Error::new_spanned(
                other,
                "gts_instance_raw! takes a single brace-delimited JSON object literal",
            ));
        }
    };
    if let Some(extra) = iter.next() {
        return Err(syn::Error::new_spanned(
            extra,
            "unexpected tokens after body; gts_instance_raw! takes a single brace-delimited JSON object literal",
        ));
    }

    let body_tokens = body_group.stream();
    let id_lit = extract_raw_id_literal(&body_tokens)?;
    let type_id_lit = instance_id_prefix(&id_lit);

    Ok(quote! {
        #crate_path::inventory::submit! {
            #crate_path::InventoryInstance {
                type_id: #type_id_lit,
                instance_id: #id_lit,
                payload_fn: || #crate_path::__private::upstream_gts_instance_raw!({
                    #body_tokens
                }),
            }
        }
    })
}
