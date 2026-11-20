//! Tests for the enforcement point's fail-closed contract.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6 — *a request whose
//! authorization decision cannot be obtained is denied rather than allowed* —
//! and CDSL steps `inst-gf-authz-4` / `inst-gf-authz-5`.
//!
//! Three things are pinned here: the projection every failure of
//! `PermissionEnforcer::access_scope` passes through; against a recording
//! decision point, what the enforcer asks and how it answers a refusal; and,
//! through the REST surface over the same decision point, which permission
//! each route asks.

use std::sync::Arc;

use authz_resolver_sdk::pep::ConstraintCompileError;
use authz_resolver_sdk::{AccessDenial, AuthZResolverApi, EnforcerError};
use serde_json::Value;
use toolkit_canonical_errors::{CanonicalError, Problem};
use uuid::Uuid;

use super::{Permission, PermissionEnforcer, deny, optional, resource};
use crate::domain::error::DomainError;
use crate::test_support::{RecordingPdp, enforcer_over};
use settings_service_sdk::gts;

fn problem_of(err: DomainError) -> Value {
    serde_json::to_value(Problem::from(CanonicalError::from(err))).expect("serializes")
}

/// Every known shape `EnforcerError` can take, kept as one list. An outcome
/// class added upstream maps to an internal failure, not to this denial.
fn every_failure() -> Vec<EnforcerError> {
    vec![
        EnforcerError::AccessDenied(AccessDenial::Pdp { reason: None }),
        // `EvaluationFailed` carries the canonical error the resolver surfaced;
        // which category it was does not matter to the projection, only that it
        // never becomes an allow.
        EnforcerError::EvaluationFailed(CanonicalError::service_unavailable().create()),
        EnforcerError::EvaluationFailed(CanonicalError::service_unavailable().create()),
        EnforcerError::CompileFailed(ConstraintCompileError::ConstraintsRequiredButAbsent),
        EnforcerError::CompileFailed(ConstraintCompileError::AllConstraintsFailed {
            reason: "unsupported constraint operator".to_owned(),
        }),
    ]
}

#[test]
fn every_enforcement_failure_denies() {
    // The core of the contract. An explicit deny, an unreachable decision
    // point, and an uncompilable constraint set are all denials — there is no
    // failure shape that lets a request through.
    for err in every_failure() {
        let label = format!("{err:?}");
        assert!(
            matches!(
                deny(Permission::CategoryRead, &err),
                DomainError::Unauthorized { .. }
            ),
            "`{label}` must deny"
        );
    }
}

#[test]
fn an_unreachable_decision_point_denies_rather_than_defaults() {
    // Step 4 of the enforcement algorithm, stated as its own test because it is
    // the one an implementation is most tempted to get wrong: an outage in the
    // policy service must not become an outage-shaped allow.
    let err = EnforcerError::EvaluationFailed(CanonicalError::service_unavailable().create());
    assert!(matches!(
        deny(Permission::CategoryRead, &err),
        DomainError::Unauthorized { .. }
    ));
}

#[test]
fn uncompilable_constraints_deny_rather_than_widen() {
    // A decision arrived, but its constraints could not become a predicate.
    // Proceeding would mean applying *no* predicate — broader than anything the
    // policy point could have returned.
    let err = EnforcerError::CompileFailed(ConstraintCompileError::AllConstraintsFailed {
        reason: "unknown constraint operator".to_owned(),
    });
    assert!(matches!(
        deny(Permission::CategoryRead, &err),
        DomainError::Unauthorized { .. }
    ));
}

#[test]
fn all_denials_are_indistinguishable_on_the_wire() {
    // A caller must not learn *why* it was denied. Told apart, the three known
    // outcomes reveal whether the policy service is down, whether a policy names them
    // specifically, and — combined with a probe — whether the resource exists.
    let rendered: Vec<Value> = every_failure()
        .into_iter()
        .map(|err| problem_of(deny(Permission::CategoryRead, &err)))
        .collect();
    let first = &rendered[0];
    for other in &rendered[1..] {
        assert_eq!(first, other, "denials must not be distinguishable");
    }
}

