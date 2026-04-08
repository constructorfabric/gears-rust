use std::collections::HashMap;

use credstore_sdk::{OwnerId, SecretRef, SecretValue, SharingMode, TenantId};
use modkit_macros::domain_model;
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::StaticCredStorePluginConfig;

/// Pre-built secret entry for O(1) lookup.
#[domain_model]
pub struct SecretEntry {
    pub value: SecretValue,
    pub sharing: SharingMode,
    pub owner_id: OwnerId,
    pub owner_tenant_id: TenantId,
}

/// Static credstore service.
///
/// Secrets are stored in four maps based on their resolved `SharingMode`
/// and whether a `tenant_id` is present:
///
/// - **`Private`**: keyed by `(TenantId, OwnerId, SecretRef)` — accessible only
///   when both tenant and subject match.
/// - **`Tenant`**: keyed by `(TenantId, SecretRef)` — accessible by any subject
///   within the matching tenant.
/// - **`Shared`**: keyed by `(TenantId, SecretRef)` — tenant-scoped but
///   accessible by descendant tenants via hierarchical resolution in the
///   gateway. The plugin stores them per-tenant; walk-up is the gateway's job.
/// - **Global**: keyed by `SecretRef` only — no `tenant_id`; returned as
///   fallback for any caller. Not a `SharingMode` variant; it is an
///   operational shortcut specific to the static plugin.
///
/// Lookup order: **Private → Tenant → Shared → Global** (most specific first).
#[domain_model]
#[allow(clippy::struct_field_names)]
pub struct Service {
    private_secrets: HashMap<(TenantId, OwnerId, SecretRef), SecretEntry>,
    tenant_secrets: HashMap<(TenantId, SecretRef), SecretEntry>,
    shared_secrets: HashMap<(TenantId, SecretRef), SecretEntry>,
    global_secrets: HashMap<SecretRef, SecretEntry>,
}

