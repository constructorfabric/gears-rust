//! Linking the `cf-gears-event-broker` library must register the `event-broker`
//! gear and nothing else. A gear self-registers into the process-global
//! `inventory` the moment it is linked, so a hard dep on another gear crate
//! would make every consumer run that gear too. Linked via `test-utils` (the
//! heaviest consumer shape) so a gear re-introduced by the harness is caught.

// Force-link the library exactly as a real consumer does: a gear's `inventory`
// registration lives in an object the linker pulls from the rlib only when the
// crate is referenced, so without this the registry below would be empty.
use event_broker as _;
use toolkit::registry::GearRegistry;

#[test]
fn the_library_links_only_the_event_broker_gear() {
    let registry = GearRegistry::discover_and_build()
        .expect("the gear registry builds (no dep edge references an unlinked gear)");

    let mut names: Vec<&str> = registry
        .gears()
        .iter()
        .map(toolkit::registry::GearEntry::name)
        .collect();
    names.sort_unstable();

    assert_eq!(
        names,
        ["event-broker"],
        "linking the event-broker library must register exactly the event-broker \
         gear; any other gear here means the library links a gear crate again"
    );
}
