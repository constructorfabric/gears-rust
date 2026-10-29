//! Hand-written doubles of the `AuthN` resolver, Pricing reads and Products for the gate adapter
//! tests. In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsRequest,
};
use bss_pricing_sdk::read::{
    AcceptedBinding, BindingSelection, ChargeKind, ImmutablePrice, PlanQuery, PriceModel,
    PriceQuery, PriceState, PricingReadV1, ResolveQuery, ResolvedBindings, ResolvedCell,
    RevisionRef,
};
use bss_pricing_sdk::terms::{BillingTiming, InputSource, InvoiceInputs, Rounding};
use bss_products_sdk::api::ProductsClient;
use bss_products_sdk::models::{Lifecycle, Sku, SkuType};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use time::{Date, Month, OffsetDateTime};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Pricing's plan resource, mirrored so its builder yields the platform's errors.
#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.bss.pricing.plan.v1~"))]
pub struct PlanResource;

/// The `AuthN` resolver: the exchange answers a context for `subject` in `subject_tenant`, or the
/// configured failure; every request is captured.
pub struct FakeResolver {
    pub subject: Uuid,
    pub subject_tenant: Uuid,
    pub failure: Option<fn() -> AuthNResolverError>,
    /// The exchange never answers within any deadline.
    pub stall: bool,
    pub exchanges: AtomicUsize,
    pub requests: parking_lot::Mutex<Vec<(String, Vec<String>)>>,
}

impl FakeResolver {
    pub fn new(subject: Uuid, subject_tenant: Uuid) -> Self {
        Self {
            subject,
            subject_tenant,
            failure: None,
            stall: false,
            exchanges: AtomicUsize::new(0),
            requests: parking_lot::Mutex::new(Vec::new()),
        }
    }
    pub fn exchanges(&self) -> usize {
        self.exchanges.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AuthNResolverClient for FakeResolver {
    async fn authenticate(&self, _token: &str) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Internal("not used".to_owned()))
    }
    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.exchanges.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .push((request.client_id.clone(), request.scopes.clone()));
        if self.stall {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        if let Some(failure) = self.failure {
            return Err(failure());
        }
        // The double stands in for the AuthN plugin, which is what builds the context.
        let security_context = SecurityContext::builder()
            .subject_id(self.subject)
            .subject_tenant_id(self.subject_tenant)
            .bearer_token("service-token")
            .build()
            .unwrap();
        Ok(AuthenticationResult { security_context })
    }
}

/// A failure [`FakePricing::fail_with`] injects into the Pricing reads (the D-233 item 6 matrix,
/// compiled only with `gate-dev-stubs`, as the matrix is).
#[cfg(feature = "gate-dev-stubs")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Both reads answer a transient `ServiceUnavailable` (retried once by `bounded`).
    Unavailable,
    /// Both reads answer a permanent `PermissionDenied` (`Unavailable(Denied)`).
    Denied,
    /// Both reads answer a permanent `InvalidArgument` (`Unavailable(Malformed)`).
    #[expect(
        dead_code,
        reason = "the matrix has no malformed row yet; kept for the next one"
    )]
    Malformed,
    /// Both reads sleep one hour, past every port deadline.
    Hang,
    /// `resolve` alone answers after the delay; `current_revision` answers at once.
    Delay(Duration),
}

/// Pricing reads: `current_revision` answers `current` (or PEP denial, or sleeps past every
/// deadline); `resolve` answers `cells` for the queried revision; with `gate-dev-stubs`, an
/// injected `Fault` wins over both. Every catalog is captured.
pub struct FakePricing {
    pub current: RevisionRef,
    pub deny_current: bool,
    pub stall_current: bool,
    /// A permanent error `current_revision` answers instead of the revision.
    pub current_error: Option<fn() -> CanonicalError>,
    /// A permanent error `resolve` answers instead of the cells.
    pub resolve_error: Option<fn() -> CanonicalError>,
    pub cells: Vec<ResolvedCell>,
    pub resolve_dates: parking_lot::Mutex<Vec<Date>>,
    pub publication: parking_lot::Mutex<Option<(RevisionRef, Vec<ResolvedCell>)>>,
    pub calls: AtomicUsize,
    /// `current_revision` calls.
    pub currents: AtomicUsize,
    /// `resolve` calls.
    pub resolves: AtomicUsize,
    pub catalogs: parking_lot::Mutex<Vec<Uuid>>,
    /// The fault every later read answers ([`Self::fail_with`]); `None` answers normally.
    #[cfg(feature = "gate-dev-stubs")]
    pub fail_with: parking_lot::Mutex<Option<Fault>>,
}

