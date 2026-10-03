use heck::ToUpperCamelCase;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Data, DeriveInput, spanned::Spanned};

/// Well-known property names that are auto-derived from dimension columns.
///
/// These mirror `toolkit_security::access_scope::properties` but are duplicated
/// here because proc-macro crates cannot depend on runtime crates.
const PEP_PROP_OWNER_TENANT_ID: &str = "owner_tenant_id";
const PEP_PROP_RESOURCE_ID: &str = "id";
const PEP_PROP_OWNER_ID: &str = "owner_id";

/// Reserved property names that must not be used in `pep_prop(...)`.
const RESERVED_PROPERTIES: &[(&str, &str)] = &[
    (PEP_PROP_OWNER_TENANT_ID, "tenant_col"),
    (PEP_PROP_RESOURCE_ID, "resource_col"),
    (PEP_PROP_OWNER_ID, "owner_col"),
];

/// Configuration parsed from `#[secure(...)]` attributes
#[derive(Default)]
struct SecureConfig {
    // Tenant dimension
    tenant_col: Option<(String, Span)>,
    no_tenant: Option<Span>,

    // Resource dimension
    resource_col: Option<(String, Span)>,
    no_resource: Option<Span>,

    // Owner dimension
    owner_col: Option<(String, Span)>,
    no_owner: Option<Span>,

    // Type dimension
    type_col: Option<(String, Span)>,
    no_type: Option<Span>,

    // Unrestricted flag
    unrestricted: Option<Span>,

    // Custom PEP property mappings: (property_name, column_name, span)
    pep_props: Vec<(String, String, Span)>,
}

#[allow(clippy::needless_pass_by_value)] // DeriveInput is consumed by proc-macro pattern
pub fn expand_derive_scopable(input: DeriveInput) -> syn::Result<TokenStream> {
    // Verify this is a struct
    if !matches!(&input.data, Data::Struct(_)) {
        return Err(syn::Error::new(
            input.span(),
            "#[derive(Scopable)] can only be applied to structs",
        ));
    }

    // Parse #[secure(...)] attributes
    let config = parse_secure_attrs(&input)?;

    // Validate configuration
    validate_config(&config, &input)?;

    let entity_ident = syn::Ident::new("Entity", input.ident.span());

    // If unrestricted, generate the empty table: nothing is scopable, so
    // every accessor the trait derives from it answers None.
    if config.unrestricted.is_some() {
        return Ok(quote! {
            impl ::toolkit_db::secure::ScopableEntity for #entity_ident {
                const IS_UNRESTRICTED: bool = true;

                fn type_col() -> ::core::option::Option<Self::Column> {
                    ::core::option::Option::None
                }

                // Nothing to scope by, so nothing resolves, the column list is
                // empty, and the three dimension accessors `ScopeProperties`
                // derives from this table all answer `None`.
                const SCOPE_PROPERTIES: &'static [(&'static str, Self::Column)] = &[];

                // An unrestricted entity scopes on nothing by construction, so
                // there is no dimension left to decide about. The check in
                // `ScopeProperties` exempts it; the const is still required.
                const UNSCOPED_DIMENSIONS: &'static [&'static str] = &[];
            }
        });
    }

    // Only type_col is generated as a method. The tenant, resource and owner
    // dimensions are the well-known properties of SCOPE_PROPERTIES, and
    // `ScopeProperties` reads them back out of it, so emitting them here would
    // put the same column in two places again (issue #4726). type_col has no
    // property name, so it has nowhere else to come from.
    let type_col_impl = generate_col_impl("type_col", config.type_col.as_ref(), input.ident.span());

    // One table; the trait derives the lookup, the column list and the three
    // dimension accessors from it, so none of them can describe a different set.
    let scope_properties_impl = generate_scope_properties(&config, input.ident.span());

    // The other half of the same decision: the dimensions `#[secure(...)]`
    // answered `no_*` to. `validate_config` has already required an answer for
    // each one, so this cannot be partial.
    let unscoped_dimensions_impl = generate_unscoped_dimensions(&config);

    // Generate the implementation
    Ok(quote! {
        impl ::toolkit_db::secure::ScopableEntity for #entity_ident {
            const IS_UNRESTRICTED: bool = false;

            #type_col_impl

            #scope_properties_impl

            #unscoped_dimensions_impl
        }
    })
}