#[test]
fn a_denial_discloses_nothing_about_the_target() {
    let doc = problem_of(deny(
        Permission::CategoryRead,
        &EnforcerError::AccessDenied(AccessDenial::Pdp { reason: None }),
    ));
    let rendered = serde_json::to_string(&doc).expect("serializes");
    assert_eq!(doc["status"], 403);
    for leak in [
        "unreachable",
        "connection",
        "constraint",
        "not found",
        "exists",
    ] {
        assert!(
            !rendered.contains(leak),
            "`{leak}` must not appear in a denial: {rendered}"
        );
    }
}

#[test]
fn the_resource_vocabulary_is_the_gts_type_ids() {
    // Policies are written against these ids, so they are a contract with the
    // policy decision point rather than an internal naming choice.
    assert_eq!(
        resource::DECLARATION.name(),
        "gts.cf.core.settings.declaration.v1~"
    );
    assert_eq!(resource::VALUE.name(), "gts.cf.core.settings.value.v1~");
    assert_eq!(
        resource::CATEGORY.name(),
        "gts.cf.core.settings.category.v1~"
    );
}

#[test]
fn a_denial_names_the_resource_that_was_enforced() {
    // Found by running the service: every denial used to carry the declaration
    // type, so a category request was refused with the wrong resource in its
    // context -- not a leak, but wrong metadata for anyone reading a log or an
    // audit trail.
    for permission in Permission::ALL {
        let doc = problem_of(deny(
            *permission,
            &EnforcerError::AccessDenied(AccessDenial::Pdp { reason: None }),
        ));
        assert_eq!(
            doc["context"]["resource_type"],
            permission.resource().name(),
            "{permission:?}"
        );
    }
    for (permission, expected) in [
        (Permission::CategoryRead, gts::CATEGORY_SCHEMA),
        (Permission::ValueRead, gts::VALUE_SCHEMA),
        (Permission::DeclarationRead, gts::DECLARATION_SCHEMA),
    ] {
        let doc = problem_of(deny(
            permission,
            &EnforcerError::AccessDenied(AccessDenial::Pdp { reason: None }),
        ));
        assert_eq!(doc["context"]["resource_type"], expected);
    }
}

/// An optional decision (the `pii` entitlement, the category counts) degrades
/// only on a known refusal. Any other failure — the internal failure an
/// unknown enforcer outcome maps to — must stop the request, not read as a
/// masked or absent answer.
#[test]
fn an_optional_decision_degrades_only_on_a_refusal() {
    let refused = optional(Err(deny(
        Permission::ValueRead,
        &EnforcerError::AccessDenied(AccessDenial::Pdp { reason: None }),
    )));
    assert!(matches!(refused, Ok(None)), "{refused:?}");

    let internal = optional(Err(DomainError::Internal {
        diagnostic: "authorization failed with an unknown outcome".to_owned(),
    }));
    assert!(
        matches!(internal, Err(DomainError::Internal { .. })),
        "an internal failure is not a refusal: {internal:?}"
    );

    let granted = optional(Ok(toolkit_security::AccessScope::allow_all()));
    assert!(matches!(granted, Ok(Some(_))), "{granted:?}");
}

// ---- `settings_authz_denial_total`: counted at the enforcement point, before
// the refusal is folded into the one `Unauthorized`.

mod counted {
    use std::sync::Arc;

    use authz_resolver_sdk::{AccessRequest, PolicyEnforcer};
    use uuid::Uuid;

    use super::{DomainError, EnforcerError, Permission, PermissionEnforcer, problem_of, resource};
    use crate::test_support::{
        RecordingSecurityMetrics, UncompilablePdp, context_for, denying_enforcer,
        uncompilable_enforcer, unreachable_enforcer,
    };

