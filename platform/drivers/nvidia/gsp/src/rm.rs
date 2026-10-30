// SPDX-License-Identifier: GPL-2.0-only
//! r570_144 RM client allocation (root, device, subdevice) over the GSP queue.
//!
//! Byte layouts follow the MIT-licensed RM excerpts shipped with nouveau r535
//! (`nvrm/client.h`, `nvrm/device.h`, `nvrm/alloc.h`); every offset used here is
//! asserted below. This module only builds and validates bytes - it grants no
//! authority and performs no allocation by itself.
use crate::{Error, Result, abi};
use alloc::vec::Vec;

/// `NV_VGPU_MSG_FUNCTION_GSP_RM_ALLOC`.
pub const RM_ALLOC: u32 = abi::NV_VGPU_MSG_FUNCTION_GSP_RM_ALLOC;

/// RMAPI handle ranges used by nouveau's RM implementation. RM rejects handles
/// outside the ranges it expects for each object kind, so these are part of the
/// protocol, not an implementation detail.
pub const HANDLE_CLIENT: u32 = 0xc1d0_0000;
pub const HANDLE_DEVICE: u32 = 0xde1d_0000;
pub const HANDLE_SUBDEVICE: u32 = 0x5d1d_0000;

pub const NV01_ROOT: u32 = 0x0000;
pub const NV01_DEVICE_0: u32 = 0x0080;
pub const NV20_SUBDEVICE_0: u32 = 0x2080;

/// `rpc_gsp_rm_alloc_v03_00`: hClient, hParent, hObject, hClass, status,
/// paramsSize, flags, reserved[4]; then the class parameters.
const HEADER: usize = 32;
const STATUS: usize = 16;
const PARAMS_SIZE: usize = 20;

const ROOT_PARAMS: usize = 108;
const DEVICE_PARAMS: usize = 56;
const SUBDEVICE_PARAMS: usize = 4;

const _: () = assert!(HEADER == 32);
const _: () = assert!(STATUS == 16);
const _: () = assert!(PARAMS_SIZE == 20);
const _: () = assert!(ROOT_PARAMS == 4 + 4 + 100);
// deviceId, hClientShare, hTargetClient, hTargetDevice, flags, 4 bytes of
// alignment padding, three 8-byte VA fields, vaMode and trailing padding.
const _: () = assert!(DEVICE_PARAMS == 5 * 4 + 4 + 3 * 8 + 4 + 4);
const _: () = assert!(SUBDEVICE_PARAMS == 4);

/// `NV0000_ALLOC_PARAMETERS`: hClient, processID, processName[100].
pub fn root_params(h_client: u32) -> [u8; ROOT_PARAMS] {
    let mut out = [0u8; ROOT_PARAMS];
    out[..4].copy_from_slice(&h_client.to_le_bytes());
    // nouveau passes ~0 for processID and leaves the name empty.
    out[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    out
}

/// `NV0080_ALLOC_PARAMETERS`: deviceId, hClientShare, hTargetClient,
/// hTargetDevice, flags, then 8-byte aligned vaSpaceSize, vaStartInternal,
/// vaLimitInternal and vaMode.
pub fn device_params(h_client_share: u32) -> [u8; DEVICE_PARAMS] {
    let mut out = [0u8; DEVICE_PARAMS];
    // deviceId stays 0 (not a vGPU); the 8-byte aligned VA window stays zero,
    // so RM picks the defaults for this device.
    out[4..8].copy_from_slice(&h_client_share.to_le_bytes());
    out
}

/// `NV2080_ALLOC_PARAMETERS`: subDeviceId.
pub fn subdevice_params() -> [u8; SUBDEVICE_PARAMS] {
    [0; SUBDEVICE_PARAMS]
}

/// Build one `GSP_RM_ALLOC` payload. `h_object` is our own handle, not an
/// address; RM returns it in the reply so mismatches are detectable.
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
    let bytes = HEADER.checked_add(params.len()).ok_or(Error::Range)?;
    let mut out = Vec::new();
    out.try_reserve_exact(bytes).map_err(|_| Error::Exhausted)?;
    out.resize(bytes, 0);
    out[0..4].copy_from_slice(&h_client.to_le_bytes());
    out[4..8].copy_from_slice(&h_parent.to_le_bytes());
    out[8..12].copy_from_slice(&h_object.to_le_bytes());
    out[12..16].copy_from_slice(&class.to_le_bytes());
    out[PARAMS_SIZE..PARAMS_SIZE + 4].copy_from_slice(&(params.len() as u32).to_le_bytes());
    out[HEADER..].copy_from_slice(params);
    Ok(out)
}