/// Build `UNSCOPED_DIMENSIONS`: the well-known dimensions this entity was asked
/// about and answered `no_tenant` / `no_resource` / `no_owner` to.
///
/// The complement of the dimension entries in [`generate_scope_properties`],
/// from the same configuration, so the two cannot disagree. It exists because
/// the trait cannot tell a dimension an entity has no column for from one whose
/// row was forgotten, and the write-side guards skip themselves on both.
fn generate_unscoped_dimensions(config: &SecureConfig) -> TokenStream {
    let mut entries = Vec::new();

    for (property, dimension) in [
        (PEP_PROP_OWNER_TENANT_ID, config.tenant_col.as_ref()),
        (PEP_PROP_RESOURCE_ID, config.resource_col.as_ref()),
        (PEP_PROP_OWNER_ID, config.owner_col.as_ref()),
    ] {
        if dimension.is_none() {
            entries.push(quote! { #property });
        }
    }

    quote! {
        const UNSCOPED_DIMENSIONS: &'static [&'static str] = &[#(#entries),*];
    }
}

/// Generate a column method implementation
fn generate_col_impl(
    method_name: &str,
    col: Option<&(String, Span)>,
    default_span: Span,
) -> TokenStream {
    let method_ident = syn::Ident::new(method_name, default_span);

    if let Some((col_name, _)) = col {
        let col_variant = snake_to_upper_camel(col_name);
        let col_ident = syn::Ident::new(&col_variant, default_span);
        quote! {
            fn #method_ident() -> ::core::option::Option<Self::Column> {
                ::core::option::Option::Some(Self::Column::#col_ident)
            }
        }
    } else {
        quote! {
            fn #method_ident() -> ::core::option::Option<Self::Column> {
                ::core::option::Option::None
            }
        }
    }
}

/// Build the `SCOPE_PROPERTIES` table: every property this entity understands,
/// paired with the column it means.
///
/// One table rather than a `resolve_property` match plus a `scope_columns`
/// list. The trait derives both from it, so a property the lookup answers and a
/// column the list omits can no longer disagree — which was possible before,
/// and mattered because a property-graph declaration builds its `PROPERTIES`
/// list from the columns, and a scope column missing from that list is silently
/// unfilterable inside `MATCH` (issue #4726).
///
/// The dimension columns take their well-known property names;
/// `pep_prop(name = "column")` entries take theirs verbatim. `type_col` gets no
/// entry: there is no property name for it, so no scope can address it, and a
/// column with no property would let an entity whose only dimension is
/// `type_col` pass the Policy 2 gates while resolving nothing.
fn generate_scope_properties(config: &SecureConfig, span: Span) -> TokenStream {
    let mut entries = Vec::new();

    for (property, dimension) in [
        (PEP_PROP_OWNER_TENANT_ID, config.tenant_col.as_ref()),
        (PEP_PROP_RESOURCE_ID, config.resource_col.as_ref()),
        (PEP_PROP_OWNER_ID, config.owner_col.as_ref()),
    ] {
        if let Some((col_name, _)) = dimension {
            let col_ident = syn::Ident::new(&snake_to_upper_camel(col_name), span);
            entries.push(quote! { (#property, Self::Column::#col_ident) });
        }
    }

    for (property, column, _) in &config.pep_props {
        let col_ident = syn::Ident::new(&snake_to_upper_camel(column), span);
        entries.push(quote! { (#property, Self::Column::#col_ident) });
    }

    quote! {
        const SCOPE_PROPERTIES: &'static [(&'static str, Self::Column)] = &[
            #(#entries),*
        ];
    }
}

/// The first attribute `unrestricted` forbids, if the entity wrote one.
///
/// `unrestricted` means the entity scopes on nothing, so every other
/// `#[secure(...)]` attribute contradicts it. The rule lives here alone: it
/// used to be checked a second time while parsing, which read
/// `config.unrestricted` and therefore only fired for an attribute written
/// *after* `unrestricted`. Which of the two diagnostics a user saw depended on
/// writing order, and one of them named the attribute while the other named
/// `unrestricted`.
///
/// The order below is fixed rather than source order, so the same
/// configuration always reports the same attribute.
fn first_other_attribute(config: &SecureConfig) -> Option<(&'static str, Span)> {
    let dimensions: [(&'static str, Option<Span>); 8] = [
        ("tenant_col", config.tenant_col.as_ref().map(|(_, s)| *s)),
        ("no_tenant", config.no_tenant),
        (
            "resource_col",
            config.resource_col.as_ref().map(|(_, s)| *s),
        ),
        ("no_resource", config.no_resource),
        ("owner_col", config.owner_col.as_ref().map(|(_, s)| *s)),
        ("no_owner", config.no_owner),
        ("type_col", config.type_col.as_ref().map(|(_, s)| *s)),
        ("no_type", config.no_type),
    ];

    for (attribute, span) in dimensions {
        if let Some(span) = span {
            return Some((attribute, span));
        }
    }

    config
        .pep_props
        .first()
        .map(|(_, _, span)| ("pep_prop", *span))
}

/// Validate the configuration for strict compile-time checks
fn validate_config(config: &SecureConfig, input: &DeriveInput) -> syn::Result<()> {
    let struct_span = input.span();

    // If unrestricted is set, no other attributes should be present.
    //
    // `pep_props` belongs in this list, not only in the parse-time guard in
    // `parse_secure_attrs`: that guard reads `config.unrestricted`, so it only
    // fires when `unrestricted` was written *before* the `pep_prop`. Written
    // after, the declared property reached the `unrestricted` branch of
    // `expand_derive_scopable`, which emits an empty `SCOPE_PROPERTIES` -- so
    // the property was dropped in silence and the entity came out fully
    // unscoped. Attribute order decided whether that was a hard error.
    if config.unrestricted.is_some() {
        if let Some((attribute, span)) = first_other_attribute(config) {
            return Err(syn::Error::new(
                span,
                format!("secure: '{attribute}' cannot be used with 'unrestricted'"),
            ));
        }
        return Ok(()); // Valid unrestricted config
    }

    // Check each scope dimension has exactly one option
    validate_dimension(
        "tenant",
        config.tenant_col.as_ref(),
        config.no_tenant,
        struct_span,
    )?;
    validate_dimension(
        "resource",
        config.resource_col.as_ref(),
        config.no_resource,
        struct_span,
    )?;
    validate_dimension(
        "owner",
        config.owner_col.as_ref(),
        config.no_owner,
        struct_span,
    )?;
    validate_dimension(
        "type",
        config.type_col.as_ref(),
        config.no_type,
        struct_span,
    )?;

    // Validate pep_prop entries
    validate_pep_props(config)
}

/// Validate `pep_prop` entries for reserved names, duplicates, and empty values.
fn validate_pep_props(config: &SecureConfig) -> syn::Result<()> {
    let mut seen = std::collections::HashSet::new();

    for (property, column, span) in &config.pep_props {
        // Check for reserved property names
        for (reserved, use_instead) in RESERVED_PROPERTIES {
            if property == reserved {
                return Err(syn::Error::new(
                    *span,
                    format!(
                        "pep_prop: '{reserved}' is a reserved property name; \
                         use `{use_instead}` instead"
                    ),
                ));
            }
        }

        // Check for empty property or column
        if property.is_empty() {
            return Err(syn::Error::new(
                *span,
                "pep_prop: property name must not be empty",
            ));
        }
        if column.is_empty() {
            return Err(syn::Error::new(
                *span,
                "pep_prop: column name must not be empty",
            ));
        }

        validate_column_name("pep_prop", column, *span)?;

        // Check for duplicate property names
        if !seen.insert(property.clone()) {
            return Err(syn::Error::new(
                *span,
                format!("pep_prop: duplicate property name '{property}'"),
            ));
        }
    }

    Ok(())
}

/// Check that `column` forms a usable `Self::Column` variant.
///
/// `syn::Ident::new` panics on anything that is not a valid Rust identifier,
/// which aborts expansion with a bare `proc macro panicked` and no span at all.
/// Every other bad input to `#[secure(...)]` gets a spanned error, and every
/// column name reaches `Ident::new`: the four dimensions through
/// `generate_col_impl` and `generate_scope_properties`, the `pep_prop` entries
/// through the latter. So the check belongs to all of them, not to one.
///
/// `what` names the attribute for the message -- `tenant_col`, `pep_prop`.
fn validate_column_name(what: &str, column: &str, span: Span) -> syn::Result<()> {
    let variant = snake_to_upper_camel(column);
    if is_ident(&variant) {
        return Ok(());
    }
    Err(syn::Error::new(
        span,
        format!(
            "{what}: column name '{column}' does not form a valid column variant \
             ('{variant}'); use a snake_case identifier"
        ),
    ))
}

/// Whether `s` is a valid Rust identifier.
///
/// ASCII only: the workspace sets `non_ascii_idents = "forbid"`, so an
/// identifier this crate generates could not use anything else anyway.
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate a single dimension has exactly one specification
fn validate_dimension(
    name: &str,
    col: Option<&(String, Span)>,
    no_col: Option<Span>,
    struct_span: Span,
) -> syn::Result<()> {
    match (col, &no_col) {
        (None, None) => {
            // Missing explicit decision
            let msg = format!(
                "secure: missing explicit decision for {name}:\n  \
                 use `{name}_col = \"column_name\"` or `no_{name}`"
            );
            Err(syn::Error::new(struct_span, msg))
        }
        (Some((_, col_span)), Some(_no_span)) => {
            // Both specified
            let msg = format!("secure: specify either `{name}_col` or `no_{name}`, not both");
            Err(syn::Error::new(*col_span, msg))
        }
        (Some((column, col_span)), None) => {
            // Exactly one is specified, and it names a column. The name still
            // has to survive `Ident::new`.
            validate_column_name(&format!("{name}_col"), column, *col_span)
        }
        (None, Some(_)) => Ok(()),
    }
}

/// Parse all `#[secure(...)]` attributes with duplicate detection
fn parse_secure_attrs(input: &DeriveInput) -> syn::Result<SecureConfig> {
    let mut config = SecureConfig::default();

    for attr in &input.attrs {
        if !attr.path().is_ident("secure") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            let span = meta.path.span();

            // Check if this is a flag (no_* or unrestricted)
            if meta.path.is_ident("unrestricted") {
                if config.unrestricted.is_some() {
                    return Err(syn::Error::new(span, "duplicate attribute 'unrestricted'"));
                }
                config.unrestricted = Some(span);
                return Ok(());
            }

            if meta.path.is_ident("no_tenant") {
                if config.no_tenant.is_some() {
                    return Err(syn::Error::new(span, "duplicate attribute 'no_tenant'"));
                }
                if config.tenant_col.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "secure: specify either `tenant_col` or `no_tenant`, not both",
                    ));
                }
                config.no_tenant = Some(span);
                return Ok(());
            }

            if meta.path.is_ident("no_resource") {
                if config.no_resource.is_some() {
                    return Err(syn::Error::new(span, "duplicate attribute 'no_resource'"));
                }
                if config.resource_col.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "secure: specify either `resource_col` or `no_resource`, not both",
                    ));
                }
                config.no_resource = Some(span);
                return Ok(());
            }

            if meta.path.is_ident("no_owner") {
                if config.no_owner.is_some() {
                    return Err(syn::Error::new(span, "duplicate attribute 'no_owner'"));
                }
                if config.owner_col.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "secure: specify either `owner_col` or `no_owner`, not both",
                    ));
                }
                config.no_owner = Some(span);
                return Ok(());
            }

            if meta.path.is_ident("no_type") {
                if config.no_type.is_some() {
                    return Err(syn::Error::new(span, "duplicate attribute 'no_type'"));
                }
                if config.type_col.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "secure: specify either `type_col` or `no_type`, not both",
                    ));
                }
                config.no_type = Some(span);
                return Ok(());
            }

            // Check for pep_prop(name = "column") — nested meta with parentheses
            if meta.path.is_ident("pep_prop") {
                meta.parse_nested_meta(|pep_meta| {
                    let property = pep_meta
                        .path
                        .get_ident()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    let column: String = pep_meta.value()?.parse::<syn::LitStr>()?.value();
                    config
                        .pep_props
                        .push((property, column, pep_meta.path.span()));
                    Ok(())
                })?;
                return Ok(());
            }

            parse_key_value_attr(&mut config, meta)
        })?;
    }

    Ok(config)
}

