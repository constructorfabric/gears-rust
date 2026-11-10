//! Parsing for `#[rest_contract]`.
//!
//! Recognized attributes on trait methods:
//! - `#[get("/path/{param}")]`, `#[post(...)]`, `#[put(...)]`, `#[delete(...)]`
//! - `#[retryable]` — marks the method as safe to retry on transport failure.
//! - `#[streaming]`, `#[streaming(sse)]`, `#[streaming(multipart_mixed)]` —
//!   marks the method as server-streaming and selects the wire framing. A bare
//!   `#[streaming]` is SSE. The method's `asyncness` selects the *open shape*
//!   (mirroring the base macro in `parse.rs`): a `#[streaming] async fn` emits
//!   the fallible open `-> Result<Stream, E>`, a non-`async` `#[streaming] fn`
//!   emits the immediate open `-> Stream`. This parser previously ignored
//!   `asyncness` and normalised `async` away, so a stray `async` on a REST
//!   streaming projection now silently changes the emitted signature — audit
//!   existing projections (see DESIGN.md, #4740).
//! - `#[server_manual]` — skip server-route generation for this method.
//! - `#[client_manual]` — the generated client delegates this method to the
//!   projection's sealed `{Projection}Manual` handler (ADR-0009).
//! - `#[passthrough("/prefix/{*rest}", methods = [get, post, ..])]` — instead
//!   of a verb: forward an opaque `http::Request` whose method is one of
//!   `methods` and whose path fills the trailing wildcard. Requires
//!   `#[client_manual]` and `#[server_manual]`.
//! - `#[exposed]` / `#[internal]` — edge visibility, overriding the trait-level
//!   `visibility` default. Internal unless said otherwise.
//! - `#[anonymous]` — the route requires no authentication.
//!
//! The trait's first non-marker supertrait is recorded as the *base contract*
//! the projection refines (e.g. `pub trait PaymentServiceRest: PaymentService`).

use proc_macro2::Span;
use syn::ext::IdentExt as _;
use syn::spanned::Spanned;
use syn::{Ident, ItemTrait, ReturnType, TraitItem, TraitItemFn, Type};

use crate::model::{MethodShape, StreamFraming};

/// Parsed arguments of the `#[rest_contract(...)]` attribute.
pub struct RestContractAttr {
    /// Base URL path prepended to every method's path template.
    pub base_path: String,
    /// `#[toolkit::rest_contract(base_path = "...", require_full_coverage)]`
    /// (ADR-0003): opt into a generated coverage assertion that every base-trait
    /// method has a REST binding (and vice-versa).
    pub require_full_coverage: bool,
    /// Default edge visibility for every method in the trait, from
    /// `visibility = "exposed" | "internal"`. Internal when absent, matching
    /// `OperationBuilder`'s own default. Per-method `#[exposed]` / `#[internal]`
    /// override it.
    pub default_exposed: bool,
    /// `manual_handler = path::Type`: the SDK's default `{Projection}Manual`
    /// implementation, which keeps the client's `new` constructors.
    pub manual_handler: Option<syn::Path>,
}

/// Parsed REST projection of a contract trait, ready for codegen.
pub struct RestContractModel {
    /// The original projection trait item as written by the user.
    pub item: ItemTrait,
    /// Identifier of the projection trait.
    pub trait_ident: Ident,
    /// Path of the base contract trait this projection mirrors.
    pub base_trait: syn::Path,
    /// Base URL path prepended to every method's path template.
    pub base_path: String,
    /// Whether a coverage assertion between base-trait methods and REST bindings is generated.
    pub require_full_coverage: bool,
    /// Trait-level default edge visibility; `true` means exposed, `false` internal.
    pub default_exposed: bool,
    /// Default `{Projection}Manual` implementation, from `manual_handler`.
    pub manual_handler: Option<syn::Path>,
    /// Projected methods in declaration order.
    pub methods: Vec<RestMethodModel>,
}

impl RestContractModel {
    /// Whether any method is `#[client_manual]`, which makes the generated
    /// client generic over its handler.
    pub fn has_client_manual(&self) -> bool {
        self.methods.iter().any(|m| m.client_manual)
    }
}