/// RM status word from an alloc reply body.
pub fn status(body: &[u8]) -> Result<u32> {
    let field = body.get(STATUS..STATUS + 4).ok_or(Error::Malformed)?;
    Ok(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}

/// Echo checks: RM must return our handle, class and parameter size. A reply
/// for a different object is not silently accepted.
pub fn check(body: &[u8], h_object: u32, class: u32, params_size: usize) -> Result<()> {
    if body.len() < HEADER {
        return Err(Error::Malformed);
    }
    let word = |at: usize| u32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
    if word(8) != h_object || word(12) != class || word(PARAMS_SIZE) as usize != params_size {
        return Err(Error::Malformed);
    }
    match word(STATUS) {
        0 => Ok(()),
        value => Err(Error::FirmwareStatus(crate::FirmwareStatus(value))),
    }
}

/// `NV_VGPU_MSG_FUNCTION_GSP_RM_CONTROL`.
pub const RM_CONTROL: u32 = abi::NV_VGPU_MSG_FUNCTION_GSP_RM_CONTROL;

/// `rpc_gsp_rm_control_v03_00`: hClient, hObject, cmd, status, paramsSize,
/// flags; then the command parameters.
const CTRL_HEADER: usize = 24;
const CTRL_STATUS: usize = 12;
const CTRL_PARAMS_SIZE: usize = 16;

const _: () = assert!(CTRL_HEADER == 24);
const _: () = assert!(CTRL_STATUS == 12);
const _: () = assert!(CTRL_PARAMS_SIZE == 16);

/// Build one `GSP_RM_CONTROL` payload.
pub fn encode_ctrl(h_client: u32, h_object: u32, cmd: u32, params: &[u8]) -> Result<Vec<u8>> {
    let bytes = CTRL_HEADER.checked_add(params.len()).ok_or(Error::Range)?;
    let mut out = Vec::new();
    out.try_reserve_exact(bytes).map_err(|_| Error::Exhausted)?;
    out.resize(bytes, 0);
    out[0..4].copy_from_slice(&h_client.to_le_bytes());
    out[4..8].copy_from_slice(&h_object.to_le_bytes());
    out[8..12].copy_from_slice(&cmd.to_le_bytes());
    out[CTRL_PARAMS_SIZE..CTRL_PARAMS_SIZE + 4]
        .copy_from_slice(&(params.len() as u32).to_le_bytes());
    out[CTRL_HEADER..].copy_from_slice(params);
    Ok(out)
}

/// Verify a control reply echoes our object, command and parameter size, then
/// surface the RM status. A reply for a different command is rejected.
pub fn check_ctrl(body: &[u8], h_object: u32, cmd: u32, params_size: usize) -> Result<()> {
    if body.len() < CTRL_HEADER {
        return Err(Error::Malformed);
    }
    let word = |at: usize| u32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
    if word(4) != h_object || word(8) != cmd || word(CTRL_PARAMS_SIZE) as usize != params_size {
        return Err(Error::Malformed);
    }
    match word(CTRL_STATUS) {
        0 => Ok(()),
        value => Err(Error::FirmwareStatus(crate::FirmwareStatus(value))),
    }
}

/// `NVA06F_CTRL_CMD_BIND` and `NVA06F_CTRL_CMD_GPFIFO_SCHEDULE`: after the
/// channel object exists, RM must be told which engine to bind and that the
/// GPFIFO may be scheduled. Without these two controls the channel is inert.
pub const CTRL_CHANNEL_BIND: u32 = 0xa06f_0104;
pub const CTRL_CHANNEL_GPFIFO_SCHEDULE: u32 = 0xa06f_0103;

pub fn bind_params(engine_type: u32) -> [u8; 4] {
    engine_type.to_le_bytes()
}

/// `NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS` is two `NvBool`s, and `NvBool` is
/// `NvU8`, so the wire payload is two bytes; a wider payload is rejected as an
/// invalid argument.
pub fn schedule_params(enable: bool) -> [u8; 2] {
    [u8::from(enable), 0]
}

/// `NVC0B5_ALLOCATION_PARAMETERS`: the copy-engine object carries a version and
/// the engine instance it drives.
pub fn ce_params(engine_type: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0..4].copy_from_slice(&1u32.to_le_bytes());
    out[4..8].copy_from_slice(&engine_type.to_le_bytes());
    out
}