impl FakePricing {
    pub fn new(current: RevisionRef, cells: Vec<ResolvedCell>) -> Self {
        Self {
            current,
            deny_current: false,
            stall_current: false,
            current_error: None,
            resolve_error: None,
            cells,
            resolve_dates: parking_lot::Mutex::new(Vec::new()),
            publication: parking_lot::Mutex::new(None),
            calls: AtomicUsize::new(0),
            currents: AtomicUsize::new(0),
            resolves: AtomicUsize::new(0),
            catalogs: parking_lot::Mutex::new(Vec::new()),
            #[cfg(feature = "gate-dev-stubs")]
            fail_with: parking_lot::Mutex::new(None),
        }
    }
    /// Inject `fault` into every later read (`None` clears it).
    #[cfg(feature = "gate-dev-stubs")]
    pub fn fail_with(&self, fault: Option<Fault>) {
        *self.fail_with.lock() = fault;
    }
    /// The injected fault's answer for one read: the error to return, or `None` to answer
    /// normally (after sleeping for `Hang`, and for `Delay` on `resolve`).
    #[cfg(feature = "gate-dev-stubs")]
    async fn injected(&self, resolve: bool) -> Option<CanonicalError> {
        let fault = *self.fail_with.lock();
        match fault? {
            Fault::Unavailable => Some(CanonicalError::service_unavailable().create()),
            Fault::Denied => Some(
                PlanResource::permission_denied()
                    .with_reason("denied")
                    .create(),
            ),
            Fault::Malformed => Some(
                PlanResource::invalid_argument()
                    .with_field_violation("revision_id", "malformed", "MALFORMED")
                    .create(),
            ),
            Fault::Hang => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                None
            }
            Fault::Delay(delay) => {
                if resolve {
                    tokio::time::sleep(delay).await;
                }
                None
            }
        }
    }
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    pub fn currents(&self) -> usize {
        self.currents.load(Ordering::SeqCst)
    }
    pub fn resolves(&self) -> usize {
        self.resolves.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl PricingReadV1 for FakePricing {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        query: ResolveQuery,
    ) -> Result<ResolvedBindings, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.resolves.fetch_add(1, Ordering::SeqCst);
        self.resolve_dates.lock().push(query.date);
        self.catalogs.lock().push(query.catalog.tenant_id);
        #[cfg(feature = "gate-dev-stubs")]
        if let Some(error) = self.injected(true).await {
            return Err(error);
        }
        if let Some(error) = self.resolve_error {
            return Err(error());
        }
        Ok(ResolvedBindings {
            plan_id: self.current.plan_id,
            revision_id: query.revision_id,
            cells: self
                .publication
                .lock()
                .as_ref()
                .map_or_else(|| self.cells.clone(), |(_, cells)| cells.clone()),
        })
    }
    async fn price(
        &self,
        _ctx: &SecurityContext,
        _query: PriceQuery,
    ) -> Result<ImmutablePrice, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(CanonicalError::internal("not used").create())
    }
    async fn current_revision(
        &self,
        _ctx: &SecurityContext,
        query: PlanQuery,
    ) -> Result<RevisionRef, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.currents.fetch_add(1, Ordering::SeqCst);
        self.catalogs.lock().push(query.catalog.tenant_id);
        #[cfg(feature = "gate-dev-stubs")]
        if let Some(error) = self.injected(false).await {
            return Err(error);
        }
        if self.stall_current {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        if let Some(error) = self.current_error {
            return Err(error());
        }
        if self.deny_current {
            return Err(PlanResource::permission_denied()
                .with_reason("denied")
                .create());
        }
        Ok(self
            .publication
            .lock()
            .as_ref()
            .map_or_else(|| self.current.clone(), |(revision, _)| revision.clone()))
    }
}

/// Products: every SKU is published and sellable; calls and tenants are captured.
#[derive(Default)]
pub struct FakeProducts {
    /// The one SKU whose read fails permanently.
    pub fail_sku: Option<Uuid>,
    pub calls: AtomicUsize,
    pub tenants: parking_lot::Mutex<Vec<Uuid>>,
}

