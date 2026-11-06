// A `local` macro in a crate that did not declare its collector.
gts_kit::local::gts_instance_raw!({
    "id": gts_kit::gts_id!("test.cf.local_fixture.widget.v1~test.cf.local_fixture.undeclared.v1"),
    "name": "undeclared",
});

fn main() {}