/// `NV2080_CTRL_CMD_FIFO_GET_DEVICE_INFO_TABLE`: the engine table tells which
/// engines this GPU exposes and which runlist each one uses. The runlist id is
/// assigned by RM, so it cannot be assumed.
pub const CTRL_FIFO_GET_DEVICE_INFO_TABLE: u32 = 0x2080_1112;
pub const ENGINE_TABLE_ENTRIES: usize = 32;
pub const ENGINE_ENTRY_BYTES: usize = 100;
pub const ENGINE_TABLE_BYTES: usize = 12 + ENGINE_TABLE_ENTRIES * ENGINE_ENTRY_BYTES;
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
    let params = body.get(CTRL_HEADER..CTRL_HEADER + ENGINE_TABLE_BYTES).ok_or(Error::Malformed)?;
    let count = u32::from_le_bytes([params[4], params[5], params[6], params[7]]) as usize;
    if count > ENGINE_TABLE_ENTRIES {
        return Err(Error::Malformed);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(count).map_err(|_| Error::Exhausted)?;
    for index in 0..count {
        let base = 12 + index * ENGINE_ENTRY_BYTES;
        let word = |at: usize| -> u32 {
            let at = base + at;
            u32::from_le_bytes([params[at], params[at + 1], params[at + 2], params[at + 3]])
        };
        let mut name = [0u8; 16];
        name.copy_from_slice(&params[base + 84..base + 100]);
        out.push(EngineEntry {
            rm_type: word(ENGINE_INFO_RM_TYPE * 4),
            runlist: word(ENGINE_INFO_RUNLIST * 4),
            instance: word(ENGINE_INFO_INSTANCE * 4),
            name,
        });
    }
    Ok(out)
}

/// `NV2080_CTRL_CMD_CE_GET_FAULT_METHOD_BUFFER_SIZE`: the fault method buffer
/// is sized by RM, not by the driver, so ask before allocating it.
pub const CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE: u32 = 0x2080_2a08;

/// The control takes no input; RM writes the required size into the reply.
pub fn mthdbuf_params() -> [u8; 4] {
    [0u8; 4]
}

