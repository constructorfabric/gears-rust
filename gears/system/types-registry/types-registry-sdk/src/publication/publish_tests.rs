use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use crate::publication::{
    PendingReason, PublicationState, PublicationStatus, PublisherContext, RejectionReason,
    SupersededEntity,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;

use super::{PublishOptions, publish_gts, publish_gts_with};
use crate::contract::PlatformTypesRegistryApi;
use crate::publication::supervised::TaskExit;
use crate::testing_platform::{FakePlatformRegistry, ReadFault};

const A: &str = "gts.cf.test.pkg.a.v1~";
const B: &str = "gts.cf.test.pkg.b.v1~";
const BASE: &str = "gts.cf.test.pkg.base.v1~";

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "publish-test".to_owned(),
        version: "3.0.0".parse().expect("version"),
    }
}

fn hub_with(fake: &Arc<FakePlatformRegistry>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn PlatformTypesRegistryApi>(
        Arc::clone(fake) as Arc<dyn PlatformTypesRegistryApi>
    );
    hub
}

fn decl(id: &str, content: Value) -> (String, Value) {
    (id.to_owned(), content)
}

async fn settled(
    rx: &mut toolkit::tokio::sync::watch::Receiver<PublicationStatus>,
) -> PublicationStatus {
    rx.wait_for(PublicationStatus::is_terminal)
        .await
        .expect("the status settles")
        .clone()
}

#[tokio::test(start_paused = true)]
async fn publication_converges_when_a_base_appears_on_a_later_cycle_with_new_keys() {
    let fake = Arc::new(FakePlatformRegistry::new().completing_after(0));
    let handle = publish_gts(
        hub_with(&fake),
        publisher(),
        vec![decl(A, json!({ "x-fake-depends-on": BASE }))],
        CancellationToken::new(),
    );
    let mut rx = handle.subscribe();
    rx.wait_for(|s| {
        matches!(
            s.entities[A],
            PublicationState::Pending(PendingReason::BlockedDependency { .. })
        )
    })
    .await
    .expect("reports the blocked dependency");
    assert!(!rx.borrow().satisfies_readiness());

    fake.seed(BASE, json!({}));

    let status = settled(&mut rx).await;
    assert_eq!(status.entities[A], PublicationState::Admitted);
    assert!(status.satisfies_readiness());
    assert_eq!(handle.join().await, TaskExit::Returned);
    let keys: Vec<String> = fake
        .submissions()
        .iter()
        .map(|(k, _)| k.as_str().to_owned())
        .collect();
    assert!(keys.len() >= 2);
    assert_eq!(
        keys.iter().collect::<BTreeSet<_>>().len(),
        keys.len(),
        "every cycle and pass uses a new key"
    );
    assert!(
        fake.submissions()
            .iter()
            .all(|(_, r)| r.publisher == publisher()),
        "the gear's context is forwarded, never rebuilt"
    );
}

#[tokio::test(start_paused = true)]
async fn two_publishers_of_identical_content_both_reach_admitted() {
    let fake = Arc::new(FakePlatformRegistry::new());
    let declarations = vec![decl(A, json!({ "same": true }))];
    let first = publish_gts(
        hub_with(&fake),
        publisher(),
        declarations.clone(),
        CancellationToken::new(),
    );
    let second = publish_gts(
        hub_with(&fake),
        publisher(),
        declarations,
        CancellationToken::new(),
    );

    for handle in [first, second] {
        let mut rx = handle.subscribe();
        let status = settled(&mut rx).await;
        assert_eq!(status.entities[A], PublicationState::Admitted);
    }
}

#[tokio::test(start_paused = true)]
async fn a_permanently_invalid_declaration_is_rejected_and_not_retried() {
    let fake = Arc::new(FakePlatformRegistry::new());
    let handle = publish_gts(
        hub_with(&fake),
        publisher(),
        vec![
            decl(A, json!({ "x-fake-invalid": true })),
            decl(B, json!({})),
        ],
        CancellationToken::new(),
    );

    assert_eq!(
        handle.join().await,
        TaskExit::Returned,
        "nothing is left to retry"
    );
    assert_eq!(fake.submissions().len(), 1, "submitted once, never retried");
}