    async fn refused(
        enforcer: &PermissionEnforcer,
        metrics: &RecordingSecurityMetrics,
        permission: Permission,
    ) -> DomainError {
        enforcer
            .access_scope(metrics, &context_for(Uuid::new_v4()), permission, None)
            .await
            .expect_err("refused")
    }

    #[tokio::test]
    async fn a_deny_counts_one_denied_under_the_operation_asked() {
        let metrics = RecordingSecurityMetrics::default();
        let err = refused(&denying_enforcer(), &metrics, Permission::ValueWrite).await;
        assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
        assert_eq!(metrics.denials(), vec![("write", "denied")]);
    }

    #[tokio::test]
    async fn an_unreachable_decision_point_counts_one_unavailable() {
        let metrics = RecordingSecurityMetrics::default();
        let err = refused(&unreachable_enforcer(), &metrics, Permission::ValueRead).await;
        assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
        assert_eq!(metrics.denials(), vec![("read", "unavailable")]);
    }

    #[tokio::test]
    async fn uncompilable_constraints_count_one_constraints_refusal() {
        // The double really is one the enforcer cannot compile.
        let direct = PolicyEnforcer::new(Arc::new(UncompilablePdp))
            .access_scope_with(
                &context_for(Uuid::new_v4()),
                &resource::VALUE,
                "delete",
                None,
                &AccessRequest::new().require_constraints(false),
            )
            .await;
        assert!(
            matches!(direct, Err(EnforcerError::CompileFailed(_))),
            "{direct:?}"
        );

        let metrics = RecordingSecurityMetrics::default();
        let err = refused(
            &uncompilable_enforcer(),
            &metrics,
            Permission::DeclarationDelete,
        )
        .await;
        assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
        assert_eq!(metrics.denials(), vec![("delete", "constraints")]);
    }

    #[tokio::test]
    async fn the_response_is_the_same_unauthorized_for_all_three_reasons() {
        let metrics = RecordingSecurityMetrics::default();
        let bodies = [
            problem_of(refused(&denying_enforcer(), &metrics, Permission::ValueRead).await),
            problem_of(refused(&unreachable_enforcer(), &metrics, Permission::ValueRead).await),
            problem_of(refused(&uncompilable_enforcer(), &metrics, Permission::ValueRead).await),
        ];
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(bodies[1], bodies[2]);
        assert_eq!(
            metrics.denials(),
            vec![
                ("read", "denied"),
                ("read", "unavailable"),
                ("read", "constraints"),
            ],
            "the metric is the operator's view; the response tells the caller nothing"
        );
    }

    #[tokio::test]
    async fn a_probe_and_the_enforcement_point_refuse_alike_and_only_the_latter_counts() {
        let ctx = context_for(Uuid::new_v4());
        let enforcers = [
            ("deny", denying_enforcer()),
            ("unreachable", unreachable_enforcer()),
            ("uncompilable", uncompilable_enforcer()),
        ];
        for (kind, enforcer) in &enforcers {
            let probed = enforcer
                .optional_scope(&ctx, Permission::ValueRead)
                .await
                .expect("a known refusal is not an error");
            let metrics = RecordingSecurityMetrics::default();
            let counted = refused(enforcer, &metrics, Permission::ValueRead).await;
            // The probe reads the refusal as an absent scope; the enforcement
            // point fails the request with the one `Unauthorized`.
            assert!(probed.is_none(), "{kind}: {probed:?}");
            assert!(
                matches!(counted, DomainError::Unauthorized { .. }),
                "{kind}: {counted:?}"
            );
            // The enforcement point counts the refusal once; nothing else moves.
            assert_eq!(
                metrics.denials().len(),
                1,
                "{kind}: {:?}",
                metrics.denials()
            );
            assert!(metrics.secret_uses().is_empty(), "{kind}");
            assert!(metrics.audit_events().is_empty(), "{kind}");
        }
    }

