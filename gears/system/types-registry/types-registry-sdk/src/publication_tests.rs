//! Publisher version precedence and publication status semantics (SPEC D18, D21).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::{
    PendingReason, PublicationState, PublicationStatus, PublisherVersion, PublisherVersionError,
    RejectionReason, SupersededEntity,
};

fn v(s: &str) -> PublisherVersion {
    s.parse().expect("valid publisher version")
}

fn hash_of(version: &PublisherVersion) -> u64 {
    let mut hasher = DefaultHasher::new();
    version.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn numeric_components_compare_as_numbers_not_strings() {
    assert!(v("0.2.0") < v("0.10.0"));
}

#[test]
fn a_prerelease_precedes_its_release() {
    assert!(v("1.0.0-rc.1") < v("1.0.0"));
    assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
}

#[test]
fn build_metadata_does_not_count_for_equality_ordering_or_hashing() {
    let a = v("1.0.0+a");
    let b = v("1.0.0+b");

    assert_eq!(a, b);
    assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);
    assert_eq!(hash_of(&a), hash_of(&b));
}

#[test]
fn display_keeps_the_declared_text_including_build_metadata() {
    assert_eq!(v("1.2.3-rc.1+build.7").to_string(), "1.2.3-rc.1+build.7");
}

#[test]
fn invalid_semver_is_refused() {
    for input in ["", "1.0", "v1.0.0", "1.0.0 ", "01.0.0", "latest"] {
        assert!(
            matches!(
                input.parse::<PublisherVersion>(),
                Err(PublisherVersionError::Invalid { .. })
            ),
            "{input:?} must be refused as invalid"
        );
    }
}

#[test]
fn input_at_the_length_bound_is_accepted_and_above_it_refused() {
    let at_bound = format!(
        "1.0.0-{}",
        "a".repeat(PublisherVersion::MAX_LEN - "1.0.0-".len())
    );
    assert_eq!(at_bound.len(), PublisherVersion::MAX_LEN);
    assert!(at_bound.parse::<PublisherVersion>().is_ok());

    let above = format!("{at_bound}a");
    assert_eq!(
        above.parse::<PublisherVersion>(),
        Err(PublisherVersionError::TooLong {
            len: PublisherVersion::MAX_LEN + 1,
            max: PublisherVersion::MAX_LEN,
        })
    );
}

#[test]
fn try_from_enforces_the_same_rules_as_parse() {
    assert_eq!(PublisherVersion::try_from("1.0.0"), Ok(v("1.0.0")));
    assert!(
        PublisherVersion::try_from("x".repeat(PublisherVersion::MAX_LEN + 1).as_str()).is_err()
    );
}

proptest::proptest! {
    /// Eq iff Ord is Equal; equal precedence hashes equally, regardless of build metadata.
    #[test]
    fn eq_ord_and_hash_agree(
        a in version_text(),
        b in version_text(),
    ) {
        use std::hash::BuildHasher as _;

        let (a, b) = (v(&a), v(&b));
        let hasher = std::collections::hash_map::RandomState::new();
        proptest::prop_assert_eq!(a == b, a.cmp(&b) == std::cmp::Ordering::Equal);
        if a == b {
            proptest::prop_assert_eq!(hasher.hash_one(&a), hasher.hash_one(&b));
        }
    }
}

/// Small `SemVer` space with prereleases/build metadata makes equal-precedence pairs common.
fn version_text() -> impl proptest::strategy::Strategy<Value = String> {
    use proptest::prelude::*;
    (
        0_u64..3,
        0_u64..3,
        0_u64..3,
        proptest::option::of(prop_oneof!["alpha", "rc\\.1", "rc\\.2", "1"]),
        proptest::option::of(prop_oneof!["a", "b", "sha\\.1"]),
    )
        .prop_map(|(major, minor, patch, pre, build)| {
            let mut text = format!("{major}.{minor}.{patch}");
            if let Some(pre) = pre {
                text.push('-');
                text.push_str(&pre);
            }
            if let Some(build) = build {
                text.push('+');
                text.push_str(&build);
            }
            text
        })
}

