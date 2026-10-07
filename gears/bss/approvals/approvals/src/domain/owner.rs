//! Which configured source holds one unit id.
//!
//! Every source is asked. One `Some` wins, including when another source is forbidden, down, or
//! missing. Two `Some`s are ambiguous. Otherwise a down or missing source is unavailable, then a
//! forbidden source is a refusal that names no gear, then a miss is not found.

use bss_approvals_sdk::InboxUnit;
use toolkit_canonical_errors::CanonicalError;

use super::error;

/// What one source answered for an id.
pub enum SourceGet {
    /// This source holds the unit.
    Found(Box<InboxUnit>),
    /// This source has no such unit for the caller's tenant.
    Absent,
    /// This source refused the read.
    Forbidden,
    /// This source is down, or it is not registered.
    Unavailable,
    /// This source refused the read for a reason the inbox does not classify.
    Failed(Box<CanonicalError>),
}

/// The inbox's decision after every source has answered.
pub enum Resolved {
    /// Exactly one source holds the unit.
    Found {
        source: String,
        unit: Box<InboxUnit>,
    },
    /// Every source missed.
    Missing,
    /// At least one source refused, and none holds the unit or is unavailable.
    Forbidden,
    /// At least one source is down or missing, and none holds the unit.
    Unavailable { sources: Vec<String> },
    /// More than one source holds the unit.
    Ambiguous { sources: Vec<String> },
    /// A source returned an error the inbox does not classify, and none holds the unit.
    Failed(Box<CanonicalError>),
}

/// Resolves the owner from every source's answer, in the order the sources were asked.
#[must_use]
pub fn resolve(answers: Vec<(String, SourceGet)>) -> Resolved {
    let mut found: Vec<(String, Box<InboxUnit>)> = Vec::new();
    let mut unavailable = Vec::new();
    let mut forbidden = false;
    let mut failed: Option<Box<CanonicalError>> = None;
    for (name, answer) in answers {
        match answer {
            SourceGet::Found(unit) => found.push((name, unit)),
            SourceGet::Absent => {}
            SourceGet::Forbidden => forbidden = true,
            SourceGet::Unavailable => unavailable.push(name),
            SourceGet::Failed(err) => {
                if failed.is_none() {
                    failed = Some(err);
                }
            }
        }
    }
    match found.len() {
        0 => {}
        1 => {
            let (source, unit) = found.remove(0);
            return Resolved::Found { source, unit };
        }
        _ => {
            return Resolved::Ambiguous {
                sources: found.into_iter().map(|(name, _)| name).collect(),
            };
        }
    }
    if let Some(err) = failed {
        return Resolved::Failed(err);
    }
    if !unavailable.is_empty() {
        return Resolved::Unavailable {
            sources: unavailable,
        };
    }
    if forbidden {
        return Resolved::Forbidden;
    }
    Resolved::Missing
}

/// The owning source and its unit, or the card's refusal.
///
/// # Errors
/// Not found, forbidden, unavailable, ambiguous, or the source's own error.
pub fn require_one(resolved: Resolved) -> Result<(String, InboxUnit), CanonicalError> {
    match resolved {
        Resolved::Found { source, unit } => Ok((source, *unit)),
        Resolved::Missing => Err(error::not_found()),
        Resolved::Forbidden => Err(error::forbidden()),
        Resolved::Unavailable { sources } => Err(error::source_unavailable(&sources)),
        Resolved::Ambiguous { sources } => Err(error::ambiguous(&sources)),
        Resolved::Failed(err) => Err(*err),
    }
}
