use super::*;

#[test]
fn parse_accepts_single_ranges_and_rejects_the_rest() {
    for (header, expected) in [
        (
            "bytes=0-1023",
            Some(ByteRange::Inclusive {
                start: 0,
                end: 1023,
            }),
        ),
        ("bytes=512-", Some(ByteRange::OpenEnded { start: 512 })),
        ("bytes=-256", Some(ByteRange::Suffix { length: 256 })),
        ("bytes=0-1,2-3", None),
        ("0-1023", None),
        ("bytes=-", None),
    ] {
        assert_eq!(parse(header), expected, "{header}");
    }
}

#[test]
fn resolve_clamps_to_total_and_rejects_unsatisfiable() {
    // (range, total, expected inclusive bounds)
    for (range, total, expected) in [
        (
            ByteRange::Inclusive { start: 2, end: 100 },
            10,
            Some((2, 9)),
        ),
        (ByteRange::Suffix { length: 3 }, 10, Some((7, 9))),
        (ByteRange::OpenEnded { start: 10 }, 10, None),
        (ByteRange::OpenEnded { start: 0 }, 0, None),
    ] {
        assert_eq!(range.resolve(total), expected, "{range:?} of {total}");
    }
}