/// Required method-buffer size in bytes from the control reply body.
pub fn mthdbuf_size(body: &[u8]) -> Result<u32> {
    let params = body.get(CTRL_HEADER..CTRL_HEADER + 4).ok_or(Error::Malformed)?;
    Ok(u32::from_le_bytes([params[0], params[1], params[2], params[3]]))
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

/// `NV_VGPU_MSG_FUNCTION_ALLOC_MEMORY` and `NV_VGPU_MSG_FUNCTION_MAP_MEMORY_DMA`.
/// RM-managed GPU VA uses the legacy context-DMA chain
/// (`NV01_MEMORY_VIRTUAL` -> `ALLOC_CTX_DMA` -> `MAP_MEMORY_DMA`); the modern
/// `FERMI_VASPACE_A` path is driver-owned page tables, which we do not use.
pub const ALLOC_MEMORY: u32 = abi::NV_VGPU_MSG_FUNCTION_ALLOC_MEMORY;
pub const MAP_MEMORY_DMA: u32 = abi::NV_VGPU_MSG_FUNCTION_MAP_MEMORY_DMA;

/// Memory classes (570.144 `class/cl0070.h`, `nvrm/fbsr.h`).
pub const NV01_MEMORY_VIRTUAL: u32 = 0x0070;
pub const NV01_MEMORY_LIST_SYSTEM: u32 = 0x0081;

/// `NVOS02` flags for a non-contiguous system-memory page list that RM must not
/// map into a kernel aperture.
pub const SYSMEM_LIST_FLAGS: u32 = (1 << 4) | (1 << 30);

const MEM_HEADER: usize = 48;
const MEM_LENGTH: usize = 32;
const MEM_PAGE_COUNT: usize = 40;
const MEM_PTE_DESC: usize = 44;
const VIRTUAL_PARAMS: usize = 24;
const MAP_PARAMS: usize = 56;
const MAP_DMA_OFFSET: usize = 40;
const MAP_STATUS: usize = 48;

const _: () = assert!(MEM_HEADER == 48);
const _: () = assert!(MEM_LENGTH == 32);
const _: () = assert!(MEM_PAGE_COUNT == 40);
const _: () = assert!(MEM_PTE_DESC == 44);
const _: () = assert!(VIRTUAL_PARAMS == 24);
const _: () = assert!(MAP_PARAMS == 56);
const _: () = assert!(MAP_DMA_OFFSET == 40);
const _: () = assert!(MAP_STATUS == 48);

/// `rpc_alloc_memory_v13_01` for a page list. Entries are the device-visible
/// addresses (IOVA) of our own pinned pages, never host physical addresses.
pub struct MemList<'a> {
    pub h_client: u32,
    pub h_device: u32,
    pub h_memory: u32,
    pub class: u32,
    pub flags: u32,
    pub format: u32,
    pub length: u64,
    pub pages: &'a [u64],
}

pub fn memlist(list: MemList<'_>) -> Result<Vec<u8>> {
    let MemList {
        h_client,
        h_device,
        h_memory,
        class,
        flags,
        format,
        length,
        pages,
    } = list;
    if pages.is_empty() || pages.len() > u16::MAX as usize {
        return Err(Error::Range);
    }
    if length == 0 || length != pages.len() as u64 * 4096 || pages.iter().any(|p| *p & 4095 != 0) {
        return Err(Error::Range);
    }
    let bytes = MEM_HEADER
        .checked_add(pages.len() * 8)
        .ok_or(Error::Range)?;
    let mut out = Vec::new();
    out.try_reserve_exact(bytes).map_err(|_| Error::Exhausted)?;
    out.resize(bytes, 0);
    out[0..4].copy_from_slice(&h_client.to_le_bytes());
    out[4..8].copy_from_slice(&h_device.to_le_bytes());
    out[8..12].copy_from_slice(&h_memory.to_le_bytes());
    out[12..16].copy_from_slice(&class.to_le_bytes());
    out[16..20].copy_from_slice(&flags.to_le_bytes());
    out[24..28].copy_from_slice(&format.to_le_bytes());
    out[MEM_LENGTH..MEM_LENGTH + 8].copy_from_slice(&length.to_le_bytes());
    out[MEM_PAGE_COUNT..MEM_PAGE_COUNT + 4].copy_from_slice(&(pages.len() as u32).to_le_bytes());
    // pte_desc: idr = 0 (PTE), reserved = 0, length = entry count.
    out[MEM_PTE_DESC..MEM_PTE_DESC + 4].copy_from_slice(&(pages.len() as u32).to_le_bytes());
    for (n, page) in pages.iter().enumerate() {
        let at = MEM_HEADER + n * 8;
        out[at..at + 8].copy_from_slice(&page.to_le_bytes());
    }
    Ok(out)
}

/// `NV_MEMORY_VIRTUAL_ALLOCATION_PARAMS`: offset, limit, hVASpace.
pub fn virtual_params(offset: u64, limit: u64, h_vaspace: u32) -> [u8; VIRTUAL_PARAMS] {
    let mut out = [0u8; VIRTUAL_PARAMS];
    out[0..8].copy_from_slice(&offset.to_le_bytes());
    out[8..16].copy_from_slice(&limit.to_le_bytes());
    out[16..20].copy_from_slice(&h_vaspace.to_le_bytes());
    out
}

