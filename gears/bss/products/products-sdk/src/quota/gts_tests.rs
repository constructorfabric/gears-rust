//! The quota GTS schemas as types-registry receives them, and the package's example records as
//! the wire types read them (P-D-267).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use serde_json::{Value, json};

fn schema(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[test]
fn unit_and_policy_schemas_are_final_and_metric_and_capability_are_not() {
    for (text, is_final) in [
        (QuotaUnitV1::gts_schema_with_refs_as_string(), true),
        (
            QuotaAccountingPolicyV1::gts_schema_with_refs_as_string(),
            true,
        ),
        (QuotaScopePolicyV1::gts_schema_with_refs_as_string(), true),
        (QuotaMetricV1::gts_schema_with_refs_as_string(), false),
        (QuotaCapabilityV1::gts_schema_with_refs_as_string(), false),
    ] {
        let s = schema(&text);
        assert_eq!(s.get("x-gts-final") == Some(&json!(true)), is_final, "{s}");
        assert_ne!(s.get("x-gts-abstract"), Some(&json!(true)), "{s}");
    }
}

#[test]
fn every_reference_is_narrowed_to_its_family() {
    let cap = schema(&QuotaCapabilityV1::gts_schema_with_refs_as_string());
    let p = &cap["properties"];
    assert_eq!(p["metric_ref"]["x-gts-instance-ref"], json!(METRIC_TYPE));
    assert_eq!(p["unit_ref"]["x-gts-instance-ref"], json!(UNIT_TYPE));
    assert_eq!(
        p["accounting_policy_ref"]["x-gts-instance-ref"],
        json!(ACCOUNTING_POLICY_TYPE)
    );
    assert_eq!(
        p["scope_policy_ref"]["x-gts-instance-ref"],
        json!(SCOPE_POLICY_TYPE)
    );
    assert_eq!(p["type_id"]["x-gts-type-ref"], json!(CAPABILITY_TYPE));
    assert!(p["subject_type_ref"].get("x-gts-type-ref").is_some());
    let metric = schema(&QuotaMetricV1::gts_schema_with_refs_as_string());
    assert_eq!(
        metric["properties"]["canonical_unit_ref"]["x-gts-instance-ref"],
        json!(UNIT_TYPE)
    );
    assert_eq!(
        metric["properties"]["quota_kind"]["enum"],
        json!(["allocation"])
    );
}

#[test]
fn the_ids_are_major_only_v1_and_the_plugin_derives_from_the_toolkit_base() {
    assert_eq!(UNIT_TYPE, "gts.cf.bss.products.quota_unit.v1~");
    assert_eq!(METRIC_TYPE, "gts.cf.bss.products.quota_metric.v1~");
    assert_eq!(
        ACCOUNTING_POLICY_TYPE,
        "gts.cf.bss.products.quota_accounting_policy.v1~"
    );
    assert_eq!(
        SCOPE_POLICY_TYPE,
        "gts.cf.bss.products.quota_scope_policy.v1~"
    );
    assert_eq!(CAPABILITY_TYPE, "gts.cf.bss.products.quota_capability.v1~");
    assert_eq!(
        DISCOVERY_PLUGIN_TYPE,
        "gts.cf.toolkit.plugins.plugin.v1~cf.bss.products.quota_discovery.v1~"
    );
    assert_eq!(
        QuotaDiscoveryPluginSpecV1::gts_type_id().as_ref(),
        DISCOVERY_PLUGIN_TYPE
    );
}

#[test]
fn the_package_example_record_parses_and_an_unknown_member_does_not() {
    let example: Value = serde_json::from_str(include_str!(
        "../../../../docs/quotas/schemas/quota-capability-discovery.example.json"
    ))
    .unwrap();
    let item = example["items"][0].clone();
    let record: QuotaCapabilityV1 = serde_json::from_value(item.clone()).unwrap();
    assert_eq!(record.limits.max.as_deref(), Some("1024"));
    assert!(record.operations.recover_operation);
    let mut extra = item;
    extra["paid"] = json!(true);
    assert!(serde_json::from_value::<QuotaCapabilityV1>(extra).is_err());
}

#[test]
fn an_absent_operation_flag_is_refused() {
    let ops = json!({"apply": true, "read_application": true, "recover_operation": true,
                     "restrict": true, "revoke": true});
    assert!(serde_json::from_value::<QuotaOperations>(ops).is_err());
}

#[test]
fn the_package_example_instances_parse() {
    let units: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../docs/quotas/schemas/quota-metric-unit.instances.json"
    ))
    .unwrap();
    let unit: QuotaUnitV1 = serde_json::from_value(units[0].clone()).unwrap();
    assert_eq!(unit.canonical_unit_ref, unit.id.as_ref());
    let metric: QuotaMetricV1 = serde_json::from_value(units[1].clone()).unwrap();
    assert_eq!(metric.display_name["en"], "Virtual CPUs");
    let policies: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../docs/quotas/schemas/quota-policies.instances.json"
    ))
    .unwrap();
    let scope: QuotaScopePolicyV1 = serde_json::from_value(policies[1].clone()).unwrap();
    assert_eq!(
        scope.required_dimensions,
        vec![ScopeDimension::Region, ScopeDimension::ResourcePool]
    );
}

