//! The SKU pickers' reads through the door: the multi-id read `$filter=id in (…)` (ask 46), and
//! the picker keys `priced_in`, `not_priced_in` and `not_in_revision` on pricing's scoped sets
//! (P-D-246, ask 52).
use super::*;

/// Ask 46: `$filter=id in (…)` of 100 ids is one page read at the list's fixed two statements
/// (the fence expiry and the read), as a page of any other filter is, and answers the 100.
#[tokio::test]
async fn a_hundred_id_read_makes_the_lists_fixed_statements() {
    let (d, recorder) = recorded_door(120).await;
    let conn = d.state.db.conn().unwrap();
    let all = repo::page_skus(
        &conn,
        &d.scope,
        d.tenant,
        d.state.db.db().backend(),
        &repo::SkuListFilter::default(),
        &toolkit_odata::ODataQuery::default().with_limit(200),
    )
    .await
    .map_err(|_| "page")
    .unwrap();
    let wanted: Vec<String> = all
        .items
        .iter()
        .take(100)
        .map(|s| s.id.to_string())
        .collect();
    let filter = format!("id in ({})", wanted.join(", "));
    assert!(filter.len() < 8 * 1024, "100 ids fit the 8 KiB filter");
    let uri = list(&[("$filter", &filter), ("$top", "200")]);
    let seen = statements(&d, &recorder, &uri).await;
    for (i, (sql, binds)) in seen.iter().enumerate() {
        eprintln!(
            "statement {i} ({binds} binds): {}",
            &sql[..sql.len().min(200)]
        );
    }
    assert_eq!(seen.len(), 2, "the fence expiry and the read: {seen:#?}");
    let (_, page) = d.get(&uri).await;
    let mut got: Vec<String> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    got.sort();
    let mut expected = wanted.clone();
    expected.sort();
    assert_eq!(got, expected);
    assert!(page["page_info"]["next_cursor"].is_null(), "one page");
}

// ------------------------------------------------------------------ P-D-246: the picker keys

/// Five SKUs A to E; pricing's book `book` holds A, B and a SKU this tenant does not hold, its
/// revision `revision` names B and C, and its priced set is A and C.
async fn pickers_door() -> (Door, Arc<SetsPort>, Uuid, Uuid) {
    let d = Door::new().await;
    let mut id = std::collections::BTreeMap::new();
    for code in ["A", "B", "C", "D", "E"] {
        id.insert(code, d.sku(seed(code)).await);
    }
    let (book, revision) = (Uuid::new_v4(), Uuid::new_v4());
    let port = SetsPort::new(Answer::Answers, &[id["A"], id["C"]], &[]);
    port.scope(UsageScope::Book(book), &[id["A"], id["B"], Uuid::new_v4()]);
    port.scope(UsageScope::Revision(revision), &[id["B"], id["C"]]);
    d.state.hub.register::<dyn SkuUsageV1>(port.clone());
    (d, port, book, revision)
}

