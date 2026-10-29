use crate::args::{PoolSize, Settings};

#[test]
fn the_pool_fits_every_caller_by_default() {
    assert_eq!(
        Settings::parse_from(["bench"]).pool_size,
        PoolSize::PerPhase
    );
    assert_eq!(PoolSize::PerPhase.connections(32), 32 + PoolSize::HEADROOM);
}

#[test]
fn a_fixed_pool_ignores_the_callers() {
    let settings = Settings::parse_from(["bench", "--pool-size", "10"]);
    assert_eq!(settings.pool_size, PoolSize::Fixed(10));
    assert_eq!(settings.pool_size.connections(32), 10);
}
