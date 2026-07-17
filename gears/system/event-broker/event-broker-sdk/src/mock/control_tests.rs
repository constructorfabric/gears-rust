use crate::api::EventBroker;
use crate::mock::MockBroker;
use toolkit_gts::gts_id;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.control.zero.v1");

#[tokio::test]
async fn register_topic_rejects_zero_partitions_without_registering_topic() {
    let broker = MockBroker::new();
    let handle = broker.handle();

    let err = tokio::spawn(async move {
        handle.register_topic(TOPIC, 0).await;
    })
    .await
    .expect_err("zero partitions must panic");

    assert!(err.is_panic(), "zero partitions must fail immediately");

    let topics = broker
        .list_topics(&toolkit_security::SecurityContext::anonymous())
        .await
        .expect("listing topics must succeed");

    assert!(
        topics.iter().all(|topic| topic.id != TOPIC),
        "failed zero-partition registration must not leave topic visible"
    );
}