fn status(entries: &[(&str, PublicationState)]) -> PublicationStatus {
    PublicationStatus {
        entities: entries
            .iter()
            .map(|(id, state)| ((*id).to_owned(), state.clone()))
            .collect(),
    }
}

fn superseded(entity: SupersededEntity) -> PublicationState {
    PublicationState::Superseded {
        stored_version: v("0.3.0"),
        offered_version: v("0.2.0"),
        entity,
    }
}

fn rejected() -> PublicationState {
    PublicationState::Rejected(RejectionReason::Registry {
        reason: "invalid_schema".to_owned(),
        message: "not a JSON Schema".to_owned(),
    })
}

#[test]
fn a_new_status_is_pending_for_every_declared_identifier() {
    let s = PublicationStatus::pending(["gts.a.b.c.d.v1~", "gts.a.b.c.e.v1~"]);

    assert_eq!(s.entities.len(), 2);
    assert!(
        s.entities
            .values()
            .all(|st| *st == PublicationState::Pending(PendingReason::Waiting))
    );
    assert!(!s.is_terminal());
    assert!(!s.satisfies_readiness());
}

#[test]
fn admitted_and_superseded_live_satisfy_readiness() {
    let s = status(&[
        ("gts.a.b.c.d.v1~", PublicationState::Admitted),
        ("gts.a.b.c.e.v1~", superseded(SupersededEntity::Live)),
    ]);
    assert!(s.is_terminal());
    assert!(s.satisfies_readiness());
}

#[test]
fn each_unsatisfied_identifier_holds_readiness_even_beside_satisfied_ones() {
    let holding = [
        PublicationState::Pending(PendingReason::Waiting),
        rejected(),
        superseded(SupersededEntity::Deleted),
    ];
    for state in holding {
        let s = status(&[
            ("gts.a.b.c.d.v1~", PublicationState::Admitted),
            ("gts.a.b.c.e.v1~", superseded(SupersededEntity::Live)),
            ("gts.a.b.c.f.v1~", state.clone()),
        ]);
        assert!(!s.satisfies_readiness(), "{state:?} must hold readiness");
    }
}

#[test]
fn terminal_means_no_identifier_is_pending() {
    let s = status(&[
        ("gts.a.b.c.d.v1~", rejected()),
        ("gts.a.b.c.e.v1~", superseded(SupersededEntity::Deleted)),
    ]);
    assert!(s.is_terminal());

    let s = status(&[
        ("gts.a.b.c.d.v1~", rejected()),
        (
            "gts.a.b.c.e.v1~",
            PublicationState::Pending(PendingReason::Waiting),
        ),
    ]);
    assert!(!s.is_terminal());
}

#[test]
fn an_empty_declaration_set_is_terminal_and_satisfied() {
    let s = PublicationStatus::pending(std::iter::empty::<&str>());
    assert!(s.is_terminal());
    assert!(s.satisfies_readiness());
}

#[test]
fn stopping_rejects_what_is_pending_and_keeps_every_settled_outcome() {
    let s = status(&[
        ("gts.a.b.c.d.v1~", PublicationState::Admitted),
        ("gts.a.b.c.e.v1~", superseded(SupersededEntity::Live)),
        ("gts.a.b.c.f.v1~", rejected()),
        (
            "gts.a.b.c.g.v1~",
            PublicationState::Pending(PendingReason::Waiting),
        ),
    ]);

    let stopped = s.clone().stopped("publisher task panicked: boom");

    assert!(stopped.is_terminal());
    assert!(!stopped.satisfies_readiness());
    for id in ["gts.a.b.c.d.v1~", "gts.a.b.c.e.v1~", "gts.a.b.c.f.v1~"] {
        assert_eq!(stopped.entities[id], s.entities[id], "{id} must be kept");
    }
    assert_eq!(
        stopped.entities["gts.a.b.c.g.v1~"],
        PublicationState::Rejected(RejectionReason::PublisherStopped {
            message: "publisher task panicked: boom".to_owned(),
        })
    );
}