#[tokio::test(start_paused = true)]
async fn a_rejected_declaration_reports_its_reason_and_holds_readiness() {
    let fake = Arc::new(FakePlatformRegistry::new());
    let handle = publish_gts(
        hub_with(&fake),
        publisher(),
        vec![decl(A, json!({ "x-fake-invalid": true }))],
        CancellationToken::new(),
    );
    let mut rx = handle.subscribe();

    let status = settled(&mut rx).await;

    assert!(matches!(
        &status.entities[A],
        PublicationState::Rejected(RejectionReason::Registry { reason, .. }) if reason == "invalid_schema"
    ));
    assert!(!status.satisfies_readiness());
}

#[tokio::test(start_paused = true)]
async fn a_panicking_publication_reports_a_terminal_failure_not_pending() {
    let fake = Arc::new(FakePlatformRegistry::new());
    fake.panic_on_submit();
    let handle = publish_gts(
        hub_with(&fake),
        publisher(),
        vec![decl(A, json!({}))],
        CancellationToken::new(),
    );
    let rx = handle.subscribe();

    assert!(matches!(handle.join().await, TaskExit::Panicked(_)));
    let status = rx.borrow().clone();
    assert!(status.is_terminal());
    assert!(matches!(
        status.entities[A],
        PublicationState::Rejected(RejectionReason::PublisherStopped { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn superseded_live_releases_readiness_and_superseded_deleted_holds_it() {
    let fake = Arc::new(FakePlatformRegistry::new());
    fake.seed(A, json!({ "newer": true }));
    fake.seed(B, json!({ "newer": true }));
    let handle = publish_gts(
        hub_with(&fake),
        publisher(),
        vec![
            decl(A, json!({ "x-fake-superseded": true })),
            decl(
                B,
                json!({ "x-fake-superseded": true, "x-fake-superseded-deletes": true }),
            ),
        ],
        CancellationToken::new(),
    );
    let mut rx = handle.subscribe();

    let status = settled(&mut rx).await;

    assert!(matches!(
        &status.entities[A],
        PublicationState::Superseded { entity: SupersededEntity::Live, stored_version, offered_version }
            if stored_version.to_string() == "9.0.0" && offered_version.to_string() == "1.0.0"
    ));
    assert!(matches!(
        status.entities[B],
        PublicationState::Superseded {
            entity: SupersededEntity::Deleted,
            ..
        }
    ));
    assert!(
        !status.satisfies_readiness(),
        "the deleted one holds readiness"
    );
}

#[tokio::test(start_paused = true)]
async fn unverified_superseded_liveness_is_re_read_until_verified_never_resubmitted() {
    let fake = Arc::new(FakePlatformRegistry::new());
    fake.seed(A, json!({ "newer": true }));
    // Read 1 is the pass read, read 2 the liveness read after the outcome.
    fake.fault_reads(ReadFault::FailOnly(2));
    let handle = publish_gts(
        hub_with(&fake),
        publisher(),
        vec![decl(A, json!({ "x-fake-superseded": true }))],
        CancellationToken::new(),
    );
    let mut rx = handle.subscribe();

    rx.wait_for(|s| {
        matches!(
            s.entities[A],
            PublicationState::Pending(PendingReason::RegistryUnreachable { .. })
        )
    })
    .await
    .expect("unverified liveness holds readiness as pending");

    let status = settled(&mut rx).await;
    assert!(matches!(
        status.entities[A],
        PublicationState::Superseded {
            entity: SupersededEntity::Live,
            ..
        }
    ));
    assert!(status.satisfies_readiness());
    assert_eq!(fake.submissions().len(), 1, "exactly one mutation");
}

#[tokio::test(start_paused = true)]
async fn a_hub_without_the_client_reports_unreachable_and_stops_on_cancellation() {
    let cancel = CancellationToken::new();
    let handle = publish_gts_with(
        Arc::new(ClientHub::new()),
        publisher(),
        vec![decl(A, json!({}))],
        cancel.clone(),
        PublishOptions {
            cycle_backoff: Duration::ZERO,
            cycle_backoff_max: Duration::ZERO,
            ..PublishOptions::default()
        },
    );
    let mut rx = handle.subscribe();
    rx.wait_for(|s| {
        matches!(
            s.entities[A],
            PublicationState::Pending(PendingReason::RegistryUnreachable { .. })
        )
    })
    .await
    .expect("reports the missing client");
    // Zero tuning must still yield for at least MIN_CYCLE_BACKOFF.
    tokio::time::sleep(Duration::from_secs(1)).await;

    cancel.cancel();

    assert_eq!(handle.join().await, TaskExit::Cancelled);
    assert!(rx.borrow().is_terminal());
}

#[tokio::test(start_paused = true)]
async fn a_non_canonical_declaration_is_rejected_under_its_own_spelling() {
    let fake = Arc::new(FakePlatformRegistry::new());
    let spaced = format!(" {A} ");
    let handle = publish_gts(
        hub_with(&fake),
        publisher(),
        vec![(spaced.clone(), json!({}))],
        CancellationToken::new(),
    );
    let mut rx = handle.subscribe();

    let status = settled(&mut rx).await;

    assert_eq!(status.entities.len(), 1);
    assert!(
        matches!(
            &status.entities[&spaced],
            PublicationState::Rejected(RejectionReason::InvalidDeclaration { reason, .. })
                if reason == crate::field::INVALID_GTS_ID
        ),
        "a locally refused declaration carries its field violation's reason: {:?}",
        status.entities[&spaced]
    );
    assert!(fake.submissions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_hung_liveness_read_is_bounded_and_a_later_cycle_verifies_without_a_second_mutation() {
    let fake = Arc::new(FakePlatformRegistry::new());
    fake.seed(A, json!({ "newer": true }));
    // Read 2 (liveness after the outcome) fails; read 3 (the first re-read) hangs.
    fake.fault_reads(ReadFault::FailOnly(2));
    fake.hang_read(3);
    let options = PublishOptions {
        reconcile: crate::ReconcileOptions {
            deadline: Duration::from_secs(5),
            ..crate::ReconcileOptions::default()
        },
        ..PublishOptions::default()
    };
    let handle = publish_gts_with(
        hub_with(&fake),
        publisher(),
        vec![decl(A, json!({ "x-fake-superseded": true }))],
        CancellationToken::new(),
        options,
    );
    let mut rx = handle.subscribe();

    let status = settled(&mut rx).await;

    assert!(matches!(
        status.entities[A],
        PublicationState::Superseded {
            entity: SupersededEntity::Live,
            ..
        }
    ));
    assert!(
        fake.batch_reads() >= 4,
        "the hung read was abandoned and retried"
    );
    assert_eq!(fake.submissions().len(), 1, "no second mutation");
}

#[tokio::test(start_paused = true)]
async fn a_client_bound_later_is_picked_up_even_under_an_oversized_initial_backoff() {
    let fake = Arc::new(FakePlatformRegistry::new());
    let hub = Arc::new(ClientHub::new());
    let handle = publish_gts_with(
        Arc::clone(&hub),
        publisher(),
        vec![decl(A, json!({}))],
        CancellationToken::new(),
        PublishOptions {
            cycle_backoff: Duration::MAX,
            cycle_backoff_max: Duration::from_secs(60),
            ..PublishOptions::default()
        },
    );
    let mut rx = handle.subscribe();
    rx.wait_for(|s| {
        matches!(
            s.entities[A],
            PublicationState::Pending(PendingReason::RegistryUnreachable { .. })
        )
    })
    .await
    .expect("reports the missing client");

    hub.register::<dyn PlatformTypesRegistryApi>(
        Arc::clone(&fake) as Arc<dyn PlatformTypesRegistryApi>
    );

    let status = settled(&mut rx).await;
    assert_eq!(status.entities[A], PublicationState::Admitted);
}

#[tokio::test(start_paused = true)]
async fn a_superseded_outcome_without_usable_versions_is_rejected_naming_why() {
    for (versions, why) in [("missing", "was not reported"), ("invalid", "is unusable")] {
        let fake = Arc::new(FakePlatformRegistry::new());
        let handle = publish_gts(
            hub_with(&fake),
            publisher(),
            vec![decl(
                A,
                json!({ "x-fake-superseded": true, "x-fake-superseded-versions": versions }),
            )],
            CancellationToken::new(),
        );
        let mut rx = handle.subscribe();

        let status = settled(&mut rx).await;

        assert!(
            matches!(
                &status.entities[A],
                PublicationState::Rejected(RejectionReason::Registry { reason, message })
                    if reason == crate::item_failure::reason::SUPERSEDED && message.contains(why)
            ),
            "{versions}: {:?}",
            status.entities[A]
        );
        assert_eq!(handle.join().await, TaskExit::Returned);
        assert_eq!(fake.submissions().len(), 1, "{versions}: never resubmitted");
    }
}

#[tokio::test(start_paused = true)]
async fn a_registry_refusing_the_call_is_reported_as_refused_not_unreachable() {
    let fake = Arc::new(FakePlatformRegistry::new());
    fake.fail_submits(
        usize::MAX >> 48,
        &toolkit_canonical_errors::CanonicalError::unauthenticated()
            .with_reason("TOKEN_INVALID")
            .create(),
    );
    let cancel = CancellationToken::new();
    let handle = publish_gts(
        hub_with(&fake),
        publisher(),
        vec![decl(A, json!({}))],
        cancel.clone(),
    );
    let mut rx = handle.subscribe();

    rx.wait_for(|s| {
        matches!(
            s.entities[A],
            PublicationState::Pending(PendingReason::RegistryRefused { .. })
        )
    })
    .await
    .expect("reports the refusal");

    cancel.cancel();
    assert_eq!(handle.join().await, TaskExit::Cancelled);
}

/// Count one-pass, one-read cycles while a dependency stays absent.
async fn cycles_within(window: Duration, backoff: Duration, max: Duration) -> u32 {
    let fake = Arc::new(FakePlatformRegistry::new());
    let mut options = PublishOptions::default();
    options.reconcile.passes = std::num::NonZeroU32::MIN;
    options.cycle_backoff = backoff;
    options.cycle_backoff_max = max;
    let cancel = CancellationToken::new();
    let handle = publish_gts_with(
        hub_with(&fake),
        publisher(),
        vec![decl(A, json!({ "x-fake-depends-on": BASE }))],
        cancel.clone(),
        options,
    );
    tokio::time::sleep(window).await;
    let cycles = fake.batch_reads();
    cancel.cancel();
    assert_eq!(handle.join().await, TaskExit::Cancelled);
    cycles
}

#[tokio::test(start_paused = true)]
async fn the_cycle_pause_doubles_across_cycles_within_its_jitter_envelope() {
    // Doubling 1–16 s pauses with half-to-full jitter bounds cycles in 31 s; fixed pacing would
    // exceed it.
    let cycles = cycles_within(
        Duration::from_secs(31),
        Duration::from_secs(1),
        Duration::from_secs(60),
    )
    .await;
    assert!((5..=7).contains(&cycles), "{cycles}");
}

#[tokio::test(start_paused = true)]
async fn the_cycle_pause_is_capped() {
    // After the first pause, the 2 s cap gives [1, 2] s jitter; uncapped doubling would yield fewer
    // cycles.
    let cycles = cycles_within(
        Duration::from_secs(20),
        Duration::from_secs(1),
        Duration::from_secs(2),
    )
    .await;
    assert!((10..=22).contains(&cycles), "{cycles}");
}

#[tokio::test(start_paused = true)]
async fn a_zero_tuning_still_pauses_at_least_the_minimum_between_cycles() {
    // At least 50 ms between cycles, jitter included: at most 21 in one second.
    let cycles = cycles_within(Duration::from_secs(1), Duration::ZERO, Duration::ZERO).await;
    assert!((2..=21).contains(&cycles), "{cycles}");
}