/// A single method of the REST projection.
#[allow(
    clippy::struct_excessive_bools,
    reason = "these are independent per-method projection flags (retryable / streaming / optional / server_manual / client_manual / anonymous) parsed from distinct attributes; a bitflags enum would obscure rather than clarify the 1:1 attribute mapping"
)]
pub struct RestMethodModel {
    /// Rust method identifier.
    pub ident: Ident,
    /// HTTP verb the method is bound to.
    pub http_method: HttpVerb,
    /// Path template relative to the base path, with `{placeholder}` segments.
    pub path_template: String,
    /// Whether the client may retry the call on transient failures.
    pub retryable: bool,
    /// Unary, or server-streaming with how its stream is opened
    /// (`#[streaming] fn` → `Stream(Immediate)`, `#[streaming] async fn` →
    /// `Stream(Awaited)`). Folds what was a `streaming: bool` + `open:
    /// StreamOpen` pair, so an `open` exists exactly when the method streams
    /// (#4740).
    pub shape: MethodShape,
    /// Wire framing selected by `#[streaming(sse | multipart_mixed)]`. A bare
    /// `#[streaming]` means SSE, which is what it has always meant. Meaningless
    /// for a `MethodShape::Unary` method, where it stays at its default.
    pub stream_framing: StreamFraming,
    /// Parameters of the method (path, query and body), excluding the receiver.
    pub params: Vec<RestParam>,
    /// `Some((ok_ty, err_ty))` extracted from the method's `Result<T, E>`
    /// return type. Populated for **both** unary and streaming methods (a
    /// streaming method is authored as `Result<Item, E>` and the macro rewrites
    /// the emitted signature to `Pin<Box<dyn Stream<Item = Result<Item, E>>>>`).
    pub result_types: Option<(Type, Type)>,
    /// `true` when the projection method declares a default body — peers
    /// MAY omit this endpoint (mirrored into `HttpMethodBindingIr.optional`).
    pub optional: bool,
    /// `true` when the method is marked `#[server_manual]` — the server-side
    /// route generator (`register_<trait>_routes()`) SKIPS this method so the
    /// author can register it by hand via `OperationBuilder`. The method stays
    /// in the generated client and the binding IR.
    pub server_manual: bool,
    /// `true` when the method is marked `#[client_manual]` — the generated
    /// client calls the projection's `{Projection}Manual` handler instead of
    /// encoding the request itself. The method stays in the binding IR.
    pub client_manual: bool,
    /// Present for a `#[passthrough(..)]` method (`http_method` is then
    /// [`HttpVerb::Passthrough`]).
    pub passthrough: Option<PassthroughSpec>,
    /// Edge visibility override from `#[exposed]` / `#[internal]`. `None` means
    /// inherit the trait-level `visibility` default — which is why this is a
    /// tri-state and not a plain `bool`: under an `exposed` default a method
    /// still needs a way back to internal.
    pub exposed: Option<bool>,
    /// `true` when the method is marked `#[anonymous]` — the generated route
    /// emits `.anonymous()` instead of `.authenticated()`. Independent of
    /// visibility: an exposed route may still require a JWT, and an anonymous
    /// one may stay internal.
    pub anonymous: bool,
}

/// The `#[passthrough(..)]` binding of a method.
pub struct PassthroughSpec {
    /// Name of the trailing `{*param}` wildcard.
    pub param: String,
    /// The opaque request parameter.
    pub field: Ident,
    /// Methods the request may use, in declaration order.
    pub methods: Vec<HttpVerb>,
}

/// A parameter of a REST projection method.
pub struct RestParam {
    /// Parameter name.
    pub ident: Ident,
    /// Parameter type.
    pub ty: Type,
}

/// HTTP verb a REST method is bound to.
#[derive(Clone, Copy)]
pub enum HttpVerb {
    /// HTTP `GET`.
    Get,
    /// HTTP `POST`.
    Post,
    /// HTTP `PUT`.
    Put,
    /// HTTP `PATCH`.
    Patch,
    /// HTTP `DELETE`.
    Delete,
    /// HTTP `HEAD`; only in a passthrough method set.
    Head,
    /// HTTP `OPTIONS`; only in a passthrough method set.
    Options,
    /// `#[passthrough]`: the method comes from the forwarded request.
    Passthrough,
}

