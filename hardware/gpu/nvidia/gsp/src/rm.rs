// SPDX-License-Identifier: GPL-2.0-only
//! r570.144 RM API. Layouts and constants come from unmodified upstream headers.
use crate::{Error, Result, abi, raw};
use alloc::vec::Vec;
use core::mem::size_of;
pub mod client;
pub mod codec;
#[cfg(test)]
#[path = "rm/tests.rs"]
mod typed_tests;
pub use client::Client;
use codec::Wire;

pub const RM_ALLOC: u32 = abi::NV_VGPU_MSG_FUNCTION_GSP_RM_ALLOC;
pub const RM_CONTROL: u32 = abi::NV_VGPU_MSG_FUNCTION_GSP_RM_CONTROL;
pub const HANDLE_CLIENT: u32 = 0xc1d0_0000;
pub const HANDLE_DEVICE: u32 = 0xde1d_0000;
pub const HANDLE_SUBDEVICE: u32 = 0x5d1d_0000;
pub use raw::{NV01_DEVICE_0, NV01_ROOT, NV20_SUBDEVICE_0};

const HEADER: usize = size_of::<raw::rpc_gsp_rm_alloc_v03_00>();
const CTRL_HEADER: usize = size_of::<raw::rpc_gsp_rm_control_v03_00>();
const ROOT_PARAMS: usize = size_of::<raw::rpc_alloc_root_v07_00>();
const DEVICE_PARAMS: usize = size_of::<raw::NV0080_ALLOC_PARAMETERS>();
const SUBDEVICE_PARAMS: usize = size_of::<raw::NV2080_ALLOC_PARAMETERS>();

fn fixed<T: Wire, const N: usize>(value: T) -> [u8; N] {
    let mut out = [0; N];
    value.write(&mut out).expect("generated ABI size");
    out
}
fn append(mut header: Vec<u8>, params: &[u8]) -> Result<Vec<u8>> {
    header
        .try_reserve_exact(params.len())
        .map_err(|_| Error::Exhausted)?;
    header.extend_from_slice(params);
    Ok(header)
}
fn check_status(status: u32) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(Error::FirmwareStatus(crate::FirmwareStatus(status)))
    }
}
pub fn root_params(h_client: u32) -> [u8; ROOT_PARAMS] {
    fixed(raw::rpc_alloc_root_v07_00 {
        hClient: h_client,
        processID: u32::MAX,
        ..Default::default()
    })
}
pub fn device_params(h_client_share: u32) -> [u8; DEVICE_PARAMS] {
    fixed(raw::NV0080_ALLOC_PARAMETERS {
        hClientShare: h_client_share,
        ..Default::default()
    })
}
pub fn subdevice_params() -> [u8; SUBDEVICE_PARAMS] {
    fixed(raw::NV2080_ALLOC_PARAMETERS::default())
}
pub fn encode(
    h_client: u32,
    h_parent: u32,
    h_object: u32,
    class: u32,
    params: &[u8],
) -> Result<Vec<u8>> {
    if class > u16::MAX as u32 {
        return Err(Error::Range);
    }
    let header = raw::rpc_gsp_rm_alloc_v03_00 {
        hClient: h_client,
        hParent: h_parent,
        hObject: h_object,
        hClass: class,
        paramsSize: params.len().try_into().map_err(|_| Error::Range)?,
        ..Default::default()
    };
    append(header.bytes()?, params)
}
pub fn status(body: &[u8]) -> Result<u32> {
    let header = raw::rpc_gsp_rm_alloc_v03_00::read(body.get(..HEADER).ok_or(Error::Malformed)?)?;
    Ok(header.status)
}
pub fn check(body: &[u8], h_object: u32, class: u32, params_size: usize) -> Result<()> {
    let header = raw::rpc_gsp_rm_alloc_v03_00::read(body.get(..HEADER).ok_or(Error::Malformed)?)?;
    if header.hObject != h_object
        || header.hClass != class
        || header.paramsSize as usize != params_size
        || body.len().checked_sub(HEADER) != Some(params_size)
    {
        return Err(Error::Malformed);
    }
    check_status(header.status)
}
pub fn encode_ctrl(h_client: u32, h_object: u32, cmd: u32, params: &[u8]) -> Result<Vec<u8>> {
    let header = raw::rpc_gsp_rm_control_v03_00 {
        hClient: h_client,
        hObject: h_object,
        cmd,
        paramsSize: params.len().try_into().map_err(|_| Error::Range)?,
        ..Default::default()
    };
    append(header.bytes()?, params)
}
pub fn check_ctrl(body: &[u8], h_object: u32, cmd: u32, params_size: usize) -> Result<()> {
    let header =
        raw::rpc_gsp_rm_control_v03_00::read(body.get(..CTRL_HEADER).ok_or(Error::Malformed)?)?;
    if header.hObject != h_object
        || header.cmd != cmd
        || header.paramsSize as usize != params_size
        || body.len().checked_sub(CTRL_HEADER) != Some(params_size)
    {
        return Err(Error::Malformed);
    }
    check_status(header.status)
}

