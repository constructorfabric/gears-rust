//! Resolution rules and the SPEC §10.3 matrix. All pure: a policy is a compiled
//! value over an identifier, with no database and no clock anywhere near it.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{PolicyConfigError, RegistrationPolicy};
use crate::config::PolicyEntry;
use crate::domain::enums::OwnershipScope;
use std::collections::BTreeMap;
use toolkit_gts::gts_id;

const RG_BASE: &str = gts_id!("cf.core.rg.type.v1~");
const RG_DERIVED: &str = gts_id!("cf.core.rg.type.v1~acme.crm.group.type.v1~");
const PLUGIN_DERIVED: &str = gts_id!("cf.toolkit.plugins.plugin.v1~zeta.pkg.ns.type.v1~");
const ACME_OWN: &str = gts_id!("acme.crm.customer.type.v1~");
const OTHER_VENDOR: &str = gts_id!("zeta.crm.customer.type.v1~");

fn id(s: &str) -> gts::GtsId {
    gts::GtsId::try_new(s).unwrap_or_else(|e| panic!("fixture identifier {s} must parse: {e}"))
}

fn vendors(list: &[&str]) -> PolicyEntry {
    PolicyEntry {
        allowed_vendors: Some(list.iter().map(|v| (*v).to_owned()).collect()),
        tenant_ownable: None,
    }
}

fn entry(list: Option<&[&str]>, ownable: Option<bool>) -> PolicyEntry {
    PolicyEntry {
        allowed_vendors: list.map(|l| l.iter().map(|v| (*v).to_owned()).collect()),
        tenant_ownable: ownable,
    }
}

fn policy(pairs: Vec<(&str, PolicyEntry)>) -> RegistrationPolicy {
    let map: BTreeMap<String, PolicyEntry> =
        pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    RegistrationPolicy::compile(&map).expect("the fixture policy must compile")
}

// ---------------------------------------------------------------------------
// The closed default and the one implicit allowance
// ---------------------------------------------------------------------------

/// Closed by default: an empty policy admits nothing but global `cf`.
#[test]
fn an_empty_policy_admits_only_the_platform_vendor() {
    let p = RegistrationPolicy::default();
    assert!(p.is_empty());
    assert!(p.admits(&id(RG_BASE), OwnershipScope::Global).is_ok());

    let refusal = p
        .admits(&id(ACME_OWN), OwnershipScope::Global)
        .expect_err("a closed policy must refuse another vendor");
    assert_eq!(refusal.parameter, "allowed_vendors");
    assert_eq!(
        refusal.region, None,
        "no entry provided the parameter, which is a different situation from one that excluded \
         the vendor"
    );
}

/// The implicit allowance is keyed on the **last** segment's vendor, which is
/// what the policy governs. A `cf` base with an `acme` derivation is an `acme`
/// candidate.
#[test]
fn the_implicit_allowance_reads_the_last_segment_not_the_first() {
    let p = RegistrationPolicy::default();
    assert!(
        p.admits(&id(RG_DERIVED), OwnershipScope::Global).is_err(),
        "a cf-rooted identifier whose last segment is acme is an acme candidate",
    );
}

// ---------------------------------------------------------------------------
// The SPEC §10.3 / DESIGN §3.2 matrix, entry by entry
// ---------------------------------------------------------------------------

/// The four documented entries, asserted as one table. Written as a `vec!` and a
/// loop rather than through a fixture macro, per the task's own instruction.
#[test]
fn the_documented_policy_matrix_behaves_as_specified() {
    let p = policy(vec![
        (gts_id!("acme.*"), entry(Some(&["acme"]), Some(true))),
        (
            gts_id!("cf.core.rg.type.v1~*"),
            entry(Some(&["acme"]), Some(true)),
        ),
        (
            gts_id!("cf.core.rg.type.v1~"),
            entry(Some(&[]), Some(false)),
        ),
        (
            gts_id!("cf.toolkit.plugins.plugin.v1~*"),
            entry(Some(&["*"]), Some(false)),
        ),
    ]);

    // (candidate, admitted, why)
    let cases: Vec<(&str, bool, &str)> = vec![
        (
            ACME_OWN,
            true,
            "acme is onboarded in its own namespace by `gts.acme.*`",
        ),
        (
            RG_DERIVED,
            true,
            "acme may derive from the resource-group type",
        ),
        (
            RG_BASE,
            true,
            "the base type's own vendor is cf, which is implicitly admitted",
        ),
        (
            PLUGIN_DERIVED,
            true,
            "the plugin region admits any vendor via [\"*\"]",
        ),
        (
            OTHER_VENDOR,
            false,
            "zeta has no entry of its own and the default is closed",
        ),
    ];

    for (candidate, admitted, why) in cases {
        let got = p.admits(&id(candidate), OwnershipScope::Global);
        assert_eq!(got.is_ok(), admitted, "{candidate}: {why} — got {got:?}");
    }
}

