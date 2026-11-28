//! Reading one complete snapshot (P-D-268): every page of one snapshot, verified whole, or no
//! answer at all; an adapter's failure or silence is unavailable and never leaks its text.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::quota_test_support::{
    FakeDiscovery, SNAPSHOT_ID, observed_at, snapshot_of, vcpu_record_numbered,
};
use crate::test_support::authed_ctx;
use bss_products_sdk::quota::digest::snapshot_digest;
use bss_products_sdk::quota::discovery::QuotaDiscoveryPage;
use serde_json::Value;

fn settings(page_size: u32) -> QuotaSettings {
    QuotaSettings {
        page_size,
        timeout: std::time::Duration::from_secs(5),
        max_items: 10_000,
        deadline: std::time::Duration::from_secs(300),
    }
}

async fn read(fake: &FakeDiscovery) -> Result<VerifiedSnapshot, SnapshotError> {
    read_snapshot(fake, &authed_ctx(Uuid::new_v4()), "t", settings(50)).await
}

fn incomplete(result: Result<VerifiedSnapshot, SnapshotError>) -> String {
    match result {
        Err(SnapshotError::Incomplete(why)) => why,
        other => panic!("expected an incomplete snapshot, got {other:?}"),
    }
}

fn a() -> Value {
    vcpu_record_numbered(1)
}

fn b() -> Value {
    vcpu_record_numbered(2)
}

/// The identity every page of one read repeats.
#[derive(Clone)]
struct Meta {
    snapshot_id: Uuid,
    revision: String,
    observed_at: OffsetDateTime,
    item_count: u64,
    content_digest: String,
}

impl Meta {
    /// The count and digest of the whole snapshot `all`.
    fn of(self, all: &[Value]) -> Self {
        self.counting(all, u64::try_from(all.len()).unwrap())
    }

    /// `all`'s digest with `count` as the snapshot's count.
    fn counting(mut self, all: &[Value], count: u64) -> Self {
        self.item_count = count;
        self.content_digest = snapshot_digest(
            self.snapshot_id,
            &self.revision,
            self.observed_at,
            count,
            all,
        )
        .unwrap();
        self
    }
}

/// One snapshot id, revision `r` and a fixed instant; [`Meta::of`] adds the count and digest.
fn rev(r: &str) -> Meta {
    Meta {
        snapshot_id: SNAPSHOT_ID,
        revision: r.to_owned(),
        observed_at: observed_at(),
        item_count: 0,
        content_digest: String::new(),
    }
}

