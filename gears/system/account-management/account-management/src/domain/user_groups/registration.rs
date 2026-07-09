//! Idempotent RG type-schema registration algorithm.
//!
//! Implements `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration`.

use std::sync::Arc;

use resource_group_sdk::{
    CreateTypeRequest, ResourceGroupClient, ResourceGroupError, ResourceGroupType,
    UpdateTypeRequest,
};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::info;

use super::{USER_GROUP_TYPE_CODE, USER_MEMBERSHIP_TYPE};
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};

/// Outcome of the registration algorithm for **one** RG type.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationOutcome {
    /// Schema already registered with equivalent traits — no-op.
    AlreadyPresent,
    /// Schema was absent; newly registered.
    RegisteredNew,
}

/// Error returned when registration cannot proceed.
#[domain_model]
#[derive(Debug, Clone)]
pub enum RegistrationError {
    /// Resource Group is unreachable (transport failure / timeout).
    ServiceUnavailable(String),
    /// An existing RG-side schema has divergent traits.
    DivergentSchema(String),
}

/// Combined outcome of the two-type registration pair.
///
/// The pair is registered in order (member handle → container), so a
/// caller that sees `member` register cleanly but `container` fail
/// can rely on the partial state being consistent: the member-handle
/// row exists in `gts_type` but no group references it yet.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserGroupRegistrationOutcome {
    /// Outcome of the [`USER_MEMBERSHIP_TYPE`] (user member-handle)
    /// registration.
    pub member: RegistrationOutcome,
    /// Outcome of the [`USER_GROUP_TYPE_CODE`] (user-group container)
    /// registration.
    pub container: RegistrationOutcome,
}

/// Single source of truth for both the CREATE payload and the
/// equivalence check, so a new field cannot drift between create
/// and classify.
#[domain_model]
#[derive(Debug, Clone)]
struct TypeSpec {
    /// RG type code. Matches `ResourceGroupType.code`.
    code: &'static str,
    /// Whether instances of this type may exist as roots (no parent).
    can_be_root: bool,
    /// Allowed parent type codes. For the user-group container this
    /// contains its own code (self-parent for hierarchical groups).
    /// For the member handle this is empty -- AM never `create_group`s
    /// instances of the handle type, so parent rules are moot.
    allowed_parent_types: Vec<&'static str>,
    /// Allowed membership type codes. For the user-group container
    /// this lists the member handle. For the handle itself, empty.
    allowed_membership_types: Vec<&'static str>,
}

impl TypeSpec {
    /// Spec for the user-member handle: type-registry placeholder that
    /// `add_membership` resolves against. AM never creates groups of
    /// this type, so `can_be_root: true` + empty parents satisfies RG's
    /// `validate_placement_invariant` without admitting any specific
    /// parent shape.
    fn user_member_handle() -> Self {
        Self {
            code: USER_MEMBERSHIP_TYPE,
            can_be_root: true,
            allowed_parent_types: Vec::new(),
            allowed_membership_types: Vec::new(),
        }
    }

    /// Spec for the user-group container: tenant-scoped RG groups
    /// representing AM user groups. `allowed_membership_types` MUST
    /// list the member handle so RG's `add_membership` validation
    /// admits AM users.
    ///
    /// The final logical spec carries the self-referential
    /// `allowed_parent_types = [USER_GROUP_TYPE_CODE]` rule so AM-owned
    /// user-groups can be nested under one another. RG's
    /// `create_type` rejects self-references because
    /// `resolve_ids(allowed_parent_types)` runs before the INSERT, so
    /// AM registers in two passes (see [`register_user_group_types`]):
    /// CREATE with empty parents → `update_type` patching the
    /// self-parent rule once the row is in `gts_type`.
    fn user_group_container() -> Self {
        Self {
            code: USER_GROUP_TYPE_CODE,
            can_be_root: true,
            allowed_parent_types: vec![USER_GROUP_TYPE_CODE],
            allowed_membership_types: vec![USER_MEMBERSHIP_TYPE],
        }
    }