/// The third row of the matrix is the one that needs the exact-key rule: an exact
/// entry governs **only** the base, while `…~*` governs the subtree. Not expressible
/// through `GtsIdPattern` — see the header of `domain/policy.rs`.
#[test]
fn an_exact_key_governs_only_the_base_not_its_subtree() {
    let p = policy(vec![
        (
            gts_id!("cf.core.rg.type.v1~*"),
            entry(Some(&["acme"]), Some(true)),
        ),
        (
            gts_id!("cf.core.rg.type.v1~"),
            entry(Some(&[]), Some(false)),
        ),
    ]);

    let (region, list) = p
        .allowed_vendors(&id(RG_BASE))
        .expect("the exact entry matches the base");
    assert_eq!(region, gts_id!("cf.core.rg.type.v1~"));
    assert!(list.is_empty(), "the base type stays closed");

    let (region, list) = p
        .allowed_vendors(&id(RG_DERIVED))
        .expect("the pattern entry matches the derivation");
    assert_eq!(
        region,
        gts_id!("cf.core.rg.type.v1~*"),
        "the exact key must not reach the subtree",
    );
    assert_eq!(list, ["acme"]);
}

// ---------------------------------------------------------------------------
// Per-parameter resolution
// ---------------------------------------------------------------------------

/// A more specific set **replaces** a less-specific one rather than extending
/// it. Without this a narrow entry could only ever widen a broad one, and
/// restricting a subtree would be inexpressible.
#[test]
fn a_more_specific_vendor_set_replaces_rather_than_extends() {
    let p = policy(vec![
        (gts_id!("cf.core.*"), vendors(&["acme", "zeta"])),
        (gts_id!("cf.core.rg.type.v1~*"), vendors(&["acme"])),
    ]);

    let (region, list) = p
        .allowed_vendors(&id(RG_DERIVED))
        .expect("an entry matches");
    assert_eq!(region, gts_id!("cf.core.rg.type.v1~*"));
    assert_eq!(list, ["acme"], "zeta must not survive from the broader set");

    let zeta_derived = gts_id!("cf.core.rg.type.v1~zeta.crm.group.type.v1~");
    assert!(
        p.admits(&id(zeta_derived), OwnershipScope::Global).is_err(),
        "the narrower entry excludes zeta inside the resource-group region",
    );
    let zeta_elsewhere = gts_id!("cf.core.other.type.v1~zeta.crm.group.type.v1~");
    assert!(
        p.admits(&id(zeta_elsewhere), OwnershipScope::Global)
            .is_ok(),
        "and leaves it admitted where only the broader entry matches",
    );
}

/// An entry that omits a parameter is skipped for it, and a less-specific entry
/// still provides it. This is why both parameters are `Option`: collapsing the
/// absent case onto the closed value would make the narrow entry close what the
/// broad one opened.
#[test]
fn an_entry_omitting_a_parameter_is_skipped_and_a_broader_one_supplies_it() {
    let p = policy(vec![
        (gts_id!("cf.core.*"), entry(Some(&["acme"]), None)),
        // Matches more specifically, but names only `tenant_ownable`.
        (gts_id!("cf.core.rg.type.v1~*"), entry(None, Some(true))),
    ]);

    let (region, list) = p
        .allowed_vendors(&id(RG_DERIVED))
        .expect("the broader entry supplies the vendor set");
    assert_eq!(region, gts_id!("cf.core.*"));
    assert_eq!(list, ["acme"]);

    let (region, ownable) = p
        .tenant_ownable(&id(RG_DERIVED))
        .expect("the narrower entry supplies the ownership parameter");
    assert_eq!(region, gts_id!("cf.core.rg.type.v1~*"));
    assert!(ownable);
}

