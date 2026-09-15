//! Source-namespace ownership: the pure half
//! (`cpt-cf-graph-storage-fr-source-ownership`).
//!
//! A reference node's identity is the triple `(source, kind, native id)`, so
//! two producers naming the same upstream object converge on one node — which
//! is the point, and which is also why a generic `write` permission must not
//! be enough to write *any* triple: `source` inside a validly typed payload
//! proves nothing about who may speak for it (DESIGN § Authorization Model).
//!
//! What lives here is the reading of a payload and the decision. Who owns what
//! is a row in the registry, and reading that row is the store's job.

use crate::domain::error::DomainError;

/// The family whose nodes carry a source namespace. Owned nodes have none,
/// and phantoms are the gear's own until they are materialized.
const REFERENCE: &str = "reference";

/// What a node's type and payload say about the namespace it is written under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Namespaced<'a> {
    /// A reference node, writing under this namespace.
    Under(&'a str),
    /// Not a reference node: no namespace, and nothing to authorize.
    None,
}

/// Read the namespace a node writes under, from its own payload.
///
/// The reference-node base requires `payload.source.{system, kind, native_id}`
/// and chain validation has already run, so a reference node without a
/// `system` is a malformed submission rather than an unowned one — refused,
/// not silently treated as unclaimed.
pub fn namespace_of<'a>(
    family: Option<&str>,
    payload: Option<&'a serde_json::Value>,
) -> Result<Namespaced<'a>, DomainError> {
    if family != Some(REFERENCE) {
        return Ok(Namespaced::None);
    }
    let system = payload
        .and_then(|p| p.pointer("/source/system"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|system| !system.is_empty());
    match system {
        Some(system) => Ok(Namespaced::Under(system)),
        None => Err(DomainError::invalid(
            "a reference node must carry `payload.source.system`: it is the namespace the write \
             is authorized against",
        )),
    }
}

/// What the store should do about one namespaced write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// Nobody holds it: the writer takes it. An unclaimed namespace is claimed
    /// by its first writer, which keeps a single-producer deployment free of
    /// setup while still making the *second* writer a decision.
    Take,
    /// The writer already holds it.
    Allowed,
    /// Someone else holds it.
    Forbidden,
}

/// Decide one write against the registry's current owner.
///
/// The registry is the authority, not `node.owner_principal`: that column
/// records who created a row and never changes, so consulting it would make an
/// ownership transfer unusable — the new owner could not touch a row the old
/// one created (DESIGN § 3.7, "the registry table … is the authority the
/// comparison consults").
#[must_use]
pub fn decide(current_owner: Option<&str>, writer: &str) -> Claim {
    match current_owner {
        None => Claim::Take,
        Some(owner) if owner == writer => Claim::Allowed,
        Some(_) => Claim::Forbidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_owned_node_has_no_namespace_to_authorize() {
        let payload = json!({ "source": { "system": "github" } });
        assert_eq!(
            namespace_of(Some("owned"), Some(&payload)).expect("owned nodes are not namespaced"),
            Namespaced::None,
            "a `source` in an owned node's payload is a payload field, not a boundary"
        );
    }

    #[test]
    fn a_reference_node_writes_under_its_declared_system() {
        let payload =
            json!({ "source": { "system": "github", "kind": "commit", "native_id": "a1" } });
        assert_eq!(
            namespace_of(Some("reference"), Some(&payload)).expect("the system is there"),
            Namespaced::Under("github")
        );
    }

    #[test]
    fn a_reference_node_without_a_system_is_malformed_rather_than_unowned() {
        for payload in [
            json!({}),
            json!({ "source": {} }),
            json!({ "source": { "system": "  " } }),
        ] {
            let error = namespace_of(Some("reference"), Some(&payload))
                .expect_err("an unnamed namespace cannot be authorized");
            assert!(error.to_string().contains("source.system"), "{error}");
        }
        assert!(namespace_of(Some("reference"), None).is_err());
    }

    /// The three states the boundary has, and the one that makes a
    /// single-producer deployment need no setup.
    #[test]
    fn an_unclaimed_namespace_is_claimed_by_its_first_writer() {
        assert_eq!(decide(None, "producer-a"), Claim::Take);
        assert_eq!(decide(Some("producer-a"), "producer-a"), Claim::Allowed);
        assert_eq!(decide(Some("producer-a"), "producer-b"), Claim::Forbidden);
    }
}
