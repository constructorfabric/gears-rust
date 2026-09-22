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
        // A namespace is an identity: it is compared to the registry's owner
        // by exact equality, and an operator reads it back out of a log line.
        // A control character breaks both. `tracing`'s `Display` fields are
        // not escaped, and the text console formatter is a supported
        // production choice, so a `system` of "github\nlevel=error msg=..."
        // reaches an operator's stream as a second, forged record -- written
        // by a producer that was refused the write. Refused here, where the
        // value is first read, so it reaches neither the log nor the registry.
        Some(system) if system.chars().any(char::is_control) => Err(DomainError::invalid(
            "`payload.source.system` cannot carry control characters: it is the namespace the \
             write is authorized against, and it is written to the operator log",
        )),
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

/// Whether a principal is in the form the security context produces.
///
/// [`decide`] compares by exact equality, and the writer's side of that
/// comparison always comes from `Subject::principal()`, which carries neither
/// surrounding whitespace nor control characters. An owner that carries either
/// therefore matches no writer that can ever exist: storing one does not
/// transfer the namespace, it strands it, and every write from the intended
/// producer is refused until an administrator notices and transfers again.
///
/// This reports rather than repairs. Trimming would store an owner the
/// administrator did not ask for, and it would fix only half the shape anyway
/// -- a principal's case is not ours to fold, because nothing says principals
/// are compared case-insensitively, and folding it would be the same silent
/// substitution in the other direction.
///
/// Emptiness is deliberately not checked here: a caller that wants to say
/// "name someone" says it in its own words, and this answers a different
/// question.
#[must_use]
pub fn is_canonical_principal(principal: &str) -> bool {
    principal.trim() == principal && !principal.chars().any(char::is_control)
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

    /// The shapes that read as a transfer but cannot be one. Each of these
    /// stores an owner no writer can equal, so the namespace stops accepting
    /// writes from the producer it was just handed to.
    /// The refusal is logged with the namespace in it, and `tracing` does not
    /// escape a `Display` field. Under the text console formatter an embedded
    /// newline is written as one, so a producer that is refused the write
    /// could still put a line of its choosing into the operator's stream.
    #[test]
    fn a_namespace_that_could_forge_a_log_line_is_malformed() {
        for system in [
            "github\nlevel=error msg=\"fake alert\"",
            "github\rlevel=error",
            "git\u{0}hub",
        ] {
            let payload = json!({
                "source": { "system": system, "kind": "commit", "native_id": "a1" }
            });
            assert!(
                namespace_of(Some("reference"), Some(&payload)).is_err(),
                "{system:?} must be refused rather than logged"
            );
        }
    }

    /// The trim happens first, so a value that is only padded is still a
    /// perfectly good namespace -- the refusal above is about what a trim
    /// cannot reach.
    #[test]
    fn a_padded_namespace_is_still_read_as_its_trimmed_self() {
        let payload = json!({
            "source": { "system": "  github  ", "kind": "commit", "native_id": "a1" }
        });
        assert_eq!(
            namespace_of(Some("reference"), Some(&payload)).expect("padding is not a control char"),
            Namespaced::Under("github")
        );
    }

    #[test]
    fn a_principal_no_writer_could_equal_is_not_canonical() {
        for principal in [
            "mirror-gear ",
            " mirror-gear",
            "mirror-gear\n",
            "mirror\tgear",
            "mirror-gear\u{0}",
        ] {
            assert!(
                !is_canonical_principal(principal),
                "{principal:?} must not pass as a principal"
            );
        }
    }

    #[test]
    fn the_shapes_a_security_context_produces_are_canonical() {
        for principal in [
            "mirror-gear",
            "0191f0a4-1b2c-7def-8a90-0123456789ab",
            "service:mirror-gear",
        ] {
            assert!(
                is_canonical_principal(principal),
                "{principal:?} must pass as a principal"
            );
        }
    }
}