    #[tokio::test]
    async fn the_unmasked_entitlement_is_a_probe_answered_by_masking() {
        // Refused three ways, the answer is the same `false` — mask — and
        // there is no metrics port to count on: the entitlement never
        // reaches the enforcement point.
        let ctx = context_for(Uuid::new_v4());
        for (kind, enforcer) in [
            ("deny", denying_enforcer()),
            ("unreachable", unreachable_enforcer()),
            ("uncompilable", uncompilable_enforcer()),
        ] {
            assert!(
                !enforcer
                    .may_read_unmasked(&ctx)
                    .await
                    .expect("a known refusal masks, it is not an error"),
                "{kind}"
            );
        }
    }
}

#[test]
fn every_permission_names_a_resource_type_the_gear_publishes() {
    // The role-definition validator resolves a rule's target type through the
    // types registry. The gear registers its resource types as the
    // SDK's stubs and no other; a permission whose resource were spelled any other way would
    // be published pointing at a type nobody registers, and no role could be
    // composed from it.
    let published = [
        gts::CATEGORY_SCHEMA,
        gts::DECLARATION_SCHEMA,
        gts::VALUE_SCHEMA,
    ];
    let collected: Vec<String> = settings_service_sdk::gts_declarations()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    for id in published {
        assert!(
            collected.iter().any(|c| c == id),
            "`{id}` is not in the SDK collector the gear publishes"
        );
    }
    for permission in Permission::ALL {
        let resource = permission.resource();
        assert!(
            published.contains(&resource.name()),
            "{permission:?} names `{}`, which the gear does not publish",
            resource.name()
        );
        assert_eq!(resource.name(), permission.resource_id(), "{permission:?}");
    }
}

#[test]
fn a_counted_permission_is_labelled_with_its_own_action() {
    // The `operation` label of `settings_authz_denial_total` is the action the
    // decision point was asked for. The unmasked-read entitlement is the one
    // permission without a label: it is a probe, never counted.
    for &permission in Permission::ALL {
        match permission.operation() {
            Some(operation) => {
                assert_eq!(operation.as_str(), permission.action(), "{permission:?}");
            }
            None => assert_eq!(permission, Permission::ValueReadUnmasked),
        }
    }
}

#[test]
fn a_permission_keeps_its_wire_spelling() {
    // A policy is written against these strings; renaming an action would
    // silently revoke every grant that names it. Written out in full so a
    // change to the table fails here, whatever the catalogue says.
    use super::Permission as P;
    let expected = [
        (P::CategoryRead, &resource::CATEGORY, "read"),
        (P::CategoryCreate, &resource::CATEGORY, "create"),
        (P::CategoryUpdate, &resource::CATEGORY, "update"),
        (P::CategoryDelete, &resource::CATEGORY, "delete"),
        (P::DeclarationRead, &resource::DECLARATION, "read"),
        (P::DeclarationCreate, &resource::DECLARATION, "create"),
        (P::DeclarationUpdate, &resource::DECLARATION, "update"),
        (P::DeclarationDelete, &resource::DECLARATION, "delete"),
        (P::ValueRead, &resource::VALUE, "read"),
        (P::ValueReadUnmasked, &resource::VALUE, "read_unmasked"),
        (P::ValueWrite, &resource::VALUE, "write"),
        (P::ValueDelegate, &resource::VALUE, "delegate"),
    ];
    assert_eq!(expected.len(), P::ALL.len(), "a permission is not pinned");
    for (permission, resource, action) in expected {
        assert_eq!(
            permission.resource().name(),
            resource.name(),
            "{permission:?}"
        );
        assert_eq!(permission.action(), action, "{permission:?}");
    }
}

fn caller() -> toolkit_security::SecurityContext {
    crate::test_support::context_for(Uuid::from_u128(0x7e))
}

