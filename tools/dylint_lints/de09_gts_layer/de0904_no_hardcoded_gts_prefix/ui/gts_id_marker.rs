macro_rules! gts_id {
    ($suffix:literal) => {
        $suffix
    };
}

const TYPE_ID: &str = gts_id!("cf.core.users.user.v1~");

fn main() {
    let _ = TYPE_ID;
}
