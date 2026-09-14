//! Tests for [`crate::authz`] — the label set, the descriptors, and the stub
//! type-schemas registered at init.

use super::{SUPPORTED_PROPERTIES, actions, authz_label_type_schemas, labels, resource_types};
use uuid::Uuid;

#[test]
fn every_label_lives_outside_the_platform_resource_family() {
    // The whole point of the label choice: `gts.cf.resources.*` is auto-covered
    // by the built-in Reader / Contributor / Owner roles, and pricing data must
    // require an explicit catalog role.
    for label in labels::ALL {
        assert!(
            !label.starts_with("gts.cf.resources."),
            "{label} is inside the auto-covered platform resource family"
        );
        assert!(
            label.starts_with("gts.cf.bss.pricing."),
            "{label} is not a pricing-owned label"
        );
        assert!(label.ends_with(".v1~"), "{label} is not a type-level id");
    }
}

#[test]
fn the_label_list_has_no_duplicates() {
    let distinct: std::collections::BTreeSet<&str> = labels::ALL.iter().copied().collect();

    assert_eq!(
        distinct.len(),
        labels::ALL.len(),
        "labels::ALL repeats a label"
    );
}

#[test]
fn descriptors_carry_their_label_and_the_supported_properties() {
    for (rt, label) in [
        (&resource_types::PLAN, labels::PLAN),
        (&resource_types::BUNDLE, labels::BUNDLE),
        (&resource_types::PRICE_OVERLAY, labels::PRICE_OVERLAY),
        (&resource_types::CUSTOMER_GROUP, labels::CUSTOMER_GROUP),
        (&resource_types::APPROVAL, labels::APPROVAL),
        (&resource_types::APPROVAL_POLICY, labels::APPROVAL_POLICY),
        (&resource_types::CONFIG, labels::CONFIG),
        (&resource_types::AUDIT, labels::AUDIT),
    ] {
        assert_eq!(rt.name(), label);
        assert_eq!(rt.supported_properties(), SUPPORTED_PROPERTIES);
    }
}

#[test]
fn the_pep_advertises_no_subtree_property() {
    // The PDP pre-expands the caller's subtree to a flat `In`; advertising a
    // subtree property here would change what the PDP compiles.
    assert_eq!(SUPPORTED_PROPERTIES.len(), 2);
    assert!(
        !SUPPORTED_PROPERTIES
            .iter()
            .any(|p| p.contains("subtree") || p.contains("group")),
        "no subtree/group property is supported"
    );
}

#[test]
fn action_names_are_distinct() {
    let all = [
        actions::WRITE,
        actions::PUBLISH,
        actions::RETIRE,
        actions::MIGRATE,
        actions::READ,
        actions::PREVIEW,
        actions::APPROVE,
        actions::EXPORT,
    ];
    let distinct: std::collections::BTreeSet<&str> = all.iter().copied().collect();

    assert_eq!(distinct.len(), all.len(), "two actions share a name");
}

#[test]
fn a_stub_schema_is_produced_for_every_label() {
    let schemas = authz_label_type_schemas();

    assert_eq!(schemas.len(), labels::ALL.len());
    for (schema, label) in schemas.iter().zip(labels::ALL) {
        assert_eq!(
            schema["$id"].as_str(),
            Some(format!("gts://{label}").as_str())
        );
        assert_eq!(schema["type"].as_str(), Some("object"));
    }
}

#[test]
fn the_registered_body_of_a_label_is_the_document_it_was_registered_as() {
    // Registration is re-run on every boot and the registry accepts an identical
    // duplicate; what it refuses is a body that **changed** between deploys. Only a
    // literal can see that. Rendering the generator twice and comparing cannot:
    // `authz_label_type_schemas` is a `map` over a static roster into `json!`
    // literals, so no implementation of it can make the two strings differ.
    // Selected by position, not by `$id`: selecting on the member the golden then
    // asserts would make that member restate its own predicate.
    let at = labels::ALL
        .iter()
        .position(|label| *label == labels::PLAN)
        .expect("the plan label is registered");
    let plan = authz_label_type_schemas()
        .into_iter()
        .nth(at)
        .expect("a schema per label, in the roster's order");

    assert_eq!(
        plan,
        serde_json::json!({
            "$id": format!("gts://{}", labels::PLAN),
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": format!("BSS Plan & Price Modeling authz label {}", labels::PLAN),
            "type": "object",
        })
    );
}

#[test]
fn a_cross_tenant_write_denial_names_every_operand_an_audit_record_needs() {
    // `inst-rb-audit` / `dod-rbac` are `p1` and say denied attempts are
    // audit-logged. They were not: all 67 routes' 403s pass one funnel
    // (`api::rest::error::authz_error_to_canonical`) whose `Denied` arm emitted
    // nothing at all - no record, no metric, no log - while the `Unavailable` arm
    // beside it logs. `AuditAction::Deny` is constructed in exactly one place, the
    // approval plane, which no PEP refusal can reach.
    //
    // The first thing a denial owes is its operands. A `String` cannot carry them,
    // so `AuthzError::Denied` now carries a `DeniedAttempt` and the compiler makes
    // the omission impossible: there is no way to deny without naming who, what,
    // which and why.
    let subject = Uuid::from_u128(0x_5e_eb);
    let subject_tenant = Uuid::from_u128(0x_7e_11);
    let target_tenant = Uuid::from_u128(0x_7e_22);
    let resource = Uuid::from_u128(0x_a11);

    let attempt = super::cross_tenant_write_denial(
        subject,
        subject_tenant,
        &super::resource_types::PLAN,
        "write",
        Some(resource),
        target_tenant,
    );

    assert_eq!(attempt.subject_principal_id, subject);
    assert_eq!(attempt.subject_tenant_id, subject_tenant);
    assert_eq!(attempt.resource_type, super::labels::PLAN);
    assert_eq!(attempt.action, "write");
    assert_eq!(attempt.resource_id, Some(resource));
    assert_eq!(attempt.owner_tenant_id, Some(target_tenant));
    assert!(
        attempt.reason.contains(&target_tenant.to_string()),
        "the reason must name the tenant that was refused: {}",
        attempt.reason
    );
}

