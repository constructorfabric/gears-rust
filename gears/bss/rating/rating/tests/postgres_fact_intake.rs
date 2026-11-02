//! PostgreSQL tier of fact intake (feature 06; DESIGN §3.7 "Database profiles for tests"): the
//! migration's widened inbox reason check and PostgreSQL-only checks, the fact head's row lock,
//! and two versions of a new fact accepted concurrently.
//! Ignored by default; run with `make test-integration LANE=rating-pg` (Docker required).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use bss_rating::BssRatingGear;
use bss_rating::domain::fact::BindingState;
use bss_rating::domain::inbox::{AcceptOutcome, QuarantineReason};
use bss_rating::infra::storage::repo::fact_repo::test_reads;
use bss_rating::test_providers::subscriptions::{
    FactSpec, FakeSubscriptionsBilling, lot, recurring_binding, term, usage_binding,
};
use bss_rating_core::engine::current::input::{BillingTiming, RatingWindowKind};
use common::TestDb;
use common::facts::{FactFixture, METER, fact_fixture_on};
use rust_decimal::Decimal;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::ConnectOpts;
use uuid::Uuid;

const OCT: &str = "2026-10-01T00:00:00Z";
const NOV: &str = "2026-11-01T00:00:00Z";
const SELLER: Uuid = Uuid::from_u128(0x5e11);

struct Pg {
    fixture: FactFixture,
    url: String,
    _container: ContainerAsync<testcontainers_modules::postgres::Postgres>,
}

impl Pg {
    async fn raw(&self, sql: &str) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
        use sea_orm::ConnectionTrait;
        let conn = sea_orm::Database::connect(&self.url).await.unwrap();
        conn.execute_unprepared(sql).await
    }
}

async fn pg() -> Pg {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = toolkit_db::connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(8),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        BssRatingGear::default().migrations(),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    Pg {
        fixture: fact_fixture_on(TestDb::from_parts(db, dir), OCT),
        url,
        _container: container,
    }
}

fn recurring(seats: &str) -> FactSpec {
    let item = Uuid::from_u128(0x17);
    let binding = recurring_binding(
        item,
        Uuid::from_u128(0x9),
        Decimal::new(10, 0),
        BillingTiming::Advance,
    );
    FakeSubscriptionsBilling::new(SELLER)
        .fact("recurring", item, OCT, NOV)
        .chargeable(OCT, NOV, term("t1", binding), vec![lot("A", seats, "t1")])
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_refused_payload_is_quarantined_under_the_widened_reason_check() {
    let pg = pg().await;
    let mut spec = recurring("10");
    spec.anchor_at = "2026-10-01T00:00:00.5Z".to_owned();
    let outcome = pg.fixture.receive(spec.payload()).await.unwrap();
    let AcceptOutcome::Quarantined(entry) = outcome else {
        panic!("not quarantined: {outcome:?}");
    };
    assert_eq!(
        entry.quarantine().unwrap().reason(),
        QuarantineReason::InstantNotWholeSecond
    );
    // The widened check still refuses a reason no quarantine produces.
    let refused = pg
        .raw(
            "UPDATE bss_rating__inbox SET quarantine_reason = 'made_up' \
             WHERE quarantine_reason = 'instant_not_whole_second'",
        )
        .await;
    assert!(refused.is_err());
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn recurring_and_usage_facts_are_stored_with_their_jsonb_columns() {
    let pg = pg().await;
    let f = &pg.fixture;
    let spec = recurring("10");
    f.receive(spec.payload()).await.unwrap();
    let item = Uuid::from_u128(0x18);
    let binding = usage_binding(
        item,
        Uuid::from_u128(0xa),
        RatingWindowKind::CalendarHourUtc,
        METER,
        None,
    );
    let mut usage = FakeSubscriptionsBilling::new(SELLER)
        .fact("usage", item, OCT, NOV)
        .chargeable(OCT, NOV, term("u1", binding), Vec::new());
    "arrears".clone_into(&mut usage.timing);
    f.receive(usage.payload()).await.unwrap();

    let conn = f.db().conn().unwrap();
    let head = test_reads::head(&conn, SELLER, usage.fact_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.binding, BindingState::Bound);
    let schedules = test_reads::schedules(&conn, SELLER, usage.fact_id)
        .await
        .unwrap();
    assert_eq!(schedules[0].expected_count, 744);
    let children = test_reads::children(&conn, SELLER, spec.fact_id)
        .await
        .unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].aggregation_key["sub_line_key"], "plan#1");
    // The head refuses a bound row without a manifest digest of hex, and a pending one with a
    // manifest.
    let refused = pg
        .raw(&format!(
            "UPDATE bss_rating__fact_head SET binding_state = 'pending', \
             binding_reason = 'derived_declaration_unavailable' WHERE fact_id = '{}'",
            usage.fact_id
        ))
        .await;
    assert!(refused.is_err(), "a pending head carries no manifest");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_versions_of_a_new_fact_accepted_at_once_leave_the_higher_head() {
    let pg = pg().await;
    let f = &pg.fixture;
    let v1 = recurring("10");
    let mut v2 = v1.next_version();
    v2.slices[0].lots = vec![lot("A", "12", "t1")];
    let (one, two) = tokio::join!(f.receive(v1.payload()), f.receive(v2.payload()));
    one.unwrap();
    two.unwrap();
    let conn = f.db().conn().unwrap();
    assert_eq!(
        test_reads::versions(&conn, SELLER, v1.fact_id)
            .await
            .unwrap(),
        vec![1, 2]
    );
    let head = test_reads::head(&conn, SELLER, v1.fact_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.current_version, 2);
    let children = test_reads::children(&conn, SELLER, v1.fact_id)
        .await
        .unwrap();
    assert_eq!(
        children.len(),
        1,
        "one period_line whichever version came first"
    );
}
