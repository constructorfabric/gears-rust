//! The stored-counter invariant names the schedule and its counters, so a
//! corrupt row can be found from the error alone.
#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;
use bss_ledger_sdk::money::CurrencySpec;

fn m(cents: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(cents, 2),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

#[test]
fn invalid_stored_counters_name_schedule_and_values() {
    let stream = ScheduleStreamState {
        revenue_stream: "SAAS".to_owned(),
        schedule_id: "sch-42".to_owned(),
        total_deferred: m(1000),
        recognized: m(1200),
        status: "COMPLETED".to_owned(),
        version: 3,
    };
    match stream.releasable_remaining() {
        Err(DomainError::Internal(detail)) => {
            for needle in ["sch-42", "SAAS", "COMPLETED", "10 USD", "12 USD"] {
                assert!(detail.contains(needle), "{needle}: {detail}");
            }
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}
