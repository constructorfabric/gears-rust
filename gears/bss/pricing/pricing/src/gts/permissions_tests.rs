//! Unit tests for the GTS permission catalog (`gts::permissions`), and for the
//! default role matrix the governance slice states over it.
//!
//! The catalog half is anti-drift: the registered id set equals the expected
//! one, no id is registered twice, each instance declares the permission type,
//! and the distinct `resource_type`s equal `crate::authz::labels::ALL`.
//!
//! The matrix half reads `design/05-governance.md` through `include_str!` and
//! asserts the separations the slice states in prose — which nothing else does,
//! the matrix being a document rather than code.

use toolkit_gts::{InventoryInstance, gts_id};

const PERMISSION_TYPE_ID: &str = gts_id!("cf.toolkit.authz.permission.v1~");
const INSTANCE_SUFFIX_PREFIX: &str = "cf.bss.pricing.";

/// Every pricing permission instance id — one per `(resource_type, action)`
/// pair the catalog surfaces enforce, per `design/05-governance.md`
/// `cpt-cf-bss-pricing-algo-authz-catalog`.
const EXPECTED_PERMISSION_IDS: &[&str] = &[
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_write.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_publish.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_retire.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_migrate.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_preview.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.bundle_write.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.bundle_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_overlay_write.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_overlay_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.customer_group_write.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.customer_group_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_approve.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_policy_write.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_policy_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.config_write.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.config_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.audit_read.v1"),
    gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.audit_export.v1"),
];

fn pricing_permission_instances() -> Vec<&'static InventoryInstance> {
    toolkit_gts::inventory::iter::<InventoryInstance>
        .into_iter()
        .filter(|e| {
            e.instance_id.starts_with(PERMISSION_TYPE_ID)
                && e.instance_id[PERMISSION_TYPE_ID.len()..].starts_with(INSTANCE_SUFFIX_PREFIX)
        })
        .collect()
}

/// Each registered instance declares the type it conforms to.
///
/// The macro derives `type_id` from the instance id's prefix **to its last
/// `~`**, so for an id carrying one `~` this restates the filter that selected
/// the entry. What it catches is an id carrying two — a nested instance id whose
/// derived type is longer than this catalog's, which the filter admits and
/// nothing else in this file reads.

#[test]
fn every_pricing_permission_declares_the_permission_type() {
    for entry in pricing_permission_instances() {
        assert_eq!(
            entry.type_id, PERMISSION_TYPE_ID,
            "instance {} declares the wrong type_id",
            entry.instance_id
        );
    }
}

/// The registered set and the expected set are the same set.
///
/// One equality rather than a membership loop beside a length: the loop names a
/// missing id and the length names a surplus one, and neither says which id is
/// surplus. The set difference names both directions at once.
#[test]
fn pricing_permission_inventory_covers_every_expected_id() {
    let actual: std::collections::BTreeSet<&str> = pricing_permission_instances()
        .iter()
        .map(|e| e.instance_id)
        .collect();
    let expected: std::collections::BTreeSet<&str> =
        EXPECTED_PERMISSION_IDS.iter().copied().collect();

    assert_eq!(
        actual,
        expected,
        "registered but unexpected: {:?}; expected but unregistered: {:?}",
        actual.difference(&expected).collect::<Vec<_>>(),
        expected.difference(&actual).collect::<Vec<_>>()
    );

    // The set cannot see a permission registered **twice** — a `gts_instance!`
    // block copy-pasted keeps one set member and two inventory entries — and
    // every other reader of this catalog collects into a set as well, so no
    // sibling would catch it either.
    let registered = pricing_permission_instances();
    assert_eq!(
        registered.len(),
        actual.len(),
        "an id is registered more than once: {:?}",
        registered
            .iter()
            .map(|e| e.instance_id)
            .filter(|id| registered.iter().filter(|e| e.instance_id == *id).count() > 1)
            .collect::<std::collections::BTreeSet<_>>()
    );
}

