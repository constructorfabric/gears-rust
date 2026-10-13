//! The tenant settings (D-438): `currencies` is required on every PUT (a full replace, `[]` = any
//! currency) and restricts the currency of a NEW book; the settings say who changed them and
//! when. The settings carry no rounding: invoice lines always round `HALF_EVEN`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use plan_support::{Catalog, Fixture, holding, setup};
use serde_json::{Value, json};
use std::sync::Arc;

async fn read(f: &Fixture) -> (Value, String) {
    let (s, b, tag) = f.call("GET", "/settings", json!({}), None, None).await;
    assert_eq!(s, 200, "{b}");
    (b, tag)
}
fn body(currencies: Value) -> Value {
    let mut b = json!({
        "default_timing": "advance",
        "default_gl": null,
        "default_tax_category": null,
        "invoice_line_templates": {},
    });
    b["currencies"] = currencies;
    b
}
async fn put(f: &Fixture, body: Value) -> (u16, Value, String) {
    let (_, tag) = read(f).await;
    f.call("PUT", "/settings", body, Some(&tag), None).await
}
async fn create_book(f: &Fixture, code: &str, currency: &str) -> (u16, Value) {
    let (s, b, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":code,"name":code,"currency":currency}),
            None,
            Some(&format!("book-{code}")),
        )
        .await;
    (s, b)
}

/// A tenant that never wrote its settings reads version 0, and the answer names no rounding.
#[tokio::test]
async fn the_default_settings_say_nobody_changed_them() {
    let (f, _) = setup().await;
    let (b, tag) = read(&f).await;
    assert_eq!(tag, "\"0\"");
    assert_eq!(
        b,
        json!({
            "default_timing": "advance",
            "default_gl": null,
            "default_tax_category": null,
            "invoice_line_templates": {},
            "currencies": [],
            "version": 0,
            "updated_at": null,
            "updated_by": null,
            // D-519: nobody wrote them, so nobody is named.
            "updated_by_name": null,
        })
    );
}

