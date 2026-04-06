#![cfg(not(feature = "k8s"))]

use super::*;

#[tokio::test]
async fn disabled_returns_immediately() {
    let elector = crate::infra::leader::noop();
    let cancel = CancellationToken::new();
    let config = OrphanWatchdogConfig {
        enabled: false,
        ..Default::default()
    };
    let deps = test_deps().await;
    let result = run(elector, config, deps, cancel).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn shutdown_on_cancel() {
    let elector = crate::infra::leader::noop();
    let cancel = CancellationToken::new();
    let config = OrphanWatchdogConfig::default();
    let deps = test_deps().await;

    let c = cancel.clone();
    let handle = tokio::spawn(async move { run(elector, config, deps, c).await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(matches!(result, Ok(Ok(Ok(())))));
}

/// Build minimal test deps using the concrete infra repos and an in-memory `SQLite` DB.
async fn test_deps() -> OrphanWatchdogDeps<
    crate::infra::db::repo::turn_repo::TurnRepository,
    crate::infra::db::repo::message_repo::MessageRepository,
> {
    use crate::domain::ports::metrics::NoopMetrics;
    use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};
    use crate::infra::db::repo::message_repo::MessageRepository as MsgRepo;
    use crate::infra::db::repo::turn_repo::TurnRepository as TurnRepo;

    let db = mock_db_provider(inmem_db().await);
    let turn_repo = Arc::new(TurnRepo);
    let message_repo = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }));

    let finalization_svc = Arc::new(FinalizationService::new(
        Arc::clone(&db),
        Arc::clone(&turn_repo),
        Arc::clone(&message_repo),
        Arc::new(NoopQuotaSettler) as Arc<dyn crate::domain::service::quota_settler::QuotaSettler>,
        Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
        Arc::new(NoopMetrics),
    ));

    OrphanWatchdogDeps {
        finalization_svc,
        turn_repo,
        db,
        metrics: Arc::new(NoopMetrics),
    }
}

struct NoopQuotaSettler;

#[async_trait::async_trait]
impl crate::domain::service::quota_settler::QuotaSettler for NoopQuotaSettler {
    async fn settle_in_tx(
        &self,
        _tx: &modkit_db::secure::DbTx<'_>,
        _scope: &modkit_security::AccessScope,
        _input: crate::domain::model::quota::SettlementInput,
    ) -> Result<crate::domain::model::quota::SettlementOutcome, crate::domain::error::DomainError>
    {
        Ok(crate::domain::model::quota::SettlementOutcome {
            settlement_method: crate::domain::model::quota::SettlementMethod::Estimated,
            actual_credits_micro: 0,
            charged_tokens: 0,
            overshoot_capped: false,
        })
    }
}

struct NoopOutboxEnqueuer;

#[async_trait::async_trait]
impl crate::domain::repos::OutboxEnqueuer for NoopOutboxEnqueuer {
    async fn enqueue_usage_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: mini_chat_sdk::UsageEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    async fn enqueue_attachment_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::repos::AttachmentCleanupEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    async fn enqueue_chat_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::repos::ChatCleanupEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    async fn enqueue_audit_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::model::audit_envelope::AuditEnvelope,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    fn flush(&self) {}
}