#[tokio::test]
async fn the_enforcer_asks_exactly_the_permission_it_was_given() {
    // The wrapper is the only road from a handler to the decision point, so
    // what it sends is what a policy is matched against: the permission's
    // resource type and action, the id the handler named, and nothing added.
    let pdp = Arc::new(RecordingPdp::default());
    let enforcer = enforcer_over(&pdp);
    let ctx = caller();
    let named = Uuid::from_u128(0x5e77);

    for &permission in Permission::ALL {
        for resource_id in [None, Some(named)] {
            enforcer
                .access_scope(
                    &crate::domain::ports::NoMetrics,
                    &ctx,
                    permission,
                    resource_id,
                )
                .await
                .unwrap_or_else(|err| panic!("{permission:?} is allowed: {err:?}"));

            let asked = pdp.take();
            assert_eq!(asked.len(), 1, "{permission:?}: one question per call");
            let request = &asked[0];
            assert_eq!(
                request.resource.resource_type,
                permission.resource().name(),
                "{permission:?}"
            );
            assert_eq!(request.action.name, permission.action(), "{permission:?}");
            assert_eq!(request.resource.id, resource_id, "{permission:?}");
            assert!(
                request.resource.properties.is_empty(),
                "{permission:?}: no resource property is sent"
            );
            assert!(
                !request.context.require_constraints,
                "{permission:?}: constraints are not required"
            );
        }
    }
}

#[tokio::test]
async fn a_refused_or_unobtainable_decision_names_each_permissions_own_resource() {
    // The fail-closed contract through the enforcer, for every permission: a
    // `false` decision and a decision point that cannot be reached both end
    // as the same `Unauthorized`, naming the resource of the permission
    // asked. How a refusal is counted is pinned once in `counted`; what is
    // pinned here per permission is the name, and that the unmasked-read
    // entitlement, which has no operation, is never a label even when it is
    // asked through the counted method.
    use crate::test_support::{RecordingSecurityMetrics, denying_enforcer, unreachable_enforcer};
    let ctx = caller();
    for (what, enforcer) in [
        ("deny", denying_enforcer()),
        ("unreachable", unreachable_enforcer()),
    ] {
        for &permission in Permission::ALL {
            let metrics = RecordingSecurityMetrics::default();
            let err = enforcer
                .access_scope(&metrics, &ctx, permission, None)
                .await
                .expect_err("no scope without a positive decision");
            let DomainError::Unauthorized { resource } = err else {
                panic!("{what}, {permission:?}: expected a denial, got {err:?}");
            };
            assert_eq!(
                resource,
                permission.resource().name(),
                "{what}, {permission:?}"
            );
            if permission.operation().is_none() {
                assert!(
                    metrics.denials().is_empty(),
                    "{what}, {permission:?}: an entitlement is not counted"
                );
            }
        }
    }
}

// ── What each route asks ─────────────────────────────────────────────────────

/// One route of the surface: a request that reaches its handler, and what the
/// handler must ask the decision point while it answers.
struct Route {
    /// The operation as the registry keys it: `METHOD:/path/template`.
    operation: &'static str,
    /// The path and query the request is sent to.
    uri: String,
    body: Option<Value>,
    /// The tenant whose stored value's state tag the request carries as its
    /// `If-Match`; without one it carries `absent`.
    tag_at: Option<Uuid>,
    /// The permissions asked, in the order of their first question.
    asks: &'static [Permission],
    /// The id the question of the route's own permission, the first of
    /// `asks`, names: the path's `{id}` on an `{id}` route, none elsewhere.
    /// Every other question the route asks names none.
    resource_id: Option<Uuid>,
}

impl Route {
    fn carrying_the_tag_at(mut self, tenant: Uuid) -> Self {
        self.tag_at = Some(tenant);
        self
    }
}

fn route(
    operation: &'static str,
    uri: impl Into<String>,
    body: Option<Value>,
    asks: &'static [Permission],
    resource_id: Option<Uuid>,
) -> Route {
    Route {
        operation,
        uri: uri.into(),
        body,
        tag_at: None,
        asks,
        resource_id,
    }
}

