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

/// The marker macro name recognised inside `type_id = ...` / `id: ...` /
/// `"id": ...` arguments. Mirrors upstream `gts_macros::gts_id`. When the
/// wrapper sees `gts_id!("<suffix>")` it expands it to the full id literal
/// by prepending the compile-time `gts_id::GTS_ID_PREFIX`.
const PREFIX_MACRO: &str = "gts_id";

/// Build a full-id `LitStr` from a suffix written inside `gts_id!("<suffix>")`,
/// preserving the suffix literal's span for diagnostics.
fn build_prefixed_lit(suffix: &LitStr) -> LitStr {
    LitStr::new(
        &format!("{}{}", gts_id::GTS_ID_PREFIX, suffix.value()),
        suffix.span(),
    )
}

/// Resolve a `gts_id` marker path (possibly qualified: `toolkit_gts::gts_id`,
/// `gts_macros::gts_id`, etc.) — returns `true` if it ends in `gts_id`.
fn is_prefix_macro_path(path: &syn::Path) -> bool {
    path.segments
        .last()
        .is_some_and(|seg| seg.ident == PREFIX_MACRO)
}

/// A GTS ID written either as a full literal or as the prefix-safe
/// `gts_id!("<suffix>")` marker.
///
/// Keep this distinction until code generation. In particular, inventory
/// registrations must re-emit the marker rather than a proc-macro-created
/// full literal: that keeps `GTS_ID_PREFIX` configurable at the consuming
/// crate's compile time and avoids materialising a hard-coded prefix in
/// generated code.
enum GtsIdInput {
    Literal(LitStr),
    PrefixMacro(LitStr),
}

impl GtsIdInput {
    /// Full literal used only for compile-time metadata calculations.
    fn full_lit(&self) -> LitStr {
        match self {
            Self::Literal(lit) => lit.clone(),
            Self::PrefixMacro(suffix) => build_prefixed_lit(suffix),
        }
    }

    /// Expression emitted into generated Rust code.
    fn emitted_expr(&self, crate_path: &TokenStream2) -> TokenStream2 {
        match self {
            Self::Literal(lit) => quote!(#lit),
            Self::PrefixMacro(suffix) => quote!(#crate_path::gts_id!(#suffix)),
        }
    }
}

/// Parse an expression that is either a plain string literal or the
/// `gts_id!("<suffix>")` marker form. Mirrors upstream's accepted input
/// shapes while retaining the marker for generated code.
fn parse_gts_id_input(expr: &syn::Expr) -> syn::Result<GtsIdInput> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Ok(GtsIdInput::Literal(s.clone())),
        syn::Expr::Macro(syn::ExprMacro { mac, .. }) if is_prefix_macro_path(&mac.path) => {
            let suffix: LitStr = mac.parse_body().map_err(|_| {
                syn::Error::new_spanned(
                    mac,
                    format!(
                        "`{PREFIX_MACRO}!` takes a single string-literal suffix, \
                         e.g. `{PREFIX_MACRO}!(\"x.core.events.topic.v1~\")`"
                    ),
                )
            })?;
            Ok(GtsIdInput::PrefixMacro(suffix))
        }
        other => Err(syn::Error::new_spanned(
            other,
            format!("expected a string literal or `{PREFIX_MACRO}!(\"...\")`"),
        )),
    }
}

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

/// Walk the attribute token stream and pull out the `type_id = "..."` (or
/// `type_id = gts_id!("...")`) pair. Used to populate
/// `InventoryTypeSchema::type_id` — the only piece of information the
/// wrapper needs from the attribute. Everything else is forwarded verbatim
/// and parsed by upstream. Both the literal and the `gts_id!("<suffix>")`
/// marker forms are accepted. The marker is retained for the emitted
/// inventory registration while its full form is used only internally.
fn extract_type_id(attr: &TokenStream2) -> syn::Result<GtsIdInput> {
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
            // Collect tokens until the next top-level `,` — this is the
            // value expression (a string literal or `gts_id!("...")`).
            let mut value_tokens: TokenStream2 = TokenStream2::new();
            while let Some(nt) = iter.peek().cloned() {
                match &nt {
                    TokenTree::Punct(p) if p.as_char() == ',' => break,
                    _ => {
                        value_tokens.extend([iter.next().unwrap()]);
                    }
                }
            }
            let expr: syn::Expr = parse2(value_tokens).map_err(|e| {
                syn::Error::new_spanned(
                    &tt,
                    format!(
                        "`type_id = ...` must be a string literal or `{PREFIX_MACRO}!(\"...\")`: {e}"
                    ),
                )
            })?;
            return parse_gts_id_input(&expr);
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
///     type_id = gts_id!("cf.toolkit.plugins.plugin.v1~"),
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
    let type_id_input = extract_type_id(attr)?;
    let type_id = type_id_input.emitted_expr(&crate_path);
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
                type_id: #type_id,
                schema_fn: || #schema_fn_body,
            }
        }
    })
}

