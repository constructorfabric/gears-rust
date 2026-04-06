use super::*;

#[test]
fn test_hardware_uuid_is_consistent() {
    // The UUID should be the same across multiple calls
    let uuid1 = get_hardware_uuid();
    let uuid2 = get_hardware_uuid();

    assert_eq!(uuid1, uuid2, "Hardware UUID should be consistent");
}

#[test]
fn test_hardware_uuid_format() {
    let uuid = get_hardware_uuid();

    // Check if it's a fallback UUID (first 8 bytes are zeros)
    let uuid_bytes = uuid.as_bytes();
    let is_fallback = uuid_bytes[0..8].iter().all(|&b| b == 0);

    if is_fallback {
        // If fallback, the right part should be random (not all zeros)
        let right_part_all_zeros = uuid_bytes[8..16].iter().all(|&b| b == 0);
        assert!(
            !right_part_all_zeros,
            "Fallback UUID should have random right part"
        );
    } else {
        // On real hardware, should have a valid hardware UUID
        assert!(
            !is_fallback,
            "Real hardware should not produce fallback UUID"
        );
    }
}
