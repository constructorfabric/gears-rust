//! The browse filter's interpretation: what is accepted and what is refused.

use toolkit_odata::ast::Expr;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{TextMatch, gate_target, interpret, qualify};
use crate::api::rest::setting_dto::Reader;
use crate::domain::error::DomainError;
use crate::domain::resolution::ScopeTarget;
use crate::domain::search::MatchedField;
use crate::test_support::ResolutionHarness;

fn parse(raw: &str) -> Expr {
    toolkit_odata::parse_filter_string(raw)
        .expect("well-formed OData")
        .into_expr()
}

#[test]
fn needs_review_is_split_off_and_the_rest_selects_declarations() {
    let f = interpret(Some(&parse(
        "needs_review eq true and category_id eq 0f7a3a8a-3a67-4a6b-9a2d-2b3f6d6f1c11",
    )))
    .expect("accepted");
    assert!(f.needs_review);
    assert!(f.keys.is_none());
    assert!(matches!(f.declarations, Some(Expr::Compare(..))));

    let only = interpret(Some(&parse("needs_review eq true"))).expect("accepted");
    assert!(only.needs_review);
    assert!(only.declarations.is_none());
}

#[test]
fn a_key_set_is_remembered_for_per_key_outcomes() {
    let f = interpret(Some(&parse("key in ('a.v1~','b.v1~')"))).expect("accepted");
    assert_eq!(
        f.keys.as_deref(),
        Some(&["a.v1~".to_owned(), "b.v1~".to_owned()][..])
    );
    assert!(matches!(f.declarations, Some(Expr::In(..))));

    let one = interpret(Some(&parse("key eq 'a.v1~'"))).expect("accepted");
    assert_eq!(one.keys.as_deref(), Some(&["a.v1~".to_owned()][..]));
}

