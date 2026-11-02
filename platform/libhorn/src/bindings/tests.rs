extern crate std;
use super::*;
#[test]
fn provisioning_roundtrips_without_runtime_parameters() {
    let entries = [
        Entry {
            name: "horn.vm",
            selector: 2,
        },
        Entry {
            name: "source",
            selector: 7,
        },
    ];
    let mut out = [0; 128];
    let n = encode(&entries, &mut out).unwrap();
    let parsed = Bindings::parse(&out[..n]).unwrap();
    assert_eq!(
        parsed
            .iter()
            .map(|e| (e.name, e.selector))
            .collect::<std::vec::Vec<_>>(),
        entries
            .iter()
            .map(|e| (e.name, e.selector))
            .collect::<std::vec::Vec<_>>()
    );
    // Page padding is not descriptor data; the provisioning RPC carries n.
    assert!(Bindings::parse(&out).is_err());
    assert_eq!(Bindings::parse(b"HORNBND2").unwrap().iter().count(), 0);
}
#[test]
fn invalid_and_truncated_descriptors_fail() {
    let mut out = [0; 64];
    let n = encode(
        &[Entry {
            name: "x",
            selector: 2,
        }],
        &mut out,
    )
    .unwrap();
    for end in 0..n {
        if end != 8 {
            assert!(Bindings::parse(&out[..end]).is_err());
        }
    }
    for (offset, data) in [
        (0, &b"BADMAGIC"[..]),
        (8, &0u64.to_le_bytes()[..]),
        (16, &u64::MAX.to_le_bytes()[..]),
        (24, &[0x80][..]),
    ] {
        let mut bad = out;
        bad[offset..offset + data.len()].copy_from_slice(data);
        assert!(Bindings::parse(&bad[..n]).is_err());
    }
    for entries in [
        [
            Entry {
                name: "x",
                selector: 1,
            },
            Entry {
                name: "x",
                selector: 2,
            },
        ],
        [
            Entry {
                name: "z",
                selector: 1,
            },
            Entry {
                name: "a",
                selector: 2,
            },
        ],
        [
            Entry {
                name: "",
                selector: 1,
            },
            Entry {
                name: "x",
                selector: 2,
            },
        ],
    ] {
        assert!(encode(&entries, &mut out).is_err());
    }
}
#[test]
fn names_have_no_transport_sized_length_limit_and_aliases_are_allowed() {
    let name = "x".repeat(8192);
    let entries = [
        Entry {
            name: "a",
            selector: 2,
        },
        Entry {
            name: &name,
            selector: 2,
        },
    ];
    let mut out = std::vec![0; 8 + 32 + 1 + name.len()];
    let n = encode(&entries, &mut out).unwrap();
    assert_eq!(
        Bindings::parse(&out[..n])
            .unwrap()
            .iter()
            .nth(1)
            .unwrap()
            .name,
        name
    );
}
