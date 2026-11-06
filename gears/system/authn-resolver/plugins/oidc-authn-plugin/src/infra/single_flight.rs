//! Per-key async single-flight synchronization.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tokio::sync::Mutex;

/// Per-key gate that serializes concurrent callers; reference-counted so the owning map can drop it once the last holder releases it.
pub struct SingleFlightGate {
    /// Held by the caller doing the underlying work while other callers for the same key wait.
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

/// Acquires the per-key gate, creating it if absent, and discards the leadership flag.
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

/// A lease on the gate for one key, released on drop.
///
/// Cancellation while waiting for or holding [`SingleFlightGate::mutex`]
/// releases the lease; the last holder removes the gate. Manual release via
/// [`release_single_flight_gate`] would leak on cancellation.
pub struct SingleFlightLease<'a> {
    in_flight: &'a DashMap<String, Arc<SingleFlightGate>>,
    key: &'a str,
    gate: Arc<SingleFlightGate>,
}

impl<'a> SingleFlightLease<'a> {
    /// Acquire the gate for `key`, creating it if absent.
    pub fn acquire(in_flight: &'a DashMap<String, Arc<SingleFlightGate>>, key: &'a str) -> Self {
        let gate = single_flight_gate(in_flight, key);
        Self {
            in_flight,
            key,
            gate,
        }
    }

    /// The leased gate.
    pub fn gate(&self) -> &SingleFlightGate {
        &self.gate
    }
}

impl Drop for SingleFlightLease<'_> {
    fn drop(&mut self) {
        release_single_flight_gate(self.in_flight, self.key, &self.gate);
    }
}

/// Releases a lease on `gate` and removes it from `in_flight` once no holders remain.
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
