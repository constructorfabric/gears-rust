use super::*;
use crate::domain::bootstrap::BootstrapConfig;
use crate::domain::root_type::RootTypeConfig;
use toolkit_gts::gts_id;
use uuid::Uuid;

const ROOT_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~");
const OTHER_ROOT_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.alternate.v1~");

fn root_type(idp_provisioning: bool) -> RootTypeConfig {
    RootTypeConfig {
        gts_id: gts::GtsTypeId::new(ROOT_TYPE),
        idp_provisioning,
    }
}

fn bootstrap() -> BootstrapConfig {
    BootstrapConfig {
        root_id: Uuid::from_u128(1),
        root_name: "root".to_owned(),
        ..BootstrapConfig::default()
    }
}

#[test]
fn default_validates_clean() {
    AccountManagementConfig::default()
        .validate()
        .expect("default config must always validate; it is the production fallback");
}

#[test]
fn independent_root_type_resolves_without_bootstrap() {
    let cfg = AccountManagementConfig {
        root_tenant_type: Some(root_type(false)),
        ..AccountManagementConfig::default()
    };
    assert_eq!(
        cfg.resolved_root_type().expect("valid"),
        cfg.root_tenant_type
    );
}

#[test]
fn deprecated_bootstrap_root_type_is_migrated() {
    let cfg = AccountManagementConfig {
        bootstrap: Some(BootstrapConfig {
            root_tenant_type: Some(gts::GtsTypeId::new(ROOT_TYPE)),
            root_tenant_type_idp_provisioning: Some(true),
            ..bootstrap()
        }),
        ..AccountManagementConfig::default()
    };
    assert_eq!(
        cfg.resolved_root_type().expect("legacy config"),
        Some(root_type(true))
    );
}

#[test]
fn modern_and_legacy_root_type_id_conflict_is_fatal() {
    let cfg = AccountManagementConfig {
        root_tenant_type: Some(root_type(false)),
        bootstrap: Some(BootstrapConfig {
            root_tenant_type: Some(gts::GtsTypeId::new(OTHER_ROOT_TYPE)),
            root_tenant_type_idp_provisioning: Some(false),
            ..bootstrap()
        }),
        ..AccountManagementConfig::default()
    };
    let error = cfg
        .validate()
        .expect_err("GTS ID conflict must fail globally");
    assert_eq!(
        error,
        format!(
            "account-management configuration is invalid: root_tenant_type.gts_id `{ROOT_TYPE}` conflicts with deprecated bootstrap.root_tenant_type `{OTHER_ROOT_TYPE}`"
        )
    );
}

#[test]
fn modern_and_legacy_idp_provisioning_conflict_is_fatal() {
    let cfg = AccountManagementConfig {
        root_tenant_type: Some(root_type(true)),
        bootstrap: Some(BootstrapConfig {
            root_tenant_type: Some(gts::GtsTypeId::new(ROOT_TYPE)),
            root_tenant_type_idp_provisioning: Some(false),
            ..bootstrap()
        }),
        ..AccountManagementConfig::default()
    };
    let error = cfg
        .validate()
        .expect_err("IdP provisioning conflict must fail globally");
    assert_eq!(
        error,
        "account-management configuration is invalid: root_tenant_type.idp_provisioning=true conflicts with deprecated bootstrap.root_tenant_type_idp_provisioning=false"
    );
}

#[test]
fn bootstrap_without_root_type_is_fatal_even_when_nonstrict() {
    let cfg = AccountManagementConfig {
        bootstrap: Some(bootstrap()),
        ..AccountManagementConfig::default()
    };
    let error = cfg.validate().expect_err("schema contract is mandatory");
    assert!(
        error.contains("bootstrap requires root_tenant_type.gts_id"),
        "{error}"
    );
}

#[test]
fn tr_plugin_vendor_default_matches_resolver_default() {
    // Pinned: AM's `tr_plugin.vendor` default MUST equal
    // `TenantResolverConfig::default().vendor` so a deploy that
    // flips `enabled = true` without touching either vendor knob
    // wires through cleanly. The literal is duplicated rather than
    // imported because AM does not depend on `tenant-resolver` at
    // the runtime-client level (the dep is init-order only) — if
    // the resolver default changes, both this test and the AM
    // default need to be re-aligned in lockstep.
    let cfg = AccountManagementConfig::default();
    assert_eq!(
        cfg.tr_plugin.vendor, "constructorfabric",
        "AM tr_plugin.vendor default must match TenantResolverConfig::default().vendor"
    );
}

