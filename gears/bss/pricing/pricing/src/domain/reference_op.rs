//! The single pure transition table used by both request and recovery execution.
use super::price_book_entry::OpState;
use uuid::Uuid;

// What an op does: a create reserves before its write, a delete releases after its removal, a
// rereserve replaces a released receipt, an attach reserves after a copied item's write
// (D-413), and a release lets an archived book's entry go (D-522).
string_enum!(OpKind {Create=>"create", Delete=>"delete", Rereserve=>"rereserve", Attach=>"attach", Release=>"release"});
// The reference an op works for, spelled as Products spells the reference kind (D-407).
string_enum!(RefKind {Entry=>"price_book_entry", PlanItem=>"plan_item"});
/// Durable protocol state; request metadata belongs to the persistence adapter.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub state: OpState,
    pub reservation_id: Option<Uuid>,
    pub refusal: Option<String>,
}
/// Observations from the registry or a committed local write.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Reserved {
        id: Uuid,
    },
    ReserveRefused {
        code: String,
    },
    RegistryUnavailable,
    SkuRefused {
        code: String,
    },
    Written,
    Confirmed,
    ConfirmFailed,
    ReleasedOnConfirm,
    Released,
    ReleaseFailed,
    /// The door answered 503 before the entry was written (spec §13: nothing is written),
    /// whether or not its reserve had already returned a receipt.
    ReservationUnknown,
}
/// Work authorized by a transition; effects are executed outside the pure model.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    ReadSku,
    Confirm,
    Release,
    Retry,
    Complete,
    /// The reservation was released before its confirm: keep the entry pending and
    /// re-reserve it; only a SKU that refuses the reservation makes the entry lost (D-401).
    Rereserve,
}
/// Illegal input is a typed failure, including all observations on terminal work.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("illegal reference event {event:?} in {state:?}")]
pub struct IllegalTransition {
    pub state: OpState,
    pub event: Event,
}
/// Advance one observation. A confirm timeout can only schedule another confirm.
/// # Errors
/// Returns `IllegalTransition` for every unlisted state/event pair.
pub fn next(mut op: Op, event: Event) -> Result<(Op, Vec<Effect>), IllegalTransition> {
    use OpState::{Cancelling, Done, Releasing, Reserving, Written};
    let (state, effect) = match (op.state, &event) {
        (Reserving, Event::Reserved { id }) => {
            op.reservation_id = Some(*id);
            (Reserving, Effect::ReadSku)
        }
        (Reserving, Event::ReserveRefused { code } | Event::SkuRefused { code }) => {
            op.refusal = Some(code.clone());
            (Cancelling, Effect::Release)
        }
        (Reserving, Event::RegistryUnavailable) => (Reserving, Effect::Retry),
        // The door gave up before the write, so no entry may be written: cancel. Without a
        // receipt the cancellation finds and releases whatever reservation the lost call may
        // have made; with one it releases that receipt.
        (Reserving, Event::ReservationUnknown) => {
            op.refusal = Some(
                if op.reservation_id.is_none() {
                    "RESERVATION_UNKNOWN"
                } else {
                    "ABANDONED_BEFORE_WRITE"
                }
                .into(),
            );
            (Cancelling, Effect::Release)
        }
        (Reserving, Event::Written) => (Written, Effect::Confirm),
        (Written, Event::Confirmed) | (Cancelling | Releasing, Event::Released) => {
            (Done, Effect::Complete)
        }
        (Written, Event::ConfirmFailed) => (Written, Effect::Retry),
        (Written, Event::ReleasedOnConfirm) => (Done, Effect::Rereserve),
        (Cancelling | Releasing, Event::ReleaseFailed) => (op.state, Effect::Retry),
        _ => {
            return Err(IllegalTransition {
                state: op.state,
                event,
            });
        }
    };
    op.state = state;
    Ok((op, vec![effect]))
}
