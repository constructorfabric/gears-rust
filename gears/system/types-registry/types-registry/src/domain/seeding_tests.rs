use serde_json::json;

use super::*;

fn fields() -> Vec<String> {
    vec!["$id".to_owned(), "gtsId".to_owned(), "id".to_owned()]
}

#[test]
fn collect_identifies_schemas_by_uri_and_instances_by_id() {
    let set = SeedSet::collect(
        [
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "type": "object"}),
            json!({"id": "gts.cf.core.seed.base.v1~cf.core.seed.one.v1"}),
        ],
        &fields(),
    )
    .expect("a valid seed set");

    let ids: Vec<&str> = set.documents.keys().map(String::as_str).collect();
    assert_eq!(
        ids,
        [
            "gts.cf.core.seed.base.v1~",
            "gts.cf.core.seed.base.v1~cf.core.seed.one.v1"
        ]
    );
}

#[test]
fn collect_collapses_identical_duplicates() {
    let doc = json!({"$id": "gts://gts.cf.core.seed.base.v1~"});
    let set = SeedSet::collect([doc.clone(), doc], &fields()).expect("identical duplicates");
    assert_eq!(set.len(), 1);
}

#[test]
fn collect_refuses_differing_duplicates_by_identifier() {
    let error = SeedSet::collect(
        [
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "description": "a"}),
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "description": "b"}),
        ],
        &fields(),
    )
    .expect_err("two documents for one identifier");
    assert!(
        matches!(&error, SeedError::ConflictingDeclarations { gts_id } if gts_id == "gts.cf.core.seed.base.v1~"),
        "{error}"
    );
}

#[test]
fn collect_refuses_a_document_without_an_identifier() {
    let error =
        SeedSet::collect([json!({"type": "object"})], &fields()).expect_err("no identifier field");
    assert!(
        matches!(error, SeedError::MissingIdentifier { index: 0, .. }),
        "{error}"
    );
}

#[test]
fn collect_refuses_duplicates_differing_only_in_a_signed_zero() {
    // `Value` equality calls them equal; the canonical text acceptance stores does not.
    let error = SeedSet::collect(
        [
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "default": 0.0}),
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "default": -0.0}),
        ],
        &fields(),
    )
    .expect_err("byte-distinct declarations of one identifier");
    assert!(
        matches!(error, SeedError::ConflictingDeclarations { .. }),
        "{error}"
    );
}

#[test]
fn a_non_canonical_identifier_refuses_the_set_before_any_request() {
    let set = SeedSet::collect([json!({"id": "not a gts id"})], &fields()).expect("collected");

    let error = set.desired().expect_err("not an identifier");

    assert!(
        matches!(&error, SeedError::InvalidIdentifier { gts_id, .. } if gts_id == "not a gts id"),
        "{error}"
    );
}

#[test]
fn an_expired_or_cancelled_budget_refuses_success() {
    let set = SeedSet::collect(
        [json!({"$id": "gts://gts.cf.core.seed.base.v1~"})],
        &fields(),
    )
    .expect("a valid seed set");
    let cancel = CancellationToken::new();
    let budget = Duration::from_secs(60);

    let expired = confirm(Instant::now(), budget, &set, &cancel);
    assert!(
        matches!(&expired, Err(SeedError::Deadline { undecided, .. })
            if undecided.len() == 1 && undecided[0].gts_id == "gts.cf.core.seed.base.v1~"),
        "{expired:?}"
    );

    let live = Instant::now() + budget;
    assert!(confirm(live, budget, &set, &cancel).is_ok());
    cancel.cancel();
    assert!(matches!(
        confirm(live, budget, &set, &cancel),
        Err(SeedError::Cancelled)
    ));
}

#[tokio::test(start_paused = true)]
async fn the_deadline_and_the_token_win_over_a_ready_call_without_polling_it() {
    let polled = std::sync::atomic::AtomicBool::new(false);
    let call = || async {
        polled.store(true, std::sync::atomic::Ordering::SeqCst);
        "answered"
    };
    let cancel = CancellationToken::new();

    let expired = guarded(Instant::now(), &cancel, call()).await;
    assert_eq!(expired, Err(Stop::Deadline));
    assert!(
        !polled.load(std::sync::atomic::Ordering::SeqCst),
        "never started"
    );

    let live = Instant::now() + Duration::from_secs(60);
    assert_eq!(guarded(live, &cancel, call()).await, Ok("answered"));

    polled.store(false, std::sync::atomic::Ordering::SeqCst);
    cancel.cancel();
    let cancelled = guarded(Instant::now(), &cancel, call()).await;
    assert_eq!(
        cancelled,
        Err(Stop::Cancelled),
        "cancellation outranks the deadline"
    );
    assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn a_call_still_running_at_the_deadline_is_cut_off_there() {
    let cancel = CancellationToken::new();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(5);

    let result = guarded(
        deadline,
        &cancel,
        tokio::time::sleep(Duration::from_secs(60)),
    )
    .await;

    assert_eq!(result, Err(Stop::Deadline));
    assert_eq!(Instant::now(), deadline, "not a moment past it");
}