fn page(meta: &Meta, items: &[Value], next: Option<&str>) -> QuotaDiscoveryPage {
    QuotaDiscoveryPage {
        snapshot_id: meta.snapshot_id,
        source_revision: meta.revision.clone(),
        observed_at: meta.observed_at,
        item_count: meta.item_count,
        content_digest: meta.content_digest.clone(),
        items: items.to_vec(),
        next_cursor: next.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_complete_snapshot_of_three_pages_is_verified_and_sorted() {
    let records: Vec<_> = (0..5).rev().map(vcpu_record_numbered).collect();
    let fake = FakeDiscovery::paged(snapshot_of(&records, "7"), 2);
    let s = read_snapshot(&fake, &authed_ctx(Uuid::new_v4()), "t", settings(2))
        .await
        .unwrap();
    assert_eq!((s.item_count, s.items.len()), (5, 5));
    let ids: Vec<_> = s
        .items
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    assert_eq!(
        fake.requests()
            .iter()
            .map(|r| r.page_size)
            .collect::<Vec<_>>(),
        vec![2, 2, 2]
    );
    assert_eq!(
        fake.requests()
            .iter()
            .map(|r| (r.target_key.as_str(), r.cursor.as_deref()))
            .collect::<Vec<_>>(),
        vec![("t", None), ("t", Some("p1")), ("t", Some("p2"))]
    );
    assert_eq!(
        (
            s.snapshot_id,
            s.source_revision.as_str(),
            s.observed_at,
            s.content_digest.as_str()
        ),
        (
            SNAPSHOT_ID,
            "7",
            observed_at(),
            snapshot_of(&records, "7").content_digest.as_str()
        )
    );
}

#[tokio::test]
async fn pages_of_two_revisions_leave_intake_unresolved() {
    let all = [a(), b()];
    let fake = FakeDiscovery::scripted(vec![
        page(&rev("7").of(&all), &[a()], Some("c1")),
        page(&rev("8").of(&all), &[b()], None),
    ]);
    assert!(incomplete(read(&fake).await).contains("more than one snapshot"));
}

#[tokio::test]
async fn a_repeated_record_id_leaves_intake_unresolved() {
    let meta = rev("7").of(&[a(), a()]);
    let fake = FakeDiscovery::scripted(vec![
        page(&meta, &[a()], Some("c1")),
        page(&meta, &[a()], None),
    ]);
    assert!(incomplete(read(&fake).await).contains("repeats"));
    // The same UUID in another spelling is the same record.
    let mut upper = a();
    upper["id"] = Value::String(a()["id"].as_str().unwrap().to_uppercase());
    let meta = rev("7").of(&[a(), upper.clone()]);
    let fake = FakeDiscovery::scripted(vec![page(&meta, &[a(), upper], None)]);
    assert!(incomplete(read(&fake).await).contains("repeats"));
}

#[tokio::test]
async fn a_count_that_disagrees_leaves_intake_unresolved() {
    let meta = rev("7").counting(&[a(), b()], 3);
    let fake = FakeDiscovery::scripted(vec![page(&meta, &[a(), b()], None)]);
    assert!(incomplete(read(&fake).await).contains("count"));
}

#[tokio::test]
async fn a_digest_that_disagrees_leaves_intake_unresolved() {
    let mut meta = rev("7").of(&[a(), b()]);
    meta.content_digest = rev("7").of(&[a()]).content_digest;
    let fake = FakeDiscovery::scripted(vec![page(&meta, &[a(), b()], None)]);
    assert!(incomplete(read(&fake).await).contains("digest"));
}

#[tokio::test]
async fn a_cursor_loop_leaves_intake_unresolved() {
    let meta = rev("7").of(&[a(), b()]);
    let fake = FakeDiscovery::scripted(vec![
        page(&meta, &[a()], Some("c1")),
        page(&meta, &[b()], Some("c1")),
    ]);
    assert!(incomplete(read(&fake).await).contains("cursor"));
}

#[tokio::test]
async fn more_items_than_the_bound_leave_intake_unresolved() {
    let records: Vec<_> = (0..3).map(vcpu_record_numbered).collect();
    let fake = FakeDiscovery::paged(snapshot_of(&records, "7"), 50);
    let mut bounded = settings(50);
    bounded.max_items = 2;
    let result = read_snapshot(&fake, &authed_ctx(Uuid::new_v4()), "t", bounded).await;
    assert!(incomplete(result).contains("bound"));
    // An adapter whose count understates its records is cut by the records read: the read stops
    // at the first record past the bound, whatever the pages' size.
    let all: Vec<_> = (0..4).map(vcpu_record_numbered).collect();
    let meta = rev("7").counting(&all, 2);
    let mut pages = Vec::new();
    for (n, record) in all.iter().enumerate() {
        let next = (n + 1 < all.len()).then(|| format!("c{n}"));
        pages.push(page(&meta, std::slice::from_ref(record), next.as_deref()));
    }
    let fake = FakeDiscovery::scripted(pages);
    let mut bounded = settings(1);
    bounded.max_items = 2;
    let result = read_snapshot(&fake, &authed_ctx(Uuid::new_v4()), "t", bounded).await;
    assert!(incomplete(result).contains("bound"));
    assert_eq!(
        fake.requests().len(),
        3,
        "two records inside the bound, then one past it"
    );
}

/// An adapter may answer fewer records than a page asks for: only the record bound limits a
/// read, never a count of pages.
#[tokio::test]
async fn short_pages_inside_the_bound_read_complete() {
    let records: Vec<_> = (0..4).map(vcpu_record_numbered).collect();
    let fake = FakeDiscovery::paged(snapshot_of(&records, "7"), 1);
    let mut asked = settings(2);
    asked.max_items = 4;
    let s = read_snapshot(&fake, &authed_ctx(Uuid::new_v4()), "t", asked)
        .await
        .unwrap();
    assert_eq!((s.item_count, s.items.len()), (4, 4));
    assert_eq!(
        fake.requests().len(),
        4,
        "four short pages of one record each"
    );
}

/// A page that is not the last holds at least one record, so an adapter that answers empty pages
/// cannot keep a read going: such a page leaves intake unresolved at once.
#[tokio::test]
async fn an_empty_page_that_is_not_the_last_is_incomplete() {
    let meta = rev("7").of(&[a(), b()]);
    let fake = FakeDiscovery::scripted(vec![
        page(&meta, &[a()], Some("c1")),
        page(&meta, &[], Some("c2")),
        page(&meta, &[b()], None),
    ]);
    assert!(incomplete(read(&fake).await).contains("empty page"));
    assert_eq!(fake.requests().len(), 2, "the read stops at the empty page");
    // The last page may be empty, and so may a snapshot of no record.
    let meta = rev("7").of(&[a()]);
    let fake = FakeDiscovery::scripted(vec![
        page(&meta, &[a()], Some("c1")),
        page(&meta, &[], None),
    ]);
    assert_eq!(read(&fake).await.unwrap().items, vec![a()]);
    let meta = rev("7").of(&[]);
    let fake = FakeDiscovery::scripted(vec![page(&meta, &[], None)]);
    assert!(read(&fake).await.unwrap().items.is_empty());
}

#[tokio::test]
async fn an_adapter_error_is_unavailable_and_carries_no_adapter_text() {
    let fake = FakeDiscovery::failing("upstream says: token=abc");
    match read(&fake).await {
        Err(SnapshotError::Unavailable(m)) => assert!(!m.contains("token"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_page_later_than_the_timeout_is_unavailable() {
    let fake = FakeDiscovery::stalled(); // never answers
    let mut s = settings(50);
    s.timeout = std::time::Duration::from_secs(5);
    assert!(matches!(
        read_snapshot(&fake, &authed_ctx(Uuid::new_v4()), "t", s).await,
        Err(SnapshotError::Unavailable(_))
    ));
}

/// One discovery has an overall deadline (P-D-268): pages that each answer within the page timeout
/// but together outlast it leave intake unresolved; within it the same pages read whole.
#[tokio::test(start_paused = true)]
async fn the_whole_read_is_bounded_by_the_discovery_deadline() {
    let all = [a(), b(), vcpu_record_numbered(3)];
    let two_seconds = std::time::Duration::from_secs(2);
    let fake = FakeDiscovery::slow(snapshot_of(&all, "7"), 1, two_seconds);
    let mut s = settings(1);
    s.deadline = std::time::Duration::from_secs(5);
    let why = incomplete(read_snapshot(&fake, &authed_ctx(Uuid::new_v4()), "t", s).await);
    assert!(why.contains("deadline"), "{why}");
    assert_eq!(
        fake.requests().len(),
        2,
        "two pages answered; the third is cut before it answers"
    );
    let fake = FakeDiscovery::slow(snapshot_of(&all, "7"), 1, two_seconds);
    s.deadline = std::time::Duration::from_secs(7);
    let read = read_snapshot(&fake, &authed_ctx(Uuid::new_v4()), "t", s).await;
    assert_eq!(read.unwrap().items.len(), 3);
}

/// Every page repeats the whole identity: a page with another snapshot id, observation time, count
/// or digest leaves intake unresolved; so do a revision that is not a canonical decimal and a
/// record without a UUID id.
#[tokio::test]
async fn every_page_repeats_the_snapshot_identity() {
    let all = [a(), b()];
    let first = rev("7").of(&all);
    let mut moved = Vec::new();
    let mut other = first.clone();
    other.snapshot_id = Uuid::new_v4();
    moved.push(other);
    let mut other = first.clone();
    other.observed_at = observed_at() + time::Duration::seconds(1);
    moved.push(other);
    moved.push(first.clone().counting(&all, 3));
    let mut other = first.clone();
    other.content_digest = "0".repeat(64);
    moved.push(other);
    for second in moved {
        let fake = FakeDiscovery::scripted(vec![
            page(&first, &[a()], Some("c1")),
            page(&second, &[b()], None),
        ]);
        assert!(incomplete(read(&fake).await).contains("more than one snapshot"));
    }
    for revision in ["07", "-1", "1.0", "", &"9".repeat(40)] {
        let fake = FakeDiscovery::scripted(vec![page(&rev(revision).of(&all), &all, None)]);
        assert!(
            incomplete(read(&fake).await).contains("revision"),
            "{revision:?}"
        );
    }
    let mut nameless = a();
    nameless["id"] = Value::String("not-a-uuid".to_owned());
    let fake = FakeDiscovery::scripted(vec![page(
        &rev("7").of(&[nameless.clone()]),
        &[nameless],
        None,
    )]);
    assert!(incomplete(read(&fake).await).contains("id"));
}