#[test]
fn an_unmapped_field_or_an_unsupported_operator_is_refused_not_ignored() {
    for raw in [
        "tenant eq 'x'",
        "needs_review eq false",
        "category_id ne 0f7a3a8a-3a67-4a6b-9a2d-2b3f6d6f1c11",
        "key eq 'a' or key eq 'b'",
        "contains(key, 'a')",
    ] {
        match interpret(Some(&parse(raw))) {
            Err(DomainError::Validation { field, .. }) => assert_eq!(field, "$filter", "{raw}"),
            other => panic!("{raw}: expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn two_declaration_selecting_clauses_stay_one_conjunction() {
    // Both halves select declarations, so both have to reach the repository —
    // dropping either would widen the page past what the client asked for.
    let f = interpret(Some(&parse(
        "key eq 'a.v1~' and category_id eq 0f7a3a8a-3a67-4a6b-9a2d-2b3f6d6f1c11",
    )))
    .expect("accepted");
    assert!(!f.needs_review);
    assert_eq!(f.keys.as_deref(), Some(&["a.v1~".to_owned()][..]));
    assert!(
        matches!(f.declarations, Some(Expr::And(..))),
        "both clauses survive the split: {:?}",
        f.declarations
    );
}

#[test]
fn a_conjunction_the_split_absorbs_entirely_leaves_no_declaration_filter() {
    // `needs_review` is a switch, not a column. When it is the whole filter
    // there is nothing left to narrow the declarations query with, and passing
    // an empty conjunction down would be a filter on nothing.
    let f = interpret(Some(&parse(
        "needs_review eq true and needs_review eq true",
    )))
    .expect("accepted");
    assert!(f.needs_review);
    assert!(f.declarations.is_none());
}

#[test]
fn in_is_refused_on_any_field_but_key() {
    // `key in (...)` exists to ask about a named set and report each member's
    // own outcome. No other field has per-member outcomes to report.
    match interpret(Some(&parse(
        "category_id in (0f7a3a8a-3a67-4a6b-9a2d-2b3f6d6f1c11)",
    ))) {
        Err(DomainError::Validation { field, message, .. }) => {
            assert_eq!(field, "$filter");
            assert!(message.contains("`key` only"), "got `{message}`");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn no_filter_browses_everything() {
    let f = interpret(None).expect("accepted");
    assert!(!f.needs_review && f.keys.is_none() && f.declarations.is_none());
}

fn caller(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .expect("context")
}

#[tokio::test]
async fn the_target_is_the_caller_or_a_reachable_descendant_and_nothing_else() {
    let h = ResolutionHarness::new().await;
    let t = &h.tree;

    // Omitted: the caller's own tenant; for the root that is platform scope.
    assert_eq!(
        gate_target(&h.resolver, &caller(t.root), None)
            .await
            .expect("own"),
        ScopeTarget::Platform
    );
    assert_eq!(
        gate_target(&h.resolver, &caller(t.a), None)
            .await
            .expect("own"),
        ScopeTarget::Tenant(t.a)
    );

    // A descendant is fine; a sibling, an ancestor and a standalone descendant are denied.
    assert_eq!(
        gate_target(&h.resolver, &caller(t.a), Some(t.b))
            .await
            .expect("descendant"),
        ScopeTarget::Tenant(t.b)
    );
    for (who, target) in [(t.a, t.c), (t.b, t.a), (t.a, t.s), (t.root, t.s)] {
        match gate_target(&h.resolver, &caller(who), Some(target)).await {
            Err(DomainError::Unauthorized { .. }) => {}
            other => panic!("{who} -> {target}: expected a denial, got {other:?}"),
        }
    }
}

async fn declaration_of(
    h: &ResolutionHarness,
    name: &str,
    classification: &str,
) -> crate::domain::declaration::Declaration {
    use crate::domain::declaration::DeclarationRepository as _;
    let id = h
        .declare_typed(
            name,
            "cascading",
            serde_json::json!("x"),
            crate::test_support::TEXT,
            classification,
        )
        .await;
    let conn = h.db.conn().expect("connection");
    crate::infra::storage::declaration_repo::DeclarationRepo
        .find_by_ids(&conn, &toolkit_security::AccessScope::allow_all(), &[id])
        .await
        .expect("declaration")
        .remove(0)
}

fn failure(kind: &str) -> DomainError {
    match kind {
        "unavailable" => DomainError::Unavailable {
            detail: "down".to_owned(),
        },
        "internal" => DomainError::Internal {
            diagnostic: "defect".to_owned(),
        },
        _ => DomainError::Retired {
            key: "k".to_owned(),
        },
    }
}

#[tokio::test]
async fn a_failed_candidate_is_decided_by_what_the_caller_could_have_seen() {
    let h = ResolutionHarness::new().await;
    let needle = crate::domain::search::Needle::parse("zzz").expect("needle");
    let public = declaration_of(&h, "open_one", "public").await;
    let secret = declaration_of(&h, "vault_one", "secret").await;
    let pii = declaration_of(&h, "contact_one", "pii").await;

    // An outage over a value the caller could have matched is the request's.
    for (declaration, entitled) in [(&public, false), (&pii, true)] {
        let outcome = qualify(
            declaration,
            TextMatch::No,
            Err(failure("unavailable")),
            &needle,
            entitled,
            Reader::Platform,
            &[],
        );
        assert!(matches!(outcome, Err(DomainError::Unavailable { .. })));
    }
    // A defect keeps its entry, as it does without `q`.
    let kept = qualify(
        &public,
        TextMatch::No,
        Err(failure("internal")),
        &needle,
        false,
        Reader::Platform,
        &[],
    )
    .expect("decided")
    .expect("kept");
    assert_eq!(kept.0.outcome, "error");
    assert!(kept.0.effective.is_none());
    // What the caller could never match is left to the text.
    for (declaration, entitled) in [(&secret, true), (&pii, false)] {
        for kind in ["unavailable", "internal"] {
            let outcome = qualify(
                declaration,
                TextMatch::No,
                Err(failure(kind)),
                &needle,
                entitled,
                Reader::Platform,
                &[],
            );
            assert!(matches!(outcome, Ok(None)), "{kind}");
        }
    }
    // A retired one qualifies by its text alone; a text match keeps any failure.
    assert!(matches!(
        qualify(
            &public,
            TextMatch::No,
            Err(failure("retired")),
            &needle,
            true,
            Reader::Platform,
            &[],
        ),
        Ok(None)
    ));
    for kind in ["retired", "unavailable", "internal"] {
        let outcome = qualify(
            &secret,
            TextMatch::In(MatchedField::Key),
            Err(failure(kind)),
            &needle,
            false,
            Reader::Platform,
            &[],
        );
        let (item, _) = outcome.expect("decided").expect("kept");
        // The failure entry names the field its text matched in.
        assert_eq!(item.matched_field.as_deref(), Some("key"), "{kind}");
    }
    // An entry kept for its own failure alone matched nothing: no field.
    assert_eq!(kept.0.matched_field, None);
}

#[tokio::test]
async fn a_qualifying_candidate_names_its_text_field_first_and_a_shown_value_as_value() {
    let h = ResolutionHarness::new().await;
    let needle = crate::domain::search::Needle::parse("lantern").expect("needle");
    let id = h
        .declare("lamp", "cascading", serde_json::json!("plain"))
        .await;
    h.set(id, h.tree.root, serde_json::json!("lantern-set"))
        .await;
    let declaration = h.load_declaration(id).await;
    let resolved = || async {
        h.resolve("lamp", ScopeTarget::Platform)
            .await
            .expect("resolves")
    };
    let field = |text: TextMatch, outcome| {
        qualify(
            &declaration,
            text,
            Ok(outcome),
            &needle,
            false,
            Reader::Platform,
            &[],
        )
        .expect("decided")
        .map(|(item, _)| item.matched_field)
    };

    // The value alone: `value`.
    assert_eq!(
        field(TextMatch::No, resolved().await),
        Some(Some("value".to_owned()))
    );
    // The text and the value: the text's field, whichever it is.
    for text in [
        MatchedField::Key,
        MatchedField::DisplayName,
        MatchedField::Description,
        MatchedField::CategoryName,
        MatchedField::DefaultValue,
    ] {
        assert_eq!(
            field(TextMatch::In(text), resolved().await),
            Some(Some(text.as_str().to_owned()))
        );
    }
    // The text in no field that can be named, and the shown value: the value
    // did match, and is what is named.
    assert_eq!(
        field(TextMatch::Unnamed, resolved().await),
        Some(Some("value".to_owned()))
    );
    // The text in no field that can be named, and nothing else: kept, with
    // no field guessed for it.
    let other = crate::domain::search::Needle::parse("zzz").expect("needle");
    let (item, _) = qualify(
        &declaration,
        TextMatch::Unnamed,
        Ok(resolved().await),
        &other,
        false,
        Reader::Platform,
        &[],
    )
    .expect("decided")
    .expect("kept by its text");
    assert_eq!(item.matched_field, None);
    assert_eq!(item.outcome, "resolved");
    // Neither: not on the page.
    let other = crate::domain::search::Needle::parse("zzz").expect("needle");
    let dropped = qualify(
        &declaration,
        TextMatch::No,
        Ok(resolved().await),
        &other,
        false,
        Reader::Platform,
        &[],
    );
    assert!(matches!(dropped, Ok(None)));
}

#[tokio::test]
async fn a_masked_value_qualifies_by_its_text_alone_and_is_never_named_as_value() {
    let h = ResolutionHarness::new().await;
    let needle = crate::domain::search::Needle::parse("lantern").expect("needle");
    let id = h
        .declare_typed(
            "contact",
            "cascading",
            serde_json::json!("x"),
            crate::test_support::TEXT,
            "pii",
        )
        .await;
    h.set_classified(
        id,
        h.tree.root,
        serde_json::json!("lantern@example.test"),
        "pii",
    )
    .await;
    let declaration = h.load_declaration(id).await;
    let run = |text: TextMatch, entitled: bool, outcome| {
        qualify(
            &declaration,
            text,
            Ok(outcome),
            &needle,
            entitled,
            Reader::Platform,
            &[],
        )
        .expect("decided")
    };
    let resolved = || async {
        h.resolve("contact", ScopeTarget::Platform)
            .await
            .expect("resolves")
    };

    // Masked for the caller, the value containing the text: no entry at all,
    // so no field can tell of it.
    assert!(run(TextMatch::No, false, resolved().await).is_none());
    // On the page by its text, it names the text's field and stays masked.
    let (item, _) = run(TextMatch::In(MatchedField::Key), false, resolved().await).expect("kept");
    assert_eq!(item.matched_field.as_deref(), Some("key"));
    assert!(item.effective.as_ref().is_some_and(|e| e.masked));
    // Shown to an entitled caller, the same value is a `value` match.
    let (item, _) = run(TextMatch::No, true, resolved().await).expect("kept");
    assert_eq!(item.matched_field.as_deref(), Some("value"));
    assert!(item.effective.as_ref().is_some_and(|e| !e.masked));
    // Kept by a text match no field names, the masked value is still not
    // named: the entry carries no field.
    let (item, _) = run(TextMatch::Unnamed, false, resolved().await).expect("kept");
    assert_eq!(item.matched_field, None);
    assert!(item.effective.as_ref().is_some_and(|e| e.masked));
}

// ── The path of one history record ───────────────────────────────────────────

mod record_path {
    use std::collections::HashMap;

    use uuid::Uuid;

    use super::super::record_path;
    use crate::api::rest::setting_dto::Reader;
    use crate::domain::resolution::Subtree;

    /// `reader -> target -> child -> grandchild`, with `stray` on no link.
    struct Fixture {
        reader: Uuid,
        target: Uuid,
        child: Uuid,
        grandchild: Uuid,
        stray: Uuid,
        walk: Subtree,
    }

    fn fixture() -> Fixture {
        let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        let (reader, target, child, grandchild, stray) = (ids[0], ids[1], ids[2], ids[3], ids[4]);
        Fixture {
            reader,
            target,
            child,
            grandchild,
            stray,
            walk: Subtree {
                order: vec![child, grandchild],
                parent: HashMap::from([(child, target), (grandchild, child)]),
                truncated: false,
            },
        }
    }

    #[test]
    fn a_record_below_the_target_extends_the_path_by_the_walks_links() {
        let f = fixture();
        let to_target = [f.reader, f.target];
        assert_eq!(
            record_path(&to_target, f.target, Some(&f.walk), Some(f.target)),
            vec![f.reader, f.target]
        );
        assert_eq!(
            record_path(&to_target, f.target, Some(&f.walk), Some(f.child)),
            vec![f.reader, f.target, f.child]
        );
        assert_eq!(
            record_path(&to_target, f.target, Some(&f.walk), Some(f.grandchild)),
            vec![f.reader, f.target, f.child, f.grandchild]
        );
    }

    #[test]
    fn a_record_at_the_target_is_on_the_targets_path_in_either_mode() {
        let f = fixture();
        let to_target = [f.reader, f.target];
        assert_eq!(
            record_path(&to_target, f.target, None, Some(f.target)),
            vec![f.reader, f.target]
        );
    }

    #[test]
    fn a_definition_record_and_a_tenant_the_walk_did_not_reach_are_on_no_path() {
        // Neither is ever shown to a tenant reader: an empty path holds no
        // actor tenant. The second cannot come out of the page query, which
        // lists the walk's tenants only; were it to, the actor is withheld
        // rather than judged on the target's path.
        let f = fixture();
        let to_target = [f.reader, f.target];
        for (walk, tenant) in [
            (Some(&f.walk), None),
            (None, None),
            (Some(&f.walk), Some(f.stray)),
            (None, Some(f.child)),
        ] {
            let path = record_path(&to_target, f.target, walk, tenant);
            assert!(path.is_empty(), "{path:?}");
            assert!(
                !Reader::Tenant(f.reader).is_shown(Some(f.reader), &path),
                "an actor at the reader tenant itself is withheld on no path"
            );
        }
    }
}
