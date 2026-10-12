//! Commercial time. One clock, named once.
use time::OffsetDateTime;
use uuid::Uuid;

/// Time and jitter are injectable so recovery tests never sleep for backoff.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
    fn jitter_millis(&self) -> i64 {
        0
    }
}

/// The process clock, cut to the whole microseconds storage keeps.
pub struct WallClock;
impl Clock for WallClock {
    fn now(&self) -> OffsetDateTime {
        crate::infra::storage::stored_now()
    }
    fn jitter_millis(&self) -> i64 {
        i64::from(Uuid::new_v4().as_bytes()[0])
    }
}
