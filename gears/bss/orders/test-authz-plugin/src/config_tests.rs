use super::*;

/// The Orders E2E suite config; its `gears.bss-orders-test-authz-plugin.config` block is the
/// policy the live suite boots with.
const SUITE_CONFIG: &str = include_str!("../../../../../testing/e2e/suites/bss/orders/config.yaml");

/// The Orders E2E fragment; the launcher appends its `bss-orders-test-authz-plugin.config`
/// `rules` and `unconditional_grants` to the suite block.
const FRAGMENT_CONFIG: &str = include_str!("../../config/e2e-orders.yaml");

/// A small fixture shaped after the suite's rows (one plain buyer rule, the delegation rule, and
/// an Event Broker producer rule and grant), not a copy of them: the targeted negative cases
/// mutate it one field at a time. The real blocks are parsed from [`SUITE_CONFIG`] and
/// [`FRAGMENT_CONFIG`].
const SUITE_YAML: &str = r#"
vendor: "constructorfabric"
priority: 10
policy_revision: "orders-e2e-s2-12"
rules:
  - id: "e2e-buyer-1"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["create", "write", "edit", "read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
  - id: "e2e-partner-delegated"
    subject: { id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
    delegation:
      proof_property: "delegation_proof_ref"
      accepted: ["e2e-proof-ok"]
  - id: "orders-producer-root-tenant-produce"
    subject:
      id: "0e0e0000-0000-4000-8000-000000000005"
      tenant_id: "0e0e0000-0000-4000-8000-0000000000aa"
    resource_type: "gts.cf.core.events.request.v1~"
    actions: ["produce"]
    paths:
      - predicates:
          - property: "owner_tenant_id"
            values: ["00000000-df51-5b42-9538-d2b56b7ee953"]
unconditional_grants:
  - id: "orders-producer-event-type-produce"
    subject:
      id: "0e0e0000-0000-4000-8000-000000000005"
      tenant_id: "0e0e0000-0000-4000-8000-0000000000aa"
    resource_type: "gts.cf.core.events.event_type.v1~"
    action: "produce"
"#;

fn parse(yaml: &str) -> Result<OrdersTestAuthZPluginConfig, serde_saphyr::Error> {
    serde_saphyr::from_str(yaml)
}

fn suite() -> OrdersTestAuthZPluginConfig {
    parse(SUITE_YAML).unwrap()
}

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

/// The `bss-orders-test-authz-plugin` block of a whole YAML document, typed. `path` is the key
/// path to the gear block (`gears.<gear>` in the suite config, top level in the fragment).
fn plugin_block(document: &str, path: &[&str]) -> OrdersTestAuthZPluginConfig {
    let mut value: serde_json::Value = serde_saphyr::from_str(document).unwrap();
    for key in path
        .iter()
        .chain(&["bss-orders-test-authz-plugin", "config"])
    {
        value = value
            .get_mut(*key)
            .unwrap_or_else(|| panic!("no `{key}` in the document"))
            .take();
    }
    serde_json::from_value(value).unwrap()
}

fn suite_block() -> OrdersTestAuthZPluginConfig {
    plugin_block(SUITE_CONFIG, &["gears"])
}

fn fragment_block() -> OrdersTestAuthZPluginConfig {
    plugin_block(FRAGMENT_CONFIG, &[])
}

#[test]
fn the_suite_config_block_parses_and_validates() {
    let cfg = suite_block();
    cfg.validate().unwrap();
    assert_eq!(cfg.vendor, "constructorfabric");
    assert_eq!(cfg.priority, 10);
    assert_eq!(cfg.policy_revision, "orders-e2e-s2-12");

    // Eight buyers, one identical rule each, authoring (preview and submit included) on
    // resource tenant A.
    let buyers: Vec<&Rule> = cfg
        .rules
        .iter()
        .filter(|r| r.id.starts_with("e2e-buyer-"))
        .collect();
    assert_eq!(
        buyers.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        (1..=8)
            .map(|i| format!("e2e-buyer-{i}"))
            .collect::<Vec<_>>()
    );
    for (i, buyer) in (1u8..).zip(&buyers) {
        assert_eq!(
            buyer.subject.id,
            uuid(&format!("1111111{i}-6a88-4768-9dfc-6bcd5187d9ed")),
            "{}",
            buyer.id
        );
        assert_eq!(buyer.subject.tenant_id, None);
        assert_eq!(buyer.resource_type, "gts.cf.bss.orders.order.v1~");
        assert_eq!(
            buyer.actions,
            ["create", "write", "edit", "read", "preview", "submit"],
            "{}",
            buyer.id
        );
        assert_eq!(buyer.paths.len(), 1);
        let predicate = &buyer.paths[0].predicates[0];
        assert_eq!(predicate.property, "resource_tenant_id");
        assert_eq!(predicate.values, ["a0000000-0000-4000-8000-00000000000a"]);
        assert_eq!(predicate.value, None);
        assert!(buyer.delegation.is_none());
    }

    // The delegated partner reads only with an accepted proof reference.
    let partner = cfg
        .rules
        .iter()
        .find(|r| r.id == "e2e-partner-delegated")
        .unwrap();
    assert_eq!(partner.actions, ["read"]);
    let delegation = partner.delegation.as_ref().unwrap();
    assert_eq!(delegation.proof_property, "delegation_proof_ref");
    assert_eq!(delegation.accepted, ["e2e-proof-ok"]);
}

#[test]
fn the_fragment_block_parses_and_validates_alone_and_appended() {
    let fragment = fragment_block();
    fragment.validate().unwrap();

    // The tenant-scope `request` `produce` rule, confined to the platform root tenant.
    assert_eq!(fragment.rules.len(), 1);
    let producer = &fragment.rules[0];
    assert_eq!(producer.id, "orders-producer-root-tenant-produce");
    assert_eq!(
        producer.subject.id,
        uuid("0e0e0000-0000-4000-8000-000000000005")
    );
    assert_eq!(
        producer.subject.tenant_id,
        Some(uuid("0e0e0000-0000-4000-8000-0000000000aa"))
    );
    assert_eq!(producer.resource_type, "gts.cf.core.events.request.v1~");
    assert_eq!(producer.actions, ["produce"]);
    let predicate = &producer.paths[0].predicates[0];
    assert_eq!(predicate.property, "owner_tenant_id");
    assert_eq!(predicate.values, ["00000000-df51-5b42-9538-d2b56b7ee953"]);

    // The grant names the concrete submitted event type: no wildcard, no allow-all.
    assert_eq!(fragment.unconditional_grants.len(), 1);
    let grant = &fragment.unconditional_grants[0];
    assert_eq!(grant.id, "orders-producer-submitted-produce");
    assert_eq!(
        grant.resource_type,
        "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.submitted.v1~"
    );
    assert_eq!(grant.action, "produce");
    assert_eq!(
        (grant.subject.id, grant.subject.tenant_id),
        (producer.subject.id, producer.subject.tenant_id)
    );

    // The launcher appends the fragment's rows to the suite block: the union still validates
    // (no id collides across the two files).
    let mut merged = suite_block();
    merged.rules.extend(fragment.rules);
    merged
        .unconditional_grants
        .extend(fragment.unconditional_grants);
    merged.validate().unwrap();
}

#[test]
fn defaults_apply_to_an_empty_block() {
    let cfg = parse("{}").unwrap();
    assert_eq!(cfg.vendor, "constructorfabric");
    assert_eq!(cfg.priority, 10);
    assert!(cfg.policy_revision.is_empty());
    assert!(cfg.rules.is_empty());
    assert!(cfg.unconditional_grants.is_empty());
    cfg.validate().unwrap();

    let default = OrdersTestAuthZPluginConfig::default();
    assert_eq!(default.vendor, cfg.vendor);
    assert_eq!(default.priority, cfg.priority);
}

#[test]
fn unknown_keys_are_refused_at_every_level() {
    // Top level.
    assert!(parse("vendor: x\nrulez: []\n").is_err());
    // Rule.
    let rule = SUITE_YAML.replace(
        "    actions: [\"read\"]\n    paths:",
        "    actions: [\"read\"]\n    effect: allow\n    paths:",
    );
    assert_ne!(rule, SUITE_YAML);
    assert!(parse(&rule).is_err());
    // Subject.
    let subject = SUITE_YAML.replace(
        "subject: { id: \"11111111-6a88-4768-9dfc-6bcd5187d9ed\" }",
        "subject: { id: \"11111111-6a88-4768-9dfc-6bcd5187d9ed\", role: x }",
    );
    assert_ne!(subject, SUITE_YAML);
    assert!(parse(&subject).is_err());
    // Predicate.
    let predicate = SUITE_YAML.replace(
        "            values: [\"00000000-df51-5b42-9538-d2b56b7ee953\"]",
        "            values: [\"00000000-df51-5b42-9538-d2b56b7ee953\"]\n            op: in",
    );
    assert_ne!(predicate, SUITE_YAML);
    assert!(parse(&predicate).is_err());
    // Delegation.
    let delegation = SUITE_YAML.replace(
        "      accepted: [\"e2e-proof-ok\"]",
        "      accepted: [\"e2e-proof-ok\"]\n      optional: true",
    );
    assert_ne!(delegation, SUITE_YAML);
    assert!(parse(&delegation).is_err());
    // Grant.
    let grant = SUITE_YAML.replace(
        "    action: \"produce\"",
        "    action: \"produce\"\n    paths: []",
    );
    assert_ne!(grant, SUITE_YAML);
    assert!(parse(&grant).is_err());
}

#[test]
fn a_non_uuid_subject_id_is_refused_at_parse() {
    let yaml = SUITE_YAML.replace(
        "subject: { id: \"11111111-6a88-4768-9dfc-6bcd5187d9ed\" }",
        "subject: { id: \"buyer-1\" }",
    );
    assert!(parse(&yaml).is_err());
}

#[test]
fn duplicate_id_across_rules_and_grants() {
    let mut cfg = suite();
    cfg.unconditional_grants[0].id = "e2e-buyer-1".to_owned();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::DuplicateId(id)) if id == "e2e-buyer-1"
    ));

    let mut cfg = suite();
    cfg.rules[1].id = "e2e-buyer-1".to_owned();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::DuplicateId(id)) if id == "e2e-buyer-1"
    ));
}