/// Anti-drift: the distinct `resource_type`s this catalog grants MUST equal
/// `crate::authz::labels::ALL` — the set the gear registers stub type-schemas
/// for so RBAC role-definitions can target them. Add a permission with a new
/// label (or a label to `ALL`) without the other and this fails.
#[test]
fn catalog_resource_types_match_authz_labels_all() {
    let catalog_types: std::collections::BTreeSet<String> = pricing_permission_instances()
        .iter()
        .map(|e| {
            (e.payload_fn)()["resource_type"]
                .as_str()
                .expect("AuthzPermissionV1 payload carries a resource_type string")
                .to_owned()
        })
        .collect();
    let labels_all: std::collections::BTreeSet<String> = crate::authz::labels::ALL
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

    assert_eq!(
        catalog_types, labels_all,
        "permission-catalog resource_types must equal crate::authz::labels::ALL"
    );
}

/// The governance slice, as a build input.
///
/// `include_str!` and not a restatement: a second copy of the role matrix in
/// Rust can agree with itself forever while the document moves, and a wave that
/// edits the matrix recompiles this test. The sibling
/// `domain::evaluation_policy_tests` takes `01-foundation.md` the same way and
/// for the same reason.
const GOVERNANCE: &str = include_str!("../../../docs/design/05-governance.md");

/// The matrix's own heading, and the only anchor this parse needs.
const MATRIX_ANCHOR: &str = " permission matrix**";

/// The multiplication sign the matrix spells `resource x action` with. Written
/// as an escape because `clippy::non_ascii_literal` is denied workspace-wide.
const TIMES: char = '\u{d7}';

/// Role name to the `(resource, action)` pairs the matrix grants that role.
type RoleMatrix = std::collections::BTreeMap<&'static str, RoleGrants>;

/// One role row's grants.
type RoleGrants = std::collections::BTreeSet<(&'static str, &'static str)>;

/// The default role matrix as `design/05-governance.md` states it: each live
/// role row mapped to the `(resource, action)` pairs it grants.
///
/// Struck rows are skipped -- `BackdateGrant` is struck by **D-330** and grants
/// nothing, because nobody issues it.
fn default_role_matrix() -> RoleMatrix {
    let table = GOVERNANCE
        .split_once(MATRIX_ANCHOR)
        .expect("05-governance.md carries the role-to-permission matrix")
        .1;

    let mut matrix = std::collections::BTreeMap::new();
    let mut inside = false;
    for line in table.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            if inside {
                break;
            }
            continue;
        }
        inside = true;
        let cells: Vec<&str> = line.split('|').collect();
        if cells.len() < 4 {
            continue;
        }
        let role = cells[1].trim();
        // The header, its dashed separator, and the rows a decision struck.
        if role.contains("~~") || role.contains("---") || role.starts_with("Role") {
            continue;
        }
        matrix.insert(role.trim_matches('*').trim(), grants_in(cells[2]));
    }
    matrix
}

/// Every `resource x action` pair a permission cell grants.
///
/// Read out of the cell's backticked spans only, so the prose beside them --
/// which names instruction ids and decisions in backticks too -- contributes
/// nothing: a span without the sign is not a grant.
fn grants_in(cell: &str) -> std::collections::BTreeSet<(&str, &str)> {
    let mut grants = std::collections::BTreeSet::new();
    for span in cell.split('`').skip(1).step_by(2) {
        let Some((resource, actions)) = span.split_once(TIMES) else {
            continue;
        };
        for action in actions.split('/') {
            grants.insert((resource.trim(), action.trim()));
        }
    }
    grants
}

/// **No default role holds both `plan` publish and `approval` approve.**
///
/// The separation of duties the governance slice states under its role matrix:
/// one principal may not both ship a price and sign it off. `CatalogAdmin`
/// publishes and deliberately cannot approve; `FinanceReviewer` approves and
/// holds no publish. The server-side `submitter != approver` rule is a
/// *different* guard -- it covers a custom role granting both, and it is tested
/// elsewhere -- so nothing at all covered the default matrix itself.
///
/// **Not asserted over the inventory.** Checking that the two permission ids are
/// registered is `pricing_permission_inventory_covers_every_expected_id`'s job
/// and cannot fail without it failing first — both ids are in
/// `EXPECTED_PERMISSION_IDS` — while naming this rule in the doc of such a case
/// is worse than leaving it uncovered: a reader counts the separation as
/// checked.

