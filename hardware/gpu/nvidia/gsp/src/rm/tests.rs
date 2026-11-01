use super::{client::*, codec::Wire, *};
use crate::{FirmwareStatus, raw};

#[test]
fn generated_layout_and_padding_are_used() {
    let params = raw::NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS {
        addressSpace: 2,
        physicalAddr: 0x1_ee5d_0000,
        limit: 0xffff,
        valid: 1,
        channelPBSize: raw::ChannelPBSize_PB_SIZE_64KB,
        ..Default::default()
    };
    let mut bytes =
        [0xa5; size_of::<raw::NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS>()];
    params.write(&mut bytes).unwrap();
    assert_eq!(&bytes[4..8], &[0; 4]);
    assert_eq!(&bytes[37..40], &[0; 3]);
    assert_eq!(&bytes[52..56], &[0; 4]);
    // Reply starts at an arbitrary (deliberately unaligned) byte boundary.
    let mut unaligned = alloc::vec![0x55];
    unaligned.extend_from_slice(&bytes);
    let decoded =
        raw::NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS::read(&unaligned[1..]).unwrap();
    assert_eq!(decoded.physicalAddr, params.physicalAddr);
    assert!(
        raw::NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS::read(&bytes[..55]).is_err()
    );
    assert_eq!(size_of::<raw::NV0000_ALLOC_PARAMETERS>(), 120);
    assert_eq!(<Root as Alloc>::Params::SIZE, 108);
    assert_eq!(<Root570 as Alloc>::Params::SIZE, 120);
    let root = raw::NV0000_ALLOC_PARAMETERS {
        hClient: 7,
        processID: u32::MAX,
        ..Default::default()
    };
    let bytes = root.bytes().unwrap();
    assert!(bytes[108..].iter().all(|byte| *byte == 0));
    let decoded = raw::NV0000_ALLOC_PARAMETERS::read(&bytes).unwrap();
    assert_eq!(decoded.hClient, 7);
    assert!(decoded.pOsPidInfo.is_null());
}

#[test]
fn typed_control_sets_command_and_parameter_size() {
    let mut client = Client::new(7, |function, body: &[u8]| {
        assert_eq!(function, RM_CONTROL);
        let header = raw::rpc_gsp_rm_control_v03_00::read(&body[..CTRL_HEADER])?;
        assert_eq!(header.hClient, 7);
        assert_eq!(header.cmd, raw::NVA06F_CTRL_CMD_GPFIFO_SCHEDULE);
        assert_eq!(header.paramsSize, 2);
        assert_eq!(&body[CTRL_HEADER..], &[1, 0]);
        Ok(body.to_vec())
    });
    let reply = client
        .control::<Schedule>(
            9,
            &raw::NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS {
                bEnable: 1,
                bSkipSubmit: 0,
            },
        )
        .unwrap();
    assert_eq!(reply.bEnable, 1);
}

#[test]
fn typed_alloc_rejects_wrong_client_and_short_body() {
    for wrong_client in [true, false] {
        let mut client = Client::new(7, move |_, body: &[u8]| {
            let mut reply = body.to_vec();
            if wrong_client {
                reply[0] = 8;
            } else {
                reply.pop();
            }
            Ok(reply)
        });
        assert!(matches!(
            client.alloc::<Subdevice>(8, 9, &Default::default()),
            Err(Error::Malformed)
        ));
    }
}

#[test]
fn typed_reply_surfaces_rm_status_before_output() {
    let mut client = Client::new(7, |_, body: &[u8]| {
        let mut header = raw::rpc_gsp_rm_alloc_v03_00::read(&body[..HEADER])?;
        header.status = raw::NV_ERR_GENERIC;
        append(header.bytes()?, &body[HEADER..])
    });
    assert_eq!(
        client.alloc::<Display>(8, 9, &()),
        Err(Error::FirmwareStatus(FirmwareStatus(raw::NV_ERR_GENERIC)))
    );
    assert_eq!(
        alloc::format!("{:?}", FirmwareStatus(raw::NV_ERR_GENERIC)),
        "GENERIC(0xffff)"
    );
}
