use super::*;

#[test]
fn default_worker_configs_are_valid() {
    OrphanWatchdogConfig::default().validate().unwrap();
    ThreadSummaryWorkerConfig::default().validate().unwrap();
    CleanupWorkerConfig::default().validate().unwrap();
}