/// `NVA06F_CTRL_CMD_BIND` and `NVA06F_CTRL_CMD_GPFIFO_SCHEDULE`: after the
/// channel object exists, RM must be told which engine to bind and that the
/// GPFIFO may be scheduled. Without these two controls the channel is inert.
pub const CTRL_CHANNEL_BIND: u32 = raw::NVA06F_CTRL_CMD_BIND;
pub const CTRL_CHANNEL_GPFIFO_SCHEDULE: u32 = raw::NVA06F_CTRL_CMD_GPFIFO_SCHEDULE;

pub fn bind_params(engine_type: u32) -> [u8; size_of::<raw::NVA06F_CTRL_BIND_PARAMS>()] {
    fixed(raw::NVA06F_CTRL_BIND_PARAMS {
        engineType: engine_type,
    })
}

/// `NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS` is two `NvBool`s, and `NvBool` is
/// `NvU8`, so the wire payload is two bytes; a wider payload is rejected as an
/// invalid argument.
pub fn schedule_params(enable: bool) -> [u8; size_of::<raw::NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS>()] {
    fixed(raw::NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS {
        bEnable: u8::from(enable),
        bSkipSubmit: 0,
    })
}

/// `NVC0B5_ALLOCATION_PARAMETERS`: the copy-engine object carries a version and
/// the engine instance it drives.
pub fn ce_params(engine_type: u32) -> [u8; size_of::<raw::NVC0B5_ALLOCATION_PARAMETERS>()] {
    fixed(raw::NVC0B5_ALLOCATION_PARAMETERS {
        version: raw::NVC0B5_ALLOCATION_PARAMETERS_VERSION_1,
        engineType: engine_type,
    })
}

/// Display objects. The root class differs per chip family; AD10x uses
/// `AD102_DISP`, and the common object carries the display control interface.
pub const NV04_DISPLAY_COMMON: u32 = raw::NV04_DISPLAY_COMMON;
pub const AD102_DISP: u32 = raw::NVC770_DISPLAY;
/// Handle for the display root: RM expects the class in the high half.
pub const HANDLE_DISP_ROOT: u32 = AD102_DISP << 16;
pub const HANDLE_DISP_COMMON: u32 = 0x0073_0000;

/// `NV0073_CTRL_CMD_SYSTEM_GET_SUPPORTED` on the display common object.
pub const CTRL_DISP_GET_SUPPORTED: u32 = raw::NV0073_CTRL_CMD_SYSTEM_GET_SUPPORTED;
/// Display channel classes: one core channel, one window channel per head.
pub const AD102_DISP_CORE_CHANNEL_DMA: u32 = raw::NVC77D_CORE_CHANNEL_DMA;
pub const GA102_DISP_WINDOW_CHANNEL_DMA: u32 = raw::NVC67E_WINDOW_CHANNEL_DMA;
/// `NV2080_CTRL_CMD_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER` on the subdevice.
pub const CTRL_DISPLAY_CHANNEL_PUSHBUFFER: u32 =
    raw::NV2080_CTRL_CMD_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER;
/// `ADDR_SYSMEM` and `ADDR_FBMEM` as the display controls spell them.
pub const DISP_ADDR_SYSMEM: u32 = 1;
pub const DISP_ADDR_FBMEM: u32 = 2;

/// `NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS`: 52 bytes of
/// fields, 8-aligned to 56.
pub struct DispPushbuffer {
    pub address_space: u32,
    pub physical: u64,
    pub limit: u64,
    pub cache_snoop: u32,
    pub class: u32,
    pub instance: u32,
    pub valid: bool,
    pub aperture: u32,
    pub size: u32,
    pub subdevice: u32,
}

pub fn disp_pushbuffer_params(
    pb: &DispPushbuffer,
) -> [u8; size_of::<raw::NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS>()] {
    fixed(
        raw::NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS {
            addressSpace: pb.address_space,
            physicalAddr: pb.physical,
            limit: pb.limit,
            cacheSnoop: pb.cache_snoop,
            hclass: pb.class,
            channelInstance: pb.instance,
            valid: u8::from(pb.valid),
            pbTargetAperture: pb.aperture,
            channelPBSize: pb.size,
            subDeviceId: pb.subdevice,
        },
    )
}

