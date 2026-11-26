use toolkit_odata_macros::ODataFilterable;

#[derive(ODataFilterable)]
struct ItemQuery {
    #[odata(filter(kind = "String", orderable = "no"))]
    region: String,
}

fn main() {}