/// `NVOS46_PARAMETERS` for `MAP_MEMORY_DMA`. `dma_offset` is filled by RM.
pub fn map_dma_params(
    h_client: u32,
    h_device: u32,
    h_dma: u32,
    h_memory: u32,
    offset: u64,
    length: u64,
    flags: u32,
) -> [u8; MAP_PARAMS] {
    let mut out = [0u8; MAP_PARAMS];
    out[0..4].copy_from_slice(&h_client.to_le_bytes());
    out[4..8].copy_from_slice(&h_device.to_le_bytes());
    out[8..12].copy_from_slice(&h_dma.to_le_bytes());
    out[12..16].copy_from_slice(&h_memory.to_le_bytes());
    out[16..24].copy_from_slice(&offset.to_le_bytes());
    out[24..32].copy_from_slice(&length.to_le_bytes());
    out[32..36].copy_from_slice(&flags.to_le_bytes());
    out
}

/// Mapping status and the VA RM selected for this mapping.
pub fn map_dma_reply(body: &[u8]) -> Result<(u64, u32)> {
    if body.len() < MAP_PARAMS {
        return Err(Error::Malformed);
    }
    let word = |at: usize| u32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
    let offset = u64::from_le_bytes(body[MAP_DMA_OFFSET..MAP_DMA_OFFSET + 8].try_into().unwrap());
    let status = word(MAP_STATUS);
    if status != 0 {
        return Err(Error::FirmwareStatus(crate::FirmwareStatus(status)));
    }
    Ok((offset, status))
}

#[cfg(test)]
mod mem_tests {
    use super::*;

    #[test]
    fn memlist_layout_matches_the_rm_excerpt() {
        let pages = [0x10000u64, 0x11000];
        let body = memlist(MemList {
            h_client: HANDLE_CLIENT,
            h_device: HANDLE_DEVICE,
            h_memory: 0x7000,
            class: NV01_MEMORY_LIST_SYSTEM,
            flags: SYSMEM_LIST_FLAGS,
            format: 0,
            length: 8192,
            pages: &pages,
        })
        .unwrap();
        assert_eq!(body.len(), 48 + 16);
        assert_eq!(u32::from_le_bytes(body[12..16].try_into().unwrap()), 0x81);
        assert_eq!(
            u32::from_le_bytes(body[16..20].try_into().unwrap()),
            0x40000010
        );
        assert_eq!(u64::from_le_bytes(body[32..40].try_into().unwrap()), 8192);
        assert_eq!(u32::from_le_bytes(body[40..44].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(body[44..48].try_into().unwrap()), 2);
        assert_eq!(
            u64::from_le_bytes(body[48..56].try_into().unwrap()),
            0x10000
        );
        assert_eq!(
            u64::from_le_bytes(body[56..64].try_into().unwrap()),
            0x11000
        );
        // Length must describe exactly the page list, and pages stay aligned.
        assert!(
            memlist(MemList {
                h_client: 1,
                h_device: 2,
                h_memory: 3,
                class: 0x81,
                flags: 0,
                format: 0,
                length: 4096,
                pages: &pages
            })
            .is_err()
        );
        assert!(
            memlist(MemList {
                h_client: 1,
                h_device: 2,
                h_memory: 3,
                class: 0x81,
                flags: 0,
                format: 0,
                length: 8192,
                pages: &[0x10001, 0x11000]
            })
            .is_err()
        );
        assert!(
            memlist(MemList {
                h_client: 1,
                h_device: 2,
                h_memory: 3,
                class: 0x81,
                flags: 0,
                format: 0,
                length: 0,
                pages: &[]
            })
            .is_err()
        );
    }

    #[test]
    fn virtual_and_map_layouts() {
        let v = virtual_params(0, 1 << 30, HANDLE_SUBDEVICE);
        assert_eq!(
            u32::from_le_bytes(v[16..20].try_into().unwrap()),
            HANDLE_SUBDEVICE
        );
        let m = map_dma_params(HANDLE_CLIENT, HANDLE_DEVICE, 0x8000, 0x7000, 0, 8192, 0);
        assert_eq!(u64::from_le_bytes(m[24..32].try_into().unwrap()), 8192);
        assert_eq!(u32::from_le_bytes(m[8..12].try_into().unwrap()), 0x8000);

        let mut reply = [0u8; MAP_PARAMS];
        reply[40..48].copy_from_slice(&0x2000_0000u64.to_le_bytes());
        assert_eq!(map_dma_reply(&reply).unwrap().0, 0x2000_0000);
        reply[48] = 0x11;
        assert!(matches!(
            map_dma_reply(&reply),
            Err(Error::FirmwareStatus(crate::FirmwareStatus(0x11)))
        ));
    }
}

/// `FERMI_VASPACE_A` and its RMAPI handle range.
pub const FERMI_VASPACE_A: u32 = 0x90f1;
pub const HANDLE_VASPACE: u32 = 0x90f1_0000;
/// `NV_VASPACE_ALLOCATION_FLAGS_IS_EXTERNALLY_OWNED`: the driver owns the
/// page tables and hands RM only the root address.
pub const VASPACE_EXTERNALLY_OWNED: u32 = 1 << 3;

/// `NV0080_CTRL_CMD_DMA_SET_PAGE_DIRECTORY` and its aperture flags.
pub const CTRL_DMA_SET_PAGE_DIRECTORY: u32 = 0x0080_1813;
pub const SET_PD_APERTURE_SYSMEM_COH: u32 = 1;

const VSPACE_PARAMS: usize = 48;
const PD_PARAMS: usize = 32;

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
    let mut out = [0u8; VSPACE_PARAMS];
    out[4..8].copy_from_slice(&flags.to_le_bytes());
    out[8..16].copy_from_slice(&va_size.to_le_bytes());
    out[16..24].copy_from_slice(&va_start.to_le_bytes());
    out[24..32].copy_from_slice(&va_limit.to_le_bytes());
    out
}