const CATEGORIES: &str = "/settings-service/v1/categories";
const DECLARATIONS: &str = "/settings-service/v1/declarations";
const SETTINGS: &str = "/settings-service/v1/settings";

/// What the table's requests name: one public setting nobody has set, its
/// category, and two tenants of the harness's tree.
struct Surface {
    root: Uuid,
    child: Uuid,
    category: Uuid,
    declaration: Uuid,
    /// The setting's key.
    key: String,
    /// The setting's own path, the key percent-encoded.
    setting: String,
    /// An id nothing is stored under: the gate is asked before the row is
    /// looked for, and the mutation it guards then finds nothing to change.
    absent: Uuid,
}

/// The categories and the declarations: each operation on its own resource
/// type. The category reads carry the tallies of the settings filed under a
/// category, which are values read; the revival report reads values too.
fn definition_routes(s: &Surface) -> Vec<Route> {
    use Permission as P;
    use serde_json::json;
    let (category, declaration, absent) = (s.category, s.declaration, s.absent);
    let declared = json!({
        "value_type_id": crate::test_support::BOOL,
        "vendor": "acme",
        "name": "tls_required",
        "category_id": category,
        "default_value": true,
        "scope_class": "cascading",
    });
    vec![
        route(
            "GET:/settings-service/v1/categories",
            CATEGORIES,
            None,
            &[P::CategoryRead, P::ValueRead],
            None,
        ),
        route(
            "GET:/settings-service/v1/categories/{id}",
            format!("{CATEGORIES}/{category}"),
            None,
            &[P::CategoryRead, P::ValueRead],
            Some(category),
        ),
        route(
            "POST:/settings-service/v1/categories",
            CATEGORIES,
            Some(json!({ "key": "security", "name": "Security", "sort_order": 1 })),
            &[P::CategoryCreate],
            None,
        ),
        route(
            "PATCH:/settings-service/v1/categories/{id}",
            format!("{CATEGORIES}/{absent}"),
            Some(json!({ "name": "Renamed" })),
            &[P::CategoryUpdate],
            Some(absent),
        ),
        route(
            "DELETE:/settings-service/v1/categories/{id}",
            format!("{CATEGORIES}/{absent}"),
            None,
            &[P::CategoryDelete],
            Some(absent),
        ),
        route(
            "GET:/settings-service/v1/declarations",
            DECLARATIONS,
            None,
            &[P::DeclarationRead],
            None,
        ),
        route(
            "GET:/settings-service/v1/declarations/{id}",
            format!("{DECLARATIONS}/{declaration}"),
            None,
            &[P::DeclarationRead],
            Some(declaration),
        ),
        route(
            "POST:/settings-service/v1/declarations",
            DECLARATIONS,
            Some(declared.clone()),
            &[P::DeclarationCreate],
            None,
        ),
        route(
            "POST:/settings-service/v1/declarations/revival-preview",
            format!("{DECLARATIONS}/revival-preview"),
            Some(declared),
            &[P::DeclarationCreate, P::ValueRead],
            None,
        ),
        route(
            "PATCH:/settings-service/v1/declarations/{id}",
            format!("{DECLARATIONS}/{absent}"),
            Some(json!({ "description": "Renamed" })),
            &[P::DeclarationUpdate],
            Some(absent),
        ),
        route(
            "DELETE:/settings-service/v1/declarations/{id}",
            format!("{DECLARATIONS}/{absent}"),
            None,
            &[P::DeclarationDelete],
            Some(absent),
        ),
        route(
            "POST:/settings-service/v1/declarations/{id}/purge",
            format!("{DECLARATIONS}/{absent}/purge"),
            None,
            &[P::DeclarationDelete],
            Some(absent),
        ),
    ]
}

