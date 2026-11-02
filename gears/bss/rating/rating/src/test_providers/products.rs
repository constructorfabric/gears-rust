//! `FakeDerivedDeclarations` (DESIGN 11 §4.12; R-31): Products' derived-declaration read, which
//! Products does not offer yet. A test declares the meters it rates and can make the read
//! unavailable, as Products' outage would.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use bss_products_sdk::derived::{
    DerivedInput, DerivedUsageDeclaration, Expr, Granularity, GranuleFold, MeterId, RoundMode,
};
use rust_decimal::Decimal;
use toolkit_security::SecurityContext;

use crate::domain::declaration::DerivedDeclarationRead;
use crate::domain::error::DomainError;
use crate::domain::ports::Dependency;
use crate::test_providers::usage::meter;

/// A scripted derived-declaration read.
#[derive(Default)]
pub struct FakeDerivedDeclarations {
    declarations: Mutex<BTreeMap<String, DerivedUsageDeclaration>>,
    unavailable: AtomicBool,
    reads: AtomicUsize,
}

impl FakeDerivedDeclarations {
    /// A read that declares nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares `meter` (`products.derived/<code>@<n>`) as `declaration`.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    pub fn declare(&self, meter: &str, declaration: DerivedUsageDeclaration) {
        self.declarations
            .lock()
            .expect("fake declarations lock")
            .insert(meter.to_owned(), declaration);
    }

    /// Makes every read fail as Products' outage would, or answer again.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// How many reads were made.
    #[must_use]
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DerivedDeclarationRead for FakeDerivedDeclarations {
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    async fn read(
        &self,
        _ctx: &SecurityContext,
        meter: &MeterId,
    ) -> Result<Option<DerivedUsageDeclaration>, DomainError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(DomainError::DependencyUnavailable {
                dependency: Dependency::Products,
                detail: "the fake derived-declaration read is unavailable".to_owned(),
                cause: None,
            });
        }
        Ok(self
            .declarations
            .lock()
            .expect("fake declarations lock")
            .get(&meter.format())
            .cloned())
    }
}

/// A cloudlet declaration: 128 MB of RAM per cloudlet-hour, from the test meter `ram`.
#[must_use]
pub fn cloudlet_declaration() -> DerivedUsageDeclaration {
    DerivedUsageDeclaration {
        output_unit: "cloudlet-hour".to_owned(),
        granularity: Granularity::Hour,
        inputs: vec![DerivedInput {
            name: "ram".to_owned(),
            usage_type_ref: meter("ram_mb").to_string(),
            granule_fold: GranuleFold::Sum,
            max_hold_seconds: None,
            unit: "MB-hour".to_owned(),
        }],
        formula: Expr::DivConst(
            Box::new(Expr::Input("ram".to_owned())),
            Decimal::new(128, 0),
        ),
        output_scale: 6,
        output_round: RoundMode::HalfEven,
    }
}