fn example_item() -> Value {
    let example: Value = serde_json::from_str(include_str!(
        "../../../../docs/quotas/schemas/quota-capability-discovery.example.json"
    ))
    .unwrap();
    example["items"][0].clone()
}

fn with_member(path: &[&str], value: Value) -> Value {
    let mut item = example_item();
    let mut slot = &mut item;
    for key in path {
        slot = if let Ok(index) = key.parse::<usize>() {
            &mut slot[index]
        } else {
            &mut slot[*key]
        };
    }
    *slot = value;
    item
}

#[test]
fn each_schema_type_id_is_its_constant() {
    assert_eq!(QuotaUnitV1::gts_type_id().as_ref(), UNIT_TYPE);
    assert_eq!(QuotaMetricV1::gts_type_id().as_ref(), METRIC_TYPE);
    assert_eq!(
        QuotaAccountingPolicyV1::gts_type_id().as_ref(),
        ACCOUNTING_POLICY_TYPE
    );
    assert_eq!(
        QuotaScopePolicyV1::gts_type_id().as_ref(),
        SCOPE_POLICY_TYPE
    );
    assert_eq!(QuotaCapabilityV1::gts_type_id().as_ref(), CAPABILITY_TYPE);
}

#[test]
fn a_malformed_type_id_is_refused_and_other_versions_still_parse() {
    for bad in [
        "capability",
        "gts.cf.bss.products.quota_capability.v1~cf.compute.quota_adapter.cap.v1",
        "gts.cf.bss.products.*",
    ] {
        let item = with_member(&["type_id"], json!(bad));
        assert!(
            serde_json::from_value::<QuotaCapabilityV1>(item).is_err(),
            "{bad}"
        );
    }
    // Another version of the capability schema is still an identity: the record judgement, not
    // the parse, refuses it.
    for other in [
        "gts.cf.bss.products.quota_capability.v1.1~",
        "gts.cf.bss.products.quota_capability.v2~",
    ] {
        let item = with_member(&["type_id"], json!(other));
        let record: QuotaCapabilityV1 = serde_json::from_value(item).unwrap();
        assert_eq!(record.gts_type.as_ref(), other);
    }
}

#[test]
fn every_reference_is_a_typed_gts_id_at_the_parse() {
    let instance_refs = [
        "metric_ref",
        "unit_ref",
        "accounting_policy_ref",
        "scope_policy_ref",
    ];
    for field in instance_refs {
        for bad in [
            "vcpu",
            "gts.cf.bss.products.quota_metric.v1~",
            "gts.cf.bss.products.quota_metric.v1~cf.compute.*",
        ] {
            let item = with_member(&[field], json!(bad));
            assert!(
                serde_json::from_value::<QuotaCapabilityV1>(item).is_err(),
                "{field} = {bad}"
            );
        }
    }
    for path in [&["subject_type_ref"][..], &["resource_type_refs", "0"][..]] {
        for bad in [
            "tenant",
            "gts.cf.core.compute.tenant.v1~cf.compute.quota_adapter.t.v1",
            "gts.*",
        ] {
            let item = with_member(path, json!(bad));
            assert!(
                serde_json::from_value::<QuotaCapabilityV1>(item).is_err(),
                "{path:?} = {bad}"
            );
        }
    }
    // A minor version is still an identity: the record judgement refuses it with its own code.
    let minor = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1.1";
    let record: QuotaCapabilityV1 =
        serde_json::from_value(with_member(&["unit_ref"], json!(minor))).unwrap();
    assert_eq!(AsRef::<str>::as_ref(&record.unit_ref), minor);
    // The wire and the schema keep plain strings.
    let cap = schema(&QuotaCapabilityV1::gts_schema_with_refs_as_string());
    for field in instance_refs.into_iter().chain(["subject_type_ref"]) {
        assert_eq!(cap["properties"][field]["type"], json!("string"), "{field}");
    }
}

