//! A seek naming several partitions writes all of their cursors or none, even
//! when the request is dropped between two cursor writes, as a client that
//! disconnects mid-request drops its handler. The validation half of the same
//! rule (one inadmissible position writes nothing) is pinned over REST in
//! `api/rest/handlers/delivery/subscriptions_tests.rs`.

use std::time::Duration;

use gts::GtsIdPattern;
use serde_json::json;
use toolkit_gts::{GtsInstanceId, gts_id};
use uuid::Uuid;

use crate::domain::delivery::{JoinRequest, Position, SeekTarget};
use crate::domain::model::{BarrierMode, ConsumerGroupCreateInput, Interest, OwnershipFilter};
use crate::domain::repo::CursorRepo;
use crate::test_support::{EventBrokerHarness, StaticTypesRegistry};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~x.eb.seekatomic.topic.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~x.eb.seekatomic.foo.v1~");
const INTEREST_TENANT: Uuid = Uuid::from_u128(0x1a01);
/// How long a test waits for a held cursor write to reach its pause point, or
/// for a seek to finish.
const BOUND: Duration = Duration::from_secs(10);

/// A harness whose topic has two partitions, both assigned to the one member.
async fn harness() -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 2 },
        ])))
        .build()
        .await
}

/// An anonymous group created by the harness's own caller.
async fn create_group(harness: &EventBrokerHarness) -> GtsInstanceId {
    harness
        .delivery()
        .create_consumer_group(
            harness.security_context(),
            ConsumerGroupCreateInput {
                client_agent: Some("seek-atomicity-test".to_owned()),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id
}

/// A subscription the harness's own caller JOINs to `group`, with one
/// interest on the test topic.
async fn join(harness: &EventBrokerHarness, group: &GtsInstanceId) -> Uuid {
    harness
        .delivery()
        .join(
            harness.security_context(),
            JoinRequest {
                consumer_group: group.clone(),
                client_agent: "seek-atomicity-test".to_owned(),
                interests: vec![Interest {
                    topic: GtsInstanceId::try_new(TOPIC).unwrap(),
                    subject_tenant: INTEREST_TENANT,
                    ownership: OwnershipFilter::EntireTenant,
                    barrier_mode: BarrierMode::Respect,
                    types: vec![GtsIdPattern::try_new(EVENT_TYPE).unwrap()],
                    subject_types: Vec::new(),
                    filter: None,
                }],
                session_timeout: Duration::from_secs(30),
            },
        )
        .await
        .expect("the JOIN is allowed")
        .subscription
        .id
}

#[tokio::test]
async fn a_seek_cancelled_between_two_cursor_writes_leaves_no_cursor() {
    let harness = harness().await;
    let group = create_group(&harness).await;
    let id = join(&harness, &group).await;
    let topic = GtsInstanceId::try_new(TOPIC).unwrap();
    let earliest_on_both = || {
        vec![
            SeekTarget {
                topic: topic.clone(),
                partition: 0,
                value: Position::Earliest,
            },
            SeekTarget {
                topic: topic.clone(),
                partition: 1,
                value: Position::Earliest,
            },
        ]
    };
    let pause = harness.repo().before_cursor_write_pause().clone();

    {
        let seek = harness
            .delivery()
            .seek(harness.security_context(), id, 1, earliest_on_both());
        tokio::pin!(seek);

        // Hold the first cursor write.
        pause.arm();
        tokio::time::timeout(BOUND, async {
            tokio::select! {
                () = pause.reached() => {}
                result = seek.as_mut() => panic!(
                    "the seek finished before its first cursor write was held: {result:?}"
                ),
            }
        })
        .await
        .expect("the first cursor write reaches the pause point");

        // Let it go and hold the next one: the held write disarmed the point
        // on arrival, so arming again targets the second write.
        pause.arm();
        pause.release();
        tokio::time::timeout(BOUND, async {
            tokio::select! {
                () = pause.reached() => {}
                result = seek.as_mut() => panic!(
                    "the seek finished before its second cursor write was held: {result:?}"
                ),
            }
        })
        .await
        .expect("the second cursor write reaches the pause point");
        // The seek is dropped here, as a disconnecting client drops it.
    }

    for partition in [0, 1] {
        let cursor =
            tokio::time::timeout(BOUND, harness.repo().find_cursor(&group, &topic, partition))
                .await
                .expect("the cursor read answers within the bound")
                .expect("reading the cursor must not fail");
        assert!(
            cursor.is_none(),
            "partition {partition}: the cancelled seek wrote {cursor:?}"
        );
    }

    // The same seek run to completion writes both cursors, so the reads above
    // can see one.
    let positions = tokio::time::timeout(
        BOUND,
        harness
            .delivery()
            .seek(harness.security_context(), id, 1, earliest_on_both()),
    )
    .await
    .expect("the seek answers within the bound")
    .expect("the member's own seek is allowed");
    assert_eq!(
        positions.len(),
        2,
        "one position per partition: {positions:?}"
    );
    for partition in [0, 1] {
        let cursor =
            tokio::time::timeout(BOUND, harness.repo().find_cursor(&group, &topic, partition))
                .await
                .expect("the cursor read answers within the bound")
                .expect("reading the cursor must not fail");
        assert!(
            cursor.is_some(),
            "partition {partition}: the completed seek wrote no cursor"
        );
    }
}