mod kw {
    syn::custom_keyword!(base_path);
    syn::custom_keyword!(require_full_coverage);
    syn::custom_keyword!(visibility);
    syn::custom_keyword!(manual_handler);
    syn::custom_keyword!(methods);
}

impl syn::parse::Parse for RestContractAttr {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self {
                base_path: String::new(),
                require_full_coverage: false,
                default_exposed: false,
                manual_handler: None,
            });
        }
        let mut base_path = None;
        let mut require_full_coverage = false;
        let mut default_exposed: Option<bool> = None;
        let mut manual_handler: Option<syn::Path> = None;
        while !input.is_empty() {
            let lookahead = input.lookahead1();
            if lookahead.peek(kw::base_path) {
                let _kw: kw::base_path = input.parse()?;
                let _eq: syn::Token![=] = input.parse()?;
                let lit: syn::LitStr = input.parse()?;
                if base_path.is_some() {
                    return Err(syn::Error::new(lit.span(), "duplicate `base_path`"));
                }
                base_path = Some(lit.value());
            } else if lookahead.peek(kw::require_full_coverage) {
                let kw: kw::require_full_coverage = input.parse()?;
                if require_full_coverage {
                    return Err(syn::Error::new(
                        kw.span,
                        "duplicate `require_full_coverage`",
                    ));
                }
                require_full_coverage = true;
            } else if lookahead.peek(kw::visibility) {
                let _kw: kw::visibility = input.parse()?;
                let _eq: syn::Token![=] = input.parse()?;
                let lit: syn::LitStr = input.parse()?;
                if default_exposed.is_some() {
                    return Err(syn::Error::new(lit.span(), "duplicate `visibility`"));
                }
                default_exposed = Some(match lit.value().as_str() {
                    "exposed" => true,
                    "internal" => false,
                    other => {
                        return Err(syn::Error::new(
                            lit.span(),
                            format!(
                                "unknown visibility `{other}`; expected \"exposed\" or \"internal\""
                            ),
                        ));
                    }
                });
            } else if lookahead.peek(kw::manual_handler) {
                let kw: kw::manual_handler = input.parse()?;
                let _eq: syn::Token![=] = input.parse()?;
                let path: syn::Path = input.parse()?;
                if manual_handler.is_some() {
                    return Err(syn::Error::new(kw.span, "duplicate `manual_handler`"));
                }
                manual_handler = Some(path);
            } else {
                return Err(lookahead.error());
            }
            if input.peek(syn::Token![,]) {
                let _comma: syn::Token![,] = input.parse()?;
            }
        }
        Ok(Self {
            base_path: base_path.unwrap_or_default(),
            require_full_coverage,
            default_exposed: default_exposed.unwrap_or(false),
            manual_handler,
        })
    }
}