/// `PBTARGETAPERTURE`: which aperture the push buffer lives in. Zero is `IOVA`,
/// which is why leaving it unset made RM reject the offset.
pub const PB_APERTURE_PHYS_NVM: u32 = 1;
/// `ChannelPBSize`: 0 means a 4 KiB buffer, so a 64 KiB buffer must say so or
/// RM sees an announced size that does not match the channel.
pub const PB_SIZE_64KB: u32 = raw::ChannelPBSize_PB_SIZE_64KB;

/// `GA102_DISP_CURSOR`: a PIO channel, allocated without a push buffer.
pub const GA102_DISP_CURSOR: u32 = raw::NVC67A_CURSOR_IMM_CHANNEL_PIO;

/// `NV50VAIO_CHANNELPIO_ALLOCATION_PARAMETERS`: 16 bytes.
pub fn disp_chan_pio_params(
    instance: u32,
) -> [u8; size_of::<raw::NV50VAIO_CHANNELPIO_ALLOCATION_PARAMETERS>()] {
    fixed(raw::NV50VAIO_CHANNELPIO_ALLOCATION_PARAMETERS {
        channelInstance: instance,
        ..Default::default()
    })
}

/// `NV50VAIO_CHANNELDMA_ALLOCATION_PARAMETERS`: 36 bytes of fields, 8-aligned to
/// 40. The push buffer itself is announced by the `DISPLAY_CHANNEL_PUSHBUFFER`
/// control that precedes the allocation; this structure only names the instance,
/// the size class and the subdevice mask.
pub fn disp_chan_dma_params(
    instance: u32,
    offset: u32,
    h_object_buffer: u32,
) -> [u8; size_of::<raw::NV50VAIO_CHANNELDMA_ALLOCATION_PARAMETERS>()] {
    fixed(raw::NV50VAIO_CHANNELDMA_ALLOCATION_PARAMETERS {
        channelInstance: instance,
        hObjectBuffer: h_object_buffer,
        offset,
        channelPBSize: raw::ChannelPBSize_PB_SIZE_64KB,
        subDeviceId: 1,
        ..Default::default()
    })
}

/// RM expects the class in the high half of a display channel handle, with the
/// channel instance in the low half.
pub fn disp_chan_handle(class: u32, instance: u32) -> u32 {
    (class << 16) | instance
}

/// `NV2080_CTRL_CMD_INTERNAL_DISPLAY_GET_STATIC_INFO` on the subdevice.
pub const CTRL_DISPLAY_GET_STATIC_INFO: u32 = raw::NV2080_CTRL_CMD_INTERNAL_DISPLAY_GET_STATIC_INFO;
/// `NV2080_CTRL_CMD_INTERNAL_DISPLAY_WRITE_INST_MEM` on the subdevice.
pub const CTRL_DISPLAY_WRITE_INST_MEM: u32 = raw::NV2080_CTRL_CMD_INTERNAL_DISPLAY_WRITE_INST_MEM;

/// `ADDR_FBMEM` and `NV_MEMORY_WRITECOMBINED`, as the display instance memory
/// descriptor expects them.
pub const DISP_INST_ADDR_SPACE: u32 = 2;
pub const DISP_INST_CACHE_ATTR: u32 = 2;

/// `NV2080_CTRL_INTERNAL_DISPLAY_WRITE_INST_MEM_PARAMS`.
pub fn disp_inst_mem_params(
    phys: u64,
    size: u64,
) -> [u8; size_of::<raw::NV2080_CTRL_INTERNAL_DISPLAY_WRITE_INST_MEM_PARAMS>()] {
    fixed(raw::NV2080_CTRL_INTERNAL_DISPLAY_WRITE_INST_MEM_PARAMS {
        instMemPhysAddr: phys,
        instMemSize: size,
        instMemAddrSpace: DISP_INST_ADDR_SPACE,
        instMemCpuCacheAttr: DISP_INST_CACHE_ATTR,
    })
}

/// `NV0073_CTRL_SYSTEM_GET_SUPPORTED_PARAMS`: the subdevice instance is an input,
/// the two masks are outputs.
pub fn disp_supported_params() -> [u8; size_of::<raw::NV0073_CTRL_SYSTEM_GET_SUPPORTED_PARAMS>()] {
    fixed(raw::NV0073_CTRL_SYSTEM_GET_SUPPORTED_PARAMS::default())
}