#[test]
fn no_default_role_holds_both_publish_and_approve() {
    let matrix = default_role_matrix();
    let publish_grant = ("plan", "publish");
    let approve_grant = ("approval", "approve");

    // Anti-vacuity, and it is the whole risk of parsing a document: a parse that
    // fell off the table yields an empty matrix, and an empty matrix satisfies
    // the rule. Every pair the parse *did* find must be a permission this
    // catalog registers, so rubbish cannot be quietly green either.
    let registered: std::collections::BTreeSet<&str> = pricing_permission_instances()
        .iter()
        .map(|e| e.instance_id)
        .collect();
    for (role, grants) in &matrix {
        for (resource, action) in grants {
            let id = format!("{PERMISSION_TYPE_ID}{INSTANCE_SUFFIX_PREFIX}{resource}_{action}.v1");
            assert!(
                registered.contains(id.as_str()),
                "the role matrix grants {role} a permission the catalog does not \
                 register: {id}"
            );
        }
    }

    let who_publishes: Vec<&str> = matrix
        .iter()
        .filter(|(_, grants)| grants.contains(&publish_grant))
        .map(|(role, _)| *role)
        .collect();
    let who_approves: Vec<&str> = matrix
        .iter()
        .filter(|(_, grants)| grants.contains(&approve_grant))
        .map(|(role, _)| *role)
        .collect();
    assert!(
        !who_publishes.is_empty() && !who_approves.is_empty(),
        "both halves of the rule must be held by somebody, or it is vacuous: \
         publishers {who_publishes:?}, approvers {who_approves:?}"
    );

    let holds_both: Vec<&str> = who_publishes
        .iter()
        .filter(|role| who_approves.contains(*role))
        .copied()
        .collect();
    assert!(
        holds_both.is_empty(),
        "a default role must not both publish a plan and approve it: {holds_both:?}"
    );
}

/// **No default role holds both `config` write and `approval_policy` write.**
///
/// D-10's separation of duties: an administrator who could move the approval
/// thresholds it operates under makes the two-person rule self-administered.
/// The matrix's own note states the arrangement — `CatalogAdmin` deliberately
/// lacks `approval_policy` write — and nothing read the matrix to check it.
#[test]
fn no_default_role_holds_both_config_write_and_approval_policy_write() {
    let matrix = default_role_matrix();
    let config_write = ("config", "write");
    let threshold_write = ("approval_policy", "write");

    let who_configures: Vec<&str> = matrix
        .iter()
        .filter(|(_, grants)| grants.contains(&config_write))
        .map(|(role, _)| *role)
        .collect();
    let who_sets_thresholds: Vec<&str> = matrix
        .iter()
        .filter(|(_, grants)| grants.contains(&threshold_write))
        .map(|(role, _)| *role)
        .collect();
    // Anti-vacuity, `no_default_role_holds_both_publish_and_approve`'s reason: a
    // parse that fell off the table grants nobody anything, and nobody holding
    // either half satisfies the rule.
    assert!(
        !who_configures.is_empty() && !who_sets_thresholds.is_empty(),
        "both halves must be held by somebody, or the rule is vacuous: \
         config writers {who_configures:?}, threshold writers {who_sets_thresholds:?}"
    );

    let holds_both: Vec<&str> = who_configures
        .iter()
        .filter(|role| who_sets_thresholds.contains(*role))
        .copied()
        .collect();
    assert!(
        holds_both.is_empty(),
        "a default role must not both administer config and set the approval \
         thresholds it operates under: {holds_both:?}"
    );
}

/// **`audit` read is one role's, and it is the Auditor's.**
///
/// D-12 confines the trail — actor identity, before/after, approval decisions —
/// and §3's route table repeats the confinement per route.
///
/// The converse is deliberately **not** asserted here. The Auditor does hold
/// `plan` read in the same matrix, so a case reading "an auditor carries no read
/// of live pricing" would contradict the document it claims to enforce; what
/// D-328 separated is the *history* surface, which moved off `plan` read onto
/// `audit`, not the Auditor's catalog read.
#[test]
fn audit_read_is_the_auditors_alone() {
    let matrix = default_role_matrix();

    let holders: Vec<&str> = matrix
        .iter()
        .filter(|(_, grants)| grants.contains(&("audit", "read")))
        .map(|(role, _)| *role)
        .collect();

    assert_eq!(
        holders,
        vec!["Auditor"],
        "the trail is Auditor-only in the default matrix"
    );
}