    /// First-pass spec for the container: same shape as
    /// [`Self::user_group_container`] but with
    /// `allowed_parent_types = []` so RG's `create_type` validation
    /// passes. The follow-up `update_type` call patches the
    /// self-parent rule against the same row.
    fn user_group_container_without_parents() -> Self {
        Self {
            code: USER_GROUP_TYPE_CODE,
            can_be_root: true,
            allowed_parent_types: Vec::new(),
            allowed_membership_types: vec![USER_MEMBERSHIP_TYPE],
        }
    }

    fn to_create_request(&self) -> CreateTypeRequest {
        CreateTypeRequest {
            code: self.code.to_owned(),
            can_be_root: self.can_be_root,
            allowed_parent_types: self
                .allowed_parent_types
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            allowed_membership_types: self
                .allowed_membership_types
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            metadata_schema: None,
        }
    }

    fn to_update_request(&self) -> UpdateTypeRequest {
        UpdateTypeRequest {
            can_be_root: self.can_be_root,
            allowed_parent_types: self
                .allowed_parent_types
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            allowed_membership_types: self
                .allowed_membership_types
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            metadata_schema: None,
        }
    }
}

// @cpt-begin:cpt-cf-account-management-algo-user-groups-rg-type-schema-registration:p1:inst-algo-rgreg-full
/// Register the AM user-group type schemas idempotently.
///
/// Two-step registration in fixed order:
///
/// 1. [`USER_MEMBERSHIP_TYPE`] — the AM-user member handle. MUST land
///    first so that step 2's `resolve_ids(allowed_membership_types)`
///    finds the row.
/// 2. [`USER_GROUP_TYPE_CODE`] — the user-group container, with
///    `allowed_membership_types = [USER_MEMBERSHIP_TYPE]` so RG's
///    `add_membership` admits AM users.
///
/// Each step runs the full idempotent algorithm independently
/// ([`register_one`]): `get_type` → classify-or-create → on
/// `AlreadyExists` race re-read and classify. A step that lands
/// on `DivergentSchema` aborts the pair; the caller (`gear init`)
/// surfaces the error and does NOT signal ready.
///
/// # Self-parent rule patched as a follow-up `update_type`
///
/// The container type's logical spec includes the self-referential
/// rule `allowed_parent_types = [USER_GROUP_TYPE_CODE]` so AM-owned
/// user-groups can be nested under one another. RG's `create_type`
/// validation runs `resolve_ids(allowed_parent_types)` BEFORE
/// inserting the row, so a self-reference fails with `validation:
/// Referenced types not found`. AM works around this by registering
/// the container with empty `allowed_parent_types` (which classifies
/// as inclusive-equivalent against either pre-state — see
/// [`classify_existing`]), then patching the self-parent rule with
/// `update_type` once the row is in RG's `gts_type` table. The
/// follow-up patch is idempotent: a prior init that already wrote
/// the self-parent rule sees the rule re-set to the same values.
///
/// Called during `AccountManagementGear::init`. On success the
/// gear may proceed to signal ready.
pub async fn register_user_group_types(
    client: &Arc<dyn ResourceGroupClient + Send + Sync>,
    ctx: &SecurityContext,
) -> Result<UserGroupRegistrationOutcome, RegistrationError> {
    // Step 1: member handle first -- step 2 depends on it being
    // resolvable in `gts_type`.
    let member = register_one(client, ctx, &TypeSpec::user_member_handle()).await?;
    // Step 2a: register the container with empty parents so RG's
    // `resolve_ids(allowed_parent_types)` passes. `register_one`'s
    // classification accepts a pre-existing row that already carries
    // the self-parent rule (inclusive-equivalence on `allowed_parent_types`).
    let container = register_one(
        client,
        ctx,
        &TypeSpec::user_group_container_without_parents(),
    )
    .await?;
    // Step 2b: patch the self-parent rule via `update_type`. Always
    // run — `update_type` accepts the self-reference because
    // `resolve_ids` resolves it against the already-inserted row.
    let final_spec = TypeSpec::user_group_container();
    if let Err(e) = client
        .update_type(ctx, USER_GROUP_TYPE_CODE, final_spec.to_update_request())
        .await
    {
        emit_metric(
            AM_DEPENDENCY_HEALTH,
            MetricKind::Counter,
            &[
                ("target", "resource_group"),
                ("op", "register_user_group_type_patch_self_parent"),
                ("outcome", "error"),
            ],
        );
        return Err(RegistrationError::ServiceUnavailable(format!(
            "resource-group: failed to patch self-parent rule on type '{USER_GROUP_TYPE_CODE}': {e}"
        )));
    }
    info!(
        target: "am.user_groups",
        code = USER_GROUP_TYPE_CODE,
        "user-groups RG container patched with self-parent rule"
    );
    Ok(UserGroupRegistrationOutcome { member, container })
}

