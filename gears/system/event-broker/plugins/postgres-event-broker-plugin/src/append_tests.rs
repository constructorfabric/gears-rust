use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use event_broker_sdk::backend::{Head, Partition, TopicPartition};
use event_broker_sdk::{GtsInstanceId, Sequence};
use tokio::sync::{Notify, oneshot};

use crate::append::finish;

#[tokio::test(start_paused = true)]
async fn a_commit_that_lands_publishes_its_heads_even_when_the_caller_goes_away() {
    let topic = GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1")
        .expect("a valid topic id");
    let head = Head {
        at: TopicPartition::new(topic, Partition::new(2)),
        sequence: Sequence::assigned(9),
    };

    let (landed_tx, landed_rx) = oneshot::channel();
    let gate = Arc::new(Notify::new());
    let commit = {
        let gate = Arc::clone(&gate);
        async move {
            landed_tx.send(()).expect("the test waits for the commit");
            gate.notified().await;
            Ok(())
        }
    };
    let published = Arc::new(Mutex::new(Vec::<Head>::new()));
    let (done_tx, done_rx) = oneshot::channel();
    let publish = {
        let published = Arc::clone(&published);
        let head = head.clone();
        move || {
            published
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(head);
            done_tx.send(()).expect("the test waits for the publish");
        }
    };

    // The caller is driven until the commit is issued, then goes away.
    let mut caller = Box::pin(async move {
        drop(finish(commit, publish).await);
    });
    tokio::select! {
        () = &mut caller => panic!("the commit waits on the gate"),
        landed = landed_rx => landed.expect("the commit signals"),
    }
    drop(caller);
    gate.notify_one();

    // Paused time: the timeout fires only once nothing else can run.
    let in_time = matches!(
        tokio::time::timeout(Duration::from_secs(1), done_rx).await,
        Ok(Ok(()))
    );
    assert_eq!(
        (
            in_time,
            published
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        ),
        (true, vec![head]),
        "a commit that lands publishes its heads"
    );
}