/// Builds a [`RestContractModel`] from the attribute arguments and the annotated projection trait.
pub fn parse(attr: RestContractAttr, item: ItemTrait) -> syn::Result<RestContractModel> {
    let trait_ident = item.ident.clone();
    let trait_name = trait_ident.to_string();

    let base_trait = extract_base_trait(&item)?;
    let base_name = base_trait
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();

    // Projection trait name must be `<Base>Rest`.
    let expected_projection = format!("{base_name}Rest");
    if trait_name != expected_projection {
        return Err(syn::Error::new(
            trait_ident.span(),
            format!(
                "rest_contract trait must be named `{expected_projection}` to project base trait `{base_name}` \
                 (PRD #1536 D1: projection trait extends `{{Base}}` and is named `{{Base}}Rest`)"
            ),
        ));
    }

    // Base must be remote-capable (`Api` or `Backend`). Classification is
    // delegated to `ContractKind` so the suffix rule — including its handling of
    // a trailing major-version marker (ADR-0007) — lives in exactly one place.
    let base_kind = crate::model::ContractKind::from_suffix(&base_name);
    if !base_kind.is_some_and(crate::model::ContractKind::is_remote_capable) {
        let suffix = base_kind.map_or("<unknown>", crate::model::ContractKind::suffix);
        return Err(syn::Error::new(
            trait_ident.span(),
            format!(
                "REST projection is not allowed for `{suffix}` contracts; \
                 only `Api` and `Backend` are remote-capable (PRD #1536 D2/D6)"
            ),
        ));
    }

    let mut methods = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for trait_item in &item.items {
        let TraitItem::Fn(method) = trait_item else {
            continue;
        };
        let parsed = parse_method(method)?;
        let key = (
            parsed.http_method.ir_variant().to_owned(),
            parsed.path_template.clone(),
        );
        if !seen.insert(key.clone()) {
            return Err(syn::Error::new(
                method.sig.ident.span(),
                format!(
                    "duplicate REST binding `{} {}`; each method must have a unique (verb, path) pair",
                    key.0.to_uppercase(),
                    key.1
                ),
            ));
        }
        methods.push(parsed);
    }

    if let Some(handler) = &attr.manual_handler
        && !methods.iter().any(|m| m.client_manual)
    {
        return Err(syn::Error::new_spanned(
            handler,
            "`manual_handler` names the default `#[client_manual]` handler, but no method \
             of this projection is `#[client_manual]`",
        ));
    }

    Ok(RestContractModel {
        item,
        trait_ident,
        base_trait,
        base_path: attr.base_path,
        require_full_coverage: attr.require_full_coverage,
        default_exposed: attr.default_exposed,
        manual_handler: attr.manual_handler,
        methods,
    })
}

fn extract_base_trait(item: &ItemTrait) -> syn::Result<syn::Path> {
    for bound in &item.supertraits {
        if let syn::TypeParamBound::Trait(t) = bound {
            return Ok(t.path.clone());
        }
    }
    Err(syn::Error::new(
        item.span(),
        "rest_contract trait must declare its base contract as a supertrait \
         (e.g. `pub trait PaymentServiceRest: PaymentService`)",
    ))
}

/// The method-level attributes of one projection method, before cross-checks.
#[derive(Default)]
struct MethodAttrs {
    http: Option<(HttpVerb, String, Span)>,
    passthrough_methods: Option<Vec<HttpVerb>>,
    retryable: Option<Span>,
    streaming: Option<crate::stream_attr::StreamingArgs>,
    server_manual: bool,
    client_manual: bool,
    exposed: Option<bool>,
    anonymous: Option<Span>,
}

impl MethodAttrs {
    fn collect(method: &TraitItemFn) -> syn::Result<Self> {
        let mut attrs = Self::default();
        for attr in &method.attrs {
            let path = attr.path();
            if let Some(verb) = verb_of(path) {
                attrs.set_verb(verb, attr)?;
            } else if path.is_ident("retryable") {
                attrs.retryable = Some(attr.span());
            } else if path.is_ident("streaming") {
                attrs.set_streaming(attr)?;
            } else if path.is_ident("server_manual") {
                attrs.server_manual = true;
            } else if path.is_ident("client_manual") {
                attrs.client_manual = true;
            } else if path.is_ident("exposed") || path.is_ident("internal") {
                attrs.set_visibility(attr, path.is_ident("exposed"))?;
            } else if path.is_ident("anonymous") {
                attrs.anonymous = Some(attr.span());
            }
        }
        Ok(attrs)
    }

    /// Reject a SECOND verb on the same method (`#[get(..)] #[post(..)]`) —
    /// silently letting the last one win would change the wire contract with
    /// no signal (M-11).
    fn set_verb(&mut self, verb: HttpVerb, attr: &syn::Attribute) -> syn::Result<()> {
        if self.http.is_some() {
            return Err(syn::Error::new(
                attr.span(),
                "duplicate HTTP method annotation: a projection method must declare \
                 exactly one of `#[get]`/`#[post]`/`#[put]`/`#[patch]`/`#[delete]`/\
                 `#[passthrough]`",
            ));
        }
        let passthrough = matches!(verb, HttpVerb::Passthrough);
        let path_lit = if passthrough {
            let (path_lit, methods) = parse_passthrough_args(attr)?;
            self.passthrough_methods = Some(methods);
            path_lit
        } else {
            parse_path_lit(attr)?
        };
        validate_path_template(&path_lit, attr.span(), passthrough)?;
        self.http = Some((verb, path_lit, attr.span()));
        Ok(())
    }

