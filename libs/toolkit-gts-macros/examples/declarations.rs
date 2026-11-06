//! Prints the fixture crate's declared GTS ids, one per line.

fn main() {
    for (id, _) in declarations_fixture::gts_declarations() {
        println!("{id}");
    }
}