/// Parse a key-value attribute like `tenant_col = "column_name"`.
#[allow(clippy::needless_pass_by_value)] // ParseNestedMeta is consumed by .value()
fn parse_key_value_attr(
    config: &mut SecureConfig,
    meta: syn::meta::ParseNestedMeta<'_>,
) -> syn::Result<()> {
    let span = meta.path.span();
    let key = meta
        .path
        .get_ident()
        .map(ToString::to_string)
        .unwrap_or_default();

    if key.is_empty() {
        return Err(syn::Error::new(span, "Expected attribute name"));
    }

    let value: String = match meta.value() {
        Ok(v) => match v.parse::<syn::LitStr>() {
            Ok(lit) => lit.value(),
            Err(_) => return Err(syn::Error::new(span, "Expected string literal")),
        },
        Err(_) => {
            return Err(syn::Error::new(
                span,
                "Expected '=' followed by a string value",
            ));
        }
    };

    match key.as_str() {
        "tenant_col" => {
            if config.tenant_col.is_some() {
                return Err(syn::Error::new(span, "duplicate attribute 'tenant_col'"));
            }
            if config.no_tenant.is_some() {
                return Err(syn::Error::new(
                    span,
                    "secure: specify either `tenant_col` or `no_tenant`, not both",
                ));
            }
            config.tenant_col = Some((value, span));
        }
        "resource_col" => {
            if config.resource_col.is_some() {
                return Err(syn::Error::new(span, "duplicate attribute 'resource_col'"));
            }
            if config.no_resource.is_some() {
                return Err(syn::Error::new(
                    span,
                    "secure: specify either `resource_col` or `no_resource`, not both",
                ));
            }
            config.resource_col = Some((value, span));
        }
        "owner_col" => {
            if config.owner_col.is_some() {
                return Err(syn::Error::new(span, "duplicate attribute 'owner_col'"));
            }
            if config.no_owner.is_some() {
                return Err(syn::Error::new(
                    span,
                    "secure: specify either `owner_col` or `no_owner`, not both",
                ));
            }
            config.owner_col = Some((value, span));
        }
        "type_col" => {
            if config.type_col.is_some() {
                return Err(syn::Error::new(span, "duplicate attribute 'type_col'"));
            }
            if config.no_type.is_some() {
                return Err(syn::Error::new(
                    span,
                    "secure: specify either `type_col` or `no_type`, not both",
                ));
            }
            config.type_col = Some((value, span));
        }
        _ => {
            return Err(syn::Error::new(
                span,
                format!(
                    "Unknown attribute '{key}'. Valid attributes: tenant_col, no_tenant, \
                     resource_col, no_resource, owner_col, no_owner, type_col, no_type, \
                     unrestricted, pep_prop"
                ),
            ));
        }
    }

    Ok(())
}

