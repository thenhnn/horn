use serde_json::{Value, json};

use super::emit_module;
use crate::parser::Blob;

fn database() -> Value {
    json!({
        "source": {"url": "test", "license": "test", "copyright": []},
        "chips": [{"mask": "0x7cf", "value": "0x641", "mac_version": 63,
                   "name": "RTL8125B", "firmware": "test"}],
        "extended_chips": [],
        "non_gmii": {
            "rtl8106eus": {"mac_version": 43, "name": "RTL8106eus", "firmware": null},
            "rtl8107e": {"mac_version": 48, "name": "RTL8107e", "firmware": null}
        }
    })
}

fn blobs() -> Vec<Blob> {
    vec![Blob {
        name: "test".into(),
        version: "test".into(),
        ops: vec![],
    }]
}

#[test]
fn validates_masks_and_order() {
    assert!(emit_module(&database().to_string(), &blobs()).is_ok());
    for mask in ["0", "0x100000000", "0x0"] {
        let mut data = database();
        data["chips"][0]["mask"] = json!(mask);
        assert!(emit_module(&data.to_string(), &blobs()).is_err());
    }
    let mut data = database();
    data["chips"][0]["value"] = json!("0x800");
    assert!(emit_module(&data.to_string(), &blobs()).is_err());
    let mut data = database();
    let specific = data["chips"][0].clone();
    let mut broad = specific.clone();
    broad["mask"] = json!("0x7c8");
    broad["value"] = json!("0x640");
    data["chips"] = json!([broad.clone(), specific.clone()]);
    assert!(
        emit_module(&data.to_string(), &blobs())
            .unwrap_err()
            .contains("shadowed")
    );
    data["chips"] = json!([specific, broad]);
    assert!(emit_module(&data.to_string(), &blobs()).is_ok());
}

#[test]
fn validates_descriptors_and_firmware_links() {
    assert!(
        emit_module(&database().to_string(), &[])
            .unwrap_err()
            .contains("unknown firmware")
    );
    let mut duplicate = blobs();
    duplicate.extend(blobs());
    assert!(
        emit_module(&database().to_string(), &duplicate)
            .unwrap_err()
            .contains("duplicate firmware")
    );
    for (field, value) in [
        ("mac_version", json!(0)),
        ("name", json!("")),
        ("mac_version", json!(256)),
        ("typo", json!(true)),
    ] {
        let mut data = database();
        data["chips"][0][field] = value;
        assert!(emit_module(&data.to_string(), &blobs()).is_err());
    }
    let mut data = database();
    data["chips"][0]["firmware"] = Value::Null;
    assert!(emit_module(&data.to_string(), &[]).is_ok());
}

#[test]
fn validates_extended_markers() {
    let mut data = database();
    let marker = json!({"mask": "0x7cf", "value": "0x7c8", "extended": true});
    let row = data["chips"][0].clone();
    data["extended_chips"] = json!([row]);
    assert!(emit_module(&data.to_string(), &blobs()).is_err());
    data["chips"].as_array_mut().unwrap().push(marker.clone());
    assert!(emit_module(&data.to_string(), &blobs()).is_ok());
    data["extended_chips"] = json!([marker]);
    assert!(emit_module(&data.to_string(), &blobs()).is_err());
    data["extended_chips"] = json!([]);
    data["chips"][1]["name"] = json!("not a descriptor");
    assert!(emit_module(&data.to_string(), &blobs()).is_err());
}