pub fn disp_supported_mask(body: &[u8]) -> Result<u32> {
    Ok(raw::NV0073_CTRL_SYSTEM_GET_SUPPORTED_PARAMS::read(
        body.get(CTRL_HEADER..).ok_or(Error::Malformed)?,
    )?
    .displayMask)
}

/// `NV2080_CTRL_INTERNAL_DISPLAY_GET_STATIC_INFO_PARAMS` is 36 bytes: three
/// words, a `NvBool` (one byte), five more words, two `NvBool`s and a final
/// word. Sending the 12-byte supported-params buffer instead is rejected as an
/// invalid argument.
pub fn disp_static_info_params()
-> [u8; size_of::<raw::NV2080_CTRL_INTERNAL_DISPLAY_GET_STATIC_INFO_PARAMS>()] {
    fixed(raw::NV2080_CTRL_INTERNAL_DISPLAY_GET_STATIC_INFO_PARAMS::default())
}

/// The three fields of `NV2080_CTRL_INTERNAL_DISPLAY_GET_STATIC_INFO_PARAMS`
/// the display path needs: present windows, heads and channels.
#[derive(Clone, Copy, Debug)]
pub struct DispStaticInfo {
    pub window_mask: u32,
    pub heads: u32,
    pub channels: u32,
}

pub fn disp_static_info(body: &[u8]) -> Result<DispStaticInfo> {
    let params = raw::NV2080_CTRL_INTERNAL_DISPLAY_GET_STATIC_INFO_PARAMS::read(
        body.get(CTRL_HEADER..).ok_or(Error::Malformed)?,
    )?;
    Ok(DispStaticInfo {
        window_mask: params.windowPresentMask,
        heads: params.numHeads,
        channels: params.numDispChannels,
    })
}

/// `NV2080_CTRL_CMD_FIFO_GET_DEVICE_INFO_TABLE`: the engine table tells which
/// engines this GPU exposes and which runlist each one uses. The runlist id is
/// assigned by RM, so it cannot be assumed.
pub const CTRL_FIFO_GET_DEVICE_INFO_TABLE: u32 = raw::NV2080_CTRL_CMD_FIFO_GET_DEVICE_INFO_TABLE;
pub const ENGINE_TABLE_ENTRIES: usize = 32;
pub const ENGINE_ENTRY_BYTES: usize = size_of::<raw::NV2080_CTRL_FIFO_DEVICE_ENTRY>();
pub const ENGINE_TABLE_BYTES: usize =
    size_of::<raw::NV2080_CTRL_FIFO_GET_DEVICE_INFO_TABLE_PARAMS>();
/// Index of `RM_ENGINE_TYPE_*` and the runlist id inside `engineData`.
pub const ENGINE_INFO_RM_TYPE: usize = 2;
pub const ENGINE_INFO_RUNLIST: usize = 3;
pub const ENGINE_INFO_INSTANCE: usize = 10;
/// `RM_ENGINE_TYPE_COPY0`.
pub const RM_ENGINE_TYPE_COPY0: u32 = 9;

pub fn engine_table_params() -> [u8; ENGINE_TABLE_BYTES] {
    [0u8; ENGINE_TABLE_BYTES]
}

/// One engine table row, as far as the copy path cares.
#[derive(Clone, Copy, Debug)]
pub struct EngineEntry {
    pub rm_type: u32,
    pub runlist: u32,
    pub instance: u32,
    pub name: [u8; 16],
}

pub fn engine_entries(body: &[u8]) -> Result<Vec<EngineEntry>> {
    let params = raw::NV2080_CTRL_FIFO_GET_DEVICE_INFO_TABLE_PARAMS::read(
        body.get(CTRL_HEADER..).ok_or(Error::Malformed)?,
    )?;
    engine_table_entries(&params)
}
pub fn engine_table_entries(
    params: &raw::NV2080_CTRL_FIFO_GET_DEVICE_INFO_TABLE_PARAMS,
) -> Result<Vec<EngineEntry>> {
    let entries = params
        .entries
        .get(..params.numEntries as usize)
        .ok_or(Error::Malformed)?;
    let mut out = Vec::new();
    out.try_reserve_exact(entries.len())
        .map_err(|_| Error::Exhausted)?;
    for entry in entries {
        out.push(EngineEntry {
            rm_type: entry.engineData[ENGINE_INFO_RM_TYPE],
            runlist: entry.engineData[ENGINE_INFO_RUNLIST],
            instance: entry.engineData[ENGINE_INFO_INSTANCE],
            name: entry.engineName.map(|c| c as u8),
        });
    }
    Ok(out)
}