#[tokio::test]
async fn a_put_requires_currencies_and_stamps_who_and_when() {
    let (f, _) = setup().await;
    // `currencies` is required: a body without it is refused.
    let mut without = body(json!([]));
    without.as_object_mut().unwrap().remove("currencies");
    let (s, b, _) = put(&f, without).await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("currencies"), "{b}");
    for bad in [
        json!(["eur"]),
        json!(["EURO"]),
        json!(["EU"]),
        json!([""]),
        json!(["EUR", "USD", "EUR"]),
        json!(["E1R"]),
    ] {
        let (s, b, _) = put(&f, body(bad.clone())).await;
        assert_eq!(s, 400, "{bad}: {b}");
        assert!(b.to_string().contains("CURRENCY_INVALID"), "{bad}: {b}");
    }
    assert_eq!(read(&f).await.1, "\"0\"", "no refusal wrote anything");
    let before = time::OffsetDateTime::now_utc();
    let (s, saved, tag) = put(&f, body(json!(["USD", "EUR"]))).await;
    assert_eq!(s, 200, "{saved}");
    assert_eq!(tag, "\"1\"");
    assert_eq!(saved["currencies"], json!(["USD", "EUR"]), "kept as sent");
    assert_eq!(saved["updated_by"], f.ctx.subject_id().to_string());
    let at = time::OffsetDateTime::parse(
        saved["updated_at"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert!(at >= before - time::Duration::seconds(1), "{saved}");
    assert_eq!(read(&f).await.0, saved);
    // Another principal writes: the stamp follows the writer; `[]` means any currency again.
    let other = f.user();
    let (_, tag2) = read(&f).await;
    let (s, again, _) = f
        .call_as(
            &other,
            "PUT",
            "/settings",
            body(json!([])),
            Some(&tag2),
            None,
        )
        .await;
    assert_eq!(s, 200, "{again}");
    assert_eq!(again["currencies"], json!([]));
    assert_eq!(again["updated_by"], other.subject_id().to_string());
    assert_eq!(again["version"], 2);
    let _ = tag;
}

/// The tenant has no rounding setting: the PUT body is closed, so a client that still sends
/// `default_rounding` gets the standard 400 for an unknown field, and nothing is written. The
/// answers carry no rounding either.
#[tokio::test]
async fn the_removed_rounding_setting_is_an_unknown_field() {
    let (f, _) = setup().await;
    for mode in ["half_even", "half_up", "bankers", ""] {
        let mut b = body(json!([]));
        b["default_rounding"] = json!(mode);
        let (s, answer, _) = put(&f, b).await;
        assert_eq!(s, 400, "a PUT that names default_rounding is refused");
        assert!(
            answer
                .to_string()
                .contains("unknown field `default_rounding`"),
            "the refusal names the unknown field"
        );
    }
    assert_eq!(read(&f).await.1, "\"0\"", "no refusal wrote anything");
    let (s, saved, _) = put(&f, body(json!([]))).await;
    assert_eq!(s, 200, "a PUT without default_rounding is accepted");
    assert!(
        saved.get("default_rounding").is_none(),
        "the PUT answer names no rounding"
    );
    assert!(
        read(&f).await.0.get("default_rounding").is_none(),
        "the GET answer names no rounding"
    );
}

// Probed in run 6.4: a book outside the offered currencies is refused.
#[tokio::test]
async fn a_new_book_takes_an_offered_currency() {
    let (f, _) = setup().await;
    // No settings, or an empty list: any currency.
    assert_eq!(create_book(&f, "jpy", "JPY").await.0, 201);
    let (s, b, _) = put(&f, body(json!(["EUR", "USD"]))).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(create_book(&f, "eur", "EUR").await.0, 201);
    let (s, b) = create_book(&f, "gbp", "GBP").await;
    assert_eq!(s, 409, "{b}");
    assert!(b.to_string().contains("CURRENCY_NOT_OFFERED"), "{b}");
    // A malformed code is still the book's own 400, judged first.
    let (s, b) = create_book(&f, "bad", "gbp").await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("BOOK_CURRENCY_INVALID"), "{b}");
    // The book author needs no settings grant for the rule to hold.
    let (s, b, _) = f
        .call_as(
            &holding(&f, "price_book:author"),
            "POST",
            "/price-books",
            json!({"code":"chf","name":"chf","currency":"CHF"}),
            None,
            Some("book-chf"),
        )
        .await;
    assert_eq!(s, 409, "{b}");
    // Existing books are untouched, and `[]` opens every currency again.
    let (s, books, _) = f.call("GET", "/price-books", json!({}), None, None).await;
    assert_eq!(s, 200);
    assert_eq!(books["items"].as_array().unwrap().len(), 2);
    assert!(
        books["page_info"]["next_cursor"].is_null(),
        "one page is the whole list: {books}"
    );
    let (s, _, _) = put(&f, body(json!([]))).await;
    assert_eq!(s, 200);
    assert_eq!(create_book(&f, "gbp2", "GBP").await.0, 201);
}

/// The first field violation of a 400: its reason and what the client is told.
fn violation(b: &Value) -> (&str, &str) {
    let first = &b["context"]["field_violations"][0];
    (
        first["reason"].as_str().unwrap_or_default(),
        first["description"].as_str().unwrap_or_default(),
    )
}