// =====================================================================
//                  gts_id! — pass-through to upstream
// =====================================================================

/// Construct a full GTS identifier string from a suffix literal.
///
/// If the literal does **not** already start with `GTS_ID_PREFIX`, the
/// prefix is prepended by delegating to upstream `gts_macros::gts_id!`.
/// If it **does** already start with `GTS_ID_PREFIX`, the literal is
/// emitted as-is — the prefix is not doubled.
///
/// `gts_id!("x.core.events.topic.v1~")` expands to a `&'static str`
/// literal equal to
/// `concat!(GTS_ID_PREFIX, "x.core.events.topic.v1~")` — i.e. the
/// configured prefix (`gts.` by default, overridable via the
/// `GTS_ID_PREFIX` environment variable at compile time) followed by the
/// given suffix.
///
/// The same `gts_id!("...")` form is also recognised as a marker inside
/// the `type_id`/`id` arguments of `#[gts_type_schema]`,
/// `toolkit_gts::gts_instance!`, and `toolkit_gts::gts_instance_raw!`,
/// so identifiers can be written prefix-free everywhere.
///
/// ```ignore
/// use toolkit_gts::gts_id;
///
/// let id: &str = gts_id!("acme.core.events.topic.v1~ven.app.x.v1");
/// ```
#[proc_macro]
pub fn gts_id(input: TokenStream) -> TokenStream {
    let suffix = parse_macro_input!(input as LitStr);
    let already_prefixed = suffix.value().starts_with(gts_id::GTS_ID_PREFIX);
    match resolve_crate_path() {
        Ok(crate_path) => {
            if already_prefixed {
                if let Err(e) = gts_id::GtsIdPattern::try_new(&suffix.value()) {
                    return syn::Error::new_spanned(
                        &suffix,
                        format!("gts_id!: invalid GTS ID pattern: {e}"),
                    )
                    .to_compile_error()
                    .into();
                }
                quote!(#suffix)
            } else {
                quote!(#crate_path::__private::upstream_gts_id!(#suffix))
            }
        }
        Err(e) => e.to_compile_error(),
    }
    .into()
}

// =====================================================================
//                  gts_uri! — URI-prefixed GTS IDs
// =====================================================================

/// Construct a GTS URI string from a GTS ID suffix literal, a literal that
/// already includes `GTS_ID_PREFIX` or `GTS_ID_URI_PREFIX`, or a runtime GTS
/// ID expression.
///
/// `gts_uri!("x.core.events.topic.v1~")` expands to a `&'static str`
/// literal equal to `GTS_ID_URI_PREFIX + toolkit_gts::gts_id!(suffix)`.
/// With the default configuration that is
/// `"gts://gts.x.core.events.topic.v1~"`.
///
/// If the literal already starts with `GTS_ID_URI_PREFIX` (e.g.
/// `"gts://gts.x.core.events.topic.v1~"`), it is emitted as-is.
///
/// If the literal already starts with `GTS_ID_PREFIX` (e.g.
/// `"gts.x.core.events.topic.v1~"`), the prefix is not added again — only
/// `GTS_ID_URI_PREFIX` is prepended.
///
/// The suffix is validated by [`gts_id!`], which delegates to upstream
/// `gts_macros::gts_id!` after applying the configured `GTS_ID_PREFIX`.
/// Non-literal expressions expand to a `String`: if the value already starts
/// with `GTS_ID_URI_PREFIX` it is returned as-is; otherwise
/// `GTS_ID_URI_PREFIX` is prepended, and `GTS_ID_PREFIX` is also inserted if
/// not already present.
///
/// ```ignore
/// use toolkit_gts::gts_uri;
///
/// let schema_uri: &str = gts_uri!("acme.core.events.topic.v1~");
/// let runtime_uri: String = gts_uri!(schema_id);
/// ```
#[proc_macro]
pub fn gts_uri(input: TokenStream) -> TokenStream {
    let expr = parse_macro_input!(input as syn::Expr);
    if let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(suffix),
        ..
    }) = &expr
    {
        let already_uri = suffix.value().starts_with(gts::GTS_ID_URI_PREFIX);
        return match resolve_crate_path() {
            Ok(crate_path) => {
                if already_uri {
                    let id_part = &suffix.value()[gts::GTS_ID_URI_PREFIX.len()..];
                    if let Err(e) = gts_id::GtsIdPattern::try_new(id_part) {
                        return syn::Error::new_spanned(
                            suffix,
                            format!("gts_uri!: invalid GTS ID pattern: {e}"),
                        )
                        .to_compile_error()
                        .into();
                    }
                    quote!(#suffix)
                } else {
                    let uri_prefix = LitStr::new(gts::GTS_ID_URI_PREFIX, suffix.span());
                    quote!(::std::concat!(#uri_prefix, #crate_path::gts_id!(#suffix)))
                }
            }
            Err(e) => e.to_compile_error(),
        }
        .into();
    }

    match resolve_crate_path() {
        Ok(crate_path) => quote!({
            let __v = #expr;
            if __v.starts_with(#crate_path::GTS_ID_URI_PREFIX) {
                ::std::format!("{}", __v)
            } else if __v.starts_with(#crate_path::GTS_ID_PREFIX) {
                ::std::format!("{}{}", #crate_path::GTS_ID_URI_PREFIX, __v)
            } else {
                ::std::format!("{}{}{}", #crate_path::GTS_ID_URI_PREFIX, #crate_path::GTS_ID_PREFIX, __v)
            }
        }),
        Err(e) => e.to_compile_error(),
    }
    .into()
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
                "expected a struct literal: `StructPath { id: gts_id!(\"...\"), ...other fields }`",
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

/// Locate the GTS id field in a struct expression.
fn extract_id_input(instance: &ExprStruct) -> syn::Result<GtsIdInput> {
    let mut found: Option<GtsIdInput> = None;
    for field in &instance.fields {
        let syn::Member::Named(ident) = &field.member else {
            continue;
        };
        if !ID_FIELD_NAMES.contains(&ident.to_string().as_str()) {
            continue;
        }
        let id_input = parse_gts_id_input(&field.expr)?;
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                field,
                "ambiguous id field: only one of `id`, `gts_id`, `gtsId` may be set",
            ));
        }
        found = Some(id_input);
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
///         id: gts_id!("cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read.v1"),
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
///     AuthzPermissionV1 { id: gts_id!("..."), /* ... */ }
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
    let id_input = extract_id_input(&parsed.instance)?;
    let type_id_lit = instance_id_prefix(&id_input.full_lit());
    let instance_id = id_input.emitted_expr(&crate_path);
    // Reconstruct the derived type through `gts_id!` so the generated
    // inventory literal stays prefix-safe as well.
    let type_id = quote!(#crate_path::gts_id!(#type_id_lit));
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
                type_id: #type_id,
                instance_id: #instance_id,
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
///     "id": gts_id!("cf.core.events.topic.v1~cf.core._.audit.v1"),
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
/// `"id"` key's value, accepting either a plain string literal or the
/// `gts_id!("...")` marker form. The wrapper retains the marker for the
/// generated `InventoryInstance` fields and only expands it internally when
/// deriving the instance's type ID.
fn extract_raw_id_input(body: &TokenStream2) -> syn::Result<GtsIdInput> {
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
        // Collect tokens until the next top-level `,` — this is the value
        // expression (a string literal or `gts_id!("...")`).
        let mut value_tokens: TokenStream2 = TokenStream2::new();
        while let Some(nt) = iter.peek().cloned() {
            match &nt {
                TokenTree::Punct(p) if p.as_char() == ',' => break,
                _ => {
                    value_tokens.extend([iter.next().unwrap()]);
                }
            }
        }
        let expr: syn::Expr = parse2(value_tokens).map_err(|e| {
            syn::Error::new_spanned(
                &tt,
                format!(
                    "`\"id\"` must be a string literal or `{PREFIX_MACRO}!(\"...\")` \
                     containing the full GTS instance id: {e}"
                ),
            )
        })?;
        return parse_gts_id_input(&expr);
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
    let id_input = extract_raw_id_input(&body_tokens)?;
    let type_id_lit = instance_id_prefix(&id_input.full_lit());
    let instance_id = id_input.emitted_expr(&crate_path);
    let type_id = quote!(#crate_path::gts_id!(#type_id_lit));

    Ok(quote! {
        #crate_path::inventory::submit! {
            #crate_path::InventoryInstance {
                type_id: #type_id,
                instance_id: #instance_id,
                payload_fn: || #crate_path::__private::upstream_gts_instance_raw!({
                    #body_tokens
                }),
            }
        }
    })
}
