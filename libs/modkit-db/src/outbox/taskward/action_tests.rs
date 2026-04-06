use super::*;

#[test]
fn directive_convenience_constructors() {
    assert_eq!(Directive::proceed(), Directive::Proceed(()));
    assert_eq!(Directive::idle(), Directive::Idle(()));
    assert_eq!(
        Directive::sleep(Duration::from_secs(1)),
        Directive::Sleep(Duration::from_secs(1), ()),
    );
}

#[test]
fn directive_strip() {
    let d = Directive::Proceed(42);
    assert_eq!(d.strip(), Directive::proceed());

    let d = Directive::Idle("hello");
    assert_eq!(d.strip(), Directive::idle());

    let d = Directive::Sleep(Duration::from_secs(5), vec![1, 2]);
    assert_eq!(d.strip(), Directive::sleep(Duration::from_secs(5)));
}

#[test]
fn directive_payload() {
    let d = Directive::Proceed(42);
    assert_eq!(*d.payload(), 42);

    let d = Directive::Idle("hi");
    assert_eq!(*d.payload(), "hi");
}

#[test]
fn directive_map() {
    let d = Directive::Proceed(42);
    let mapped = d.map(|n| n.to_string());
    assert_eq!(mapped, Directive::Proceed("42".to_owned()));
}

#[test]
fn directive_unit_is_copy() {
    let d = Directive::idle();
    let d2 = d;
    assert_eq!(d, d2);
}

#[test]
fn directive_variants_are_distinct() {
    assert_ne!(Directive::proceed(), Directive::idle());
    assert_ne!(Directive::proceed(), Directive::sleep(Duration::ZERO));
    assert_ne!(Directive::idle(), Directive::sleep(Duration::from_secs(1)));
}

#[test]
fn directive_sleep_equality() {
    let d = Duration::from_millis(500);
    assert_eq!(Directive::sleep(d), Directive::sleep(d));
}