/// P-D-246: `priced_in` keeps the SKUs of pricing's book set, `not_priced_in` drops them, and
/// `not_in_revision` drops the SKUs the revision names — alone, together, beside `priced`, `q`
/// and `$filter` — and the counts narrow alike. Each key is ONE `sku_ids_in` call of its scope;
/// the list still asks `usage` once for its page (none for an empty page).
#[tokio::test]
async fn each_picker_key_narrows_by_its_scope_with_one_call_per_key() {
    let (d, port, book, revision) = pickers_door().await;
    let (b, r) = (book.to_string(), revision.to_string());
    for (params, expected, calls, asked) in [
        (
            vec![("priced_in", b.as_str())],
            vec!["A", "B"],
            vec!["sku_ids_in", "usage"],
            vec![UsageScope::Book(book)],
        ),
        (
            vec![("not_priced_in", b.as_str())],
            vec!["C", "D", "E"],
            vec!["sku_ids_in", "usage"],
            vec![UsageScope::Book(book)],
        ),
        (
            vec![("not_in_revision", r.as_str())],
            vec!["A", "D", "E"],
            vec!["sku_ids_in", "usage"],
            vec![UsageScope::Revision(revision)],
        ),
        (
            vec![
                ("not_priced_in", b.as_str()),
                ("not_in_revision", r.as_str()),
            ],
            vec!["D", "E"],
            vec!["sku_ids_in", "sku_ids_in", "usage"],
            vec![UsageScope::Book(book), UsageScope::Revision(revision)],
        ),
        (
            vec![("priced_in", b.as_str()), ("not_in_revision", r.as_str())],
            vec!["A"],
            vec!["sku_ids_in", "sku_ids_in", "usage"],
            vec![UsageScope::Book(book), UsageScope::Revision(revision)],
        ),
        (
            vec![("priced", "true"), ("priced_in", b.as_str())],
            vec!["A"],
            vec!["usage_sets", "sku_ids_in", "usage"],
            vec![UsageScope::Book(book)],
        ),
        (
            vec![
                ("priced_in", b.as_str()),
                ("$filter", "code ne 'A'"),
                ("q", "b"),
            ],
            vec!["B"],
            vec!["sku_ids_in", "usage"],
            vec![UsageScope::Book(book)],
        ),
        (
            vec![
                ("priced_in", b.as_str()),
                ("not_in_revision", r.as_str()),
                ("q", "z"),
            ],
            vec![],
            vec!["sku_ids_in", "sku_ids_in"],
            vec![UsageScope::Book(book), UsageScope::Revision(revision)],
        ),
    ] {
        assert_eq!(
            d.codes(&list(&params)).await,
            sorted(expected),
            "{params:?}"
        );
        assert_eq!(port.calls(), calls, "{params:?}: one call per key");
        assert_eq!(port.asked(), asked, "{params:?}");
    }
    for (params, all, calls) in [
        (vec![("priced_in", b.as_str())], 2, vec!["sku_ids_in"]),
        (vec![("not_priced_in", b.as_str())], 3, vec!["sku_ids_in"]),
        (vec![("not_in_revision", r.as_str())], 3, vec!["sku_ids_in"]),
        (
            vec![
                ("not_priced_in", b.as_str()),
                ("not_in_revision", r.as_str()),
            ],
            2,
            vec!["sku_ids_in", "sku_ids_in"],
        ),
        (
            vec![("in_plan", "false"), ("priced_in", b.as_str())],
            2,
            vec!["usage_sets", "sku_ids_in"],
        ),
    ] {
        let (status, body) = d.get(&counts(&params)).await;
        assert_eq!(status, StatusCode::OK, "{params:?}: {body}");
        assert_eq!(body["all"], all, "{params:?}: {body}");
        assert_eq!(port.calls(), calls, "{params:?}");
        port.asked();
    }
}

/// P-D-246: the cursor's hash covers each picker key: a cursor replayed with another book,
/// another key or without its key is 400 `FILTER_MISMATCH`. A list without picker keys hashes as
/// it did before them, so its cursors keep reading across the change.
#[tokio::test]
async fn the_cursor_carries_each_picker_key() {
    let (d, port, book, revision) = pickers_door().await;
    let (b, r) = (book.to_string(), revision.to_string());
    let other = Uuid::new_v4().to_string();
    let (_, page) = d.get(&list(&[("not_priced_in", &b), ("limit", "1")])).await;
    let cursor = page["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        d.codes(&list(&[
            ("not_priced_in", &b),
            ("limit", "1"),
            ("cursor", &cursor)
        ]))
        .await,
        ["D"]
    );
    for params in [
        vec![
            ("not_priced_in", other.as_str()),
            ("cursor", cursor.as_str()),
        ],
        vec![("priced_in", b.as_str()), ("cursor", cursor.as_str())],
        vec![("cursor", cursor.as_str())],
        vec![
            ("not_priced_in", b.as_str()),
            ("not_in_revision", r.as_str()),
            ("cursor", cursor.as_str()),
        ],
    ] {
        let body = d.refused(&list(&params)).await;
        assert_eq!(problem_code(&body), "FILTER_MISMATCH", "{params:?}: {body}");
    }
    port.calls();
    // The hash of a list without picker keys is the one its cursors carried before them.
    let query = toolkit_odata::ODataQuery::default();
    let params = super::super::ListParams {
        q: Some("a".into()),
        priced: Some(true),
        ..super::super::ListParams::default()
    };
    assert_eq!(
        super::super::list_hash(&query, &params),
        super::super::cursor_hash(&serde_json::json!({
            "filter": query.filter_hash,
            "q": params.q,
            "priced": params.priced,
            "in_plan": params.in_plan,
        })),
        "a list without picker keys keeps its hash"
    );
}