impl Service {
    /// Create a service from plugin configuration.
    ///
    /// Validates each secret key via `SecretRef::new` and builds the lookup maps.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - any configured key fails `SecretRef` validation
    /// - duplicate keys within the same sharing scope
    /// - a global secret has an explicit sharing mode other than `Shared`
    /// - a secret without `owner_id` has an explicit `SharingMode::Private`
    /// - `tenant_id` or `owner_id` is an explicit nil UUID
    /// - `owner_id` is set without `tenant_id`
    pub fn from_config(cfg: &StaticCredStorePluginConfig) -> anyhow::Result<Self> {
        let mut private_secrets: HashMap<(TenantId, OwnerId, SecretRef), SecretEntry> =
            HashMap::new();
        let mut tenant_secrets: HashMap<(TenantId, SecretRef), SecretEntry> = HashMap::new();
        let mut shared_secrets: HashMap<(TenantId, SecretRef), SecretEntry> = HashMap::new();
        let mut global_secrets: HashMap<SecretRef, SecretEntry> = HashMap::new();

        for entry in &cfg.secrets {
            if entry.tenant_id == Some(Uuid::nil()) {
                anyhow::bail!("secret '{}': tenant_id must not be nil UUID", entry.key);
            }
            if entry.owner_id == Some(Uuid::nil()) {
                anyhow::bail!("secret '{}': owner_id must not be nil UUID", entry.key);
            }

            if entry.tenant_id.is_none() && entry.owner_id.is_some() {
                anyhow::bail!(
                    "secret '{}': owner_id cannot be set without tenant_id",
                    entry.key
                );
            }

            let sharing = entry.resolve_sharing();

            if entry.owner_id.is_some() && sharing != SharingMode::Private {
                anyhow::bail!(
                    "secret '{}': owner_id is only valid for private sharing mode, \
                     but resolved sharing is {sharing:?}",
                    entry.key
                );
            }

            if entry.owner_id.is_none() && sharing == SharingMode::Private {
                anyhow::bail!(
                    "secret '{}' with sharing mode 'private' requires an explicit owner_id",
                    entry.key
                );
            }

            let key = SecretRef::new(&entry.key)?;

            match (sharing, entry.tenant_id) {
                (SharingMode::Shared, None) => {
                    // Global secret: no tenant_id, accessible by any caller.
                    let secret_entry = SecretEntry {
                        value: SecretValue::from(entry.value.as_str()),
                        sharing,
                        owner_id: OwnerId::nil(),
                        owner_tenant_id: TenantId::nil(),
                    };
                    if global_secrets.contains_key(&key) {
                        anyhow::bail!("duplicate global secret key '{}'", entry.key);
                    }
                    global_secrets.insert(key, secret_entry);
                }
                (SharingMode::Shared, Some(raw_tenant_id)) => {
                    // Shared secret: tenant-scoped, visible to descendants
                    // via gateway hierarchical resolution.
                    let tenant_id = TenantId(raw_tenant_id);
                    let secret_entry = SecretEntry {
                        value: SecretValue::from(entry.value.as_str()),
                        sharing,
                        owner_id: OwnerId::nil(),
                        owner_tenant_id: tenant_id,
                    };
                    let map_key = (tenant_id, key);
                    if shared_secrets.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate shared secret key '{}' for tenant {}",
                            entry.key,
                            tenant_id
                        );
                    }
                    shared_secrets.insert(map_key, secret_entry);
                }
                (SharingMode::Tenant, _) => {
                    let tenant_id = TenantId(entry.tenant_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': tenant sharing mode requires tenant_id",
                            entry.key
                        )
                    })?);
                    let secret_entry = SecretEntry {
                        value: SecretValue::from(entry.value.as_str()),
                        sharing,
                        owner_id: OwnerId::nil(),
                        owner_tenant_id: tenant_id,
                    };
                    let map_key = (tenant_id, key);
                    if tenant_secrets.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate tenant secret key '{}' for tenant {}",
                            entry.key,
                            tenant_id
                        );
                    }
                    tenant_secrets.insert(map_key, secret_entry);
                }
                (SharingMode::Private, _) => {
                    let tenant_id = TenantId(entry.tenant_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': private sharing mode requires tenant_id",
                            entry.key
                        )
                    })?);
                    // owner_id is guaranteed Some by the validation above.
                    let owner_id = OwnerId(entry.owner_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': private sharing mode requires owner_id",
                            entry.key
                        )
                    })?);
                    let secret_entry = SecretEntry {
                        value: SecretValue::from(entry.value.as_str()),
                        sharing,
                        owner_id,
                        owner_tenant_id: tenant_id,
                    };
                    let map_key = (tenant_id, owner_id, key);
                    if private_secrets.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate private secret key '{}' for tenant {} owner {}",
                            entry.key,
                            tenant_id,
                            owner_id
                        );
                    }
                    private_secrets.insert(map_key, secret_entry);
                }
            }
        }

        Ok(Self {
            private_secrets,
            tenant_secrets,
            shared_secrets,
            global_secrets,
        })
    }

    /// Look up a secret using the caller's security context.
    ///
    /// Lookup order: **Private → Tenant → Shared → Global** (most specific first).
    #[must_use]
    pub fn get(&self, ctx: &SecurityContext, key: &SecretRef) -> Option<&SecretEntry> {
        let tenant_id = TenantId(ctx.subject_tenant_id());
        let subject_id = OwnerId(ctx.subject_id());

        self.private_secrets
            .get(&(tenant_id, subject_id, key.clone()))
            .or_else(|| self.tenant_secrets.get(&(tenant_id, key.clone())))
            .or_else(|| self.shared_secrets.get(&(tenant_id, key.clone())))
            .or_else(|| self.global_secrets.get(key))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::config::SecretConfig;
    use uuid::Uuid;

    fn tenant_a() -> Uuid {
        Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
    }

    fn tenant_b() -> Uuid {
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
    }

    fn owner_a() -> Uuid {
        Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()
    }

    fn owner_b() -> Uuid {
        Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap()
    }

    fn ctx(tenant_id: Uuid, subject_id: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(subject_id)
            .subject_tenant_id(tenant_id)
            .build()
            .unwrap()
    }

    /// Default config: `tenant_a` + `owner_a` → Private secret.
    fn cfg_with_single_secret() -> StaticCredStorePluginConfig {
        StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: Some(owner_a()),
                key: "openai_api_key".to_owned(),
                value: "sk-test-123".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        }
    }

    #[test]
    fn from_config_rejects_invalid_secret_ref() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: Some(owner_a()),
                key: "invalid:key".to_owned(),
                value: "value".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };

        let result = Service::from_config(&cfg);
        assert!(result.is_err());
    }

    // --- Private secret lookup ---

    #[test]
    fn private_secret_returned_for_matching_tenant_and_owner() {
        let service = Service::from_config(&cfg_with_single_secret()).unwrap();
        let key = SecretRef::new("openai_api_key").unwrap();

        let entry = service.get(&ctx(tenant_a(), owner_a()), &key).unwrap();
        assert_eq!(entry.value.as_bytes(), b"sk-test-123");
        assert_eq!(entry.owner_id, OwnerId(owner_a()));
        assert_eq!(entry.owner_tenant_id, TenantId(tenant_a()));
        assert_eq!(entry.sharing, SharingMode::Private);
    }

    #[test]
    fn private_secret_not_returned_for_different_owner() {
        let service = Service::from_config(&cfg_with_single_secret()).unwrap();
        let key = SecretRef::new("openai_api_key").unwrap();

        assert!(service.get(&ctx(tenant_a(), owner_b()), &key).is_none());
    }

    #[test]
    fn private_secret_not_returned_for_different_tenant() {
        let service = Service::from_config(&cfg_with_single_secret()).unwrap();
        let key = SecretRef::new("openai_api_key").unwrap();

        assert!(service.get(&ctx(tenant_b(), owner_a()), &key).is_none());
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let service = Service::from_config(&cfg_with_single_secret()).unwrap();
        let key = SecretRef::new("missing").unwrap();

        assert!(service.get(&ctx(tenant_a(), owner_a()), &key).is_none());
    }

    #[test]
    fn from_config_with_empty_secrets_returns_none() {
        let cfg = StaticCredStorePluginConfig::default();
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("any-key").unwrap();
        assert!(service.get(&ctx(tenant_a(), owner_a()), &key).is_none());
    }

    // --- Tenant secret lookup ---

    #[test]
    fn tenant_secret_returned_for_any_subject_in_same_tenant() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: None,
                key: "team_key".to_owned(),
                value: "team-val".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("team_key").unwrap();

        let e1 = service.get(&ctx(tenant_a(), owner_a()), &key).unwrap();
        assert_eq!(e1.value.as_bytes(), b"team-val");
        assert_eq!(e1.sharing, SharingMode::Tenant);

        let e2 = service.get(&ctx(tenant_a(), owner_b()), &key).unwrap();
        assert_eq!(e2.value.as_bytes(), b"team-val");

        assert!(service.get(&ctx(tenant_b(), owner_a()), &key).is_none());
    }

    // --- Global secret lookup ---

    #[test]
    fn global_secret_returned_for_any_tenant_and_subject() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: None,
                owner_id: None,
                key: "global_key".to_owned(),
                value: "global-val".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("global_key").unwrap();

        let e1 = service.get(&ctx(tenant_a(), owner_a()), &key).unwrap();
        assert_eq!(e1.value.as_bytes(), b"global-val");
        assert_eq!(e1.sharing, SharingMode::Shared);

        let e2 = service.get(&ctx(tenant_b(), owner_b()), &key).unwrap();
        assert_eq!(e2.value.as_bytes(), b"global-val");
    }

    // --- Shared (tenant-scoped) secret lookup ---

    #[test]
    fn shared_secret_returned_only_for_owning_tenant() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: None,
                key: "shared_key".to_owned(),
                value: "shared-val".to_owned(),
                sharing: Some(SharingMode::Shared),
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("shared_key").unwrap();

        // Same tenant — accessible
        let e = service.get(&ctx(tenant_a(), owner_a()), &key).unwrap();
        assert_eq!(e.value.as_bytes(), b"shared-val");
        assert_eq!(e.sharing, SharingMode::Shared);
        assert_eq!(e.owner_tenant_id, TenantId(tenant_a()));

        // Different tenant — not accessible at plugin level
        // (gateway walk-up would call the plugin with parent tenant_id)
        assert!(service.get(&ctx(tenant_b(), owner_a()), &key).is_none());
    }

    // --- Lookup precedence: Private > Tenant > Shared > Global ---

    #[test]
    fn private_takes_precedence_over_tenant_shared_and_global() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: None,
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "global-val".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "shared-val".to_owned(),
                    sharing: Some(SharingMode::Shared),
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "tenant-val".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: Some(owner_a()),
                    key: "k".to_owned(),
                    value: "private-val".to_owned(),
                    sharing: None,
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("k").unwrap();

        // owner_a in tenant_a → Private
        let e = service.get(&ctx(tenant_a(), owner_a()), &key).unwrap();
        assert_eq!(e.value.as_bytes(), b"private-val");
        assert_eq!(e.sharing, SharingMode::Private);

        // owner_b in tenant_a → Tenant (no private match)
        let e = service.get(&ctx(tenant_a(), owner_b()), &key).unwrap();
        assert_eq!(e.value.as_bytes(), b"tenant-val");
        assert_eq!(e.sharing, SharingMode::Tenant);

        // tenant_b → Global (no private, tenant, or shared match)
        let e = service.get(&ctx(tenant_b(), owner_a()), &key).unwrap();
        assert_eq!(e.value.as_bytes(), b"global-val");
        assert_eq!(e.sharing, SharingMode::Shared);
    }

    #[test]
    fn tenant_takes_precedence_over_shared_and_global() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: None,
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "global-val".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "shared-val".to_owned(),
                    sharing: Some(SharingMode::Shared),
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "tenant-val".to_owned(),
                    sharing: None,
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("k").unwrap();

        let e = service.get(&ctx(tenant_a(), owner_a()), &key).unwrap();
        assert_eq!(e.value.as_bytes(), b"tenant-val");

        let e = service.get(&ctx(tenant_b(), owner_a()), &key).unwrap();
        assert_eq!(e.value.as_bytes(), b"global-val");
    }

    #[test]
    fn shared_takes_precedence_over_global() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: None,
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "global-val".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "shared-val".to_owned(),
                    sharing: Some(SharingMode::Shared),
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("k").unwrap();

        // tenant_a has a shared secret → takes precedence over global
        let e = service.get(&ctx(tenant_a(), owner_a()), &key).unwrap();
        assert_eq!(e.value.as_bytes(), b"shared-val");
        assert_eq!(e.sharing, SharingMode::Shared);

        // tenant_b has no shared secret → falls through to global
        let e = service.get(&ctx(tenant_b(), owner_a()), &key).unwrap();
        assert_eq!(e.value.as_bytes(), b"global-val");
    }

    // --- Duplicate key validation ---

    #[test]
    fn from_config_rejects_duplicate_private_key() {
        let secret = SecretConfig {
            tenant_id: Some(tenant_a()),
            owner_id: Some(owner_a()),
            key: "dup".to_owned(),
            value: "v1".to_owned(),
            sharing: None,
        };
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                secret.clone(),
                SecretConfig {
                    value: "v2".to_owned(),
                    ..secret
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };

        match Service::from_config(&cfg) {
            Ok(_) => panic!("expected error for duplicate private key"),
            Err(e) => {
                let err = e.to_string();
                assert!(err.contains("duplicate"), "expected 'duplicate' in: {err}");
                assert!(err.contains("dup"), "expected key name in: {err}");
            }
        }
    }

    #[test]
    fn from_config_rejects_duplicate_tenant_key() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "dup".to_owned(),
                    value: "v1".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "dup".to_owned(),
                    value: "v2".to_owned(),
                    sharing: None,
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };

        match Service::from_config(&cfg) {
            Ok(_) => panic!("expected error for duplicate tenant key"),
            Err(e) => {
                let err = e.to_string();
                assert!(err.contains("duplicate"), "expected 'duplicate' in: {err}");
            }
        }
    }

    #[test]
    fn from_config_rejects_duplicate_global_key() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: None,
                    owner_id: None,
                    key: "dup".to_owned(),
                    value: "v1".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: None,
                    owner_id: None,
                    key: "dup".to_owned(),
                    value: "v2".to_owned(),
                    sharing: None,
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };

        match Service::from_config(&cfg) {
            Ok(_) => panic!("expected error for duplicate global key"),
            Err(e) => {
                let err = e.to_string();
                assert!(err.contains("duplicate"), "expected 'duplicate' in: {err}");
            }
        }
    }

    #[test]
    fn from_config_rejects_duplicate_shared_key() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "dup".to_owned(),
                    value: "v1".to_owned(),
                    sharing: Some(SharingMode::Shared),
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "dup".to_owned(),
                    value: "v2".to_owned(),
                    sharing: Some(SharingMode::Shared),
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };

        match Service::from_config(&cfg) {
            Ok(_) => panic!("expected error for duplicate shared key"),
            Err(e) => {
                let err = e.to_string();
                assert!(err.contains("duplicate"), "expected 'duplicate' in: {err}");
            }
        }
    }

    // --- Config validation ---

    #[test]
    fn from_config_rejects_non_shared_global_secret() {
        for mode in [SharingMode::Private, SharingMode::Tenant] {
            let cfg = StaticCredStorePluginConfig {
                secrets: vec![SecretConfig {
                    tenant_id: None,
                    owner_id: None,
                    key: "global_key".to_owned(),
                    value: "val".to_owned(),
                    sharing: Some(mode),
                }],
                ..StaticCredStorePluginConfig::default()
            };

            assert!(
                Service::from_config(&cfg).is_err(),
                "expected error for global secret with {mode:?} sharing"
            );
        }
    }

    #[test]
    fn from_config_rejects_private_without_owner_id() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: None,
                key: "private_key".to_owned(),
                value: "val".to_owned(),
                sharing: Some(SharingMode::Private),
            }],
            ..StaticCredStorePluginConfig::default()
        };

        match Service::from_config(&cfg) {
            Ok(_) => panic!("expected error for private without owner_id"),
            Err(e) => {
                let err = e.to_string();
                assert!(err.contains("requires an explicit owner_id"), "got: {err}");
            }
        }
    }

    #[test]
    fn from_config_rejects_owner_id_without_tenant_id() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: None,
                owner_id: Some(owner_a()),
                key: "bad_key".to_owned(),
                value: "val".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };

        match Service::from_config(&cfg) {
            Ok(_) => panic!("expected error for owner_id without tenant_id"),
            Err(e) => {
                let err = e.to_string();
                assert!(
                    err.contains("owner_id cannot be set without tenant_id"),
                    "got: {err}"
                );
            }
        }
    }

    #[test]
    fn from_config_rejects_owner_id_for_non_private() {
        for mode in [SharingMode::Tenant, SharingMode::Shared] {
            let cfg = StaticCredStorePluginConfig {
                secrets: vec![SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: Some(owner_a()),
                    key: "bad_key".to_owned(),
                    value: "val".to_owned(),
                    sharing: Some(mode),
                }],
                ..StaticCredStorePluginConfig::default()
            };

            match Service::from_config(&cfg) {
                Ok(_) => panic!("expected error for owner_id with {mode:?} sharing"),
                Err(e) => {
                    let err = e.to_string();
                    assert!(
                        err.contains("owner_id is only valid for private sharing mode"),
                        "got: {err}"
                    );
                }
            }
        }
    }

    #[test]
    fn from_config_accepts_shared_with_tenant_id() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: None,
                key: "k".to_owned(),
                value: "v".to_owned(),
                sharing: Some(SharingMode::Shared),
            }],
            ..StaticCredStorePluginConfig::default()
        };

        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("k").unwrap();
        let e = service.get(&ctx(tenant_a(), owner_a()), &key).unwrap();
        assert_eq!(e.sharing, SharingMode::Shared);
        assert_eq!(e.owner_tenant_id, TenantId(tenant_a()));
    }

    #[test]
    fn from_config_rejects_nil_tenant_id() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(Uuid::nil()),
                owner_id: Some(owner_a()),
                key: "k".to_owned(),
                value: "v".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };

        match Service::from_config(&cfg) {
            Ok(_) => panic!("expected error for nil tenant_id"),
            Err(e) => {
                let err = e.to_string();
                assert!(err.contains("tenant_id must not be nil UUID"), "got: {err}");
            }
        }
    }

    #[test]
    fn from_config_rejects_nil_owner_id() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: Some(Uuid::nil()),
                key: "k".to_owned(),
                value: "v".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };

        match Service::from_config(&cfg) {
            Ok(_) => panic!("expected error for nil owner_id"),
            Err(e) => {
                let err = e.to_string();
                assert!(err.contains("owner_id must not be nil UUID"), "got: {err}");
            }
        }
    }

    // --- Sharing mode defaults ---

    #[test]
    fn default_sharing_is_shared_for_global() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: None,
                owner_id: None,
                key: "g".to_owned(),
                value: "v".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("g").unwrap();
        assert_eq!(
            service
                .get(&ctx(tenant_a(), owner_a()), &key)
                .unwrap()
                .sharing,
            SharingMode::Shared
        );
    }

    #[test]
    fn default_sharing_is_tenant_for_scoped_without_owner() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: None,
                key: "t".to_owned(),
                value: "v".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("t").unwrap();
        assert_eq!(
            service
                .get(&ctx(tenant_a(), owner_a()), &key)
                .unwrap()
                .sharing,
            SharingMode::Tenant
        );
    }

    #[test]
    fn default_sharing_is_private_for_scoped_with_owner() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: Some(owner_a()),
                key: "p".to_owned(),
                value: "v".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("p").unwrap();
        assert_eq!(
            service
                .get(&ctx(tenant_a(), owner_a()), &key)
                .unwrap()
                .sharing,
            SharingMode::Private
        );
    }

    #[test]
    fn explicit_sharing_overrides_default() {
        // tenant_id + no owner_id defaults to Tenant; override to Shared.
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: None,
                key: "k".to_owned(),
                value: "v".to_owned(),
                sharing: Some(SharingMode::Shared),
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("k").unwrap();
        assert_eq!(
            service
                .get(&ctx(tenant_a(), owner_a()), &key)
                .unwrap()
                .sharing,
            SharingMode::Shared
        );
    }

    // --- Same key in different scopes ---

    #[test]
    fn allows_same_key_in_different_tenants() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "api_key".to_owned(),
                    value: "val-a".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_b()),
                    owner_id: None,
                    key: "api_key".to_owned(),
                    value: "val-b".to_owned(),
                    sharing: None,
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let key = SecretRef::new("api_key").unwrap();

        assert_eq!(
            service
                .get(&ctx(tenant_a(), owner_a()), &key)
                .unwrap()
                .value
                .as_bytes(),
            b"val-a"
        );
        assert_eq!(
            service
                .get(&ctx(tenant_b(), owner_a()), &key)
                .unwrap()
                .value
                .as_bytes(),
            b"val-b"
        );
    }

    #[test]
    fn same_key_across_all_four_scopes() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: None,
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "global".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "shared".to_owned(),
                    sharing: Some(SharingMode::Shared),
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "tenant".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: Some(owner_a()),
                    key: "k".to_owned(),
                    value: "private".to_owned(),
                    sharing: None,
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };

        assert!(Service::from_config(&cfg).is_ok());
    }
}
