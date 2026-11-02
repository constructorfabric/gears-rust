//! Fact intake's fixture (feature 06): the real intake over a migrated database, an inbox that
//! applies `fact` entries through it, the Pricing and Products doubles and a recording work queue.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bss_pricing_sdk::read::PricingReadV1;
use bss_rating::authz::ServiceGrant;
use bss_rating::domain::audit::OperatorAudit;
use bss_rating::domain::child::ChildWork;
use bss_rating::domain::declaration::{
    DeclarationSource, DerivedDeclarationRead, DerivedDeclarationStore,
};
use bss_rating::domain::error::DomainError;
use bss_rating::domain::exception::ExceptionRegister;
use bss_rating::domain::fact::intake::{FactIntake, FactIntakeParts};
use bss_rating::domain::inbox::{
    AcceptOutcome, Inbox, InboxApplier, InboxParts, InboxSource, ProducerOrigin, ProducerPolicy,
};
use bss_rating::domain::ports::{Clock, RatingMetricsPort, TenantGrant};
use bss_rating::domain::pricing::{PricingBindingStore, PricingClient};
use bss_rating::domain::work::{FactBindWork, Wakeup, WorkQueue};
use bss_rating::infra::storage::repo::audit_repo::OrmAuditRepo;
use bss_rating::infra::storage::repo::exception_repo::OrmExceptionRepo;
use bss_rating::infra::storage::repo::fact_repo::OrmFactRepo;
use bss_rating::infra::storage::repo::inbox_repo::OrmInboxRepo;
use bss_rating::infra::storage::repo::pricing_repo::OrmPricingRepo;
use bss_rating::test_providers::products::cloudlet_declaration;
use bss_rating::test_providers::{
    FakeDerivedDeclarations, FakePdp, FakePricingCatalog, RecordingMetrics,
};
use serde_json::Value;
use toolkit_db::Db;
use toolkit_db::secure::DbTx;

use super::usage::TestClock;
use super::{TestDb, migrated_db};

/// The derived meter every usage test rates.
pub const METER: &str = "products.derived/cloudlet@1";

/// A work queue that records what it was handed.
#[derive(Default)]
pub struct RecordingQueue {
    children: Mutex<Vec<ChildWork>>,
    binds: Mutex<Vec<FactBindWork>>,
}

impl RecordingQueue {
    pub fn children(&self) -> Vec<ChildWork> {
        self.children.lock().unwrap().clone()
    }

    pub fn binds(&self) -> Vec<FactBindWork> {
        self.binds.lock().unwrap().clone()
    }
}

#[async_trait]
impl WorkQueue for RecordingQueue {
    async fn enqueue_children(
        &self,
        _tx: &DbTx<'_>,
        items: &[ChildWork],
    ) -> Result<Wakeup, DomainError> {
        self.children.lock().unwrap().extend_from_slice(items);
        Ok(Wakeup::default())
    }

    async fn enqueue_fact_bind(
        &self,
        _tx: &DbTx<'_>,
        item: FactBindWork,
    ) -> Result<Wakeup, DomainError> {
        self.binds.lock().unwrap().push(item);
        Ok(Wakeup::default())
    }
}

struct FixedPricing(Arc<FakePricingCatalog>);

impl PricingClient for FixedPricing {
    fn client(&self) -> Result<Arc<dyn PricingReadV1>, DomainError> {
        Ok(Arc::clone(&self.0) as Arc<dyn PricingReadV1>)
    }
}

struct FixedDeclarations(Arc<FakeDerivedDeclarations>);

impl DeclarationSource for FixedDeclarations {
    fn reader(&self) -> Result<Arc<dyn DerivedDeclarationRead>, DomainError> {
        Ok(Arc::clone(&self.0) as Arc<dyn DerivedDeclarationRead>)
    }
}

/// Everything a fact intake test needs.
pub struct FactFixture {
    pub test_db: TestDb,
    pub pdp: Arc<FakePdp>,
    pub clock: Arc<TestClock>,
    pub pricing: Arc<FakePricingCatalog>,
    pub declarations: Arc<FakeDerivedDeclarations>,
    pub queue: Arc<RecordingQueue>,
    pub store: PricingBindingStore,
    pub intake: FactIntake,
    pub inbox: Inbox,
}

impl FactFixture {
    pub fn db(&self) -> &Db {
        &self.test_db.db
    }

    /// Hands `payload` to fact intake as a recovery read would.
    pub async fn receive(&self, payload: Value) -> Result<AcceptOutcome, DomainError> {
        self.intake
            .receive(&self.inbox, payload, ProducerOrigin::PullRead)
            .await
    }
}

/// The fixture at `now` over a migrated SQLite database, with the cloudlet meter declared.
pub async fn fact_fixture(now: &str) -> FactFixture {
    fact_fixture_on(migrated_db().await, now)
}

/// The fixture at `now` over `test_db`, with the cloudlet meter declared.
pub fn fact_fixture_on(test_db: TestDb, now: &str) -> FactFixture {
    let db = test_db.db.clone();
    let pdp = FakePdp::new();
    let clock = TestClock::at(now);
    let as_clock: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let pricing = Arc::new(FakePricingCatalog::new());
    let declarations = Arc::new(FakeDerivedDeclarations::new());
    declarations.declare(METER, cloudlet_declaration());
    let queue = Arc::new(RecordingQueue::default());
    let store = PricingBindingStore::new(
        db.clone(),
        Arc::new(OrmPricingRepo),
        Arc::new(FixedPricing(Arc::clone(&pricing))),
        Arc::clone(&grant),
        Arc::clone(&as_clock),
    );
    let declaration_store = DerivedDeclarationStore::new(
        db.clone(),
        Arc::new(OrmPricingRepo),
        Arc::new(FixedDeclarations(Arc::clone(&declarations))),
        Arc::clone(&grant),
        Arc::clone(&as_clock),
    );
    let exceptions = ExceptionRegister::new(Arc::new(OrmExceptionRepo), Arc::clone(&as_clock));
    let intake = FactIntake::new(FactIntakeParts {
        db: db.clone(),
        facts: Arc::new(OrmFactRepo),
        children: Arc::new(OrmFactRepo),
        schedules: Arc::new(OrmFactRepo),
        projections: Arc::new(OrmFactRepo),
        pricing: store.clone(),
        declarations: declaration_store,
        exceptions: exceptions.clone(),
        queue: Arc::clone(&queue) as Arc<dyn WorkQueue>,
        grant: Arc::clone(&grant),
        clock: Arc::clone(&as_clock),
        shards: 16,
    });
    let appliers: BTreeMap<InboxSource, Arc<dyn InboxApplier>> =
        BTreeMap::from([(InboxSource::Fact, intake.applier())]);
    let inbox = Inbox::new(InboxParts {
        db,
        enforcer: pdp.enforcer(),
        repo: Arc::new(OrmInboxRepo),
        appliers,
        producers: ProducerPolicy::default(),
        grant,
        exceptions,
        audit: OperatorAudit::new(Arc::new(OrmAuditRepo), Arc::clone(&as_clock)),
        metrics: Arc::new(RecordingMetrics::default()) as Arc<dyn RatingMetricsPort>,
        clock: as_clock,
    });
    FactFixture {
        test_db,
        pdp,
        clock,
        pricing,
        declarations,
        queue,
        store,
        intake,
        inbox,
    }
}
