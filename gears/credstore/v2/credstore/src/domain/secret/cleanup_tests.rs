use credstore_sdk::{DestroySelector, StoreKey, TenantId, ValueVersion};
use uuid::Uuid;

use super::{DeadContent, cleanup_tasks};
use crate::domain::secret::model::CleanupTask;

fn key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::from_u128(1)), Uuid::from_u128(2))
}

fn v(s: &str) -> ValueVersion {
    ValueVersion::new(s)
}

fn destroy(selector: DestroySelector) -> CleanupTask {
    CleanupTask::Destroy {
        key: key(),
        selector,
    }
}

#[test]
fn superseded() {
    let (k, new) = (key(), v("7"));
    let dead = DeadContent::Superseded { key: &k, new: &new };
    assert_eq!(
        cleanup_tasks(dead, true),
        vec![destroy(DestroySelector::Below(v("7")))]
    );
    assert_eq!(cleanup_tasks(dead, false), Vec::new());
}

#[test]
fn lost_write_row_exists() {
    let (k, version) = (key(), v("3"));
    let dead = DeadContent::LostWrite {
        key: &k,
        version: &version,
        row_exists: true,
    };
    assert_eq!(
        cleanup_tasks(dead, true),
        vec![destroy(DestroySelector::Exactly(v("3")))]
    );
    assert_eq!(cleanup_tasks(dead, false), Vec::new());
}

#[test]
fn lost_write_row_missing() {
    let (k, version) = (key(), v("3"));
    let dead = DeadContent::LostWrite {
        key: &k,
        version: &version,
        row_exists: false,
    };
    assert_eq!(cleanup_tasks(dead, true), vec![CleanupTask::Purge(key())]);
    assert_eq!(cleanup_tasks(dead, false), vec![CleanupTask::Purge(key())]);
}

#[test]
fn removed() {
    let (k, old) = (key(), v("5"));
    let dead = DeadContent::Removed { key: &k, old: &old };
    assert_eq!(
        cleanup_tasks(dead, true),
        vec![
            destroy(DestroySelector::Below(v("5"))),
            destroy(DestroySelector::Exactly(v("5"))),
        ]
    );
    assert_eq!(cleanup_tasks(dead, false), Vec::new());
}

#[test]
fn dead_key() {
    let k = key();
    let dead = DeadContent::DeadKey { key: &k };
    assert_eq!(cleanup_tasks(dead, true), vec![CleanupTask::Purge(key())]);
    assert_eq!(cleanup_tasks(dead, false), vec![CleanupTask::Purge(key())]);
}

#[test]
fn purges_never_depend_on_destroy_support() {
    let (k, version) = (key(), v("1"));
    for dead in [
        DeadContent::DeadKey { key: &k },
        DeadContent::LostWrite {
            key: &k,
            version: &version,
            row_exists: false,
        },
    ] {
        assert_eq!(cleanup_tasks(dead, true), cleanup_tasks(dead, false));
        assert_eq!(cleanup_tasks(dead, false), vec![CleanupTask::Purge(key())]);
    }
}