/// P-D-246: at most one of `priced_in` and `not_priced_in`; each key names one id, given once.
/// Every refusal is 400 before pricing is asked, on the list and on the counts.
#[tokio::test]
async fn the_book_keys_are_one_at_a_time_and_each_key_names_one_id() {
    let (d, port, book, revision) = pickers_door().await;
    let (b, r) = (book.to_string(), revision.to_string());
    for params in [
        vec![("priced_in", b.as_str()), ("not_priced_in", b.as_str())],
        vec![("priced_in", "not-a-uuid")],
        vec![("not_priced_in", "")],
        vec![("not_in_revision", "42")],
        vec![
            ("not_in_revision", r.as_str()),
            ("not_in_revision", r.as_str()),
        ],
        vec![("priced_in", b.as_str()), ("priced_in", b.as_str())],
    ] {
        for uri in [list(&params), counts(&params)] {
            let body = d.refused(&uri).await;
            assert_eq!(problem_code(&body), "INVALID_QUERY_PARAMS", "{uri}: {body}");
        }
    }
    assert!(
        port.calls().is_empty(),
        "a refused query asks pricing nothing"
    );
}

/// P-D-246: a picker key pricing cannot answer fails the read — 403 `USAGE_FORBIDDEN` when it
/// refuses the caller (a revision without plan read, a book without entry read), 503
/// `USAGE_UNAVAILABLE` when no port is registered, when it fails and when it breaks — never an
/// unfiltered page.
#[tokio::test]
async fn a_picker_key_pricing_cannot_answer_fails_the_read() {
    for answer in [
        None,
        Some(Answer::Refuses),
        Some(Answer::Fails),
        Some(Answer::Panics),
    ] {
        let d = Door::new().await;
        d.sku(seed("A")).await;
        let port = answer.map(|a| SetsPort::new(a, &[], &[]));
        if let Some(port) = &port {
            d.state.hub.register::<dyn SkuUsageV1>(port.clone());
        }
        let (status, code) = match answer {
            Some(Answer::Refuses) => (StatusCode::FORBIDDEN, "USAGE_FORBIDDEN"),
            _ => (StatusCode::SERVICE_UNAVAILABLE, "USAGE_UNAVAILABLE"),
        };
        let id = Uuid::new_v4().to_string();
        for key in ["priced_in", "not_priced_in", "not_in_revision"] {
            for uri in [list(&[(key, &id)]), counts(&[(key, &id)])] {
                let (got, body) = d.get(&uri).await;
                assert_eq!(got, status, "{answer:?} {uri}: {body}");
                assert!(body.get("items").is_none(), "never a page: {body}");
                assert!(body.to_string().contains(code), "{answer:?} {uri}: {body}");
            }
        }
        if let Some(port) = port {
            assert_eq!(
                port.calls(),
                ["sku_ids_in"; 6],
                "{answer:?}: one call per read"
            );
        }
    }
}

