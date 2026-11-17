use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

use crate::config::RetentionConfig;
use crate::domain::deletion::RetentionPeriods;
use crate::domain::error::DomainError;

/// One day.
const DAY: Duration = Duration::from_hours(24);

/// [`RetentionPeriods`] from the gear's config, until Construct's settings
/// are declared in the settings service; that implementation replaces this
/// one behind the same interface.
///
/// @cpt-dod:cpt-cf-construct-dod-retention-period:p2
#[derive(Debug, Clone)]
pub struct ConfigRetentionPeriods {
    config: RetentionConfig,
}

impl ConfigRetentionPeriods {
    #[must_use]
    pub fn new(config: RetentionConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl RetentionPeriods for ConfigRetentionPeriods {
    async fn period(&self, tenant_id: Uuid) -> Result<Option<Duration>, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-period:p2:inst-period-read
        let days = self
            .config
            .tenants
            .get(&tenant_id)
            .copied()
            .or(self.config.default_days);
        Ok(days.map(|days| DAY.saturating_mul(days.get())))
        // @cpt-end:cpt-cf-construct-algo-retention-period:p2:inst-period-read
    }

    fn listed(&self) -> BTreeSet<Uuid> {
        self.config.tenants.keys().copied().collect()
    }

    fn has_default(&self) -> bool {
        self.config.default_days.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn days(days: u32) -> NonZeroU32 {
        NonZeroU32::new(days).unwrap()
    }

    #[tokio::test]
    async fn a_listed_tenant_has_its_own_period_and_every_other_the_default_or_none() {
        let (listed, other) = (Uuid::new_v4(), Uuid::new_v4());
        let config = RetentionConfig {
            default_days: None,
            tenants: [(listed, days(30))].into(),
        };
        let periods = ConfigRetentionPeriods::new(config.clone());
        assert_eq!(periods.period(listed).await.unwrap(), Some(DAY * 30));
        assert_eq!(
            periods.period(other).await.unwrap(),
            None,
            "kept until deleted"
        );
        assert_eq!(periods.listed(), BTreeSet::from([listed]));
        assert!(!periods.has_default());

        let periods = ConfigRetentionPeriods::new(RetentionConfig {
            default_days: Some(days(365)),
            ..config
        });
        assert_eq!(periods.period(listed).await.unwrap(), Some(DAY * 30));
        assert_eq!(periods.period(other).await.unwrap(), Some(DAY * 365));
        assert!(periods.has_default());
    }
}