/// Split an argument list on its top-level commas.
fn split_top_level(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0_usize;
    for (index, byte) in args.as_bytes().iter().enumerate() {
        match byte {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' | b'>' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&args[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&args[start..]);
    // The call sites are rustfmt'd across seven lines with a **trailing comma**,
    // which splits into an eighth, empty element. Dropping blanks rather than
    // special-casing the tail: an empty argument anywhere else is not valid Rust,
    // so there is nothing else this can discard.
    out.retain(|arg| !arg.trim().is_empty());
    out
}

/// **Every read gate passes `None` for `owner_tenant_id`.**
///
/// [`crate::authz::access_scope`] splits its contract: reads pass `None` and let
/// the PDP derive the scope from the subject and its role; only a write passes
/// `Some(target_tenant)`, so the membership assertion has a target to test.
/// Four read gates passed `Some(tenant)` until 2026-08-18 and `plan × preview`
/// until this one — a write-only assertion running on a read.
///
/// **A scan and not a request, deliberately.** Under every fixture in the crate
/// the two spellings answer identically: `tenant` is `ctx.subject_tenant_id()` at
/// each site, which a correctly compiled read scope already contains. No
/// behavioural probe can see the divergence, and the thing that breaks is a
/// *future* grant compiled to a tenant the caller does not authenticate in.
#[test]
fn every_read_gate_passes_no_owner_tenant_id() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api/rest");
    let mut stack = vec![root];
    let mut offenders: Vec<String> = Vec::new();
    let mut read_gates = 0_usize;
    // The census the gate count is held against — see the two assertions below.
    let mut read_actions_named = 0_usize;

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a readable directory") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
                || name.ends_with("_tests.rs")
            {
                continue;
            }
            let label = name.to_owned();
            let text = std::fs::read_to_string(&path).expect("a readable source file");
            // Blanked first: the call sites carry `/* owner_tenant_id */` markers
            // inside the argument list, and a raw scan would read those as code.
            let code = crate::source_scan::blank_comments_and_literals(&text);

            // Counted over the same blanked text the gate walk reads, so prose
            // that mentions a read action is not a gate: `history.rs` names
            // `actions::EXPORT` in a comment beside the one that is a call.
            for action in ["actions::READ", "actions::PREVIEW", "actions::EXPORT"] {
                read_actions_named += code.matches(action).count();
            }

            let needle = "access_scope(";
            let mut from = 0_usize;
            while let Some(hit) = code[from..].find(needle) {
                let open = from + hit + needle.len() - 1;
                from = open + 1;
                let Some(close) = crate::source_scan::matching_delim(&code, open, b'(', b')')
                else {
                    continue;
                };
                let args = split_top_level(&code[open + 1..close]);
                // `access_scope`'s arity. A call that does not have it is not the
                // function this walk judges — and a *skip* is how the walk goes
                // silently vacuous, which is why the equality below is the
                // assertion of record rather than the offender list.
                if args.len() != 6 {
                    continue;
                }
                let action = args[3].trim();
                if !(action.ends_with("::READ")
                    || action.ends_with("::PREVIEW")
                    || action.ends_with("::EXPORT"))
                {
                    continue;
                }
                read_gates += 1;
                let owner = args[4].trim();
                if owner != "None" {
                    offenders.push(format!("{label}: {action} passes `{owner}`"));
                }
            }
        }
    }

    // Without this the test is vacuously green the moment the call spelling moves
    // — which is the failure mode a source scan has and a request does not.
    //
    // **Held against the real population, not a token.** The floor was `>= 5`
    // until 2026-08-20, while the walk was matching 27 gates: a change that
    // stopped it matching four fifths of them — a subset of endpoints moved out
    // of `src/api/rest`, an argument added to some sites and not others — left
    // it green with an empty offender list, which is the one thing the floor
    // exists to prevent. So the count is pinned to the census the same walk
    // takes of the read actions themselves, and that census carries the floor:
    // rename the action constants and the equality would go vacuous at 0 = 0.
    assert!(
        read_actions_named >= 27,
        "the walk found only {read_actions_named} read actions named under \
         src/api/rest; it has stopped reading the endpoint sources"
    );
    assert_eq!(
        read_gates, read_actions_named,
        "the walk named {read_actions_named} read actions but matched only \
         {read_gates} as `access_scope` gates; a call site has moved out of \
         reach of the scan, and its owner_tenant_id is no longer checked"
    );
    assert!(
        offenders.is_empty(),
        "a read gate must pass `None` for owner_tenant_id: {offenders:#?}"
    );
}