/// P-D-246: a picker key whose call never returns is 503 once the two-second bound elapses, and
/// the call is aborted, not left running.
#[tokio::test]
async fn a_picker_key_that_never_answers_is_503_after_its_bound_and_is_aborted() {
    let d = Door::new().await;
    d.sku(seed("A")).await;
    let port = SetsPort::new(Answer::Hangs, &[], &[]);
    d.state.hub.register::<dyn SkuUsageV1>(port.clone());
    let started = std::time::Instant::now();
    let (status, body) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        d.get(&list(&[("not_in_revision", &Uuid::new_v4().to_string())])),
    )
    .await
    .expect("the read answers although the port never does");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.to_string().contains("USAGE_UNAVAILABLE"), "{body}");
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(2),
        "the 2 s bound"
    );
    for _ in 0..100 {
        if port.abandoned.load(Ordering::SeqCst) == 1 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("a call past its bound is aborted, not left running");
}

/// P-D-246: a book or a revision the tenant does not hold is pricing's empty set: `priced_in`
/// keeps nothing, `not_priced_in` and `not_in_revision` keep every SKU.
#[tokio::test]
async fn a_foreign_book_or_revision_is_the_empty_set() {
    let (d, _port, _, _) = pickers_door().await;
    let (foreign_book, foreign_revision) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    assert!(
        d.codes(&list(&[("priced_in", &foreign_book)]))
            .await
            .is_empty()
    );
    let every = sorted(vec!["A", "B", "C", "D", "E"]);
    assert_eq!(
        d.codes(&list(&[("not_priced_in", &foreign_book)])).await,
        every
    );
    assert_eq!(
        d.codes(&list(&[("not_in_revision", &foreign_revision)]))
            .await,
        every
    );
    let (_, body) = d.get(&counts(&[("priced_in", &foreign_book)])).await;
    assert_eq!(body["all"], 0, "{body}");
}

/// P-D-246 (P-D-212's one bind): a picker key's id set is ONE bind — the list and the counts make
/// the same statements with the same binds for a scope of 10 ids and one of 5000.
// Probed (PROBE-9-8-3): one bind per id.
#[tokio::test]
async fn a_scoped_set_of_any_size_filters_through_one_bind() {
    let mut traces = Vec::new();
    for extra in [10, 5000] {
        let (d, recorder) = recorded_door(20).await;
        let conn = d.state.db.conn().unwrap();
        let page = repo::page_skus(
            &conn,
            &d.scope,
            d.tenant,
            d.state.db.db().backend(),
            &repo::SkuListFilter::default(),
            &toolkit_odata::ODataQuery::default().with_limit(3),
        )
        .await
        .map_err(|_| "page")
        .unwrap();
        let mut held: Vec<Uuid> = page.items.iter().map(|s| s.id).collect();
        held.extend((0..extra).map(|_| Uuid::new_v4()));
        let (book, revision) = (Uuid::new_v4(), Uuid::new_v4());
        let port = SetsPort::new(Answer::Answers, &[], &[]);
        port.scope(UsageScope::Book(book), &held);
        port.scope(UsageScope::Revision(revision), &held);
        d.state.hub.register::<dyn SkuUsageV1>(port.clone());
        let (b, r) = (book.to_string(), revision.to_string());
        let listed = statements(
            &d,
            &recorder,
            &list(&[("priced_in", &b), ("not_in_revision", &r), ("limit", "200")]),
        )
        .await;
        let (_, body) = d.get(&list(&[("priced_in", &b)])).await;
        assert_eq!(codes(&body), ["R000", "R001", "R002"], "{extra}");
        let (_, body) = d.get(&list(&[("not_in_revision", &r)])).await;
        assert_eq!(body["items"].as_array().unwrap().len(), 17, "{extra}");
        let counted = statements(&d, &recorder, &counts(&[("not_priced_in", &b)])).await;
        let (_, body) = d.get(&counts(&[("not_priced_in", &b)])).await;
        assert_eq!(body["all"], 17, "{extra}: {body}");
        traces.push((listed, counted));
    }
    for (listed, counted) in &traces {
        assert_eq!(listed.len(), 2, "{listed:#?}");
        assert_eq!(counted.len(), 2, "{counted:#?}");
    }
    assert_eq!(
        traces[0], traces[1],
        "the same statements and binds for 10 and 5000 ids"
    );
}