    /// Reject a SECOND `#[streaming]` before it overwrites the framing / open
    /// selectors — last-one-wins would silently change the emitted signature
    /// with no signal, exactly the hazard the verb dedup guards against.
    fn set_streaming(&mut self, attr: &syn::Attribute) -> syn::Result<()> {
        if self.streaming.is_some() {
            return Err(syn::Error::new(
                attr.span(),
                "duplicate `#[streaming]` attribute: a streaming method declares it exactly \
                 once (all framing and open selectors go in that one attribute)",
            ));
        }
        self.streaming = Some(crate::stream_attr::parse_streaming_args(attr)?);
        Ok(())
    }

    /// Both on one method is a contradiction, not a last-one-wins: silently
    /// picking one would change what the gateway serves.
    fn set_visibility(&mut self, attr: &syn::Attribute, wants_exposed: bool) -> syn::Result<()> {
        if self.exposed.is_some_and(|prev| prev != wants_exposed) {
            return Err(syn::Error::new(
                attr.span(),
                "conflicting visibility: a method cannot be both `#[exposed]` and \
                 `#[internal]`. Pick one, or drop both to inherit the trait's \
                 `visibility` default",
            ));
        }
        self.exposed = Some(wants_exposed);
        Ok(())
    }
}

fn verb_of(path: &syn::Path) -> Option<HttpVerb> {
    const VERBS: [(&str, HttpVerb); 6] = [
        ("get", HttpVerb::Get),
        ("post", HttpVerb::Post),
        ("put", HttpVerb::Put),
        ("patch", HttpVerb::Patch),
        ("delete", HttpVerb::Delete),
        ("passthrough", HttpVerb::Passthrough),
    ];
    VERBS
        .iter()
        .find(|(name, _)| path.is_ident(name))
        .map(|&(_, verb)| verb)
}

fn parse_method(method: &TraitItemFn) -> syn::Result<RestMethodModel> {
    let attrs = MethodAttrs::collect(method)?;

    let Some((http_method, path_template, verb_span)) = attrs.http else {
        return Err(syn::Error::new(
            method.span(),
            "rest_contract method requires one of `#[get(\"...\")]`, `#[post(\"...\")]`, \
             `#[put(\"...\")]`, `#[patch(\"...\")]`, `#[delete(\"...\")]` or \
             `#[passthrough(\"...\", methods = [..])]`",
        ));
    };

    // ADR-0009 §4: a manual client method sends through its handler, which
    // the generated retry loop cannot see; reject rather than silently ignore.
    if let Some(span) = attrs.retryable
        && attrs.client_manual
    {
        return Err(syn::Error::new(
            span,
            "`#[retryable]` cannot mark a `#[client_manual]` method: the handler sends the \
             request, so the generated retry loop never runs",
        ));
    }

    let params = parse_params(method)?;
    check_security_context(method, &params, attrs.anonymous)?;

    // Both unary and streaming methods declare their return types as
    // `Result<T, E>`. For streaming methods the macro rewrites the emitted
    // signature to `Pin<Box<dyn Stream<Item = Result<T, E>>>>`.
    let result_types = Some(parse_return_type(
        &method.sig.output,
        method.sig.ident.span(),
    )?);
    let optional = method.default.is_some();

    // A bare `#[streaming]` is SSE — what the marker has always meant. The
    // open shape is selected by `#[streaming(open = fallible)]` and
    // cross-checked against `async` — the same rule as the base and gRPC
    // parsers, shared in `stream_attr`.
    let (shape, stream_framing) = match &attrs.streaming {
        Some(args) => {
            let open = crate::stream_attr::resolve_stream_open(
                args.open,
                method.sig.asyncness.is_some(),
                method.sig.ident.span(),
            )?;
            (MethodShape::Stream(open), args.framing.unwrap_or_default())
        }
        None => (MethodShape::Unary, StreamFraming::default()),
    };

    let passthrough = match attrs.passthrough_methods {
        Some(methods) => {
            if !attrs.client_manual || !attrs.server_manual {
                return Err(syn::Error::new(
                    verb_span,
                    "`#[passthrough]` forwards an opaque request that neither side can \
                     generate: mark the method `#[client_manual]` and `#[server_manual]`",
                ));
            }
            if attrs.streaming.is_some() {
                return Err(syn::Error::new(
                    verb_span,
                    "`#[passthrough]` returns the response as is; it cannot also be \
                     `#[streaming]`",
                ));
            }
            Some(passthrough_spec(method, &params, &path_template, methods)?)
        }
        None => None,
    };

    Ok(RestMethodModel {
        ident: method.sig.ident.clone(),
        http_method,
        path_template,
        retryable: attrs.retryable.is_some(),
        shape,
        stream_framing,
        params,
        result_types,
        optional,
        server_manual: attrs.server_manual,
        client_manual: attrs.client_manual,
        passthrough,
        exposed: attrs.exposed,
        anonymous: attrs.anonymous.is_some(),
    })
}

