use super::*;
fn config() -> RuntimeConfig {
    RuntimeConfig {
        ipc_buffer: 0x400000,
        cspace_root: 1,
        cspace_bits: 8,
        free_start: 16,
        free_end: 256,
    }
}
fn entries() -> [Entry<'static>; 2] {
    [
        Entry {
            name: "horn.vm",
            selector: 2,
        },
        Entry {
            name: "source",
            selector: 3,
        },
    ]
}
#[test]
fn roundtrip_and_borrowed_lookup() {
    let mut bytes = [0; 256];
    let size = encode(config(), &entries(), &mut bytes).unwrap();
    let context = Context::parse(&bytes[..size]).unwrap();
    assert_eq!(context.lookup("horn.vm"), Some(2));
    assert_eq!(context.lookup("source"), Some(3));
    assert_eq!(context.lookup("missing"), None);
    assert_eq!(context.header().total_size, size as u64);
}
#[test]
fn unaligned_record_is_valid() {
    let mut bytes = [0; 257];
    let size = encode(config(), &entries(), &mut bytes[1..]).unwrap();
    assert_eq!(
        Context::parse(&bytes[1..1 + size])
            .unwrap()
            .lookup("source"),
        Some(3)
    );
}
#[test]
fn header_and_offsets_are_checked() {
    let mut original = [0; 256];
    let size = encode(config(), &entries(), &mut original).unwrap();
    for (offset, value) in [
        (0, 0),
        (16, u64::MAX),
        (24, u64::MAX),
        (32, u64::MAX),
        (40, 0),
        (48, u64::MAX),
        (56, 0),
        (64, 0),
        (80, 0),
        (88, 257),
    ] {
        let mut bytes = original;
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        assert!(Context::parse(&bytes[..size]).is_err(), "offset {offset}");
    }
    for end in 0..size {
        assert!(Context::parse(&original[..end]).is_err());
    }
    let mut bytes = original;
    bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
    assert!(matches!(
        Context::parse(&bytes[..size]),
        Err(Error::UnsupportedVersion)
    ));
    bytes = original;
    bytes[76] = 1;
    assert!(Context::parse(&bytes[..size]).is_err());
}
#[test]
fn invalid_names_duplicates_and_free_slot_aliases_are_rejected() {
    for bad in ["", "with space", "nul\0", "не-ascii"] {
        assert_eq!(
            encode(
                config(),
                &[Entry {
                    name: bad,
                    selector: 2
                }],
                &mut [0; 256]
            ),
            Err(Error::Invalid)
        );
    }
    let duplicate = [
        Entry {
            name: "a",
            selector: 2,
        },
        Entry {
            name: "a",
            selector: 3,
        },
    ];
    assert_eq!(
        encode(config(), &duplicate, &mut [0; 256]),
        Err(Error::Invalid)
    );
    let reversed = [
        Entry {
            name: "z",
            selector: 2,
        },
        Entry {
            name: "a",
            selector: 3,
        },
    ];
    assert_eq!(
        encode(config(), &reversed, &mut [0; 256]),
        Err(Error::Invalid)
    );
    assert_eq!(
        encode(
            config(),
            &[Entry {
                name: "a",
                selector: 16
            }],
            &mut [0; 256]
        ),
        Err(Error::Invalid)
    );
}
#[test]
fn wire_entries_are_not_trusted() {
    let mut original = [0; 256];
    let size = encode(config(), &entries(), &mut original).unwrap();
    for (offset, value) in [(96, u64::MAX), (104, u64::MAX), (112, 0), (112, 16)] {
        let mut bytes = original;
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        assert!(Context::parse(&bytes[..size]).is_err());
    }
    let strings = Context::parse(&original[..size])
        .unwrap()
        .header()
        .strings_offset as usize;
    original[strings] = 0;
    assert!(Context::parse(&original[..size]).is_err());
}
#[test]
fn aliases_are_allowed_and_small_output_is_not_modified() {
    let aliases = [
        Entry {
            name: "a",
            selector: 2,
        },
        Entry {
            name: "b",
            selector: 2,
        },
    ];
    let mut bytes = [0; 256];
    let size = encode(config(), &aliases, &mut bytes).unwrap();
    assert_eq!(Context::parse(&bytes[..size]).unwrap().lookup("b"), Some(2));
    let mut small = [0xa5; 16];
    assert_eq!(
        encode(config(), &aliases, &mut small),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(small, [0xa5; 16]);
}
#[test]
fn c_lookup_preserves_output_on_failure() {
    let mut bytes = [0; 256];
    let size = encode(config(), &entries(), &mut bytes).unwrap();
    let mut selector = 99;
    unsafe {
        assert_eq!(horn_startup_validate(bytes.as_ptr(), size), HORN_STARTUP_OK);
        assert_eq!(
            horn_startup_lookup(bytes.as_ptr(), size, b"source".as_ptr(), 6, &mut selector),
            HORN_STARTUP_OK
        );
        assert_eq!(selector, 3);
        assert_eq!(
            horn_startup_lookup(bytes.as_ptr(), size, b"missing".as_ptr(), 7, &mut selector),
            HORN_STARTUP_NOT_FOUND
        );
        assert_eq!(selector, 3);
        assert_eq!(
            horn_startup_validate(core::ptr::null(), 0),
            HORN_STARTUP_INVALID
        );
    }
}
