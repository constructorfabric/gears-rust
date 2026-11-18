//! Rules over a caller's tenant chain (`[caller, parent, ..., root]`), as
//! alias resolution builds it.

use uuid::Uuid;

/// Whether `owner` is `auth_owner` or a descendant of it on `chain` (index 0
/// is the caller, the last the root). An owner or auth owner off the chain is
/// outside. ADR-0022 guard 3 bounds a secret's owner by it.
#[must_use]
pub(crate) fn owner_within(owner: Uuid, auth_owner: Uuid, chain: &[Uuid]) -> bool {
    let position = |t: Uuid| chain.iter().position(|c| *c == t);
    match (position(owner), position(auth_owner)) {
        (Some(owner), Some(auth_owner)) => owner <= auth_owner,
        _ => false,
    }
}
