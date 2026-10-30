//! The provisioning transaction keeps business rejections across its boundary,
//! and the registry rows it seeds never override an ISO 4217 scale.

use bss_ledger_sdk::{
    AccountClass, CurrencySpec, FiscalCalendarSpec, Granularity, ProvisionAccount,
    ProvisionCurrencyScale, ProvisionRequest, Side,
};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

use super::{ProvisioningService, repo_to_db};
use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::infra::currency_scale::CurrencyScaleResolver;
use crate::infra::posting::error_transport::decode_business_error;
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo::ReferenceRepo;

fn round_trip(error: RepoError) -> DomainError {
    decode_business_error(&repo_to_db(error))
}

#[test]
fn a_scale_lock_and_a_concurrent_registration_keep_their_meaning() {
    assert!(matches!(
        round_trip(RepoError::CurrencyScaleLocked("XTS".into())),
        DomainError::CurrencyScaleLocked(_)
    ));
    assert!(matches!(
        round_trip(RepoError::Conflict("concurrent grain insertion".into())),
        DomainError::ConcurrentModification(_)
    ));
}

#[test]
fn scale_out_of_range_keeps_its_flow_contract_and_the_rest_is_internal() {
    match round_trip(RepoError::ScaleOutOfRange("XTS".into())) {
        DomainError::ScaleOutOfRange(detail) => {
            assert!(detail.contains("XTS"), "the refusal names the currency");
        }
        _ => panic!("expected ScaleOutOfRange"),
    }
    assert!(matches!(
        round_trip(RepoError::Db("connection reset".into())),
        DomainError::Internal(_)
    ));
}

async fn seeded_service() -> (ProvisioningService, ReferenceRepo) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .unwrap();
    let provider = DBProvider::<DbError>::new(db);
    (
        ProvisioningService::new(provider.clone()),
        ReferenceRepo::new(provider),
    )
}

fn request(tenant_id: Uuid, scales: &[(&str, u8)]) -> ProvisionRequest {
    ProvisionRequest {
        tenant_id,
        accounts: vec![ProvisionAccount {
            account_class: AccountClass::Ar,
            currency: "USD".to_owned(),
            revenue_stream: None,
            normal_side: Side::Debit,
            may_go_negative: false,
        }],
        currency_scales: scales
            .iter()
            .map(|(code, scale)| ProvisionCurrencyScale {
                currency: CurrencySpec::try_new((*code).to_owned(), *scale).unwrap(),
                source: "TENANT".to_owned(),
            })
            .collect(),
        fiscal_calendar: FiscalCalendarSpec {
            timezone: "UTC".to_owned(),
            granularity: Granularity::Month,
            fy_start_month: 1,
            functional_currency: None,
        },
    }
}

#[tokio::test]
async fn an_iso_code_at_another_scale_is_refused_and_nothing_is_written() {
    let (service, reference) = seeded_service().await;
    let tenant = Uuid::now_v7();
    // The valid non-ISO row comes first: the whole request is refused, not just the bad row.
    let refused = service
        .provision(request(tenant, &[("XTS", 6), ("HUF", 0)]))
        .await
        .unwrap_err();
    match refused {
        DomainError::InvalidRequest(message) => {
            assert_eq!(
                message,
                "currency HUF is an ISO 4217 code with scale 2; \
                 the registry cannot set scale 0 for it",
                "the refusal names the code, its ISO scale and the refused scale"
            );
        }
        _ => panic!("an ISO code at another scale must be an invalid request"),
    }
    let scope = AccessScope::for_tenant(tenant);
    for code in ["XTS", "HUF"] {
        assert!(
            reference
                .find_currency_scale(&scope, tenant, code)
                .await
                .unwrap()
                .is_none(),
            "a refused request writes no registry row"
        );
    }
}

#[tokio::test]
async fn an_iso_code_at_its_iso_scale_is_accepted_and_idempotent() {
    let (service, reference) = seeded_service().await;
    let tenant = Uuid::now_v7();
    let first = service
        .provision(request(tenant, &[("HUF", 2)]))
        .await
        .unwrap();
    assert_eq!((first.scales_created, first.scales_existing), (1, 0));
    let again = service
        .provision(request(tenant, &[("HUF", 2)]))
        .await
        .unwrap();
    assert_eq!((again.scales_created, again.scales_existing), (0, 1));
    let scope = AccessScope::for_tenant(tenant);
    assert_eq!(
        CurrencyScaleResolver::new(reference)
            .resolve(&scope, tenant, "HUF")
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn a_non_iso_code_is_registered_at_its_own_scale_and_resolved() {
    let (service, reference) = seeded_service().await;
    let tenant = Uuid::now_v7();
    let outcome = service
        .provision(request(tenant, &[("XTS", 6)]))
        .await
        .unwrap();
    assert_eq!(outcome.scales_created, 1);
    let scope = AccessScope::for_tenant(tenant);
    assert_eq!(
        CurrencyScaleResolver::new(reference)
            .resolve(&scope, tenant, "XTS")
            .await
            .unwrap(),
        6
    );
}
