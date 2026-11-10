use super::*;

#[test]
fn validate_bounds() {
    assert!(LicenseEnforcementConfig::default().validate().is_ok());
    let bad: [fn(&mut LicenseEnforcementConfig); 7] = [
        |c| c.idempotency_window_secs = 0,
        |c| c.idempotency_window_secs = -1,
        |c| c.idempotency_window_secs = MAX_IDEMPOTENCY_WINDOW_SECS + 1,
        |c| c.default_page_size = 0,
        |c| c.default_page_size = c.max_page_size + 1,
        |c| {
            c.default_page_size = u64::MAX;
            c.max_page_size = u64::MAX;
        },
        |c| c.max_candidates = 0,
    ];
    for (i, f) in bad.into_iter().enumerate() {
        let mut cfg = LicenseEnforcementConfig::default();
        f(&mut cfg);
        assert!(cfg.validate().is_err(), "case {i}");
    }
}
