use modkit_macros::ExpandVars;
use std::collections::HashMap;

#[derive(ExpandVars)]
struct Inner {
    #[expand_vars]
    secret: String,
    #[expand_vars]
    label: Option<String>,
    count: u32,
}

#[derive(ExpandVars)]
struct Cfg {
    #[expand_vars]
    api_key: String,
    #[expand_vars]
    endpoint: Option<String>,
    #[expand_vars]
    providers: HashMap<String, Inner>,
    #[expand_vars]
    tags: Vec<Inner>,
    retries: u32,
}

fn main() {}
