// Created: 2026-08-13 by Constructor Tech
//! Tests for category visibility.
//!
//! A wrong predicate here is a disclosure bug in one direction and an
//! invisible-category bug in the other, so both arms are pinned.

use toolkit_security::{AccessScope, EqScopeFilter, InScopeFilter, ScopeConstraint, ScopeFilter};

use super::{DOMAIN_PROPERTY, DomainVisibility, domain_visibility, is_visible};

fn scoped_to(domains: &[&str]) -> AccessScope {
    let values = domains.iter().map(|d| (*d).into()).collect();
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::In(
        InScopeFilter::new(DOMAIN_PROPERTY, values),
    )]))
}

#[test]
fn an_unconstrained_scope_is_unrestricted() {
    assert_eq!(
        domain_visibility(&AccessScope::allow_all()),
        DomainVisibility::Unrestricted
    );
}

#[test]
fn a_scope_without_a_domain_constraint_is_unrestricted() {
    // Step 2: the query is returned unchanged rather than augmented with a
    // predicate that matches everything.
    let unrelated = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::Eq(
        EqScopeFilter::new("tenant_id", "acme"),
    )]));
    assert_eq!(
        domain_visibility(&unrelated),
        DomainVisibility::Unrestricted
    );
}

#[test]
fn a_domain_constraint_is_read_off_the_scope() {
    assert_eq!(
        domain_visibility(&scoped_to(&["infra", "billing"])),
        DomainVisibility::Restricted(vec!["infra".to_owned(), "billing".to_owned()])
    );
}

#[test]
fn an_undomained_category_is_visible_to_a_scoped_caller() {
    // The null arm, and the rule most easily dropped. Without it every category
    // with no domain disappears for every scoped administrator -- categories
    // nobody can find, which still occupy their keys.
    let restricted = DomainVisibility::Restricted(vec!["infra".to_owned()]);
    assert!(is_visible(&restricted, None));
}

#[test]
fn a_category_in_a_permitted_domain_is_visible() {
    let restricted = DomainVisibility::Restricted(vec!["infra".to_owned()]);
    assert!(is_visible(&restricted, Some("infra")));
}

#[test]
fn a_category_outside_the_permitted_domains_is_not_visible() {
    let restricted = DomainVisibility::Restricted(vec!["infra".to_owned()]);
    assert!(!is_visible(&restricted, Some("billing")));
}

#[test]
fn domain_matching_is_exact() {
    // Not a prefix and not case-insensitive. "infra" must not admit "infra-2",
    // or a domain name could be widened by whoever chose the adjacent one.
    let restricted = DomainVisibility::Restricted(vec!["infra".to_owned()]);
    for outside in ["infra-2", "Infra", "INFRA", "inf"] {
        assert!(
            !is_visible(&restricted, Some(outside)),
            "`{outside}` must not match `infra`"
        );
    }
}

#[test]
fn an_unrestricted_caller_sees_every_domain_and_the_undomained() {
    for domain in [Some("infra"), Some("billing"), None] {
        assert!(is_visible(&DomainVisibility::Unrestricted, domain));
    }
}
