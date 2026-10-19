use super::*;

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
    for name in names {
        assert!(is_valid_leaf(&name.leaf()), "{name}");
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
fn the_leaf_rule_refuses_the_source_id_prefix() {
    // `source_id` is `usage:<uuid>`; `:` is not a leaf character, so the UUID alone is used.
    assert!(!is_valid_leaf("usage:00000000-0000-0000-0000-000000000000"));
    assert!(!is_valid_leaf(""));
    assert!(!is_valid_leaf(&"a".repeat(MAX_LEAF_LEN + 1)));
    assert!(!is_valid_leaf("a/b"));
}
