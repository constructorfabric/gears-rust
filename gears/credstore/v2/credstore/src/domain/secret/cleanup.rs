//! The ADR-0006 rule that decides which store content a committed
//! transaction made dead.
//!
//! Repositories gather the facts only their transaction knows (the new and
//! old value versions, whether a row with the record id still exists), call
//! [`cleanup_tasks`] inside that transaction and record the returned tasks in
//! the same transaction. Every repository implementation, including test
//! doubles, MUST call this function, so the rule exists exactly once.

use credstore_sdk::{DestroySelector, StoreKey, ValueVersion};
use toolkit_macros::domain_model;

use super::model::CleanupTask;

/// What a repository transaction made dead in the store.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub enum DeadContent<'a> {
    /// A pointer switch committed to `new` (replace, or a patch carrying a
    /// secret): produced by the tx1 commit of `SecretRepo::switch_value`
    /// and by the retried switch of `SecretRepo::verify_switch_value`.
    Superseded {
        key: &'a StoreKey,
        new: &'a ValueVersion,
    },
    /// A write of `version` lost definitively or was not applied; `row_exists`
    /// says whether a row with this record id still exists. Produced by a
    /// create that lost to a concurrent create (`SecretRepo::insert_active`,
    /// `row_exists: false`), a lost CAS (`SecretRepo::switch_value`), and the
    /// verification "not applied" outcome (`SecretRepo::verify_insert_active`,
    /// `SecretRepo::verify_switch_value`).
    LostWrite {
        key: &'a StoreKey,
        version: &'a ValueVersion,
        row_exists: bool,
    },
    /// `PATCH {"secret": null}` removed the value version `old`: produced by
    /// `SecretRepo::remove_value`.
    Removed {
        key: &'a StoreKey,
        old: &'a ValueVersion,
    },
    /// The record was deleted (`SecretRepo::delete_by_id`, or its retried leg
    /// in `SecretRepo::verify_delete`), or a failed create was healed
    /// (`SecretRepo::heal_failed_creates`): the key can never hold a live
    /// value.
    DeadKey { key: &'a StoreKey },
}

/// The cleanup tasks to record for `dead`.
///
/// Destroy tasks are produced only when the plugin supports `destroy`
/// (`destroy_supported`); purges never depend on it.
#[must_use]
pub fn cleanup_tasks(dead: DeadContent<'_>, destroy_supported: bool) -> Vec<CleanupTask> {
    match dead {
        DeadContent::Superseded { key, new } if destroy_supported => {
            vec![CleanupTask::Destroy {
                key: key.clone(),
                selector: DestroySelector::Below(new.clone()),
            }]
        }
        DeadContent::LostWrite {
            key,
            row_exists: false,
            ..
        }
        | DeadContent::DeadKey { key } => vec![CleanupTask::Purge(key.clone())],
        DeadContent::LostWrite { key, version, .. } if destroy_supported => {
            vec![CleanupTask::Destroy {
                key: key.clone(),
                selector: DestroySelector::Exactly(version.clone()),
            }]
        }
        // `Below` is exclusive, so the version itself needs its own task;
        // never a purge: a concurrent writer may already have put a newer
        // version under the same key.
        DeadContent::Removed { key, old } if destroy_supported => vec![
            CleanupTask::Destroy {
                key: key.clone(),
                selector: DestroySelector::Below(old.clone()),
            },
            CleanupTask::Destroy {
                key: key.clone(),
                selector: DestroySelector::Exactly(old.clone()),
            },
        ],
        DeadContent::Superseded { .. }
        | DeadContent::LostWrite { .. }
        | DeadContent::Removed { .. } => Vec::new(),
    }
}

#[cfg(test)]
#[path = "cleanup_tests.rs"]
mod cleanup_tests;
