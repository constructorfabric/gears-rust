//! Shared parsing + validation for the `#[streaming(...)]` method attribute.
//!
//! One place so the base contract, the REST projection and the gRPC projection
//! agree on two things: the set of legal arguments — a framing selector (`sse` /
//! `multipart_mixed`) and/or an open selector (`open = fallible | immediate`) —
//! and the rule that reconciles an explicit `open` with the method's `async`
//! keyword.
//!
//! `open = fallible` is the greppable, source-of-truth selector for the fallible
//! (awaited) open. `async` is still allowed, but it must *agree* with `open`, so
//! a stray `async` — added or deleted — can no longer silently change the
//! generated client's open shape (#4734). A bare `#[streaming]` with no `open`
//! stays immediate, exactly as it always has.

use proc_macro2::Span;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned as _;
use syn::{Meta, Token};

use crate::model::{StreamFraming, StreamOpen};

/// Parsed `#[streaming(...)]` arguments.
#[derive(Default)]
pub struct StreamingArgs {
    /// Wire framing (`sse` / `multipart_mixed`). Only meaningful on the REST
    /// projection; the base and gRPC parsers reject a `Some` here (no HTTP media
    /// type exists there to configure).
    pub framing: Option<StreamFraming>,
    /// Explicit open selector (`open = fallible | immediate`), if written.
    pub open: Option<StreamOpen>,
}

const EXPECTED_ARGS: &str = "expected `#[streaming]` with an optional framing selector \
    (`sse` or `multipart_mixed`) and/or an open selector (`open = fallible` or \
    `open = immediate`), e.g. `#[streaming(multipart_mixed, open = fallible)]`";

/// Parse the arguments of one `#[streaming(...)]` attribute. A bare
/// `#[streaming]` yields all-`None`.
pub fn parse_streaming_args(attr: &syn::Attribute) -> syn::Result<StreamingArgs> {
    match &attr.meta {
        // Bare `#[streaming]`: the historical form — no framing, no explicit open.
        Meta::Path(_) => return Ok(StreamingArgs::default()),
        Meta::List(_) => {}
        Meta::NameValue(_) => return Err(syn::Error::new_spanned(attr, EXPECTED_ARGS)),
    }

    let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;

    let mut args = StreamingArgs::default();
    for meta in metas {
        match meta {
            // A bare name is a framing selector.
            Meta::Path(path) => {
                let ident = path
                    .get_ident()
                    .ok_or_else(|| syn::Error::new(path.span(), EXPECTED_ARGS))?;
                let framing = match ident.to_string().as_str() {
                    "sse" => StreamFraming::ServerSentEvents,
                    "multipart_mixed" => StreamFraming::MultipartMixed,
                    other => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!(
                                "unknown stream framing `{other}`; expected sse or multipart_mixed"
                            ),
                        ));
                    }
                };
                if args.framing.replace(framing).is_some() {
                    return Err(syn::Error::new(ident.span(), "duplicate framing selector"));
                }
            }
            // `open = fallible | immediate`.
            Meta::NameValue(nv) if nv.path.is_ident("open") => {
                let open = parse_open_value(&nv.value)?;
                if args.open.replace(open).is_some() {
                    return Err(syn::Error::new(nv.path.span(), "duplicate `open` selector"));
                }
            }
            other => return Err(syn::Error::new(other.span(), EXPECTED_ARGS)),
        }
    }
    Ok(args)
}

fn parse_open_value(value: &syn::Expr) -> syn::Result<StreamOpen> {
    let ident = match value {
        syn::Expr::Path(p) => p.path.get_ident().cloned(),
        _ => None,
    };
    let ident = ident.ok_or_else(|| {
        syn::Error::new(
            value.span(),
            "expected `open = fallible` or `open = immediate`",
        )
    })?;
    match ident.to_string().as_str() {
        "fallible" => Ok(StreamOpen::Awaited),
        "immediate" => Ok(StreamOpen::Immediate),
        other => Err(syn::Error::new(
            ident.span(),
            format!("unknown open selector `{other}`; expected fallible or immediate"),
        )),
    }
}

/// Reconcile an explicit `open = ...` selector with the method's `async fn`,
/// returning the resolved [`StreamOpen`]. See the module docs for the rule; in
/// short, `open = fallible` and `async` must agree, and a stray `async` with no
/// explicit `open` is rejected rather than silently selecting the fallible open.
///
/// `span` locates the diagnostic on the method (its identifier).
pub fn resolve_stream_open(
    explicit: Option<StreamOpen>,
    is_async: bool,
    span: Span,
) -> syn::Result<StreamOpen> {
    match (explicit, is_async) {
        (Some(StreamOpen::Awaited), true) => Ok(StreamOpen::Awaited),
        (None | Some(StreamOpen::Immediate), false) => Ok(StreamOpen::Immediate),
        (Some(StreamOpen::Awaited), false) => Err(syn::Error::new(
            span,
            "`#[streaming(open = fallible)]` requires `async fn`: the fallible open is an awaited \
             operation. Add `async`, or drop `open = fallible` for the immediate \
             (synchronous-handback) shape.",
        )),
        (Some(StreamOpen::Immediate), true) => Err(syn::Error::new(
            span,
            "`#[streaming(open = immediate)]` conflicts with `async fn`. Remove `async` for the \
             immediate shape, or write `open = fallible` for the awaited fallible open.",
        )),
        (None, true) => Err(syn::Error::new(
            span,
            "`async fn` on a streaming method selects the fallible open, which must be stated \
             explicitly: write `#[streaming(open = fallible)]` (on a REST projection a framing \
             selector may accompany it, e.g. `#[streaming(multipart_mixed, open = fallible)]`; \
             gRPC framing is fixed, so no selector is allowed there). Remove `async` for the \
             immediate shape.",
        )),
    }
}
