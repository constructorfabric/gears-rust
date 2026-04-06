use super::*;

#[test]
fn test_service_add() {
    let service = Service::new();
    assert_eq!(service.add(10, 20), 30);
}

#[test]
fn test_negative_numbers() {
    let service = Service::new();
    assert_eq!(service.add(-5, 3), -2);
}
