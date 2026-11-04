//! The sink contract's closed error set and its object safety.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::notifications::{DispatchError, QuotaEvent, QuotaNotificationSinkV1};

/// A sink that takes every event.
struct Accepting;

#[async_trait]
impl QuotaNotificationSinkV1 for Accepting {
    fn id(&self) -> &'static str {
        "accepting"
    }

    async fn dispatch(
        &self,
        _ctx: &SecurityContext,
        _event: QuotaEvent,
    ) -> Result<(), DispatchError> {
        Ok(())
    }
}

#[test]
fn dispatch_errors_carry_their_reason() {
    assert_eq!(DispatchError::Timeout.to_string(), "the sink timed out");
    assert!(
        DispatchError::Transient("busy".to_owned())
            .to_string()
            .contains("busy")
    );
    assert!(
        DispatchError::Permanent("gone".to_owned())
            .to_string()
            .contains("gone")
    );
}

#[test]
fn the_sink_contract_is_object_safe() {
    let sink: Box<dyn QuotaNotificationSinkV1> = Box::new(Accepting);
    assert_eq!(sink.id(), "accepting");
}