/// A currency the table does not resolve is refused at the book and at the settings list, before
/// anything is written: `ZZZ` is no ISO 4217 code, and `XTS` is ISO's test code, which has no
/// scale. Without a configured table the ISO codes only are known. The settings refusal names the
/// code and says whether the table does not know it or the list repeats it.
#[tokio::test]
async fn an_unknown_currency_is_refused_by_the_book_and_the_settings() {
    let (f, _) = setup().await;
    for code in ["ZZZ", "XTS", "XAU", "BTC"] {
        let (s, b) = create_book(&f, &code.to_lowercase(), code).await;
        assert_eq!(s, 400, "an unknown book currency is refused");
        assert_eq!(
            violation(&b).0,
            "BOOK_CURRENCY_INVALID",
            "the book names its currency"
        );
        let (s, b, _) = put(&f, body(json!([code]))).await;
        assert_eq!(s, 400, "an unknown offered currency is refused");
        let (reason, description) = violation(&b);
        assert_eq!(reason, "CURRENCY_INVALID", "the settings name the list");
        assert!(
            description.contains(&format!(
                "\"{code}\" is not a currency the currency table knows"
            )),
            "the refusal names the unknown code"
        );
    }
    // One unknown code refuses the whole list.
    let (s, b, _) = put(&f, body(json!(["EUR", "ZZZ"]))).await;
    assert_eq!(s, 400, "one unknown code refuses the list");
    assert_eq!(violation(&b).0, "CURRENCY_INVALID", "the list is refused");
    // A code listed twice is named as repeated, not as unknown.
    let (s, b, _) = put(&f, body(json!(["EUR", "JPY", "EUR"]))).await;
    assert_eq!(s, 400, "a repeated code refuses the list");
    assert_eq!(
        violation(&b),
        ("CURRENCY_INVALID", "EUR is listed more than once"),
        "the refusal names the repeated code"
    );
    assert_eq!(read(&f).await.1, "\"0\"", "no refusal wrote anything");
    // The ISO codes pass, at every scale.
    for code in ["EUR", "JPY", "KWD", "CLF"] {
        let (s, _) = create_book(&f, &code.to_lowercase(), code).await;
        assert_eq!(s, 201, "an ISO code is a book currency");
    }
    let (s, _, _) = put(&f, body(json!(["EUR", "JPY", "KWD", "CLF"]))).await;
    assert_eq!(s, 200, "ISO codes are offered");
}

/// A value that is not three uppercase letters is refused before the currency table is asked, with
/// a fixed message: the refusal never repeats the value, however long it is.
#[tokio::test]
async fn an_offered_value_that_is_not_a_code_is_refused_without_repeating_it() {
    let (f, _) = setup().await;
    let long = "Q".repeat(4096);
    for bad in [long.as_str(), "USDOLLAR", "<img src=x>"] {
        let (s, b, _) = put(&f, body(json!(["EUR", bad]))).await;
        assert_eq!(s, 400, "a value that is not a code is refused");
        assert_eq!(
            violation(&b),
            (
                "CURRENCY_INVALID",
                "an offered currency is a currency code: three uppercase letters"
            ),
            "the refusal has a fixed message"
        );
        assert!(
            !b.to_string().contains(bad),
            "the refusal does not repeat the value"
        );
    }
    assert_eq!(read(&f).await.1, "\"0\"", "no refusal wrote anything");
}

/// A non-ISO code is known only where the deployment configures it, and only that code: the
/// configured table adds to the ISO one and never replaces it.
#[tokio::test]
async fn a_configured_non_iso_currency_is_accepted_by_the_book_and_the_settings() {
    let f = Fixture::with_currency_scales(Arc::new(Catalog::default()), &[("XTS", 2)]).await;
    let (s, b) = create_book(&f, "xts", "XTS").await;
    assert_eq!(s, 201, "a configured code is a book currency");
    assert_eq!(b["currency"], "XTS");
    let (s, b, _) = put(&f, body(json!(["XTS", "EUR"]))).await;
    assert_eq!(s, 200, "a configured code is offered");
    assert_eq!(b["currencies"], json!(["XTS", "EUR"]));
    // The other unknown codes stay refused.
    let (s, b) = create_book(&f, "zzz", "ZZZ").await;
    assert_eq!(s, 400, "an unlisted code stays unknown to a book");
    assert_eq!(
        violation(&b).0,
        "BOOK_CURRENCY_INVALID",
        "the book refuses it"
    );
    let (s, b, _) = put(&f, body(json!(["XTS", "ZZZ"]))).await;
    assert_eq!(s, 400, "an unlisted code stays unknown to the settings");
    assert_eq!(
        violation(&b).0,
        "CURRENCY_INVALID",
        "the settings refuse it"
    );
}