/// The two parameters resolve independently: the most specific entry for one is
/// not necessarily the most specific for the other.
#[test]
fn the_two_parameters_resolve_independently() {
    let p = policy(vec![
        (gts_id!("cf.*"), entry(Some(&["acme"]), Some(true))),
        (gts_id!("cf.core.*"), entry(Some(&["zeta"]), None)),
    ]);
    let candidate = id(gts_id!("cf.core.rg.type.v1~zeta.crm.group.type.v1~"));

    assert_eq!(
        p.allowed_vendors(&candidate).expect("vendors").0,
        gts_id!("cf.core.*"),
    );
    assert_eq!(
        p.tenant_ownable(&candidate).expect("ownable").0,
        gts_id!("cf.*")
    );
}

/// Longest literal prefix wins among patterns.
#[test]
fn the_longest_literal_prefix_wins_among_patterns() {
    let p = policy(vec![
        (gts_id!("*"), vendors(&["acme"])),
        (gts_id!("cf.*"), vendors(&["zeta"])),
        (gts_id!("cf.core.*"), vendors(&["omega"])),
    ]);
    let candidate = id(gts_id!("cf.core.rg.type.v1~omega.crm.group.type.v1~"));
    let (region, list) = p.allowed_vendors(&candidate).expect("vendors");
    assert_eq!(region, gts_id!("cf.core.*"));
    assert_eq!(list, ["omega"]);
}

/// An exact key beats a pattern even where the pattern's literal prefix is
/// longer, which is the rule stated separately from prefix length.
#[test]
fn an_exact_key_beats_a_pattern_regardless_of_length() {
    let exact = gts_id!("acme.crm.customer.type.v1~");
    let p = policy(vec![
        (exact, vendors(&[])),
        (gts_id!("acme.crm.customer.type.v1~*"), vendors(&["acme"])),
    ]);
    let (region, list) = p.allowed_vendors(&id(exact)).expect("vendors");
    assert_eq!(region, exact);
    assert!(list.is_empty());
}

// ---------------------------------------------------------------------------
// tenant_ownable: parsed, validated, inert
// ---------------------------------------------------------------------------

/// A policy carrying `tenant_ownable: true` compiles, resolves, and still does
/// not admit a candidate asking for tenant ownership: P0 has no tenant-owned
/// rows at all, so this is a fail-closed assertion about an unreachable request
/// shape rather than a feature (SPEC §10.3).
#[test]
fn tenant_ownable_is_resolved_but_never_admits_tenant_ownership() {
    let p = policy(vec![(
        gts_id!("acme.*"),
        entry(Some(&["acme"]), Some(true)),
    )]);
    let candidate = id(ACME_OWN);

    assert_eq!(
        p.tenant_ownable(&candidate).expect("resolved"),
        (gts_id!("acme.*"), true),
        "the parameter is parsed and resolvable, which is what makes it inert rather than dropped",
    );
    assert!(
        p.admits(&candidate, OwnershipScope::Global).is_ok(),
        "a global candidate in an open region is admitted",
    );

    let refusal = p
        .admits(&candidate, OwnershipScope::Tenant)
        .expect_err("tenant ownership must be refused in P0");
    assert_eq!(refusal.parameter, "tenant_ownable");
    assert_eq!(refusal.region.as_deref(), Some(gts_id!("acme.*")));
}

