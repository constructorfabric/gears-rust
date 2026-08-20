//! Parsing for `#[rest_contract]`.
//!
//! Recognized attributes on trait methods:
//! - `#[get("/path/{param}")]`, `#[post(...)]`, `#[put(...)]`, `#[delete(...)]`
//! - `#[retryable]` — marks the method as safe to retry on transport failure.
//! - `#[streaming]` — marks the method as server-streaming (SSE).
//! - `#[server_manual]` — skip server-route generation for this method.
//! - `#[exposed]` / `#[internal]` — edge visibility, overriding the trait-level
//!   `visibility` default. Internal unless said otherwise.
//! - `#[anonymous]` — the route requires no authentication.
//!
//! The trait's first non-marker supertrait is recorded as the *base contract*
//! the projection refines (e.g. `pub trait PaymentServiceRest: PaymentService`).

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{Ident, ItemTrait, ReturnType, TraitItem, TraitItemFn, Type};

pub struct RestContractAttr {
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
}

pub struct RestContractModel {
    pub item: ItemTrait,
    pub trait_ident: Ident,
    pub base_trait: syn::Path,
    pub base_path: String,
    pub require_full_coverage: bool,
    pub default_exposed: bool,
    pub methods: Vec<RestMethodModel>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "these are independent per-method projection flags (retryable / streaming / optional / server_manual / anonymous) parsed from distinct attributes; a bitflags enum would obscure rather than clarify the 1:1 attribute mapping"
)]
pub struct RestMethodModel {
    pub ident: Ident,
    pub http_method: HttpVerb,
    pub path_template: String,
    pub retryable: bool,
    pub streaming: bool,
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

pub struct RestParam {
    pub ident: Ident,
    pub ty: Type,
}

#[derive(Clone, Copy)]
pub enum HttpVerb {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

mod kw {
    syn::custom_keyword!(base_path);
    syn::custom_keyword!(require_full_coverage);
    syn::custom_keyword!(visibility);
}

impl syn::parse::Parse for RestContractAttr {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self {
                base_path: String::new(),
                require_full_coverage: false,
                default_exposed: false,
            });
        }
        let mut base_path = None;
        let mut require_full_coverage = false;
        let mut default_exposed: Option<bool> = None;
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
        })
    }
}

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

    Ok(RestContractModel {
        item,
        trait_ident,
        base_trait,
        base_path: attr.base_path,
        require_full_coverage: attr.require_full_coverage,
        default_exposed: attr.default_exposed,
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

fn parse_method(method: &TraitItemFn) -> syn::Result<RestMethodModel> {
    let ident = method.sig.ident.clone();

    let mut http: Option<(HttpVerb, String, Span)> = None;
    let mut retryable = false;
    let mut streaming = false;
    let mut server_manual = false;
    let mut exposed: Option<bool> = None;
    let mut anonymous = false;

    for attr in &method.attrs {
        let path = attr.path();
        // Recognize the verb (if any) on this attribute, then reject a SECOND
        // verb on the same method (`#[get(..)] #[post(..)]`) — silently letting
        // the last one win would change the wire contract with no signal (M-11).
        let verb = if path.is_ident("get") {
            Some(HttpVerb::Get)
        } else if path.is_ident("post") {
            Some(HttpVerb::Post)
        } else if path.is_ident("put") {
            Some(HttpVerb::Put)
        } else if path.is_ident("patch") {
            Some(HttpVerb::Patch)
        } else if path.is_ident("delete") {
            Some(HttpVerb::Delete)
        } else {
            None
        };
        if let Some(verb) = verb {
            if http.is_some() {
                return Err(syn::Error::new(
                    attr.span(),
                    "duplicate HTTP method annotation: a projection method must declare \
                     exactly one of `#[get]`/`#[post]`/`#[put]`/`#[patch]`/`#[delete]`",
                ));
            }
            let path_lit = parse_path_lit(attr)?;
            validate_path_template(&path_lit, attr.span())?;
            http = Some((verb, path_lit, attr.span()));
        } else if path.is_ident("retryable") {
            retryable = true;
        } else if path.is_ident("streaming") {
            streaming = true;
        } else if path.is_ident("server_manual") {
            server_manual = true;
        } else if path.is_ident("exposed") || path.is_ident("internal") {
            let wants_exposed = path.is_ident("exposed");
            // Both on one method is a contradiction, not a last-one-wins:
            // silently picking one would change what the gateway serves.
            if exposed.is_some_and(|prev| prev != wants_exposed) {
                return Err(syn::Error::new(
                    attr.span(),
                    "conflicting visibility: a method cannot be both `#[exposed]` and \
                     `#[internal]`. Pick one, or drop both to inherit the trait's \
                     `visibility` default",
                ));
            }
            exposed = Some(wants_exposed);
        } else if path.is_ident("anonymous") {
            anonymous = true;
        }
    }

    let (http_method, path_template, _span) = http.ok_or_else(|| {
        syn::Error::new(
            method.span(),
            "rest_contract method requires one of `#[get(\"...\")]`, `#[post(\"...\")]`, \
             `#[put(\"...\")]`, `#[patch(\"...\")]`, or `#[delete(\"...\")]`",
        )
    })?;

    let params = parse_params(method)?;

    // DESIGN §2.2 (`cpt-cf-binding-constraint-security-context`): every method
    // on a remote-capable contract MUST take a security-plane context as its
    // first non-self argument — tenant `SecurityContext` (→ `Authorization`) or
    // platform `PlatformSecurityContext` (→ transport-injected internal token),
    // in value or reference form. A REST projection is always remote-capable
    // (its base ends in `Api`/`Backend`, enforced above), so a missing context
    // is a hard error rather than a silently-unauthenticated call.
    match params.first() {
        // Either plane is accepted. The context type is the compile-time plane
        // marker: tenant `SecurityContext` routes to `Authorization: Bearer`
        // (from the argument), platform `PlatformSecurityContext` routes to the
        // transport-injected `X-ToolKit-Internal-Token` (sourced by the runtime
        // from the process's `InternalCredential`, never from the argument). The
        // generated client keeps whichever marker off the wire and fills the
        // carrier below the contract layer — see `generate_client_method` in
        // `rest_contract.rs` (cpt-cf-adr-two-plane-auth / -platform-plane-auth).
        Some(first) if crate::projection::is_security_context_type(&first.ty) => {}
        _ => {
            return Err(syn::Error::new(
                method.sig.ident.span(),
                "remote contract method must take a security-plane context as its first \
                 argument: `SecurityContext` (tenant plane) or `PlatformSecurityContext` \
                 (platform plane), by value or by reference \
                 (DESIGN section 2.2 cpt-cf-binding-constraint-security-context)",
            ));
        }
    }

    // Both unary and streaming methods declare their return types as
    // `Result<T, E>`. For streaming methods the macro rewrites the emitted
    // signature to `Pin<Box<dyn Stream<Item = Result<T, E>>>>`.
    let result_types = Some(parse_return_type(
        &method.sig.output,
        method.sig.ident.span(),
    )?);
    let optional = method.default.is_some();

    Ok(RestMethodModel {
        ident,
        http_method,
        path_template,
        retryable,
        streaming,
        params,
        result_types,
        optional,
        server_manual,
        exposed,
        anonymous,
    })
}

fn parse_path_lit(attr: &syn::Attribute) -> syn::Result<String> {
    let lit: syn::LitStr = attr.parse_args()?;
    Ok(lit.value())
}

/// Validate a path template at macro-expansion time (M-12) so malformed routes
/// (`"/a/{"`, `"/a/{}"`, `"/a/{x}}"`, nested `"{{x}}"`) are a clear compile
/// error at the attribute span rather than surfacing far away (or only at
/// `validate_http_binding` test/`provides` time). Each `{...}` segment must be
/// a non-empty balanced placeholder with a valid Rust-ish identifier inside.
fn validate_path_template(path: &str, span: Span) -> syn::Result<()> {
    let err = |msg: &str| {
        Err(syn::Error::new(
            span,
            format!("malformed path template `{path}`: {msg}"),
        ))
    };
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
    pub fn ir_variant(self) -> &'static str {
        match self {
            HttpVerb::Get => "Get",
            HttpVerb::Post => "Post",
            HttpVerb::Put => "Put",
            HttpVerb::Patch => "Patch",
            HttpVerb::Delete => "Delete",
        }
    }

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