/// `NV0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS`.
pub fn page_directory_params(
    phys_address: u64,
    num_entries: u32,
    flags: u32,
    h_vaspace: u32,
) -> [u8; PD_PARAMS] {
    let mut out = [0u8; PD_PARAMS];
    out[0..8].copy_from_slice(&phys_address.to_le_bytes());
    out[8..12].copy_from_slice(&num_entries.to_le_bytes());
    out[12..16].copy_from_slice(&flags.to_le_bytes());
    out[16..20].copy_from_slice(&h_vaspace.to_le_bytes());
    out
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
pub const NV01_MEMORY_LOCAL_USER: u32 = 0x0040;
/// `NVOS32_TYPE_IMAGE`: plain memory surface.
pub const NVOS32_TYPE_IMAGE: u32 = 0;
/// `NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE`.
pub const NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE: u32 = 0x0000_0100;

const VRAM_PARAMS: usize = 128;

// owner..pitch (20), attr/attr2/format/comprCovg/zcullCovg (20) + 4 padding,
// then six 8-byte fields, then ctagOffset..numaNode and trailing padding.
const _: () = assert!(VRAM_PARAMS == 44 + 48 + 20 + 16);

/// `NV_MEMORY_ALLOCATION_PARAMS` for a plain VRAM buffer. `h_vaspace` is 0:
/// the allocation is not bound to a VA space here, the caller maps it.
pub fn vram_params(owner: u32, size: u64, alignment: u64) -> Result<[u8; VRAM_PARAMS]> {
    if size == 0 || size & 4095 != 0 || alignment & 4095 != 0 {
        return Err(Error::Range);
    }
    let mut out = [0u8; VRAM_PARAMS];
    // owner identifies the RM client that owns the surface; zero is rejected
    // with NV_ERR_INVALID_OWNER.
    out[0..4].copy_from_slice(&owner.to_le_bytes());
    out[4..8].copy_from_slice(&NVOS32_TYPE_IMAGE.to_le_bytes());
    // The alignment field is only meaningful with ALIGNMENT_FORCE on, so an
    // alignment of zero leaves both unset and lets RM place the surface.
    if alignment != 0 {
        out[8..12].copy_from_slice(&NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE.to_le_bytes());
        out[72..80].copy_from_slice(&alignment.to_le_bytes());
    }
    out[64..72].copy_from_slice(&size.to_le_bytes());
    Ok(out)
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
        assert_eq!(u64::from_le_bytes(unaligned[64..72].try_into().unwrap()), 0x2000);
        assert!(vram_params(HANDLE_CLIENT, 0x1000, 1).is_err());
    }
}