/// DESIGN §2.2 (`cpt-cf-binding-constraint-security-context`): every method
/// on a remote-capable contract MUST take a security-plane context as its
/// first non-self argument — tenant `SecurityContext` (→ `Authorization`) or
/// platform `PlatformSecurityContext` (→ transport-injected internal token),
/// in value or reference form. A REST projection is always remote-capable
/// (its base ends in `Api`/`Backend`, enforced in [`parse`]), so a missing
/// context is a hard error rather than a silently-unauthenticated call.
///
/// The context type is the compile-time plane marker: tenant `SecurityContext`
/// routes to `Authorization: Bearer` (from the argument), platform
/// `PlatformSecurityContext` routes to the transport-injected
/// `X-ToolKit-Internal-Token` (sourced by the runtime from the process's
/// `InternalCredential`, never from the argument). The generated client keeps
/// whichever marker off the wire and fills the carrier below the contract
/// layer — see `generate_client_method` in `rest_contract.rs`
/// (cpt-cf-adr-two-plane-auth / -platform-plane-auth).
fn check_security_context(
    method: &TraitItemFn,
    params: &[RestParam],
    anonymous: Option<Span>,
) -> syn::Result<()> {
    let Some(first) = params
        .first()
        .filter(|first| crate::projection::is_security_context_type(&first.ty))
    else {
        return Err(syn::Error::new(
            method.sig.ident.span(),
            "remote contract method must take a security-plane context as its first \
             argument: `SecurityContext` (tenant plane) or `PlatformSecurityContext` \
             (platform plane), by value or by reference \
             (DESIGN section 2.2 cpt-cf-binding-constraint-security-context)",
        ));
    };

    // Reject `#[anonymous]` on platform methods: it would remove the required internal-token gate.
    if let Some(span) = anonymous
        && crate::projection::is_platform_security_context_type(&first.ty)
    {
        return Err(syn::Error::new(
            span,
            "`#[anonymous]` cannot mark a platform-plane method: a \
             `PlatformSecurityContext` method always requires a validated \
             `X-ToolKit-Internal-Token` (cpt-cf-adr-two-plane-auth)",
        ));
    }
    Ok(())
}

/// A passthrough method's only argument besides its security context is the
/// forwarded request.
fn passthrough_spec(
    method: &TraitItemFn,
    params: &[RestParam],
    path_template: &str,
    methods: Vec<HttpVerb>,
) -> syn::Result<PassthroughSpec> {
    let mut request = params
        .iter()
        .filter(|p| !crate::projection::is_security_context_type(&p.ty));
    let (Some(field), None) = (request.next(), request.next()) else {
        return Err(syn::Error::new(
            method.sig.ident.span(),
            "a `#[passthrough]` method takes exactly one argument besides its security \
             context: the forwarded request",
        ));
    };
    // `validate_path_template` already guarantees a single trailing wildcard.
    let param = path_template
        .rsplit_once("{*")
        .and_then(|(_, rest)| rest.strip_suffix('}'))
        .unwrap_or_default()
        .to_owned();
    Ok(PassthroughSpec {
        param,
        field: field.ident.clone(),
        methods,
    })
}

fn parse_path_lit(attr: &syn::Attribute) -> syn::Result<String> {
    let lit: syn::LitStr = attr.parse_args()?;
    Ok(lit.value())
}