/// `NV2080_CTRL_CMD_CE_GET_FAULT_METHOD_BUFFER_SIZE`: the fault method buffer
/// is sized by RM, not by the driver, so ask before allocating it.
pub const CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE: u32 =
    raw::NV2080_CTRL_CMD_CE_GET_FAULT_METHOD_BUFFER_SIZE;

/// The control takes no input; RM writes the required size into the reply.
pub fn mthdbuf_params() -> [u8; size_of::<raw::NV2080_CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE_PARAMS>()]
{
    fixed(raw::NV2080_CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE_PARAMS::default())
}

/// Required method-buffer size in bytes from the control reply body.
pub fn mthdbuf_size(body: &[u8]) -> Result<u32> {
    Ok(
        raw::NV2080_CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE_PARAMS::read(
            body.get(CTRL_HEADER..).ok_or(Error::Malformed)?,
        )?
        .size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_layout_matches_the_rm_excerpts() {
        let body = encode(0, 0, 0x1000, NV01_ROOT, &root_params(0x1000)).unwrap();
        assert_eq!(body.len(), HEADER + ROOT_PARAMS);
        assert_eq!(u32::from_le_bytes(body[8..12].try_into().unwrap()), 0x1000);
        assert_eq!(u32::from_le_bytes(body[20..24].try_into().unwrap()), 108);

        let device = device_params(0x1000);
        assert_eq!(u32::from_le_bytes(device[4..8].try_into().unwrap()), 0x1000);
        assert_eq!(
            encode(1, 1, 2, NV01_DEVICE_0, &device).unwrap().len(),
            32 + 56
        );
        assert_eq!(
            encode(1, 2, 3, NV20_SUBDEVICE_0, &subdevice_params())
                .unwrap()
                .len(),
            36
        );
    }

    #[test]
    fn reply_must_echo_our_object_class_and_size() {
        let mut reply = encode(0, 0, 0x1000, NV01_ROOT, &root_params(0x1000)).unwrap();
        check(&reply, 0x1000, NV01_ROOT, ROOT_PARAMS).unwrap();
        assert!(check(&reply, 0x1001, NV01_ROOT, ROOT_PARAMS).is_err());
        assert!(check(&reply, 0x1000, NV01_DEVICE_0, ROOT_PARAMS).is_err());
        assert!(check(&reply, 0x1000, NV01_ROOT, 4).is_err());
        reply[16] = 0x20; // RM status: invalid object
        assert!(matches!(
            check(&reply, 0x1000, NV01_ROOT, ROOT_PARAMS),
            Err(Error::FirmwareStatus(crate::FirmwareStatus(0x20)))
        ));
    }
    #[test]
    fn control_layout_and_echo() {
        let params = [0u8; 32];
        let body = encode_ctrl(HANDLE_CLIENT, HANDLE_DEVICE, 0x801813, &params).unwrap();
        assert_eq!(body.len(), CTRL_HEADER + 32);
        assert_eq!(
            u32::from_le_bytes(body[8..12].try_into().unwrap()),
            0x801813
        );
        assert_eq!(u32::from_le_bytes(body[16..20].try_into().unwrap()), 32);
        check_ctrl(&body, HANDLE_DEVICE, 0x801813, 32).unwrap();
        assert!(check_ctrl(&body, HANDLE_DEVICE, 0x801814, 32).is_err());
        assert!(check_ctrl(&body, HANDLE_SUBDEVICE, 0x801813, 32).is_err());
    }
}

/// `FERMI_VASPACE_A` and its RMAPI handle range.
pub const FERMI_VASPACE_A: u32 = raw::FERMI_VASPACE_A;
pub const HANDLE_VASPACE: u32 = 0x90f1_0000;
/// `NV_VASPACE_ALLOCATION_FLAGS_IS_EXTERNALLY_OWNED`: the driver owns the
/// page tables and hands RM only the root address.
pub const VASPACE_EXTERNALLY_OWNED: u32 = raw::HORN_VASPACE_EXTERNALLY_OWNED;

/// `NV0080_CTRL_CMD_DMA_SET_PAGE_DIRECTORY` and its aperture flags.
pub const CTRL_DMA_SET_PAGE_DIRECTORY: u32 = raw::NV0080_CTRL_CMD_DMA_SET_PAGE_DIRECTORY;
pub const SET_PD_APERTURE_SYSMEM_COH: u32 = 1;

const VSPACE_PARAMS: usize = size_of::<raw::NV_VASPACE_ALLOCATION_PARAMETERS>();
const PD_PARAMS: usize = size_of::<raw::NV0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS>();

const _: () = assert!(VSPACE_PARAMS == 8 + 8 + 8 + 8 + 4 + 8 + 4);
const _: () = assert!(PD_PARAMS == 8 + 4 + 4 + 4 + 4 + 4 + 4);

/// `NV_VASPACE_ALLOCATION_PARAMETERS`: index, flags, then 8-byte aligned
/// vaSize, vaStartInternal, vaLimitInternal, bigPageSize and vaBase.
pub fn vspace_params(
    flags: u32,
    va_size: u64,
    va_start: u64,
    va_limit: u64,
) -> [u8; VSPACE_PARAMS] {
    fixed(raw::NV_VASPACE_ALLOCATION_PARAMETERS {
        flags,
        vaSize: va_size,
        vaStartInternal: va_start,
        vaLimitInternal: va_limit,
        ..Default::default()
    })
}

/// `NV0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS`.
pub fn page_directory_params(
    phys_address: u64,
    num_entries: u32,
    flags: u32,
    h_vaspace: u32,
) -> [u8; PD_PARAMS] {
    fixed(raw::NV0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS {
        physAddress: phys_address,
        numEntries: num_entries,
        flags,
        hVASpace: h_vaspace,
        ..Default::default()
    })
}

#[cfg(test)]
mod vspace_tests {
    use super::*;

    #[test]
    fn vspace_and_page_directory_layouts() {
        let v = vspace_params(VASPACE_EXTERNALLY_OWNED, 1 << 40, 0, (1 << 40) - 1);
        assert_eq!(u32::from_le_bytes(v[4..8].try_into().unwrap()), 8);
        assert_eq!(u64::from_le_bytes(v[8..16].try_into().unwrap()), 1 << 40);
        assert_eq!(
            u64::from_le_bytes(v[24..32].try_into().unwrap()),
            (1 << 40) - 1
        );

        let p = page_directory_params(0x1_0000, 512, SET_PD_APERTURE_SYSMEM_COH, HANDLE_VASPACE);
        assert_eq!(u64::from_le_bytes(p[0..8].try_into().unwrap()), 0x1_0000);
        assert_eq!(u32::from_le_bytes(p[8..12].try_into().unwrap()), 512);
        assert_eq!(u32::from_le_bytes(p[12..16].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(p[16..20].try_into().unwrap()),
            HANDLE_VASPACE
        );
    }
}

/// `NV01_MEMORY_LOCAL_USER`: RM-owned VRAM allocation.
pub const NV01_MEMORY_LOCAL_USER: u32 = raw::NV01_MEMORY_LOCAL_USER;
/// `NVOS32_TYPE_IMAGE`: plain memory surface.
pub const NVOS32_TYPE_IMAGE: u32 = raw::NVOS32_TYPE_IMAGE;
/// `NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE`.
pub const NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE: u32 = raw::NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE;

const VRAM_PARAMS: usize = size_of::<raw::NV_MEMORY_ALLOCATION_PARAMS>();

// owner..pitch (20), attr/attr2/format/comprCovg/zcullCovg (20) + 4 padding,
// then six 8-byte fields, then ctagOffset..numaNode and trailing padding.
const _: () = assert!(VRAM_PARAMS == 44 + 48 + 20 + 16);

/// `NV_MEMORY_ALLOCATION_PARAMS` for a plain VRAM buffer. `h_vaspace` is 0:
/// the allocation is not bound to a VA space here, the caller maps it.
/// `NVOS32_ALLOC_FLAGS_FIXED_ADDRESS_ALLOCATE`: place the surface at the offset
/// given in the request instead of letting RM choose.
pub const ALLOC_FIXED_ADDRESS: u32 = raw::NVOS32_ALLOC_FLAGS_FIXED_ADDRESS_ALLOCATE;

/// Ask for a surface at a chosen VRAM offset. The display engine addresses its
/// buffers with 32-bit values, so they cannot live at the high offsets RM hands
/// out by default.
pub fn vram_params_fixed(owner: u32, size: u64, offset: u64) -> Result<[u8; VRAM_PARAMS]> {
    Ok(fixed(vram_request_fixed(owner, size, offset)?))
}
pub fn vram_request_fixed(
    owner: u32,
    size: u64,
    offset: u64,
) -> Result<raw::NV_MEMORY_ALLOCATION_PARAMS> {
    if size == 0 || size & 4095 != 0 || offset & 4095 != 0 {
        return Err(Error::Range);
    }
    Ok(raw::NV_MEMORY_ALLOCATION_PARAMS {
        owner,
        type_: raw::NVOS32_TYPE_IMAGE,
        flags: raw::NVOS32_ALLOC_FLAGS_FIXED_ADDRESS_ALLOCATE,
        size,
        offset,
        ..Default::default()
    })
}

pub fn vram_params(owner: u32, size: u64, alignment: u64) -> Result<[u8; VRAM_PARAMS]> {
    Ok(fixed(vram_request(owner, size, alignment)?))
}
pub fn vram_request(
    owner: u32,
    size: u64,
    alignment: u64,
) -> Result<raw::NV_MEMORY_ALLOCATION_PARAMS> {
    if size == 0 || size & 4095 != 0 || alignment & 4095 != 0 {
        return Err(Error::Range);
    }
    Ok(raw::NV_MEMORY_ALLOCATION_PARAMS {
        owner,
        type_: raw::NVOS32_TYPE_IMAGE,
        size,
        alignment,
        flags: if alignment == 0 {
            0
        } else {
            raw::NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE
        },
        ..Default::default()
    })
}

#[cfg(test)]
mod vram_tests {
    use super::*;

    #[test]
    fn vram_params_layout() {
        let p = vram_params(HANDLE_CLIENT, 0x2000, 4096).unwrap();
        assert_eq!(
            u32::from_le_bytes(p[4..8].try_into().unwrap()),
            NVOS32_TYPE_IMAGE
        );
        assert_eq!(
            u32::from_le_bytes(p[8..12].try_into().unwrap()),
            NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE
        );
        assert_eq!(u64::from_le_bytes(p[64..72].try_into().unwrap()), 0x2000);
        assert_eq!(u64::from_le_bytes(p[72..80].try_into().unwrap()), 4096);
        assert!(vram_params(HANDLE_CLIENT, 0, 4096).is_err());
        let unaligned = vram_params(HANDLE_CLIENT, 0x2000, 0).unwrap();
        assert_eq!(u32::from_le_bytes(unaligned[8..12].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(unaligned[72..80].try_into().unwrap()), 0);
        assert_eq!(
            u64::from_le_bytes(unaligned[64..72].try_into().unwrap()),
            0x2000
        );
        assert!(vram_params(HANDLE_CLIENT, 0x1000, 1).is_err());
    }
}

/// `VOLTA_CHANNEL_GPFIFO_A`, `AMPERE_CHANNEL_GPFIFO_A` and `AMPERE_DMA_COPY_A`.
/// GA10x (and therefore AD106) binds channels with the Ampere GPFIFO class;
/// the Volta class is only used by older chips.
pub const VOLTA_CHANNEL_GPFIFO_A: u32 = 0xc36f;
pub const AMPERE_CHANNEL_GPFIFO_A: u32 = raw::AMPERE_CHANNEL_GPFIFO_A;
pub const AMPERE_DMA_COPY_A: u32 = raw::AMPERE_DMA_COPY_A;
/// AD10x (and every Ampere-derived part) binds copy engines with the B class;
/// `AMPERE_DMA_COPY_A` is the GA100-era class.
pub const AMPERE_DMA_COPY_B: u32 = raw::AMPERE_DMA_COPY_B;
/// `NV2080_ENGINE_TYPE_COPY0`.
pub const ENGINE_COPY0: u32 = 9;
/// Handle ranges for the first channel and its copy engine.
/// `NVKM_RM_CHAN(chid)` in nouveau: the handle encodes the channel id, and
/// channel id 0 is reserved (RM keeps it for its own GR golden context).
pub const HANDLE_CHANNEL: u32 = 0xf1f0_0001;
pub const CHANNEL_CID: u32 = 1;
pub const HANDLE_CE: u32 = 0xc6b5_0000;

/// `NV_CHANNEL_ALLOC_PARAMS` in the 570 ABI: `hmacNonce[8]` ends at 360 and
/// `tpcConfigID` occupies 360..364, so the 8-aligned struct is 368 bytes.
const CHAN_PARAMS: usize = size_of::<raw::NV_CHANNEL_ALLOC_PARAMS>();
const _: () = assert!(CHAN_PARAMS == 360 + 4 + 4);

/// `NVOS04` flags we set: privileged kernel channel with a fixed USERD page.
pub const CHAN_FLAG_PRIVILEGED: u32 = raw::HORN_CHANNEL_PRIVILEGED;
pub const CHAN_FLAG_USERD_PAGE_FIXED: u32 = raw::HORN_CHANNEL_USERD_PAGE_FIXED;
/// Skip RM's own context-buffer allocation for this channel.
pub const CHAN_FLAG_SKIP_CTXBUFFER: u32 = 1 << 31;
/// `NV_KERNELCHANNEL_ALLOC_INTERNALFLAGS_PRIVILEGE_ADMIN` (bits 1:0 = 1).
pub const CHAN_INTERNAL_ADMIN: u32 = 1;

/// `NV_MEMORY_DESC_PARAMS`: base, size, address space (1 sysmem, 2 vidmem),
/// cache attribute.
pub fn memory_desc(
    base: u64,
    size: u64,
    address_space: u32,
    cache: u32,
) -> [u8; size_of::<raw::NV_MEMORY_DESC_PARAMS>()] {
    fixed(raw::NV_MEMORY_DESC_PARAMS {
        base,
        size,
        addressSpace: address_space,
        cacheAttrib: cache,
    })
}

/// `NV_CHANNEL_ALLOC_PARAMS` for a kernel CE channel.
pub struct Channel<'a> {
    pub h_vaspace: u32,
    pub gp_fifo: u64,
    pub gp_fifo_entries: u32,
    pub cid: u32,
    pub instance: &'a [u8],
    pub userd: &'a [u8],
    pub ramfc: &'a [u8],
    pub mthdbuf: &'a [u8],
}

pub fn channel_params(channel: Channel<'_>) -> Result<[u8; CHAN_PARAMS]> {
    if channel.gp_fifo_entries == 0 || channel.gp_fifo & 7 != 0 {
        return Err(Error::Range);
    }
    Ok(fixed(raw::NV_CHANNEL_ALLOC_PARAMS {
        gpFifoOffset: channel.gp_fifo,
        gpFifoEntries: channel.gp_fifo_entries,
        flags: CHAN_FLAG_PRIVILEGED
            | CHAN_FLAG_USERD_PAGE_FIXED
            | ((channel.cid & raw::HORN_CHANNEL_USERD_MASK) << raw::HORN_CHANNEL_USERD_SHIFT),
        hVASpace: channel.h_vaspace,
        engineType: ENGINE_COPY0,
        cid: channel.cid,
        subDeviceId: 1,
        internalFlags: CHAN_INTERNAL_ADMIN,
        instanceMem: raw::NV_MEMORY_DESC_PARAMS::read(channel.instance)?,
        userdMem: raw::NV_MEMORY_DESC_PARAMS::read(channel.userd)?,
        ramfcMem: raw::NV_MEMORY_DESC_PARAMS::read(channel.ramfc)?,
        mthdbufMem: raw::NV_MEMORY_DESC_PARAMS::read(channel.mthdbuf)?,
        ..Default::default()
    }))
}

#[cfg(test)]
mod channel_tests {
    use super::*;

    #[test]
    fn channel_params_layout() {
        let inst = memory_desc(0, 0x1000, 2, 1);
        let userd = memory_desc(0x1000, 0x200, 2, 1);
        let ramfc = memory_desc(0x1200, 0x200, 2, 1);
        let mthdbuf = memory_desc(0x3dc_0000, 0x1000, 1, 0);
        let p = channel_params(Channel {
            h_vaspace: HANDLE_VASPACE,
            gp_fifo: 0x3dd_0000,
            gp_fifo_entries: 512,
            cid: 1,
            instance: &inst,
            userd: &userd,
            ramfc: &ramfc,
            mthdbuf: &mthdbuf,
        })
        .unwrap();
        assert_eq!(p.len(), 368);
        assert_eq!(u64::from_le_bytes(p[8..16].try_into().unwrap()), 0x3dd_0000);
        assert_eq!(u32::from_le_bytes(p[16..20].try_into().unwrap()), 512);
        assert_eq!(
            u32::from_le_bytes(p[20..24].try_into().unwrap()),
            CHAN_FLAG_PRIVILEGED | CHAN_FLAG_USERD_PAGE_FIXED | (1 << 8)
        );
        assert_eq!(
            u32::from_le_bytes(p[28..32].try_into().unwrap()),
            HANDLE_VASPACE
        );
        assert_eq!(
            u32::from_le_bytes(p[128..132].try_into().unwrap()),
            ENGINE_COPY0
        );
        assert_eq!(
            u32::from_le_bytes(p[244..248].try_into().unwrap()),
            CHAN_INTERNAL_ADMIN
        );
        // tpcConfigID: present from the 570 ABI on, zero for no DTD-PG.
        assert_eq!(u32::from_le_bytes(p[360..364].try_into().unwrap()), 0);
        // Instance descriptor: base 0, size 0x1000, vidmem, cache 1.
        assert_eq!(u64::from_le_bytes(p[152..160].try_into().unwrap()), 0x1000);
        assert_eq!(u32::from_le_bytes(p[160..164].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(p[164..168].try_into().unwrap()), 1);
    }
}
