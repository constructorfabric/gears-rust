use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, Fallback, HandlerOutcome,
    InMemoryOffsetManager, RawEvent, SingleEventHandler,
};

use super::common::{publish_json, topic_fixture, wait_until};

const TOPIC: &str = "gts.cf.core.events.topic.v1~example.mock.showcase.remote.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event_type.v1~example.mock.showcase.remote.v1";

type RemoteCall = (String, &'static str);
type RecordedRemoteCalls = Arc<Mutex<Vec<RemoteCall>>>;

#[derive(Clone)]
struct FakeRemoteClient {
    token: &'static str,
    calls: RecordedRemoteCalls,
}

struct ForwardingHandler {
    remote: FakeRemoteClient,
}

#[async_trait]
impl SingleEventHandler for ForwardingHandler {
    async fn handle(
        &self,
        event: RawEvent,
        _attempts: u16,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.remote
            .calls
            .lock()
            .unwrap()
            .push((event.subject, self.remote.token));
        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn if_i_need_remote_calls_the_handler_owns_its_client_and_auth() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let remote = FakeRemoteClient {
        token: "service-token",
        calls: calls.clone(),
    };

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-remote-calls"))
        .topics([TOPIC])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(ForwardingHandler { remote })
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        TOPIC,
        EVENT_TYPE,
        "remote-1",
        None,
        serde_json::json!({ "forward": true }),
    )
    .await;

    wait_until(|| calls.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[("remote-1".to_owned(), "service-token")]
    );
}