/// Idempotent registration of a single RG type against the spec.
///
/// Three branches, all routed through [`classify_existing`] to keep
/// the equivalence predicate single-source-of-truth:
///
/// * `get_type` returns the row → classify it.
/// * `get_type` returns `NotFound` → `create_type` → on success done,
///   on `AlreadyExists` re-read and classify the peer's row.
/// * `get_type` / `create_type` returns transport failure →
///   `ServiceUnavailable`.
#[allow(
    clippy::cognitive_complexity,
    reason = "flat match-based dispatch across the FEATURE-pinned idempotent-registration matrix; splitting into sub-functions would obscure the deterministic outcome branches the tests check"
)]
async fn register_one(
    client: &Arc<dyn ResourceGroupClient + Send + Sync>,
    ctx: &SecurityContext,
    spec: &TypeSpec,
) -> Result<RegistrationOutcome, RegistrationError> {
    // Step 1: query existing type definition. The trait boundary is
    // `CanonicalError` (ADR 0005); project into the typed SDK view to
    // dispatch on `NotFound` vs. any transport failure.
    let existing = match client
        .get_type(ctx, spec.code)
        .await
        .map_err(ResourceGroupError::from)
    {
        Ok(t) => Some(t),
        Err(ResourceGroupError::NotFound { .. }) => None,
        Err(e) => {
            emit_metric(
                AM_DEPENDENCY_HEALTH,
                MetricKind::Counter,
                &[
                    ("target", "resource_group"),
                    ("op", "get_type"),
                    ("outcome", "error"),
                ],
            );
            return Err(RegistrationError::ServiceUnavailable(format!(
                "resource-group unreachable: {e}"
            )));
        }
    };

    if let Some(existing) = existing {
        return classify_existing(spec, &existing);
    }

    // Type is absent -- register it.
    match client
        .create_type(ctx, spec.to_create_request())
        .await
        .map_err(ResourceGroupError::from)
    {
        Ok(_) => {
            emit_metric(
                AM_DEPENDENCY_HEALTH,
                MetricKind::Counter,
                &[
                    ("target", "resource_group"),
                    ("op", "register_user_group_type"),
                    ("outcome", "registered_new"),
                ],
            );
            info!(
                target: "am.user_groups",
                code = spec.code,
                "user-groups RG type registered"
            );
            Ok(RegistrationOutcome::RegisteredNew)
        }
        // Race condition: another AM instance registered between our
        // step-1 `get_type` and `create_type`. Re-read and run the
        // SAME equivalence check the step-1 path uses -- a peer that
        // beat us to the registration with DIVERGENT traits must
        // surface as `DivergentSchema` instead of being silently
        // accepted. If the re-read fails for transport reasons,
        // surface that as `ServiceUnavailable` so the operator can
        // retry init; do NOT default to `AlreadyPresent` because the
        // traits-equivalence invariant is unverified.
        Err(ResourceGroupError::AlreadyExists { .. }) => {
            match client.get_type(ctx, spec.code).await {
                Ok(racy) => {
                    info!(
                        target: "am.user_groups",
                        code = spec.code,
                        "user-groups RG type registered by concurrent init; re-reading to verify traits"
                    );
                    classify_existing(spec, &racy)
                }
                Err(e) => {
                    emit_metric(
                        AM_DEPENDENCY_HEALTH,
                        MetricKind::Counter,
                        &[
                            ("target", "resource_group"),
                            ("op", "register_user_group_type_race_reread"),
                            ("outcome", "error"),
                        ],
                    );
                    Err(RegistrationError::ServiceUnavailable(format!(
                        "resource-group: unable to verify race-winner traits for type \
                         '{}': {e}",
                        spec.code
                    )))
                }
            }
        }
        Err(e) => {
            emit_metric(
                AM_DEPENDENCY_HEALTH,
                MetricKind::Counter,
                &[
                    ("target", "resource_group"),
                    ("op", "create_type"),
                    ("outcome", "error"),
                ],
            );
            Err(RegistrationError::ServiceUnavailable(format!(
                "resource-group: failed to register type '{}': {e}",
                spec.code
            )))
        }
    }
}
// @cpt-end:cpt-cf-account-management-algo-user-groups-rg-type-schema-registration:p1:inst-algo-rgreg-full

