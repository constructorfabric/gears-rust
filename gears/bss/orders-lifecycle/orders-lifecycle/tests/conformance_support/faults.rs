//! Deterministic test seams; no production hook registration or wall-clock sleeps.
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    BeforeRemoteAcceptance,
    AfterRemoteAcceptance,
    BeforeLocalCommit,
    AfterLocalCommit,
    BeforeOutboxEnqueue,
    AfterOutboxEnqueue,
    BeforeOutboxAck,
    AfterOutboxAck,
    BeforeReceiverAdmission,
    AfterReceiverAdmission,
    BeforeReceiverConfirmation,
    AfterReceiverConfirmation,
    BeforeGrantRevocation,
    AfterGrantRevocation,
    BeforePaymentResponse,
    AfterPaymentResponse,
}
impl FaultPoint {
    pub const ALL: [Self; 16] = [
        Self::BeforeRemoteAcceptance,
        Self::AfterRemoteAcceptance,
        Self::BeforeLocalCommit,
        Self::AfterLocalCommit,
        Self::BeforeOutboxEnqueue,
        Self::AfterOutboxEnqueue,
        Self::BeforeOutboxAck,
        Self::AfterOutboxAck,
        Self::BeforeReceiverAdmission,
        Self::AfterReceiverAdmission,
        Self::BeforeReceiverConfirmation,
        Self::AfterReceiverConfirmation,
        Self::BeforeGrantRevocation,
        Self::AfterGrantRevocation,
        Self::BeforePaymentResponse,
        Self::AfterPaymentResponse,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::BeforeRemoteAcceptance => "before_remote_acceptance",
            Self::AfterRemoteAcceptance => "after_remote_acceptance",
            Self::BeforeLocalCommit => "before_local_commit",
            Self::AfterLocalCommit => "after_local_commit",
            Self::BeforeOutboxEnqueue => "before_outbox_enqueue",
            Self::AfterOutboxEnqueue => "after_outbox_enqueue",
            Self::BeforeOutboxAck => "before_outbox_ack",
            Self::AfterOutboxAck => "after_outbox_ack",
            Self::BeforeReceiverAdmission => "before_receiver_admission",
            Self::AfterReceiverAdmission => "after_receiver_admission",
            Self::BeforeReceiverConfirmation => "before_receiver_confirmation",
            Self::AfterReceiverConfirmation => "after_receiver_confirmation",
            Self::BeforeGrantRevocation => "before_grant_revocation",
            Self::AfterGrantRevocation => "after_grant_revocation",
            Self::BeforePaymentResponse => "before_payment_response",
            Self::AfterPaymentResponse => "after_payment_response",
        }
    }
}

pub struct FailOnce {
    point: FaultPoint,
    fired: AtomicBool,
}
impl FailOnce {
    pub fn at(point: FaultPoint) -> Self {
        Self {
            point,
            fired: AtomicBool::new(false),
        }
    }
    pub fn hit(&self, point: FaultPoint) -> anyhow::Result<()> {
        anyhow::ensure!(
            point != self.point || self.fired.swap(true, Ordering::SeqCst),
            "injected fault: {}",
            point.name()
        );
        Ok(())
    }
}

pub struct Clock(AtomicI64);
impl Clock {
    pub fn at(micros: i64) -> Self {
        Self(AtomicI64::new(micros))
    }
    pub fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    pub fn set(&self, micros: i64) {
        self.0.store(micros, Ordering::SeqCst);
    }
}

pub struct Gate {
    arrived: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}
pub struct Controller {
    pub arrived: oneshot::Receiver<()>,
    pub release: oneshot::Sender<()>,
}
impl Gate {
    pub fn pair() -> (Self, Controller) {
        let (arrived_tx, arrived_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        (
            Self {
                arrived: arrived_tx,
                release: release_rx,
            },
            Controller {
                arrived: arrived_rx,
                release: release_tx,
            },
        )
    }
    pub async fn wait(self) -> anyhow::Result<()> {
        self.arrived
            .send(())
            .map_err(|()| anyhow::anyhow!("controller gone"))?;
        self.release
            .await
            .map_err(|_| anyhow::anyhow!("gate cancelled"))
    }
}
