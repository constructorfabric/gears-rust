use std::collections::HashSet;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::record_intake::ConnectorSwitch;

/// [`ConnectorSwitch`] from the deployment config: every connector is on unless
/// the config lists it as off. This is the first version, until the settings
/// service holds connectors on or off as a tenant setting (Story 5.11 decides
/// whether it runs in Construct's host); the domain sees only the switch, so
/// the change does not reach it.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-connector-switch:p1
#[derive(Debug, Default)]
pub struct ConfigConnectorSwitch {
    off: HashSet<Uuid>,
}

impl ConfigConnectorSwitch {
    #[must_use]
    pub fn new(off: impl IntoIterator<Item = Uuid>) -> Self {
        Self {
            off: off.into_iter().collect(),
        }
    }
}

#[async_trait]
impl ConnectorSwitch for ConfigConnectorSwitch {
    async fn is_on(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        connector: Uuid,
    ) -> Result<bool, DomainError> {
        Ok(!self.off.contains(&connector))
    }
}
