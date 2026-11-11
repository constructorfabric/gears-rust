//! Doubles for the end-of-request publish hook, shared by the suites of the
//! components that call it.

#![allow(
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    reason = "test doubles: a poisoned lock is a test failure"
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use event_broker_sdk::EventV1;
use uuid::Uuid;

use super::{
    BroadcastContext, BroadcastGateway, BroadcastMetrics, BroadcastSink, BroadcastState,
    CoherencePublisher, PublishError, TenantKeys,
};
use crate::infra::broadcast::build_broadcasts;

/// What a gateway's sessions record into, shared with the gateway the test
/// reads it back from.
type Seen = Arc<Mutex<Vec<EventV1>>>;
type Probe = Arc<dyn Fn() -> bool + Send + Sync>;

/// A broker client double: records what it was handed and answers as scripted.
pub struct RecordingGateway {
    pub broker: bool,
    pub state: BroadcastState,
    pub root: Option<Uuid>,
    pub refuse: bool,
    pub seen: Seen,
    /// Evaluated at each publish: what the world looks like when the broker
    /// is handed an event (for instance, whether the local cache is evicted).
    pub probe: Option<Probe>,
    pub probed: Arc<Mutex<Vec<bool>>>,
}

impl RecordingGateway {
    pub fn ready(root: Uuid) -> Self {
        Self {
            broker: true,
            state: BroadcastState {
                types_registered: true,
                root_tenant_known: true,
            },
            root: Some(root),
            refuse: false,
            seen: Seen::default(),
            probe: None,
            probed: Arc::default(),
        }
    }

    pub fn without_broker() -> Self {
        Self {
            broker: false,
            ..Self::ready(Uuid::nil())
        }
    }

    pub fn events(&self) -> Vec<EventV1> {
        self.seen.lock().unwrap().clone()
    }
}

/// The session a [`RecordingGateway`] hands a request.
struct RecordingSession {
    refuse: bool,
    seen: Seen,
    probe: Option<Probe>,
    probed: Arc<Mutex<Vec<bool>>>,
}

#[async_trait]
impl BroadcastSink for RecordingSession {
    async fn publish(&self, event: EventV1) -> Result<(), PublishError> {
        if let Some(probe) = &self.probe {
            self.probed.lock().unwrap().push(probe());
        }
        self.seen.lock().unwrap().push(event);
        if self.refuse {
            Err(PublishError::Refused("no produce grant".to_owned()))
        } else {
            Ok(())
        }
    }
}

impl BroadcastGateway for RecordingGateway {
    fn state(&self) -> BroadcastState {
        self.state
    }
    fn root_tenant(&self) -> Option<Uuid> {
        self.root
    }
    fn session(&self) -> Option<Box<dyn BroadcastSink>> {
        self.broker.then(|| {
            Box::new(RecordingSession {
                refuse: self.refuse,
                seen: Arc::clone(&self.seen),
                probe: self.probe.clone(),
                probed: Arc::clone(&self.probed),
            }) as Box<dyn BroadcastSink>
        })
    }
    fn broadcasts(&self, touched: &[TenantKeys], ctx: &BroadcastContext) -> Vec<EventV1> {
        build_broadcasts(touched, ctx)
    }
}

/// The counter double.
#[derive(Default)]
pub struct CountingMetrics(Mutex<Vec<&'static str>>);

impl CountingMetrics {
    pub fn all(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

impl BroadcastMetrics for CountingMetrics {
    fn broadcast(&self, result: &'static str) {
        self.0.lock().unwrap().push(result);
    }
}

/// A hook over `gateway`, with the counter that records its results.
pub fn publisher(
    gateway: &Arc<RecordingGateway>,
) -> (Arc<CoherencePublisher>, Arc<CountingMetrics>) {
    let metrics = Arc::new(CountingMetrics::default());
    (
        Arc::new(CoherencePublisher::new(
            Arc::clone(gateway) as Arc<dyn BroadcastGateway>,
            Arc::clone(&metrics) as Arc<dyn BroadcastMetrics>,
        )),
        metrics,
    )
}
