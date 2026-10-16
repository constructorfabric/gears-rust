//! The stored integers of the `SMALLINT` enums are pinned literally (they are a
//! storage contract, see `enums.rs`), and every value round-trips to its
//! domain / SDK counterpart.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use credstore_sdk::{DestroySelector, SharingMode, StoreKey, TenantId, ValueVersion};
use sea_orm::{ActiveEnum, Iterable};
use uuid::Uuid;

use super::{DbCleanupOp, DbDestroySelector, DbFallback, DbSharing, DbStatus};
use crate::domain::secret::model::{CleanupTask, Fallback, SecretStatus};

/// The stored integer of every variant, and that it parses back to the same
/// variant; also that the case list covers every variant.
fn assert_codes<T>(cases: &[(T, i16)])
where
    T: ActiveEnum<Value = i16> + Iterable + PartialEq + std::fmt::Debug + Clone,
{
    for (variant, code) in cases {
        assert_eq!(&variant.to_value(), code, "{variant:?}");
        assert_eq!(&T::try_from_value(code).unwrap(), variant);
    }
    assert_eq!(cases.len(), T::iter().count(), "a variant has no case");
}

#[test]
fn numeric_codes_are_pinned() {
    assert_codes(&[
        (DbSharing::Private, 1),
        (DbSharing::Tenant, 2),
        (DbSharing::Shared, 3),
    ]);
    assert_codes(&[(DbStatus::Active, 2), (DbStatus::Declared, 4)]);
    assert_codes(&[(DbFallback::Inherit, 1), (DbFallback::None, 2)]);
    assert_codes(&[(DbCleanupOp::Purge, 1), (DbCleanupOp::Destroy, 2)]);
    assert_codes(&[
        (DbDestroySelector::Below, 1),
        (DbDestroySelector::Exactly, 2),
    ]);
}

#[test]
fn retired_and_unknown_codes_do_not_decode() {
    // 1 (provisioning) and 3 (deprovisioning) are retired by ADR-0006.
    for code in [0, 1, 3, 5, -1] {
        assert!(DbStatus::try_from_value(&code).is_err(), "{code}");
    }
    for code in [0, 3] {
        assert!(DbFallback::try_from_value(&code).is_err(), "{code}");
        assert!(DbCleanupOp::try_from_value(&code).is_err(), "{code}");
        assert!(DbDestroySelector::try_from_value(&code).is_err(), "{code}");
    }
    assert!(DbSharing::try_from_value(&0).is_err());
    assert!(DbSharing::try_from_value(&4).is_err());
}

#[test]
fn sharing_round_trips() {
    for v in DbSharing::iter() {
        assert_eq!(DbSharing::from(SharingMode::from(v)), v);
    }
    for m in [
        SharingMode::Private,
        SharingMode::Tenant,
        SharingMode::Shared,
    ] {
        assert_eq!(SharingMode::from(DbSharing::from(m)), m);
    }
}

#[test]
fn status_round_trips() {
    for v in DbStatus::iter() {
        assert_eq!(DbStatus::from(SecretStatus::from(v)), v);
    }
    for s in [SecretStatus::Active, SecretStatus::Declared] {
        assert_eq!(SecretStatus::from(DbStatus::from(s)), s);
    }
}

#[test]
fn fallback_round_trips() {
    for v in DbFallback::iter() {
        assert_eq!(DbFallback::from(Fallback::from(v)), v);
    }
    for f in [Fallback::Inherit, Fallback::None] {
        assert_eq!(Fallback::from(DbFallback::from(f)), f);
    }
}

#[test]
fn cleanup_op_follows_the_task() {
    let key = StoreKey::new(TenantId(Uuid::nil()), Uuid::nil());
    let v = || ValueVersion("7".to_owned());
    assert_eq!(
        DbCleanupOp::from(&CleanupTask::Purge(key.clone())),
        DbCleanupOp::Purge
    );
    for selector in [DestroySelector::Below(v()), DestroySelector::Exactly(v())] {
        let task = CleanupTask::Destroy {
            key: key.clone(),
            selector,
        };
        assert_eq!(DbCleanupOp::from(&task), DbCleanupOp::Destroy);
    }
}

#[test]
fn destroy_selector_round_trips_with_its_version() {
    let v = || ValueVersion("7".to_owned());
    for selector in [DestroySelector::Below(v()), DestroySelector::Exactly(v())] {
        let kind = DbDestroySelector::from(&selector);
        assert_eq!(kind.with_version(v()), selector);
    }
    assert_eq!(
        DbDestroySelector::from(&DestroySelector::Below(v())),
        DbDestroySelector::Below
    );
    assert_eq!(
        DbDestroySelector::from(&DestroySelector::Exactly(v())),
        DbDestroySelector::Exactly
    );
}
