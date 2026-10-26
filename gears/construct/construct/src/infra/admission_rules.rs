use std::collections::HashSet;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::AdmissionConfig;
use crate::domain::admission::{AdmissionRules, AdmissionRulesSource, Allowed};
use crate::domain::error::DomainError;

/// [`AdmissionRulesSource`] from the deployment config: the same rules for
/// every tenant, until Construct's settings are declared in the settings
/// service. The domain sees one set of rules, so that change does not reach
/// it.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-reader-wiring:p1
#[derive(Debug, Clone)]
pub struct ConfigAdmissionRules {
    rules: AdmissionRules,
}

/// A config list as the rule it sets: absent lets every member through, and a
/// list, also an empty one, only its members.
fn allowed<T: ToString>(members: Option<&[T]>) -> Allowed<String> {
    match members {
        None => Allowed::Any,
        Some(members) => Allowed::Only(
            members
                .iter()
                .map(ToString::to_string)
                .collect::<HashSet<_>>(),
        ),
    }
}

impl ConfigAdmissionRules {
    #[must_use]
    pub fn new(config: &AdmissionConfig) -> Self {
        Self {
            rules: AdmissionRules {
                trusted_connectors: allowed(config.trusted_connectors.as_deref()),
                confidence_floor: config.confidence_floor,
                entity_kinds: allowed(config.entity_kinds.as_deref()),
                max_steps: config.max_steps,
                max_value_chars: config.max_value_chars,
            },
        }
    }
}

#[async_trait]
impl AdmissionRulesSource for ConfigAdmissionRules {
    async fn rules(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
    ) -> Result<AdmissionRules, DomainError> {
        Ok(self.rules.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(config: &AdmissionConfig) -> AdmissionRules {
        ConfigAdmissionRules::new(config).rules
    }

    #[test]
    fn an_absent_list_lets_every_member_through_and_an_empty_one_none() {
        let open = rules(&AdmissionConfig::default());
        assert_eq!(open.trusted_connectors, Allowed::Any);
        assert_eq!(open.entity_kinds, Allowed::Any);

        let closed = rules(&AdmissionConfig {
            trusted_connectors: Some(Vec::new()),
            entity_kinds: Some(Vec::new()),
            ..AdmissionConfig::default()
        });
        assert_eq!(closed.trusted_connectors, Allowed::Only(HashSet::new()));
        assert_eq!(closed.entity_kinds, Allowed::Only(HashSet::new()));
    }

    #[test]
    fn a_trusted_connector_is_its_identity_as_the_caller_names_it() {
        let connector = Uuid::new_v4();
        let rules = rules(&AdmissionConfig {
            trusted_connectors: Some(vec![connector]),
            ..AdmissionConfig::default()
        });
        assert!(rules.trusted_connectors.permits(&connector.to_string()));
        assert!(
            !rules
                .trusted_connectors
                .permits(&Uuid::new_v4().to_string()),
            "a connector off the list is not trusted"
        );
    }
}
