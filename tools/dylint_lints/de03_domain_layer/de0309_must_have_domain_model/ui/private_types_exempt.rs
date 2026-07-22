// simulated_dir=/cf-gears/gears/example/src/domain/

// Test: strictly module-private (no `pub`) domain types are exempt from DE0309.
// They never cross a layer boundary, and their fields are still guarded against
// infrastructure leakage by DE0301/DE0308 regardless of the attribute. DE0309 only
// requires #[domain_model] on types visible beyond their module.

// Should not trigger DE0309 - domain_model attribute
#[allow(dead_code)]
struct TokenKey(String);

// Should not trigger DE0309 - domain_model attribute
#[allow(dead_code)]
enum InternalState {
    Ready,
    Pending,
}

fn main() {}
