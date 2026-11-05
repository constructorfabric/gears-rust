use std::time::Duration;

use super::{GearInstallers, GrpcInstallerData, GrpcInstallerStore};

fn gear(name: &str) -> GearInstallers {
    GearInstallers {
        gear_name: name.to_owned(),
        installers: Vec::new(),
    }
}

#[test]
fn the_initial_batch_is_set_once_and_taken_once() {
    let store = GrpcInstallerStore::new();
    store
        .set(GrpcInstallerData {
            gears: vec![gear("directory")],
            more_batches: true,
        })
        .unwrap();
    assert!(store.set(GrpcInstallerData::default()).is_err());

    let initial = store.take().expect("set");
    assert_eq!(initial.gears[0].gear_name, "directory");
    assert!(initial.more_batches);
    assert!(store.take().is_none(), "taken once");
}

/// A late batch reaches the hub after it started waiting, and its publisher
/// learns the outcome from the ack.
#[tokio::test]
async fn a_late_batch_is_delivered_and_acknowledged() {
    let store = std::sync::Arc::new(GrpcInstallerStore::new());
    let hub = {
        let store = std::sync::Arc::clone(&store);
        tokio::spawn(async move {
            let batch = store.next_late().await.expect("a batch");
            let names: Vec<_> = batch.gears.iter().map(|g| g.gear_name.clone()).collect();
            batch.ack(Ok(()));
            names
        })
    };
    tokio::task::yield_now().await;

    let installed = store.publish_late(vec![gear("cluster")]).unwrap();

    tokio::time::timeout(Duration::from_secs(1), installed)
        .await
        .expect("acknowledged")
        .expect("not dropped")
        .expect("installed");
    assert_eq!(hub.await.unwrap(), ["cluster"]);
}

/// A batch the hub drops without installing it is an error, not a hang.
#[tokio::test]
async fn a_dropped_batch_errs_its_publisher() {
    let store = GrpcInstallerStore::new();
    let installed = store.publish_late(vec![gear("cluster")]).unwrap();

    drop(store.next_late().await);

    assert!(installed.await.is_err());
}

#[tokio::test]
async fn closing_ends_the_wait_after_the_queued_batches() {
    let store = GrpcInstallerStore::new();
    let _installed = store.publish_late(vec![gear("a")]).unwrap();
    store.close();

    assert!(store.next_late().await.is_some(), "queued before close");
    assert!(store.next_late().await.is_none());
    assert!(store.publish_late(Vec::new()).is_err(), "closed");
}