/// Convert `snake_case` to `UpperCamelCase` for enum variant names
fn snake_to_upper_camel(s: &str) -> String {
    s.to_upper_camel_case()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_snake_to_upper_camel() {
        assert_eq!(snake_to_upper_camel("tenant_id"), "TenantId");
        assert_eq!(snake_to_upper_camel("id"), "Id");
        assert_eq!(snake_to_upper_camel("owner_user_id"), "OwnerUserId");
        assert_eq!(snake_to_upper_camel("custom_col"), "CustomCol");
    }

    /// Every attribute `unrestricted` forbids, and the one setter that turns it
    /// on in a config.
    ///
    /// The list is the test's own, written out rather than derived from
    /// `first_other_attribute`: a table that builds itself from the code it
    /// checks agrees with that code by construction.
    #[expect(
        clippy::type_complexity,
        reason = "a table of (name, setter) pairs reads better inline than behind an alias"
    )]
    const EVERY_FORBIDDEN_ATTRIBUTE: [(&str, fn(&mut SecureConfig)); 9] = [
        ("tenant_col", |c| {
            c.tenant_col = Some(("tenant_id".to_owned(), Span::call_site()));
        }),
        ("no_tenant", |c| c.no_tenant = Some(Span::call_site())),
        ("resource_col", |c| {
            c.resource_col = Some(("id".to_owned(), Span::call_site()));
        }),
        ("no_resource", |c| c.no_resource = Some(Span::call_site())),
        ("owner_col", |c| {
            c.owner_col = Some(("owner_id".to_owned(), Span::call_site()));
        }),
        ("no_owner", |c| c.no_owner = Some(Span::call_site())),
        ("type_col", |c| {
            c.type_col = Some(("kind".to_owned(), Span::call_site()));
        }),
        ("no_type", |c| c.no_type = Some(Span::call_site())),
        ("pep_prop", |c| {
            c.pep_props.push((
                "department_id".to_owned(),
                "department_id".to_owned(),
                Span::call_site(),
            ));
        }),
    ];

    fn unrestricted_config() -> SecureConfig {
        SecureConfig {
            unrestricted: Some(Span::call_site()),
            ..SecureConfig::default()
        }
    }

    /// Each attribute is reported under its own name.
    ///
    /// `first_other_attribute` pairs a name with the field it reads, nine
    /// times, and a pair written the wrong way round type-checks: every field
    /// carries a `Span`, so `("owner_col", config.no_owner)` compiles and sends
    /// the user looking for an attribute they did not write. The UI fixtures
    /// pin the span on real source, but only for the few attributes it is worth
    /// compiling a crate for; this covers all nine.
    #[test]
    fn every_forbidden_attribute_is_reported_under_its_own_name() {
        for (name, set_it) in EVERY_FORBIDDEN_ATTRIBUTE {
            let mut config = unrestricted_config();
            set_it(&mut config);
            assert_eq!(
                first_other_attribute(&config).map(|(attribute, _)| attribute),
                Some(name),
                "an entity whose only other attribute is `{name}` must be told so"
            );
        }
    }

    /// Nothing else present, nothing to report -- the negative control, without
    /// which the test above would pass for a function that always returned the
    /// name it was handed.
    #[test]
    fn unrestricted_on_its_own_forbids_nothing() {
        assert!(first_other_attribute(&unrestricted_config()).is_none());
    }

    /// With several present, the answer is the first in the fixed order, not
    /// the first in the source.
    ///
    /// The order is the reason the diagnostic no longer depends on how the
    /// attributes were written, so reordering the table changes behaviour and
    /// should fail a test rather than only a fixture.
    #[test]
    fn several_attributes_report_the_first_in_the_fixed_order() {
        let mut config = unrestricted_config();
        // Set them back to front: the last entry first, the first entry last.
        for (_, set_it) in EVERY_FORBIDDEN_ATTRIBUTE.iter().rev() {
            set_it(&mut config);
        }
        assert_eq!(
            first_other_attribute(&config).map(|(attribute, _)| attribute),
            Some("tenant_col"),
            "the fixed order decides, and `tenant_col` heads it"
        );
    }
}
