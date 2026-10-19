use uuid::Uuid;

use super::LeadershipName;

#[test]
fn every_leadership_name_is_a_valid_scoped_cluster_name() {
    let names = [
        LeadershipName::UsageFeed(Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef)),
        LeadershipName::Scheduler(0),
        LeadershipName::Scheduler(u16::MAX),
        LeadershipName::Recovery(3),
        LeadershipName::Reconcile(4),
        LeadershipName::Verify(5),
        LeadershipName::ExceptionSweep,
        LeadershipName::Purge,
    ];
    // The Cluster SDK's own rule, not a copy of it, decides.
    for name in names {
        assert!(
            cluster_sdk::validate_cluster_name(&name.leaf()).is_ok(),
            "{name}"
        );
        assert!(
            cluster_sdk::validate_scoped_cluster_name(&name.scoped()).is_ok(),
            "{name}"
        );
    }
}

#[test]
fn names_follow_adr_0008() {
    assert_eq!(
        LeadershipName::UsageFeed(Uuid::nil()).scoped(),
        "bss-rating/usage-feed-00000000-0000-0000-0000-000000000000"
    );
    assert_eq!(LeadershipName::Scheduler(7).leaf(), "scheduler-7");
    assert_eq!(
        LeadershipName::ExceptionSweep.scoped(),
        "bss-rating/exception-sweep"
    );
    assert_eq!(LeadershipName::Purge.leaf(), "purge");
}

#[test]
fn the_cluster_sdk_refuses_the_source_id_prefix_rating_leaves_out() {
    // `source_id` is `usage:<uuid>`; the leaf carries the UUID alone because `:` is refused.
    assert!(
        cluster_sdk::validate_cluster_name("usage:00000000-0000-0000-0000-000000000000").is_err()
    );
}
