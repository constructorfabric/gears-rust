// simulated_dir=/workspace/modules/system/resource-group/resource-group/src/api/rest/
// Should not trigger DE1101 - #[cfg(not(test))] is production-only code, not a test module
#[cfg(not(test))]
mod production_diagnostics {
    pub fn init() {}
}

fn main() {}