/// A book keeps its currency for life, so an edit does not judge it against today's table: a book
/// stored under a currency the table no longer knows (a code a deployment stopped configuring)
/// stays editable.
#[tokio::test]
async fn a_book_of_a_currency_the_table_forgot_stays_editable() {
    use bss_pricing::infra::storage::{entity::price_book, repo::book_repo};
    let (f, _) = setup().await;
    let now = time::OffsetDateTime::now_utc();
    let id = uuid::Uuid::new_v4();
    book_repo::insert(
        &f.db.conn().unwrap(),
        &plan_support::scope(&f),
        price_book::Model {
            id,
            tenant_id: f.ctx.subject_tenant_id(),
            code: "old".into(),
            name: "old".into(),
            currency: "ZZZ".into(),
            valid_from: None,
            valid_until: None,
            description: None,
            version: 1,
            created_at: now,
            updated_at: now,
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap();
    let (s, b, tag) = f
        .call(
            "PATCH",
            &format!("/price-books/{id}"),
            json!({"name":"renamed"}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(s, 200, "the book is edited");
    assert_eq!(b["name"], "renamed");
    assert_eq!(b["currency"], "ZZZ");
    assert_eq!(tag, "\"2\"");
}

/// Fix run W1c L2 (D-457): only a changed text is judged. Settings stored before the caps, with a
/// GL code, a tax category and an invoice line template each over its cap, never lock the
/// settings: a PUT that sends them back unchanged and changes only the timing passes. A PUT that
/// changes one of them to another text over its cap is 400 `FIELD_TOO_LONG` on that field, and
/// nothing is written.
#[tokio::test]
async fn a_stored_text_over_its_cap_never_locks_the_settings() {
    use bss_pricing::infra::storage::{entity::settings, repo::settings_repo};
    let (f, _) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let long = |c: char, n: usize| c.to_string().repeat(n);
    let (gl, tax, line) = (long('g', 65), long('t', 65), long('l', 2001));
    let now = time::OffsetDateTime::now_utc();
    settings_repo::insert(
        &f.db.conn().unwrap(),
        &plan_support::scope(&f),
        settings::Model {
            tenant_id: tenant,
            default_timing: "advance".into(),
            default_gl: Some(gl.clone()),
            default_tax_category: Some(tax.clone()),
            invoice_line_templates: json!({ "usage": line }),
            version: 1,
            created_at: now,
            updated_at: now,
            currencies: json!([]),
            updated_by: None,
        },
    )
    .await
    .unwrap();
    let stored = |timing: &str| {
        json!({
            "default_timing": timing,
            "default_gl": gl,
            "default_tax_category": tax,
            "invoice_line_templates": { "usage": line },
            "currencies": [],
        })
    };
    let (s, saved, _) = put(&f, stored("arrears")).await;
    assert_eq!(s, 200, "{saved}");
    assert_eq!(saved["default_timing"], "arrears");
    assert_eq!(saved["default_gl"], json!(gl), "kept as stored");
    for (field, value) in [
        ("default_gl", json!(long('h', 65))),
        ("default_tax_category", json!(long('u', 65))),
        (
            "invoice_line_templates",
            json!({ "usage": long('m', 2001) }),
        ),
        (
            "invoice_line_templates",
            json!({ "usage": line, "recurring": line }),
        ),
    ] {
        let mut changed = stored("arrears");
        changed[field] = value.clone();
        let (s, b, _) = put(&f, changed).await;
        assert_eq!(s, 400, "{field} {value}: {b}");
        assert_eq!(
            b["context"]["field_violations"][0]["reason"], "FIELD_TOO_LONG",
            "{field}: {b}"
        );
        assert_eq!(
            b["context"]["field_violations"][0]["field"], field,
            "{field}: {b}"
        );
    }
    assert_eq!(read(&f).await.0["version"], 2, "no refusal wrote anything");
}