/// The reads of values, and tenant access. `read_unmasked` is asked whatever
/// the content by the search; the other reads ask it only when the answer
/// carries something it governs, and over a public setting nobody has set
/// they do not. Reading a restriction is a value read; changing one is
/// `delegate`, never `write`, and the set answers with the restriction it
/// stored, whose setter is an identity `read_unmasked` governs.
fn read_and_access_routes(s: &Surface) -> Vec<Route> {
    use Permission as P;
    use serde_json::json;
    let (setting, child) = (&s.setting, s.child);
    vec![
        route(
            "GET:/settings-service/v1/settings",
            SETTINGS,
            None,
            &[P::ValueRead],
            None,
        ),
        route(
            "GET:/settings-service/v1/settings/review",
            format!("{SETTINGS}/review"),
            None,
            &[P::ValueRead],
            None,
        ),
        route(
            "GET:/settings-service/v1/settings/{key}",
            setting.clone(),
            None,
            &[P::ValueRead],
            None,
        ),
        route(
            "GET:/settings-service/v1/settings/{key}/history",
            format!("{setting}/history"),
            None,
            &[P::ValueRead],
            None,
        ),
        route(
            "GET:/settings-service/v1/search",
            "/settings-service/v1/search?q=proxy",
            None,
            &[P::ValueRead, P::ValueReadUnmasked],
            None,
        ),
        route(
            "GET:/settings-service/v1/settings/{key}/permissions",
            format!("{setting}/permissions?tenant={child}"),
            None,
            &[P::ValueRead],
            None,
        ),
        route(
            "GET:/settings-service/v1/settings/{key}/permissions/all",
            format!("{setting}/permissions/all"),
            None,
            &[P::ValueRead],
            None,
        ),
        route(
            "PUT:/settings-service/v1/settings/{key}/permissions",
            format!("{setting}/permissions?tenant={child}"),
            Some(json!({ "access": "read_only" })),
            &[P::ValueDelegate, P::ValueReadUnmasked],
            None,
        ),
        route(
            "DELETE:/settings-service/v1/settings/{key}/permissions",
            format!("{setting}/permissions?tenant={child}"),
            None,
            &[P::ValueDelegate],
            None,
        ),
    ]
}

/// The write surface of values, `validate` and `impact` with it: they render
/// a value in their answer and ask `read_unmasked` to do it. A write renders
/// one only when it goes through, so each one here meets the state its
/// `If-Match` names: the set and the clone find nothing stored, the removal
/// and the revert carry the tag of the row those two left.
fn value_write_routes(s: &Surface) -> Vec<Route> {
    use Permission as P;
    use serde_json::json;
    let (setting, root, child) = (&s.setting, s.root, s.child);
    vec![
        route(
            "POST:/settings-service/v1/settings/{key}/validate",
            format!("{setting}/validate"),
            Some(json!({ "value": false })),
            &[P::ValueRead, P::ValueReadUnmasked],
            None,
        ),
        route(
            "POST:/settings-service/v1/settings/{key}/impact",
            format!("{setting}/impact"),
            Some(json!({ "value": false })),
            &[P::ValueRead, P::ValueReadUnmasked],
            None,
        ),
        route(
            "POST:/settings-service/v1/settings/{key}/secret-stage",
            format!("{setting}/secret-stage"),
            Some(json!({ "value": "hunter2" })),
            &[P::ValueWrite],
            None,
        ),
        route(
            "PUT:/settings-service/v1/settings/{key}/value",
            format!("{setting}/value"),
            Some(json!({ "value": false })),
            &[P::ValueWrite, P::ValueReadUnmasked],
            None,
        ),
        route(
            "POST:/settings-service/v1/settings/{key}/value/clone",
            format!("{setting}/value/clone?tenant={child}"),
            Some(json!({ "from": root })),
            &[P::ValueRead, P::ValueWrite, P::ValueReadUnmasked],
            None,
        ),
        route(
            "DELETE:/settings-service/v1/settings/{key}/value",
            format!("{setting}/value"),
            None,
            &[P::ValueWrite, P::ValueReadUnmasked],
            None,
        )
        .carrying_the_tag_at(root),
        route(
            "POST:/settings-service/v1/settings/{key}/value/revert",
            format!("{setting}/value/revert?tenant={child}"),
            None,
            &[P::ValueWrite, P::ValueReadUnmasked],
            None,
        )
        .carrying_the_tag_at(child),
        route(
            "POST:/settings-service/v1/settings/batch",
            format!("{SETTINGS}/batch"),
            Some(json!({ "changes": [
                { "key": s.key, "value": false, "if_match": "absent" }
            ] })),
            &[P::ValueWrite, P::ValueReadUnmasked],
            None,
        ),
    ]
}