/// The vendor gate runs first, so a candidate that fails both is reported by the
/// parameter an operator can act on.
#[test]
fn the_vendor_parameter_is_reported_before_the_ownership_one() {
    let p = RegistrationPolicy::default();
    let refusal = p
        .admits(&id(ACME_OWN), OwnershipScope::Tenant)
        .expect_err("both parameters refuse");
    assert_eq!(refusal.parameter, "allowed_vendors");
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

/// An unparsable region fails compilation with the region named, so startup
/// fails rather than silently treating it as closed.
#[test]
fn an_unparsable_region_names_itself() {
    let mut map = BTreeMap::new();
    map.insert("not-a-gts-identifier".to_owned(), vendors(&["acme"]));
    let err = RegistrationPolicy::compile(&map).expect_err("an invalid region must be rejected");
    match err {
        PolicyConfigError::InvalidRegion { key, .. } => assert_eq!(key, "not-a-gts-identifier"),
        PolicyConfigError::EmptyVendor { .. } => panic!("expected InvalidRegion, got EmptyVendor"),
    }
}

/// A wildcard that is not a single trailing one is `gts-rust`'s verdict, not a
/// rule reimplemented here.
#[test]
fn a_non_trailing_wildcard_is_rejected_by_the_library() {
    let mut map = BTreeMap::new();
    map.insert("gts.*.crm.customer.type.v1~".to_owned(), vendors(&["acme"]));
    let err =
        RegistrationPolicy::compile(&map).expect_err("a mid-string wildcard must be rejected");
    assert!(matches!(err, PolicyConfigError::InvalidRegion { .. }));
}

/// A blank vendor token would sit in the set matching nothing, which reads as a
/// typo an operator should be told about.
#[test]
fn a_blank_vendor_token_is_rejected() {
    let mut map = BTreeMap::new();
    map.insert(gts_id!("acme.*").to_owned(), vendors(&["acme", "  "]));
    let err = RegistrationPolicy::compile(&map).expect_err("a blank vendor must be rejected");
    match err {
        PolicyConfigError::EmptyVendor { key } => assert_eq!(key, gts_id!("acme.*")),
        PolicyConfigError::InvalidRegion { .. } => {
            panic!("expected EmptyVendor, got InvalidRegion")
        }
    }
}

/// Configuration whitespace is not part of a vendor token. Normalize it once
/// at compile time so the exact admission comparison cannot silently turn an
/// otherwise valid entry into a region that matches nothing.
#[test]
fn vendor_tokens_are_trimmed_when_the_policy_is_compiled() {
    let p = policy(vec![(gts_id!("acme.*"), vendors(&["  acme\t"]))]);
    let (region, allowed) = p
        .allowed_vendors(&id(ACME_OWN))
        .expect("the configured region must resolve");

    assert_eq!(region, gts_id!("acme.*"));
    assert_eq!(allowed, ["acme"]);
    assert!(p.admits(&id(ACME_OWN), OwnershipScope::Global).is_ok());
}

/// An empty vendor list is *not* an error: it is how the matrix keeps a base type
/// closed while its subtree is open.
#[test]
fn an_empty_vendor_list_compiles_and_closes_the_region() {
    let p = policy(vec![(gts_id!("acme.*"), vendors(&[]))]);
    assert_eq!(p.len(), 1);
    let refusal = p
        .admits(&id(ACME_OWN), OwnershipScope::Global)
        .expect_err("an empty set admits nobody");
    assert_eq!(refusal.region.as_deref(), Some(gts_id!("acme.*")));
}

/// The evidence for the exact-key rule, asserted against `gts-rust` directly rather
/// than taken on trust: a **bare** identifier used as a pattern accepts its
/// derivations too (GTS spec §3.6 implicit derived-type coverage). Compiled to a
/// pattern, `gts.cf.core.rg.type.v1~` would close its whole subtree — and, being
/// exact, outrank the `…~*` entry that opens it.
#[test]
fn a_bare_identifier_used_as_a_pattern_covers_its_derivations() {
    let pattern = gts::GtsIdPattern::try_new(RG_BASE).expect("a bare identifier is a pattern");
    assert!(
        id(RG_DERIVED).matches_pattern(&pattern),
        "this is why an exact policy key is compared by equality, not matched as a pattern",
    );
    assert!(id(RG_BASE).matches_pattern(&pattern));
}

/// A UUID tail is not a named segment: it carries no vendor at all. The vendor
/// therefore comes from the last *named* segment, so such a candidate is judged
/// on the region it actually names and is then refused by the identifier-profile
/// gate (§8.1 step 4) rather than by a policy message about an empty vendor.
#[test]
fn a_uuid_tail_resolves_the_vendor_from_the_last_named_segment() {
    let with_tail = format!("{RG_BASE}550e8400-e29b-41d4-a716-446655440000");
    let candidate = id(&with_tail);
    assert!(
        RegistrationPolicy::default()
            .admits(&candidate, OwnershipScope::Global)
            .is_ok(),
        "the last named segment's vendor is cf, which is implicitly admitted",
    );

    let acme_tail = format!("{ACME_OWN}550e8400-e29b-41d4-a716-446655440000");
    let p = policy(vec![(gts_id!("acme.*"), vendors(&["acme"]))]);
    assert!(
        p.admits(&id(&acme_tail), OwnershipScope::Global).is_ok(),
        "and an opened region admits it, leaving the tail to the profile gate",
    );
}
