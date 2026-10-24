//! Deployment-controlled identity mappings. Browser claims never choose a tenant.
use serde::Deserialize;
use std::collections::BTreeSet;
use uuid::Uuid;

/// The plugin's configuration.
#[derive(Debug, Clone, Deserialize, toolkit_macros::ExpandVars)]
#[serde(default, deny_unknown_fields)]
pub struct FirebaseAuthnPluginConfig {
    /// Vendor this plugin instance registers under.
    pub vendor: String,
    /// Selection priority among `AuthN` resolver plugins; lower wins.
    pub priority: i16,
    /// The Firebase project: the expected audience, and part of the issuer.
    #[expand_vars]
    pub project_id: String,
    /// `UUIDv5` namespace for subject ids derived from Firebase uids.
    pub subject_namespace: Uuid,
    /// The accounts admitted, each with the tenant and scopes it gets.
    #[expand_vars]
    pub identities: Vec<IdentityMapping>,
}

impl Default for FirebaseAuthnPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 25,
            project_id: String::new(),
            subject_namespace: Uuid::NAMESPACE_URL,
            identities: Vec::new(),
        }
    }
}

/// One admitted account. The token's uid and verified email must both match.
#[derive(Debug, Clone, Deserialize, toolkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct IdentityMapping {
    /// The Firebase user id.
    #[expand_vars]
    pub uid: String,
    /// The account's address, compared case-insensitively.
    #[expand_vars]
    pub email: String,
    /// The tenant this account acts in. Never taken from the token.
    pub tenant_id: Uuid,
    /// The scopes this account's requests carry. Never taken from the token.
    pub token_scopes: Vec<String>,
}

impl FirebaseAuthnPluginConfig {
    /// Validate the deployment allowlist before registering any plugin instance.
    /// # Errors
    /// Refuses missing project/mappings, duplicate UIDs and nil tenant identities.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.project_id.is_empty()
                && self.project_id.len() <= 63
                && self
                    .project_id
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
            "invalid Firebase project ID"
        );
        anyhow::ensure!(
            !self.subject_namespace.is_nil(),
            "subject namespace must not be nil"
        );
        anyhow::ensure!(
            !self.identities.is_empty(),
            "Firebase identity mappings are required"
        );
        let mut uids = BTreeSet::new();
        for identity in &self.identities {
            anyhow::ensure!(
                !identity.uid.is_empty() && identity.uid.len() <= 128 && uids.insert(&identity.uid),
                "invalid or duplicate Firebase UID mapping"
            );
            anyhow::ensure!(
                identity.email.contains('@') && !identity.email.trim().is_empty(),
                "verified email mapping is required"
            );
            anyhow::ensure!(
                !identity.tenant_id.is_nil(),
                "mapped tenant must not be nil"
            );
            anyhow::ensure!(
                !identity.token_scopes.is_empty()
                    && identity
                        .token_scopes
                        .iter()
                        .all(|scope| !scope.trim().is_empty()),
                "identity scopes are required"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> FirebaseAuthnPluginConfig {
        FirebaseAuthnPluginConfig {
            project_id: "firebase-authn-test".to_owned(),
            identities: vec![IdentityMapping {
                uid: "uid-a".to_owned(),
                email: "a@example.com".to_owned(),
                tenant_id: Uuid::from_u128(1),
                token_scopes: vec!["scope:a".to_owned()],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_complete_config_is_valid() {
        assert!(valid().validate().is_ok());
    }

    type Breaker = fn(&mut FirebaseAuthnPluginConfig);

    #[test]
    fn each_missing_or_malformed_field_is_refused() {
        let cases: Vec<(&str, Breaker)> = vec![
            ("empty project", |c| c.project_id.clear()),
            ("uppercase project", |c| {
                c.project_id = "Firebase-Test".to_owned();
            }),
            ("overlong project", |c| c.project_id = "a".repeat(64)),
            ("nil namespace", |c| c.subject_namespace = Uuid::nil()),
            ("no identities", |c| c.identities.clear()),
            ("empty uid", |c| c.identities[0].uid.clear()),
            ("overlong uid", |c| c.identities[0].uid = "u".repeat(129)),
            ("duplicate uid", |c| {
                let copy = c.identities[0].clone();
                c.identities.push(copy);
            }),
            ("email without @", |c| {
                c.identities[0].email = "a.example.com".to_owned();
            }),
            ("nil tenant", |c| c.identities[0].tenant_id = Uuid::nil()),
            ("no scopes", |c| c.identities[0].token_scopes.clear()),
            ("blank scope", |c| {
                c.identities[0].token_scopes = vec![" ".to_owned()];
            }),
        ];
        for (case, break_it) in cases {
            let mut config = valid();
            break_it(&mut config);
            assert!(config.validate().is_err(), "{case}");
        }
    }
}