#[tokio::test]
async fn every_route_asks_the_decision_point_for_its_own_permission() {
    // The enforcer can only be asked for a `Permission`, and nothing in the
    // type system says which one a handler picks: `set_access` asking `write`
    // instead of `delegate` compiles and answers. Each route is driven once
    // over a decision point that allows everything and keeps what it was
    // asked; the pairs it saw are the route's row of the authorization table
    // (DESIGN §4.8).
    let pdp = Arc::new(RecordingPdp::default());
    let h = {
        let pdp: Arc<dyn AuthZResolverApi> = pdp.clone();
        crate::test_support::RestHarness::with_pdp(pdp).await
    };
    let root = h.inner.tree.root;
    let declaration = h
        .inner
        .declare("proxy_enabled", "cascading", serde_json::json!(true))
        .await;
    let key = h.inner.key("proxy_enabled").to_string();
    let setting = format!("{SETTINGS}/{}", key.replace('~', "%7E"));
    let surface = Surface {
        root,
        child: h.inner.tree.a,
        category: h.inner.category_id(),
        declaration,
        key,
        setting,
        absent: Uuid::from_u128(0xab5e),
    };
    let setting = &surface.setting;

    let routes: Vec<Route> = definition_routes(&surface)
        .into_iter()
        .chain(read_and_access_routes(&surface))
        .chain(value_write_routes(&surface))
        .collect();

    for route in &routes {
        let (method, _) = route.operation.split_once(':').expect("METHOD:path");
        let tag = match route.tag_at {
            Some(tenant) => {
                let (_, read) = h.get(&format!("{setting}?tenant={tenant}"), root).await;
                read["etag"].as_str().expect("a state tag").to_owned()
            }
            None => "absent".to_owned(),
        };
        pdp.take();
        let answer = h
            .send(method, &route.uri, route.body.clone(), Some(&tag), root)
            .await;
        let expected: Vec<(String, String)> = route
            .asks
            .iter()
            .map(|p| (p.resource().name().to_owned(), p.action().to_owned()))
            .collect();
        let mut asked: Vec<(String, String)> = Vec::new();
        for request in pdp.take() {
            let pair = (request.resource.resource_type, request.action.name);
            // The route's own question names the row's id; any other names
            // none, so a handler that drops the path's id, or names one where
            // it should not, fails here.
            let named = if expected.first() == Some(&pair) {
                route.resource_id
            } else {
                None
            };
            assert_eq!(
                request.resource.id, named,
                "`{}` asked {pair:?} naming the wrong resource",
                route.operation
            );
            if !asked.contains(&pair) {
                asked.push(pair);
            }
        }
        assert_eq!(
            asked, expected,
            "`{}` answered {} after asking something else: {}",
            route.operation, answer.status, answer.body
        );
    }

    // A route added to the surface has to be added to the table.
    let mut registered: Vec<String> = h
        .openapi
        .operation_specs
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    registered.sort();
    let mut listed: Vec<String> = routes.iter().map(|r| r.operation.to_owned()).collect();
    listed.sort();
    assert_eq!(listed, registered, "the table lists every registered route");
}