#[test]
fn empty_actions() {
    let mut cfg = suite();
    cfg.rules[0].actions.clear();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::EmptyActions(id)) if id == "e2e-buyer-1"
    ));

    let mut cfg = suite();
    cfg.unconditional_grants[0].action.clear();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::EmptyActions(id)) if id == "orders-producer-event-type-produce"
    ));
}

#[test]
fn empty_paths() {
    let mut cfg = suite();
    cfg.rules[0].paths.clear();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::EmptyPaths(id)) if id == "e2e-buyer-1"
    ));
}

#[test]
fn empty_predicates() {
    let mut cfg = suite();
    cfg.rules[0].paths[0].predicates.clear();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::EmptyPredicates(id)) if id == "e2e-buyer-1"
    ));
}

#[test]
fn a_blank_predicate_value_is_refused() {
    for blank in ["", "  "] {
        let mut cfg = suite();
        cfg.rules[0].paths[0].predicates[0].values = vec![blank.to_owned()];
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::PredicateValues(id, property))
                if id == "e2e-buyer-1" && property == "resource_tenant_id"
        ));

        let mut cfg = suite();
        cfg.rules[0].paths[0].predicates[0].values.clear();
        cfg.rules[0].paths[0].predicates[0].value = Some(blank.to_owned());
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::PredicateValues(id, property))
                if id == "e2e-buyer-1" && property == "resource_tenant_id"
        ));
    }
}