#[test]
fn rejects_empty_tr_plugin_vendor() {
    // Empty vendor would let `enabled = true` register an instance
    // that the gateway can never select (`choose_plugin_instance`
    // filters by exact-match vendor; empty matches nothing in any
    // realistic deploy). Fail at validate() rather than producing a
    // silently-orphaned registration.
    let cfg = AccountManagementConfig {
        tr_plugin: TrPluginConfig {
            vendor: String::new(),
            ..TrPluginConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("empty vendor must reject");
    assert!(err.contains("tr_plugin.vendor"), "{err}");
}

#[test]
fn tr_plugin_disabled_by_default() {
    // Pinned: while AM's `tr_plugin` is still in build-out, the
    // master switch defaults to `false` so a deploy that
    // incidentally pulls AM into its binary does NOT register the
    // plugin in types-registry / `ClientHub` at all. This is the
    // primary defense against silent traffic capture in an AM-only
    // binary, where AM would otherwise be the sole candidate for
    // its vendor and `choose_plugin_instance` would pick it
    // regardless of priority. The switch-over commit that ships
    // the feature-complete plugin flips this default to `true` and
    // inverts this test.
    let cfg = AccountManagementConfig::default();
    assert!(
        !cfg.tr_plugin.enabled,
        "AM tr_plugin must be disabled by default while the plugin is in build-out"
    );
}

#[test]
fn tr_plugin_priority_default_loses_to_in_tree_alternatives() {
    // Pinned: even when `enabled = true`, AM's default priority
    // MUST be strictly **higher** than every in-tree alternative
    // (`rg-tr-plugin` = 50, `static-tr-plugin` = 100) so a deploy
    // that flips `enabled = true` without picking a priority still
    // loses to coexisting plugins under the configured vendor.
    // Secondary defense to `tr_plugin.enabled = false`; the
    // primary defense (registration skipped entirely) lives in
    // `tr_plugin_disabled_by_default`.
    let cfg = AccountManagementConfig::default();
    assert!(
        cfg.tr_plugin.priority > 100,
        "AM tr_plugin default priority must lose to static-tr-plugin (100); got {}",
        cfg.tr_plugin.priority
    );
}

#[test]
fn idp_required_defaults_to_false() {
    // Pinned: deployments inheriting the default keep the existing
    // NoopIdpProvider-fallback behaviour. Production deployments
    // that want fail-closed init must opt in explicitly.
    let cfg = AccountManagementConfig::default();
    assert!(
        !cfg.idp.required,
        "idp.required must default to false; production deployments opt in explicitly"
    );
}

#[test]
fn rejects_zero_retention_tick() {
    let cfg = AccountManagementConfig {
        retention: RetentionConfig {
            tick_secs: 0,
            ..RetentionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("zero tick must reject");
    assert!(err.contains("retention.tick_secs"), "{err}");
}

#[test]
fn rejects_zero_reaper_tick() {
    let cfg = AccountManagementConfig {
        reaper: ReaperConfig {
            tick_secs: 0,
            ..ReaperConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("zero tick must reject");
    assert!(err.contains("reaper.tick_secs"), "{err}");
}

#[test]
fn rejects_zero_provisioning_timeout() {
    // Zero staleness threshold would make every fresh `Provisioning`
    // row instantly reaper-eligible — the reaper would compensate
    // creates that haven't even reached the IdP step yet.
    let cfg = AccountManagementConfig {
        reaper: ReaperConfig {
            provisioning_timeout_secs: 0,
            ..ReaperConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("zero provisioning timeout must reject");
    assert!(err.contains("reaper.provisioning_timeout_secs"), "{err}");
}

#[test]
fn rejects_zero_hard_delete_batch_size() {
    let cfg = AccountManagementConfig {
        retention: RetentionConfig {
            hard_delete_batch_size: 0,
            ..RetentionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("zero batch must reject");
    assert!(err.contains("retention.hard_delete_batch_size"), "{err}");
}

#[test]
fn rejects_zero_reaper_batch_size() {
    let cfg = AccountManagementConfig {
        reaper: ReaperConfig {
            batch_size: 0,
            ..ReaperConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("zero batch must reject");
    assert!(err.contains("reaper.batch_size"), "{err}");
}

#[test]
fn rejects_zero_hard_delete_concurrency() {
    let cfg = AccountManagementConfig {
        retention: RetentionConfig {
            hard_delete_concurrency: 0,
            ..RetentionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("zero concurrency must reject");
    assert!(err.contains("retention.hard_delete_concurrency"), "{err}");
}

#[test]
fn rejects_zero_deprovision_concurrency() {
    let cfg = AccountManagementConfig {
        reaper: ReaperConfig {
            deprovision_concurrency: 0,
            ..ReaperConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("zero concurrency must reject");
    assert!(err.contains("reaper.deprovision_concurrency"), "{err}");
}

#[test]
fn rejects_zero_max_top() {
    let cfg = AccountManagementConfig {
        listing: ListingConfig { max_top: 0 },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("zero top must reject");
    assert!(err.contains("listing.max_top"), "{err}");
}

#[test]
fn rejects_excessive_depth_threshold() {
    let cfg = AccountManagementConfig {
        hierarchy: HierarchyConfig {
            depth_threshold: AccountManagementConfig::MAX_DEPTH_THRESHOLD + 1,
            ..HierarchyConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("depth_threshold > MAX must reject");
    assert!(err.contains("hierarchy.depth_threshold"), "{err}");
}

// ---- ConversionConfig validation paths ----------------------------
//
// Pin every `ConversionConfig::validate()` rejection path: TTL /
// retention / cleanup / batch out-of-range, plus the cross-check
// `resolved_retention_secs <= retention.default_window_secs`. The
// happy-path defaults are already covered by `default_validates_clean`.

#[test]
fn rejects_conversion_approval_ttl_below_floor() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            approval_ttl_secs: ConversionConfig::MIN_APPROVAL_TTL_SECS - 1,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("TTL below floor must reject");
    assert!(err.contains("conversion.approval_ttl_secs"), "{err}");
}

#[test]
fn rejects_conversion_approval_ttl_above_ceiling() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            approval_ttl_secs: ConversionConfig::MAX_APPROVAL_TTL_SECS + 1,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("TTL above ceiling must reject");
    assert!(err.contains("conversion.approval_ttl_secs"), "{err}");
}

#[test]
fn rejects_conversion_resolved_retention_below_floor() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            resolved_retention_secs: ConversionConfig::MIN_RESOLVED_RETENTION_SECS - 1,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("retention below floor must reject");
    assert!(err.contains("conversion.resolved_retention_secs"), "{err}");
}

#[test]
fn rejects_conversion_resolved_retention_above_ceiling() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            resolved_retention_secs: ConversionConfig::MAX_RESOLVED_RETENTION_SECS + 1,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("retention above MAX_RESOLVED_RETENTION_SECS must reject");
    assert!(err.contains("conversion.resolved_retention_secs"), "{err}");
}

#[test]
fn rejects_conversion_cleanup_interval_below_floor() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            cleanup_interval_secs: ConversionConfig::MIN_CLEANUP_INTERVAL_SECS - 1,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("cleanup_interval below floor must reject");
    assert!(err.contains("conversion.cleanup_interval_secs"), "{err}");
}

#[test]
fn rejects_conversion_cleanup_interval_above_ceiling() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            cleanup_interval_secs: ConversionConfig::MAX_CLEANUP_INTERVAL_SECS + 1,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("cleanup_interval above ceiling must reject");
    assert!(err.contains("conversion.cleanup_interval_secs"), "{err}");
}

#[test]
fn rejects_conversion_zero_expire_batch_size() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            expire_batch_size: 0,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("zero batch must reject");
    assert!(err.contains("conversion.expire_batch_size"), "{err}");
}

#[test]
fn rejects_conversion_expire_batch_size_above_ceiling() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            expire_batch_size: ConversionConfig::MAX_BATCH_SIZE + 1,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("expire batch above MAX_BATCH_SIZE must reject");
    assert!(err.contains("conversion.expire_batch_size"), "{err}");
}

#[test]
fn rejects_conversion_oversized_retention_batch() {
    let cfg = AccountManagementConfig {
        conversion: ConversionConfig {
            retention_batch_size: ConversionConfig::MAX_BATCH_SIZE + 1,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("batch above MAX_BATCH_SIZE must reject (PG IN(...) ceiling)");
    assert!(err.contains("conversion.retention_batch_size"), "{err}");
}

#[test]
fn rejects_resolved_retention_exceeding_tenant_default_window() {
    let cfg = AccountManagementConfig {
        retention: RetentionConfig {
            default_window_secs: 24 * 60 * 60, // 1 day
            ..RetentionConfig::default()
        },
        conversion: ConversionConfig {
            // Default 30d > deliberately-shortened 1-day tenant
            // retention. Cross-check MUST fire because resolved
            // conversion history would outlive the tenant cascade.
            resolved_retention_secs: 30 * 24 * 60 * 60,
            ..ConversionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("resolved_retention > tenant default_window must reject");
    assert!(err.contains("conversion.resolved_retention_secs"), "{err}");
    assert!(
        err.contains("retention.default_window_secs"),
        "diagnostic must name the cross-checked field; got: {err}"
    );
}

#[test]
fn allows_default_resolved_retention_when_tenant_retention_disabled() {
    // `retention.default_window_secs = 0` means "tenants are
    // immediately hard-delete-eligible", and the FK
    // `conversion_requests.tenant_id REFERENCES tenants(id) ON DELETE
    // CASCADE` reclaims the resolved-conversion rows alongside the
    // tenant. The cross-check therefore only fires when tenant
    // retention is positive — disabled tenant retention does not
    // require disabling conversion retention as well.
    let cfg = AccountManagementConfig {
        retention: RetentionConfig {
            default_window_secs: 0,
            ..RetentionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    cfg.validate()
        .expect("default conversion config must validate when tenant retention is disabled");
}

#[test]
fn aggregates_multiple_failures_in_one_message() {
    let cfg = AccountManagementConfig {
        retention: RetentionConfig {
            tick_secs: 0,
            hard_delete_batch_size: 0,
            ..RetentionConfig::default()
        },
        reaper: ReaperConfig {
            tick_secs: 0,
            ..ReaperConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let err = cfg.validate().expect_err("triple-bad must reject");
    assert!(err.contains("retention.tick_secs"), "{err}");
    assert!(err.contains("reaper.tick_secs"), "{err}");
    assert!(err.contains("retention.hard_delete_batch_size"), "{err}");
}