/// `#[passthrough("/prefix/{*rest}", methods = [get, post, ..])]`.
fn parse_passthrough_args(attr: &syn::Attribute) -> syn::Result<(String, Vec<HttpVerb>)> {
    attr.parse_args_with(|input: syn::parse::ParseStream<'_>| {
        let path: syn::LitStr = input.parse()?;
        let _comma: syn::Token![,] = input.parse()?;
        let _kw: kw::methods = input.parse()?;
        let _eq: syn::Token![=] = input.parse()?;
        let list;
        syn::bracketed!(list in input);
        let idents = list.parse_terminated(Ident::parse_any, syn::Token![,])?;
        if input.peek(syn::Token![,]) {
            let _comma: syn::Token![,] = input.parse()?;
        }
        let mut methods = Vec::new();
        for ident in &idents {
            let verb = match ident.to_string().as_str() {
                "get" => HttpVerb::Get,
                "post" => HttpVerb::Post,
                "put" => HttpVerb::Put,
                "patch" => HttpVerb::Patch,
                "delete" => HttpVerb::Delete,
                "head" => HttpVerb::Head,
                "options" => HttpVerb::Options,
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown passthrough method `{other}`; expected get, post, put, \
                             patch, delete, head or options"
                        ),
                    ));
                }
            };
            if methods
                .iter()
                .any(|m: &HttpVerb| m.ir_variant() == verb.ir_variant())
            {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("duplicate passthrough method `{ident}`"),
                ));
            }
            methods.push(verb);
        }
        if methods.is_empty() {
            return Err(syn::Error::new(
                attr.span(),
                "`#[passthrough]` needs at least one method in `methods = [..]`",
            ));
        }
        Ok((path.value(), methods))
    })
}

/// Validate a path template at macro-expansion time (M-12) so malformed routes
/// (`"/a/{"`, `"/a/{}"`, `"/a/{x}}"`, nested `"{{x}}"`) are a clear compile
/// error at the attribute span rather than surfacing far away (or only at
/// `validate_http_binding` test/`provides` time). Each `{...}` segment must be
/// a non-empty balanced placeholder with a valid Rust-ish identifier inside.
///
/// With `wildcard`, the template must end with exactly one `/{*name}` segment, which
/// the forwarded request's path fills; anywhere else a wildcard is an error.
fn validate_path_template(path: &str, span: Span, wildcard: bool) -> syn::Result<()> {
    let err = |msg: &str| {
        Err(syn::Error::new(
            span,
            format!("malformed path template `{path}`: {msg}"),
        ))
    };
    let wildcards = path.matches("{*").count();
    if wildcard {
        let tail_ok = path.rsplit_once("{*").is_some_and(|(head, rest)| {
            head.ends_with('/') && rest.ends_with('}') && !rest.contains(['/', '{'])
        });
        if wildcards != 1 || !tail_ok {
            return err(
                "a `#[passthrough]` template must end with exactly one `/{*name}` wildcard segment",
            );
        }
    } else if wildcards != 0 {
        return err("`{*name}` wildcards are only valid in `#[passthrough]` templates");
    }
    let mut chars = path.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        match c {
            '}' => return err("unmatched `}`"),
            '{' => {
                let mut name = String::new();
                let mut closed = false;
                for (_, cc) in chars.by_ref() {
                    match cc {
                        '}' => {
                            closed = true;
                            break;
                        }
                        '{' => return err("nested `{` in a path parameter"),
                        _ => name.push(cc),
                    }
                }
                if !closed {
                    return err("unterminated `{` (missing `}`)");
                }
                let name = name.strip_prefix('*').unwrap_or(&name);
                if name.is_empty() {
                    return err("empty path parameter `{}`");
                }
                if !name.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
                    return err("path parameter must be an identifier (alphanumeric/underscore)");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_params(method: &TraitItemFn) -> syn::Result<Vec<RestParam>> {
    let mut params = Vec::new();
    for arg in &method.sig.inputs {
        let syn::FnArg::Typed(pat_type) = arg else {
            continue;
        };
        let syn::Pat::Ident(pat_ident) = pat_type.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &pat_type.pat,
                "expected an identifier pattern for method parameter",
            ));
        };
        params.push(RestParam {
            ident: pat_ident.ident.clone(),
            ty: (*pat_type.ty).clone(),
        });
    }

    // Exactly one security-plane context per method: `capture_auth_credentials`
    // (`rest_contract.rs`) picks a single param — via `.find`, unconditionally
    // the first — to source the auth metadata from. A second context param
    // (e.g. a stray `PlatformSecurityContext` alongside a tenant
    // `SecurityContext`) would silently be dropped from the wire without ever
    // being consulted for token attachment, mixing planes. Mirrors the same
    // check in `grpc_contract_parse::parse_params`.
    let secctx_count = params
        .iter()
        .filter(|p| crate::projection::is_security_context_type(&p.ty))
        .count();
    if secctx_count > 1 {
        return Err(syn::Error::new(
            method.sig.ident.span(),
            "rest_contract method must take at most one security-plane context \
             parameter (`SecurityContext` or `PlatformSecurityContext`); found more \
             than one",
        ));
    }

    Ok(params)
}

