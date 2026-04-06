use super::*;

// --- Queue name: valid ---

#[test]
fn queue_name_simple() {
    assert!(validate_queue_name("orders").is_ok());
}

#[test]
fn queue_name_with_dots_and_dashes() {
    assert!(validate_queue_name("orders.v2").is_ok());
    assert!(validate_queue_name("my-queue").is_ok());
    assert!(validate_queue_name("my_queue").is_ok());
}

#[test]
fn queue_name_single_char() {
    assert!(validate_queue_name("a").is_ok());
    assert!(validate_queue_name("9").is_ok());
}

#[test]
fn queue_name_1024_chars() {
    let name = "a".repeat(1024);
    assert!(validate_queue_name(&name).is_ok());
}

#[test]
fn queue_name_mixed_case() {
    assert!(validate_queue_name("OrderEvents").is_ok());
}

// --- Queue name: invalid ---

#[test]
fn queue_name_empty() {
    assert!(validate_queue_name("").is_err());
}

#[test]
fn queue_name_too_long() {
    let name = "a".repeat(1025);
    assert!(validate_queue_name(&name).is_err());
}

#[test]
fn queue_name_starts_with_dot() {
    assert!(validate_queue_name(".orders").is_err());
}

#[test]
fn queue_name_ends_with_dash() {
    assert!(validate_queue_name("orders-").is_err());
}

#[test]
fn queue_name_null_byte() {
    assert!(validate_queue_name("orders\0evil").is_err());
}

#[test]
fn queue_name_spaces() {
    assert!(validate_queue_name("my queue").is_err());
}

#[test]
fn queue_name_unicode() {
    assert!(validate_queue_name("\u{0437}\u{0430}\u{043a}\u{0430}\u{0437}\u{044b}").is_err());
}

#[test]
fn queue_name_slashes() {
    assert!(validate_queue_name("orders/v2").is_err());
}

// --- Payload type: valid ---

#[test]
fn payload_type_simple() {
    assert!(validate_payload_type("json").is_ok());
}

#[test]
fn payload_type_mime_style() {
    assert!(validate_payload_type("application/json").is_ok());
    assert!(validate_payload_type("application/json;orders.created.v1").is_ok());
}

#[test]
fn payload_type_1024_chars() {
    let pt = "a".repeat(1024);
    assert!(validate_payload_type(&pt).is_ok());
}

// --- Payload type: invalid ---

#[test]
fn payload_type_empty() {
    assert!(validate_payload_type("").is_err());
}

#[test]
fn payload_type_too_long() {
    let pt = "a".repeat(1025);
    assert!(validate_payload_type(&pt).is_err());
}

#[test]
fn payload_type_null_byte() {
    assert!(validate_payload_type("json\0").is_err());
}

#[test]
fn payload_type_newline() {
    assert!(validate_payload_type("json\n").is_err());
}

#[test]
fn payload_type_control_char() {
    assert!(validate_payload_type("json\x01").is_err());
}

#[test]
fn payload_type_non_ascii() {
    assert!(validate_payload_type("\u{0434}\u{0430}\u{043d}\u{043d}\u{044b}\u{0435}").is_err());
}
