use toolkit::DatabaseCapability;

use super::*;

#[test]
fn gear_provides_p1_and_p2_migrations() {
    // The DatabaseCapability wiring must hand the runtime all current migrations:
    //   1. P1 initial (control-plane metadata tables)
    //   2. P2 initial (policy store + retention rules + multipart + idempotency
    //      keys + audit outbox + file events outbox, in one step)
    //   3. P2 multipart plan columns (declared_size + part_size on multipart_uploads)
    // (init()/register_rest() need a live GearCtx — those seams are covered by
    // the E2E suite, not here.)
    let gear = FileStorageGear::default();
    assert_eq!(
        gear.migrations().len(),
        3,
        "gear must provide the P1, P2 initial, and P2 multipart plan columns migrations"
    );
}