fn parse_return_type(ret: &ReturnType, span: Span) -> syn::Result<(Type, Type)> {
    let ReturnType::Type(_, ty) = ret else {
        return Err(syn::Error::new(
            span,
            "rest_contract methods must return `Result<T, E>`",
        ));
    };
    extract_result_types(ty)
}

fn extract_result_types(ty: &Type) -> syn::Result<(Type, Type)> {
    let Type::Path(p) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "expected `Result<T, E>` return type",
        ));
    };
    let Some(last) = p.path.segments.last() else {
        return Err(syn::Error::new_spanned(ty, "expected `Result<T, E>`"));
    };
    if last.ident != "Result" {
        return Err(syn::Error::new_spanned(
            ty,
            "expected `Result<T, E>` return type",
        ));
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "expected `Result<T, E>` with generic arguments",
        ));
    };
    let mut iter = args.args.iter();
    let ok = iter
        .next()
        .ok_or_else(|| syn::Error::new_spanned(ty, "Result must have two type arguments"))?;
    let err = iter
        .next()
        .ok_or_else(|| syn::Error::new_spanned(ty, "Result must have two type arguments"))?;
    let syn::GenericArgument::Type(ok_ty) = ok else {
        return Err(syn::Error::new_spanned(ok, "expected a type argument"));
    };
    let syn::GenericArgument::Type(err_ty) = err else {
        return Err(syn::Error::new_spanned(err, "expected a type argument"));
    };
    Ok((ok_ty.clone(), err_ty.clone()))
}

impl HttpVerb {
    /// Variant name in the runtime IR's HTTP verb enum.
    pub fn ir_variant(self) -> &'static str {
        match self {
            HttpVerb::Get => "Get",
            HttpVerb::Post => "Post",
            HttpVerb::Put => "Put",
            HttpVerb::Patch => "Patch",
            HttpVerb::Delete => "Delete",
            HttpVerb::Head => "Head",
            HttpVerb::Options => "Options",
            HttpVerb::Passthrough => "Passthrough",
        }
    }

    /// Whether this verb conventionally carries a request body.
    pub fn allows_body(self) -> bool {
        matches!(self, HttpVerb::Post | HttpVerb::Put | HttpVerb::Patch)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_params;
    use syn::parse_quote;

    #[test]
    fn rejects_methods_with_two_security_context_params() {
        let method: syn::TraitItemFn = parse_quote! {
            async fn charge(
                &self,
                ctx: SecurityContext,
                other: PlatformSecurityContext,
                body: String,
            ) -> Result<u32, std::io::Error>;
        };
        let err = parse_params(&method)
            .map(|_| ())
            .expect_err("two security-context params must be rejected");
        assert!(
            err.to_string()
                .contains("at most one security-plane context"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_single_security_context_param() {
        let method: syn::TraitItemFn = parse_quote! {
            async fn charge(&self, ctx: SecurityContext, body: String) -> Result<u32, std::io::Error>;
        };
        let params = parse_params(&method).expect("a single security-context param is valid");
        // `self` (a `Receiver`, not `FnArg::Typed`) never lands in `params`.
        assert_eq!(params.len(), 2);
    }
}