#[test]
fn metric_and_capability_are_closed_at_the_top_level_and_open_in_properties() {
    for text in [
        QuotaMetricV1::gts_schema_with_refs_as_string(),
        QuotaCapabilityV1::gts_schema_with_refs_as_string(),
    ] {
        let s = schema(&text);
        assert_eq!(s["additionalProperties"], json!(false), "{s}");
        assert_eq!(
            s["properties"]["properties"]["additionalProperties"],
            json!(true),
            "{s}"
        );
    }
}

#[test]
fn the_unit_and_resource_type_references_are_narrowed() {
    let unit = schema(&QuotaUnitV1::gts_schema_with_refs_as_string());
    assert_eq!(
        unit["properties"]["canonical_unit_ref"]["x-gts-instance-ref"],
        json!(UNIT_TYPE)
    );
    let cap = schema(&QuotaCapabilityV1::gts_schema_with_refs_as_string());
    assert_eq!(
        cap["properties"]["resource_type_refs"]["items"]["x-gts-type-ref"],
        json!("gts.*")
    );
    assert_eq!(
        cap["properties"]["subject_type_ref"]["x-gts-type-ref"],
        json!("gts.*")
    );
}

#[test]
fn every_enum_property_is_a_flat_enum() {
    let metric = schema(&QuotaMetricV1::gts_schema_with_refs_as_string());
    let accounting = schema(&QuotaAccountingPolicyV1::gts_schema_with_refs_as_string());
    let scope = schema(&QuotaScopePolicyV1::gts_schema_with_refs_as_string());
    for (property, values) in [
        (&metric["properties"]["quota_kind"], json!(["allocation"])),
        (
            &accounting["properties"]["quantity_basis"],
            json!(["allocated_capacity"]),
        ),
        (&accounting["properties"]["aggregation"], json!(["sum"])),
        (
            &scope["properties"]["subject_selector"],
            json!(["resource_owner"]),
        ),
        (
            &scope["properties"]["aggregation"],
            json!(["single_verified_scope"]),
        ),
        (
            &scope["properties"]["required_dimensions"]["items"],
            json!(["region", "resource_pool"]),
        ),
    ] {
        assert_eq!(property["enum"], values, "{property}");
        assert_eq!(property["type"], json!("string"), "{property}");
        assert!(property.get("oneOf").is_none(), "{property}");
        assert!(property.get("$ref").is_none(), "{property}");
    }
}

#[test]
fn a_zero_step_is_outside_the_schema_and_max_has_one_encoding() {
    let cap = schema(&QuotaCapabilityV1::gts_schema_with_refs_as_string());
    let limits = &cap["properties"]["limits"];
    assert_eq!(
        limits["properties"]["step"]["pattern"],
        json!(POSITIVE_DECIMAL)
    );
    assert_eq!(limits["properties"]["max"]["type"], json!("string"));
    assert!(
        !limits["required"]
            .as_array()
            .unwrap()
            .contains(&json!("max"))
    );
    // An absent `max` reads as unbounded and writes back absent; `null` is not a second spelling.
    let unbounded: QuotaLimits = serde_json::from_value(json!({"min": "0", "step": "1"})).unwrap();
    assert_eq!(unbounded.max, None);
    assert_eq!(
        serde_json::to_value(&unbounded).unwrap(),
        json!({"min": "0", "step": "1"})
    );
    assert!(
        serde_json::from_value::<QuotaLimits>(json!({"min": "0", "step": "1", "max": null}))
            .is_err()
    );
    let bounded: QuotaLimits =
        serde_json::from_value(json!({"min": "0", "step": "1", "max": "8"})).unwrap();
    assert_eq!(bounded.max.as_deref(), Some("8"));
}
