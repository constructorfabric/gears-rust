use super::*;

#[test]
fn worker_id_format() {
    let id = generate_worker_id("orders");
    assert!(id.starts_with("orders-"), "expected orders- prefix: {id}");
    let suffix = &id["orders-".len()..];
    assert_eq!(suffix.len(), 6, "suffix should be 6 chars: {suffix}");
    assert!(
        suffix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        "suffix should be A-Z0-9: {suffix}"
    );
}

#[test]
fn worker_ids_differ() {
    let id1 = generate_worker_id("q");
    std::thread::sleep(std::time::Duration::from_millis(1));
    let id2 = generate_worker_id("q");
    assert_ne!(id1, id2, "worker IDs should differ: {id1} vs {id2}");
}
