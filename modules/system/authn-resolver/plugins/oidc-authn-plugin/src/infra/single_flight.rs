//! Per-key async single-flight synchronization.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tokio::sync::Mutex;

pub struct SingleFlightGate {
    pub mutex: Mutex<()>,
    leases: AtomicUsize,
}

impl SingleFlightGate {
    fn new() -> Self {
        Self {
            mutex: Mutex::new(()),
            leases: AtomicUsize::new(1),
        }
    }

    fn retain(&self) {
        self.leases.fetch_add(1, Ordering::AcqRel);
    }

    fn release(&self) -> bool {
        self.leases.fetch_sub(1, Ordering::AcqRel) == 1
    }

    fn is_idle(&self) -> bool {
        self.leases.load(Ordering::Acquire) == 0
    }
}

pub fn single_flight_gate(
    in_flight: &DashMap<String, Arc<SingleFlightGate>>,
    key: &str,
) -> Arc<SingleFlightGate> {
    single_flight_gate_with_leadership(in_flight, key).0
}

/// Acquire the per-key gate and report whether this caller created it.
///
/// The boolean is `true` for the first caller for `key`; that caller is
/// responsible for doing the underlying work. Later callers get `false`, wait on
/// the same gate, and can reuse whatever result/cache state the first caller
/// produced.
pub fn single_flight_gate_with_leadership(
    in_flight: &DashMap<String, Arc<SingleFlightGate>>,
    key: &str,
) -> (Arc<SingleFlightGate>, bool) {
    match in_flight.entry(key.to_owned()) {
        Entry::Occupied(entry) => {
            let gate = Arc::clone(entry.get());
            gate.retain();
            (gate, false)
        }
        Entry::Vacant(entry) => {
            let gate = Arc::new(SingleFlightGate::new());
            entry.insert(Arc::clone(&gate));
            (gate, true)
        }
    }
}

pub fn release_single_flight_gate(
    in_flight: &DashMap<String, Arc<SingleFlightGate>>,
    key: &str,
    gate: &Arc<SingleFlightGate>,
) {
    if gate.release() {
        in_flight.remove_if(key, |_, current| {
            Arc::ptr_eq(current, gate) && gate.is_idle()
        });
    }
}
