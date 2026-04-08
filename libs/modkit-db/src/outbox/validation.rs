use super::types::OutboxError;

/// Maximum queue name length (fits `MySQL` VARCHAR(255) with headroom).
const MAX_QUEUE_NAME_LEN: usize = 63;

/// Maximum payload type length.
const MAX_PAYLOAD_TYPE_LEN: usize = 255;

/// Validate a queue name: `[a-zA-Z0-9._-]{1,63}`, must start and end with
/// alphanumeric.
pub fn validate_queue_name(name: &str) -> Result<(), OutboxError> {
    if name.is_empty() || name.len() > MAX_QUEUE_NAME_LEN {
        return Err(OutboxError::InvalidQueueName(name.to_owned()));
    }

    let bytes = name.as_bytes();

    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return Err(OutboxError::InvalidQueueName(name.to_owned()));
    }

    for &b in bytes {
        if !(b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-') {
            return Err(OutboxError::InvalidQueueName(name.to_owned()));
        }
    }

    Ok(())
}

/// Validate a payload type: 1-255 printable ASCII chars (`0x20..=0x7E`).
pub fn validate_payload_type(payload_type: &str) -> Result<(), OutboxError> {
    if payload_type.is_empty() || payload_type.len() > MAX_PAYLOAD_TYPE_LEN {
        return Err(OutboxError::InvalidPayloadType(payload_type.to_owned()));
    }

    for &b in payload_type.as_bytes() {
        if !(0x20..=0x7E).contains(&b) {
            return Err(OutboxError::InvalidPayloadType(payload_type.to_owned()));
        }
    }

    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
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
    fn queue_name_63_chars() {
        let name = "a".repeat(63);
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
        let name = "a".repeat(64);
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
    fn payload_type_255_chars() {
        let pt = "a".repeat(255);
        assert!(validate_payload_type(&pt).is_ok());
    }

    // --- Payload type: invalid ---

    #[test]
    fn payload_type_empty() {
        assert!(validate_payload_type("").is_err());
    }

    #[test]
    fn payload_type_too_long() {
        let pt = "a".repeat(256);
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
}
