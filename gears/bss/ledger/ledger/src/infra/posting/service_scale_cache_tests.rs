//! The per-attempt scale cache still checks every line and functional amount:
//! a later line in a cached currency at another scale is refused.
#![allow(clippy::unwrap_used)]

use super::decimal_tests::{counts, money, setup};
use super::*;

#[tokio::test]
async fn a_cached_currency_at_another_scale_on_a_later_line_is_refused() {
    let (svc, db, entry, lines) = setup().await;
    let before = counts(&db).await;
    let mut functional = lines.clone();
    // The functional amounts agree with each other (EUR@3, balanced), so the
    // domain validation passes; EUR was resolved to scale 2 from the first
    // line's transaction money, and the cached value must still refuse them.
    for line in &mut functional {
        line.functional_money = Some(money("1", "EUR", 3));
    }
    let result = svc
        .post(
            &SecurityContext::anonymous(),
            &AccessScope::allow_all(),
            entry,
            functional,
            None,
        )
        .await;
    assert!(
        matches!(&result, Err(DomainError::InconsistentScale(d)) if d.contains("registry")),
        "{result:?}"
    );
    assert_eq!(counts(&db).await, before, "nothing written");
}
