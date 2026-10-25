use super::*;

#[test]
fn firmware_selection_and_first_match() {
    let chip = match_chip(0x641, true, None).unwrap();
    assert_eq!(chip.mac_version, MacVersion::V63);
    assert_eq!(chip.name, "RTL8125B");
    let firmware = chip.firmware.unwrap();
    assert_eq!(firmware.name, "rtl8125b-2");
    assert!(core::ptr::eq(
        firmware,
        FIRMWARE.iter().find(|fw| fw.name == firmware.name).unwrap()
    ));

    // The exact rule must precede the broader 0x7c8 mask.
    assert_eq!(
        match_chip(0x3c9, true, None).unwrap().mac_version,
        MacVersion::V23
    );
    assert_eq!(
        match_chip(0x3ca, true, None).unwrap().mac_version,
        MacVersion::V24
    );
    assert!(match_chip(0x54b, true, None).unwrap().firmware.is_none());
    assert_eq!(
        match_chip(0x54a, true, None)
            .unwrap()
            .firmware
            .unwrap()
            .name,
        "rtl8168fp-3"
    );
    assert!(match_chip(0x008, true, None).unwrap().firmware.is_none());
    assert_eq!(
        match_chip(0, true, None).unwrap_err(),
        MatchError::UnknownXid(0)
    );
}

#[test]
fn gmii_overrides_and_extended_xids() {
    assert_eq!(
        match_chip(0x509, true, None).unwrap().mac_version,
        MacVersion::V42
    );
    let chip = match_chip(0x509, false, None).unwrap();
    assert_eq!(chip.mac_version, MacVersion::V43);
    assert_eq!(chip.name, "RTL8106eus");
    assert_eq!(chip.firmware.unwrap().name, "rtl8106e-2");
    let chip = match_chip(0x541, false, None).unwrap();
    assert_eq!(chip.mac_version, MacVersion::V48);
    assert_eq!(chip.firmware.unwrap().name, "rtl8107e-2");
    assert_eq!(
        match_chip(0x7c8, true, None).unwrap_err(),
        MatchError::NeedsExtendedXid
    );
    for xid in [0, 0x8000_0000] {
        let chip = match_chip(0x7c8, false, Some(xid)).unwrap();
        assert_eq!(chip.name, "RTL9151AS");
        assert_eq!(chip.mac_version, MacVersion::V64);
        assert_eq!(chip.firmware.unwrap().name, "rtl9151a-1");
    }
    assert_eq!(
        match_chip(0x7c8, true, Some(1)).unwrap_err(),
        MatchError::UnknownExtendedXid(1)
    );
    assert_eq!(match_chip(0x641, true, Some(1)).unwrap().name, "RTL8125B");
}

fn hex(value: &serde_json::Value) -> u32 {
    u32::from_str_radix(value.as_str().unwrap().strip_prefix("0x").unwrap(), 16).unwrap()
}

#[test]
fn generated_matcher_agrees_with_ordered_json() {
    let data: serde_json::Value = serde_json::from_str(include_str!("../chips.json")).unwrap();
    for xid in 0..=0xfff {
        for gmii in [false, true] {
            let row = data["chips"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| xid & hex(&row["mask"]) == hex(&row["value"]));
            let Some(row) = row else {
                assert_eq!(
                    match_chip(xid, gmii, None).unwrap_err(),
                    MatchError::UnknownXid(xid)
                );
                continue;
            };
            if row["extended"] == true {
                assert_eq!(
                    match_chip(xid, gmii, None).unwrap_err(),
                    MatchError::NeedsExtendedXid
                );
                continue;
            }
            let expected = match (row["mac_version"].as_u64().unwrap(), gmii) {
                (42, false) => &data["non_gmii"]["rtl8106eus"],
                (46, false) => &data["non_gmii"]["rtl8107e"],
                _ => row,
            };
            let actual = match_chip(xid, gmii, None).unwrap();
            assert_eq!(
                u64::from(actual.mac_version as u8),
                expected["mac_version"].as_u64().unwrap()
            );
            assert_eq!(actual.name, expected["name"].as_str().unwrap());
            assert_eq!(
                actual.firmware.map(|fw| fw.name),
                expected["firmware"].as_str()
            );
        }
    }
}