#[test]
fn predicate_values_needs_exactly_one_non_empty_form() {
    // Neither form.
    let mut cfg = suite();
    cfg.rules[0].paths[0].predicates[0].values.clear();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::PredicateValues(id, property))
            if id == "e2e-buyer-1" && property == "resource_tenant_id"
    ));

    // Both forms.
    let mut cfg = suite();
    cfg.rules[0].paths[0].predicates[0].value =
        Some("a0000000-0000-4000-8000-00000000000a".to_owned());
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::PredicateValues(id, property))
            if id == "e2e-buyer-1" && property == "resource_tenant_id"
    ));

    // The single `value` form alone is valid.
    let yaml = SUITE_YAML.replace(
        "{ property: \"resource_tenant_id\", values: [\"a0000000-0000-4000-8000-00000000000a\"] }",
        "{ property: \"resource_tenant_id\", value: \"a0000000-0000-4000-8000-00000000000a\" }",
    );
    assert_ne!(yaml, SUITE_YAML);
    let cfg = parse(&yaml).unwrap();
    assert_eq!(
        cfg.rules[0].paths[0].predicates[0].value.as_deref(),
        Some("a0000000-0000-4000-8000-00000000000a")
    );
    assert!(cfg.rules[0].paths[0].predicates[0].values.is_empty());
    cfg.validate().unwrap();
}

#[test]
fn uuid_typed_properties_must_hold_uuids() {
    for property in UUID_PROPERTIES {
        let mut cfg = suite();
        let predicate = &mut cfg.rules[0].paths[0].predicates[0];
        property.clone_into(&mut predicate.property);
        predicate.values = vec![
            "a0000000-0000-4000-8000-00000000000a".to_owned(),
            "not-a-uuid".to_owned(),
        ];
        assert!(
            matches!(
                cfg.validate(),
                Err(ConfigError::InvalidUuid(ref id, ref p)) if id == "e2e-buyer-1" && p == property
            ),
            "{property} must refuse a non-UUID value"
        );

        // The single `value` form is checked the same way.
        let mut cfg = suite();
        let predicate = &mut cfg.rules[0].paths[0].predicates[0];
        property.clone_into(&mut predicate.property);
        predicate.values.clear();
        predicate.value = Some("e2e-proof-ok".to_owned());
        assert!(
            matches!(cfg.validate(), Err(ConfigError::InvalidUuid(_, ref p)) if p == property),
            "{property} must refuse a non-UUID single value"
        );
    }
}

