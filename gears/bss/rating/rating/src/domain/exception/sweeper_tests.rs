use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use toolkit_db::Db;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::ExceptionSweeper;
use crate::authz::ServiceGrant;
use crate::domain::error::DomainError;
use crate::domain::exception::{
    ExceptionKey, ExceptionReason, ExceptionRecord, ExceptionRegister, ExceptionRetrier,
    SubjectKind, SubjectRef,
};
use crate::infra::clock::WallClock;
use crate::infra::storage::repo::exception_repo::OrmExceptionRepo;
use crate::test_providers::FakePdp;
use crate::test_support::migrated_db;

const TENANT: Uuid = Uuid::from_u128(0xA);

/// A retrier whose first call never returns, as one stalled on a downstream call; every later
/// call succeeds. It records the exceptions it was handed.
#[derive(Default)]
struct StallsOnce {
    handed: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl ExceptionRetrier for StallsOnce {
    fn subject_kind(&self) -> SubjectKind {
        SubjectKind::Child
    }

    fn retries(&self, _reason_code: &str) -> bool {
        true
    }

    async fn retry(
        &self,
        _db: &Db,
        _scope: &AccessScope,
        exception: &ExceptionRecord,
    ) -> Result<(), DomainError> {
        let first = {
            let mut handed = self.handed.lock().unwrap();
            handed.push(exception.exception_id);
            handed.len() == 1
        };
        if first {
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

/// A database with `count` open `child` exceptions of [`TENANT`].
async fn with_open_exceptions(count: usize) -> Db {
    let db = migrated_db().await;
    let register = ExceptionRegister::new(Arc::new(OrmExceptionRepo), Arc::new(WallClock));
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let scope = AccessScope::for_tenant(TENANT);
            for n in 0..count {
                let key = ExceptionKey::new(
                    TENANT,
                    SubjectKind::Child,
                    SubjectRef::parse(format!("child-{n}"))?,
                    ExceptionReason::parse("awaiting_attribution")?,
                );
                register.record(tx, &scope, &key, "d").await?;
            }
            Ok::<_, DomainError>(())
        })
    })
    .await
    .unwrap();
    db
}

fn sweeper(retrier: &Arc<StallsOnce>) -> ExceptionSweeper {
    ExceptionSweeper::new(
        vec![Arc::clone(retrier) as Arc<dyn ExceptionRetrier>],
        Arc::new(ServiceGrant::new(FakePdp::new().enforcer())),
        Arc::new(OrmExceptionRepo),
    )
}

#[tokio::test]
async fn a_stalled_retry_fails_after_its_budget_and_the_sweep_moves_on() {
    let db = with_open_exceptions(2).await;
    let retrier = Arc::new(StallsOnce::default());
    let report = tokio::time::timeout(
        Duration::from_secs(10),
        sweeper(&retrier)
            .with_retry_budget(Duration::from_millis(50))
            .sweep_once(&db, &CancellationToken::new()),
    )
    .await
    .expect("a stalled retry does not hold the sweep")
    .unwrap();
    assert_eq!(report.failed, 1, "the stalled retry is counted as failed");
    assert_eq!(report.retried, 1, "the next exception is still retried");
    let handed = { retrier.handed.lock().unwrap().len() };
    assert_eq!(handed, 2);
}

#[tokio::test]
async fn the_end_of_the_term_drops_a_retry_in_flight() {
    let db = with_open_exceptions(2).await;
    let retrier = Arc::new(StallsOnce::default());
    let stop = CancellationToken::new();
    let sweep = sweeper(&retrier);
    let swept = sweep.sweep_once(&db, &stop);
    let stopper = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.cancel();
    };
    let (report, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(swept, stopper)
    })
    .await
    .expect("a stop never waits out a stalled retry");
    let report = report.unwrap();
    assert_eq!((report.retried, report.failed), (0, 0));
    let handed = { retrier.handed.lock().unwrap().len() };
    assert_eq!(handed, 1, "no further exception is handed out");
}
