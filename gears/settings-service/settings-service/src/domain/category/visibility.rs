// Created: 2026-08-13 by Virtuozzo International GmbH
//! Which categories an administrator may see.
//!
//! The policy decision point returns more than allow or deny — it returns
//! **constraints**. An administrator scoped to one administrative domain is
//! allowed to read categories, but not all of them. This turns that constraint
//! into a predicate.
//!
//! # Two rules that are easy to get wrong
//!
//! **An undomained category is visible to everyone.** The predicate is
//! `domain_affinity IS NULL OR domain_affinity IN (permitted)`, never the `IN`
//! alone. Dropping the null arm hides every category that has no domain from
//! every scoped administrator — categories nobody can find, which still occupy
//! their keys and still reject a create that reuses one.
//!
//! **The predicate goes inside the query.** Post-filtering returns a page of 25
//! rows, discards the ones the caller cannot see, and hands back 3 — while the
//! cursor still points at row 25. The caller gets short pages, wrong counts,
//! and a next-page link that skips rows it *was* entitled to. That leaks the
//! shape of what it cannot see without ever showing the content.
//!
//! **A constraint that names no usable domain is still a constraint.** The
//! policy decision point meant to narrow this caller; if what it sent cannot be
//! read as domain names — uuids, integers, booleans where names are due — the
//! narrowing must not evaporate into "see everything". It narrows to the
//! undomained categories, which every scoped caller sees anyway, until the
//! policy is fixed. Only the *absence* of a domain constraint means unrestricted.

use toolkit_security::{AccessScope, ScopeFilter, ScopeValue};

/// The `AccessScope` property carrying an administrative-domain restriction.
///
/// Policies are written against this name, so it is a contract with the policy
/// decision point rather than an internal choice.
pub const DOMAIN_PROPERTY: &str = "domain_affinity";

/// The domains a caller may see, or unrestricted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainVisibility {
    /// No domain restriction — every category is visible.
    Unrestricted,
    /// Only categories in these domains, plus every undomained one. An empty
    /// list is the fail-closed reading of a domain constraint that carried no
    /// usable domain name: the undomained categories and nothing else.
    Restricted(Vec<String>),
}

/// Read the administrative-domain restriction off a caller's scope.
///
/// Returns [`DomainVisibility::Unrestricted`] when the scope carries no domain
/// constraint, which is step 2 of the algorithm: an unconstrained caller's
/// query is returned unchanged rather than augmented with a predicate matching
/// everything. A domain constraint that is present but yields no usable name is
/// the opposite case and restricts to the undomained categories alone.
#[must_use]
pub fn domain_visibility(scope: &AccessScope) -> DomainVisibility {
    // @cpt-begin:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-1
    if scope.is_unconstrained() {
        return DomainVisibility::Unrestricted;
    }

    let mut domains: Vec<String> = Vec::new();
    let mut constrained = false;
    for constraint in scope.constraints() {
        for filter in constraint.filters() {
            let (property, values) = match filter {
                ScopeFilter::Eq(f) => (f.property(), vec![f.value().clone()]),
                ScopeFilter::In(f) => (f.property(), f.values().to_vec()),
                // Group and tenant-subtree filters describe a hierarchy, not an
                // administrative domain. Ignoring them here is deliberate: they
                // are enforced by the secure query layer, and reinterpreting one
                // as a domain would narrow visibility on a rule nobody wrote.
                _ => continue,
            };
            if property == DOMAIN_PROPERTY {
                constrained = true;
                domains.extend(values.iter().filter_map(|v| match v {
                    ScopeValue::String(s) => Some(s.clone()),
                    // A domain is a name. A uuid, integer or boolean in this
                    // position is a policy authoring error, and silently
                    // stringifying one would invent a domain that matches
                    // nothing -- hiding every category from that caller.
                    _ => None,
                }));
            }
        }
    }
    // @cpt-end:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-1

    // @cpt-begin:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-2
    if !constrained {
        return DomainVisibility::Unrestricted;
    }
    // @cpt-end:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-2
    // A constraint was there but none of its values named a domain: the
    // policy is broken, not absent. Restricting to the undomained categories
    // is what the caller would see under any domain list; widening to every
    // domain is what the policy set out to prevent.

    // @cpt-begin:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-5
    DomainVisibility::Restricted(domains)
    // @cpt-end:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-5
}

/// Whether a category with this domain is visible under the given restriction.
///
/// The row-level form of the same rule, for a single loaded category. `list`
/// applies the predicate in SQL; `get` loads one row and asks this — a
/// domain-scoped caller fetching a category outside its domain must receive the
/// not-found answer rather than the row.
#[must_use]
pub fn is_visible(visibility: &DomainVisibility, domain_affinity: Option<&str>) -> bool {
    match visibility {
        DomainVisibility::Unrestricted => true,
        // The null arm. An undomained category belongs to no domain and is
        // therefore excluded by no domain restriction.
        DomainVisibility::Restricted(_) if domain_affinity.is_none() => true,
        DomainVisibility::Restricted(permitted) => {
            domain_affinity.is_some_and(|d| permitted.iter().any(|p| p == d))
        }
    }
}

#[cfg(test)]
#[path = "visibility_tests.rs"]
mod visibility_tests;