#[test]
fn other_properties_hold_free_text() {
    let mut cfg = suite();
    let predicate = &mut cfg.rules[0].paths[0].predicates[0];
    "delegation_proof_ref".clone_into(&mut predicate.property);
    predicate.values = vec!["e2e-proof-ok".to_owned(), "not-a-uuid".to_owned()];
    cfg.validate().unwrap();

    // Delegation accepted values are free text too.
    let mut cfg = suite();
    cfg.rules[1].delegation.as_mut().unwrap().accepted = vec!["any proof text".to_owned()];
    cfg.validate().unwrap();
}

#[test]
fn wildcard_resource_type_only_as_trailing_suffix() {
    let mut cfg = suite();
    "gts.cf.bss.pricing.*".clone_into(&mut cfg.rules[0].resource_type);
    cfg.validate().unwrap();

    let mut cfg = suite();
    "gts.cf.*.order.v1~".clone_into(&mut cfg.rules[0].resource_type);
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::Wildcard(id)) if id == "e2e-buyer-1"
    ));

    let mut cfg = suite();
    "gts.cf.core.events.*".clone_into(&mut cfg.unconditional_grants[0].resource_type);
    cfg.validate().unwrap();

    let mut cfg = suite();
    "gts.*.events.event_type.v1~".clone_into(&mut cfg.unconditional_grants[0].resource_type);
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::Wildcard(id)) if id == "orders-producer-event-type-produce"
    ));
}

#[test]
fn wildcard_action_only_alone() {
    let mut cfg = suite();
    cfg.rules[0].actions = vec!["*".to_owned()];
    cfg.validate().unwrap();

    let mut cfg = suite();
    cfg.rules[0].actions = vec!["read".to_owned(), "*".to_owned()];
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::Wildcard(id)) if id == "e2e-buyer-1"
    ));

    let mut cfg = suite();
    cfg.rules[0].actions = vec!["re*".to_owned()];
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::Wildcard(id)) if id == "e2e-buyer-1"
    ));

    let mut cfg = suite();
    "*".clone_into(&mut cfg.unconditional_grants[0].action);
    cfg.validate().unwrap();

    let mut cfg = suite();
    "pro*".clone_into(&mut cfg.unconditional_grants[0].action);
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::Wildcard(id)) if id == "orders-producer-event-type-produce"
    ));
}

#[test]
fn empty_delegation_is_refused() {
    let mut cfg = suite();
    "  ".clone_into(&mut cfg.rules[1].delegation.as_mut().unwrap().proof_property);
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::EmptyDelegation(id)) if id == "e2e-partner-delegated"
    ));

    let mut cfg = suite();
    cfg.rules[1].delegation.as_mut().unwrap().accepted.clear();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::EmptyDelegation(id)) if id == "e2e-partner-delegated"
    ));

    for blank in ["", " "] {
        let mut cfg = suite();
        cfg.rules[1]
            .delegation
            .as_mut()
            .unwrap()
            .accepted
            .push(blank.to_owned());
        assert!(
            matches!(
                cfg.validate(),
                Err(ConfigError::EmptyDelegation(ref id)) if id == "e2e-partner-delegated"
            ),
            "accepted value {blank:?} must be refused"
        );
    }

    suite().validate().unwrap();
}

#[test]
fn empty_or_bare_wildcard_resource_type_is_refused() {
    let mut cfg = suite();
    cfg.rules[0].resource_type.clear();
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::EmptyResourceType(id)) if id == "e2e-buyer-1"
    ));

    let mut cfg = suite();
    "*".clone_into(&mut cfg.rules[0].resource_type);
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::Wildcard(id)) if id == "e2e-buyer-1"
    ));

    let mut cfg = suite();
    "*".clone_into(&mut cfg.unconditional_grants[0].resource_type);
    assert!(matches!(
        cfg.validate(),
        Err(ConfigError::Wildcard(id)) if id == "orders-producer-event-type-produce"
    ));

    let mut cfg = suite();
    "gts.cf.bss.pricing.*".clone_into(&mut cfg.rules[0].resource_type);
    cfg.validate().unwrap();
}

#[test]
fn blank_predicate_property_is_refused() {
    for blank in ["", "  "] {
        let mut cfg = suite();
        blank.clone_into(&mut cfg.rules[0].paths[0].predicates[0].property);
        assert!(
            matches!(
                cfg.validate(),
                Err(ConfigError::EmptyProperty(ref id)) if id == "e2e-buyer-1"
            ),
            "property {blank:?} must be refused"
        );
    }
}