/// `VOLTA_CHANNEL_GPFIFO_A`, `AMPERE_CHANNEL_GPFIFO_A` and `AMPERE_DMA_COPY_A`.
/// GA10x (and therefore AD106) binds channels with the Ampere GPFIFO class;
/// the Volta class is only used by older chips.
pub const VOLTA_CHANNEL_GPFIFO_A: u32 = 0xc36f;
pub const AMPERE_CHANNEL_GPFIFO_A: u32 = 0xc56f;
pub const AMPERE_DMA_COPY_A: u32 = 0xc6b5;
/// AD10x (and every Ampere-derived part) binds copy engines with the B class;
/// `AMPERE_DMA_COPY_A` is the GA100-era class.
pub const AMPERE_DMA_COPY_B: u32 = 0xc7b5;
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
const CHAN_PARAMS: usize = 368;
const _: () = assert!(CHAN_PARAMS == 360 + 4 + 4);

/// `NVOS04` flags we set: privileged kernel channel with a fixed USERD page.
pub const CHAN_FLAG_PRIVILEGED: u32 = 1 << 5;
pub const CHAN_FLAG_USERD_PAGE_FIXED: u32 = 1 << 21;
/// Skip RM's own context-buffer allocation for this channel.
pub const CHAN_FLAG_SKIP_CTXBUFFER: u32 = 1 << 31;
/// `NV_KERNELCHANNEL_ALLOC_INTERNALFLAGS_PRIVILEGE_ADMIN` (bits 1:0 = 1).
pub const CHAN_INTERNAL_ADMIN: u32 = 1;

/// `NV_MEMORY_DESC_PARAMS`: base, size, address space (1 sysmem, 2 vidmem),
/// cache attribute.
pub fn memory_desc(base: u64, size: u64, address_space: u32, cache: u32) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[0..8].copy_from_slice(&base.to_le_bytes());
    out[8..16].copy_from_slice(&size.to_le_bytes());
    out[16..20].copy_from_slice(&address_space.to_le_bytes());
    out[20..24].copy_from_slice(&cache.to_le_bytes());
    out
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
    for desc in [
        channel.instance,
        channel.userd,
        channel.ramfc,
        channel.mthdbuf,
    ] {
        if desc.len() != 24 {
            return Err(Error::Range);
        }
    }
    if channel.gp_fifo_entries == 0 || channel.gp_fifo & 7 != 0 {
        return Err(Error::Range);
    }
    let mut out = [0u8; CHAN_PARAMS];
    out[8..16].copy_from_slice(&channel.gp_fifo.to_le_bytes());
    out[16..20].copy_from_slice(&channel.gp_fifo_entries.to_le_bytes());
    // The USERD page is shared by eight channels; the index selects this
    // channel's slot inside it (nouveau: chid % 8).
    let userd_index = (channel.cid & 7) << 8;
    out[20..24].copy_from_slice(
        &(CHAN_FLAG_PRIVILEGED | CHAN_FLAG_USERD_PAGE_FIXED | userd_index).to_le_bytes(),
    );
    out[28..32].copy_from_slice(&channel.h_vaspace.to_le_bytes());
    out[128..132].copy_from_slice(&ENGINE_COPY0.to_le_bytes());
    out[132..136].copy_from_slice(&channel.cid.to_le_bytes());
    out[136..140].copy_from_slice(&1u32.to_le_bytes()); // subDeviceId bitmask
    out[144..168].copy_from_slice(channel.instance);
    out[168..192].copy_from_slice(channel.userd);
    out[192..216].copy_from_slice(channel.ramfc);
    out[216..240].copy_from_slice(channel.mthdbuf);
    out[244..248].copy_from_slice(&CHAN_INTERNAL_ADMIN.to_le_bytes());
    Ok(out)
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
