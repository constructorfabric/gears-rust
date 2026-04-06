use super::types::OutboxError;

/// Maximum queue name length (fits VARCHAR(1024) column).
const MAX_QUEUE_NAME_LEN: usize = 1024;

/// Maximum payload type length.
const MAX_PAYLOAD_TYPE_LEN: usize = 1024;

/// Validate a queue name: `[a-zA-Z0-9._-]{1,1024}`, must start and end with
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

/// Validate a payload type: 1-1024 printable ASCII chars (`0x20..=0x7E`).
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
#[path = "validation_tests.rs"]
mod tests;