impl FakeProducts {
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ProductsClient for FakeProducts {
    async fn get_sku(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
        sku_id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.tenants.lock().push(tenant_id);
        if self.fail_sku == Some(sku_id) {
            return Err(PlanResource::permission_denied()
                .with_reason("denied")
                .create());
        }
        Ok(sku(sku_id, tenant_id))
    }
}

pub fn date(y: i32, m: Month, d: u8) -> Date {
    Date::from_calendar_date(y, m, d).unwrap()
}

pub fn sku(id: Uuid, tenant_id: Uuid) -> Sku {
    let t = OffsetDateTime::UNIX_EPOCH;
    Sku {
        id,
        tenant_id,
        code: "VM-SMALL".into(),
        name: "Small VM".into(),
        r#type: SkuType::Recurring,
        category_id: None,
        description: "small".into(),
        sellable: true,
        lifecycle: Lifecycle::Published,
        retire_pending: false,
        lifecycle_next: None,
        revision: 1,
        published_version: 3,
        gl_code: None,
        tax_category: None,
        invoice_line_template: None,
        billing_timing: None,
        usage_type_ref: None,
        unit: None,
        type_change_pending: false,
        pending_unit_id: None,
        approved_by_unit_id: None,
        created_by: Uuid::from_u128(31),
        created_at: t,
        updated_at: t,
        archived_at: None,
        archived_by: None,
    }
}

/// An approved binding whose price and template digests are the Pricing digests of their
/// content, so `selected_bindings_digest` accepts it (submit freezes its digest, S3-12).
pub fn binding(item: Uuid, dim: Option<&str>, sku: Uuid) -> AcceptedBinding {
    let mut binding = AcceptedBinding {
        item_id: item,
        price_book_entry_id: Uuid::from_u128(20),
        dimension_key: dim.map(|_| "region".to_owned()),
        dimension_value: dim.map(str::to_owned),
        sku_id: sku,
        sku_version: 3,
        sku_code: "VM-SMALL".into(),
        sku_name: "Small VM".into(),
        unit: Some("instance".into()),
        meter: None,
        price: ImmutablePrice {
            price_id: Uuid::from_u128(21),
            price_book_entry_id: Uuid::from_u128(20),
            money_digest: [0; 32],
            currency: bss_money::CurrencySpec::try_new("USD".into(), 2).unwrap(),
            model: PriceModel::Flat {
                amount: bss_money::parse_decimal("10").unwrap(),
            },
            minimum_fee: None,
            effective_from: date(2026, Month::January, 1),
            ends_on: None,
            state: PriceState::Approved,
        },
        kind: ChargeKind::Recurring,
        recurring_period: Some(bss_pricing_sdk::terms::BillingCycle::Month),
        via_default: false,
        usage_rating_policy: None,
        invoice: InvoiceInputs {
            template: "{sku_name}".into(),
            template_digest: bss_pricing_sdk::digest::template_digest("{sku_name}"),
            template_source: InputSource::Entry,
            gl_code: "4000".into(),
            tax_category: "standard".into(),
            timing: BillingTiming::Advance,
            rounding: Rounding::HalfEven,
        },
    };
    binding.price.money_digest = bss_pricing_sdk::digest::money_digest(&binding.price);
    binding
}

pub fn bound_cell(item: Uuid, dim: Option<&str>, sku: Uuid) -> ResolvedCell {
    ResolvedCell {
        selection: BindingSelection {
            item_id: item,
            dimension_value: dim.map(str::to_owned),
        },
        binding: Some(binding(item, dim, sku)),
    }
}

/// The production catalog adapter over `pricing` and [`FakeProducts`], presenting `seller`'s
/// commercial identity through [`FakeResolver`], under `budget` (the orchestrator tests swap it
/// in with `Orchestrator::with_catalog`). Only the `gate-dev-stubs` matrix uses it.
#[cfg(feature = "gate-dev-stubs")]
pub fn pricing_catalog(
    pricing: std::sync::Arc<FakePricing>,
    seller: Uuid,
    budget: std::sync::Arc<crate::infra::gate::budget::Budget>,
) -> crate::infra::gate::pricing_read::PricingCatalogReads {
    use crate::infra::gate::authority::{AuthnAuthority, CommercialAuthority};
    let identity = crate::config::CommercialIdentityConfig {
        seller_tenant_id: seller,
        client_id: "orders-gate".to_owned(),
        client_secret: secrecy::SecretString::from("x"),
        scopes: Vec::new(),
    };
    let authority: std::sync::Arc<dyn CommercialAuthority> =
        std::sync::Arc::new(AuthnAuthority::new(
            std::sync::Arc::new(FakeResolver::new(
                Uuid::from_u128(0x0bd5),
                Uuid::from_u128(0x40e),
            )),
            &[identity],
        ));
    crate::infra::gate::pricing_read::PricingCatalogReads::new(
        pricing,
        std::sync::Arc::new(FakeProducts::default()),
        authority,
        budget,
    )
}