/// Equivalence predicate: every trait `spec` declares MUST be honoured
/// by `existing`. Shared by the step-1 "type already exists" path and
/// the `AlreadyExists` race-loser path so both honour the same
/// "AM MUST NOT auto-repair divergent traits" invariant.
///
/// The check is **inclusive-equivalence**: RG is allowed to seed
/// broader policies AM does not control (extra parent types, extra
/// membership types, etc.), so we check that every spec entry is
/// present in the observed row, not that the lists are identical.
/// The cascade hook's "every descendant of a root user-group is itself
/// a user-group" invariant does not rely on RG-side policy width —
/// it relies on AM being the sole writer of user-group rows (every
/// user-group AM creates is parented under another user-group it
/// itself created), and RG enforcing `allowed_parent_types` at
/// `create_group` time. A broader RG seed widens what RG *would*
/// admit if asked, but AM never asks for a non-user-group parent.
///
/// `can_be_root` is checked exactly: a `false` on RG's side defeats
/// AM's ability to create root groups of this type, so even a
/// "broader" RG policy that disabled root creation diverges from
/// AM's expectation.
fn classify_existing(
    spec: &TypeSpec,
    existing: &ResourceGroupType,
) -> Result<RegistrationOutcome, RegistrationError> {
    if existing.can_be_root != spec.can_be_root {
        return Err(divergent(spec, existing, "can_be_root mismatch"));
    }
    for parent in &spec.allowed_parent_types {
        if !existing.allowed_parent_types.iter().any(|p| p == *parent) {
            return Err(divergent(
                spec,
                existing,
                &format!("missing allowed_parent_type `{parent}`"),
            ));
        }
    }
    for membership in &spec.allowed_membership_types {
        if !existing
            .allowed_membership_types
            .iter()
            .any(|m| m == *membership)
        {
            return Err(divergent(
                spec,
                existing,
                &format!("missing allowed_membership_type `{membership}`"),
            ));
        }
    }

    emit_metric(
        AM_DEPENDENCY_HEALTH,
        MetricKind::Counter,
        &[
            ("target", "resource_group"),
            ("op", "register_user_group_type"),
            ("outcome", "already_present"),
        ],
    );
    info!(
        target: "am.user_groups",
        code = spec.code,
        "user-groups RG type already registered with equivalent traits"
    );
    Ok(RegistrationOutcome::AlreadyPresent)
}

fn divergent(spec: &TypeSpec, existing: &ResourceGroupType, reason: &str) -> RegistrationError {
    RegistrationError::DivergentSchema(format!(
        "RG type '{}' exists but traits diverge: {reason}; observed can_be_root={}, \
         allowed_parent_types={:?}, allowed_membership_types={:?}",
        spec.code,
        existing.can_be_root,
        existing.allowed_parent_types,
        existing.allowed_membership_types
    ))
}
