use modkit_macros::ExpandVars;

#[derive(ExpandVars)]
struct Cfg {
    #[expand_vars]
    retries: u32,
}

fn main() {}
