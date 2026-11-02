//! One outstanding RM request, with IDs tied to generated parameter types.
use super::{RM_ALLOC, RM_CONTROL, check, check_ctrl, codec::Wire, encode, encode_ctrl};
use crate::{Result, raw};
use alloc::vec::Vec;
use core::mem::size_of;

pub trait Alloc {
    const CLASS: u32;
    type Params: Wire;
}
pub trait Control {
    const COMMAND: u32;
    type Params: Wire;
}
// Only the command/type relationship is declared here. Never redeclare C fields.
macro_rules! alloc_class {
    ($($name:ident: $class:path => $params:ty;)*) => {$(
        pub enum $name {}
        impl Alloc for $name {
            const CLASS: u32 = $class;
            type Params = $params;
        }
    )*};
}
macro_rules! control {
    ($($name:ident: $command:path => $params:ty;)*) => {$(
        pub enum $name {}
        impl Control for $name {
            const COMMAND: u32 = $command;
            type Params = $params;
        }
    )*};
}
alloc_class! {
    // GSP_RM_ALLOC takes SDK class parameters, not ALLOC_ROOT's legacy RPC.
    Root: raw::NV01_ROOT => raw::NV0000_ALLOC_PARAMETERS;
    Device: raw::NV01_DEVICE_0 => raw::NV0080_ALLOC_PARAMETERS;
    Subdevice: raw::NV20_SUBDEVICE_0 => raw::NV2080_ALLOC_PARAMETERS;
    Vram: raw::NV01_MEMORY_LOCAL_USER => raw::NV_MEMORY_ALLOCATION_PARAMS;
    Vaspace: raw::FERMI_VASPACE_A => raw::NV_VASPACE_ALLOCATION_PARAMETERS;
    DisplayCommon: raw::NV04_DISPLAY_COMMON => ();
    Display: raw::NVC770_DISPLAY => ();
    DisplayCore: raw::NVC77D_CORE_CHANNEL_DMA => raw::NV50VAIO_CHANNELDMA_ALLOCATION_PARAMETERS;
    DisplayWindow: raw::NVC67E_WINDOW_CHANNEL_DMA => raw::NV50VAIO_CHANNELDMA_ALLOCATION_PARAMETERS;
    DisplayCursor: raw::NVC67A_CURSOR_IMM_CHANNEL_PIO => raw::NV50VAIO_CHANNELPIO_ALLOCATION_PARAMETERS;
    Channel: raw::AMPERE_CHANNEL_GPFIFO_A => raw::NV_CHANNEL_ALLOC_PARAMS;
    CopyEngine: raw::AMPERE_DMA_COPY_B => raw::NVC0B5_ALLOCATION_PARAMETERS;
}
control! {
    ConnectorData: raw::NV0073_CTRL_CMD_SPECIFIC_GET_CONNECTOR_DATA => raw::NV0073_CTRL_SPECIFIC_GET_CONNECTOR_DATA_PARAMS;
    OutputInfo: raw::NV0073_CTRL_CMD_SPECIFIC_OR_GET_INFO => raw::NV0073_CTRL_SPECIFIC_OR_GET_INFO_PARAMS;
    AssignSor: raw::NV0073_CTRL_CMD_DFP_ASSIGN_SOR => raw::NV0073_CTRL_DFP_ASSIGN_SOR_PARAMS;
    ManualDisplayPort: raw::NV0073_CTRL_CMD_DP_SET_MANUAL_DISPLAYPORT => raw::NV0073_CTRL_CMD_DP_SET_MANUAL_DISPLAYPORT_PARAMS;
    DpCaps: raw::NV0073_CTRL_CMD_DP_GET_CAPS => raw::NV0073_CTRL_CMD_DP_GET_CAPS_PARAMS;
    DpAux: raw::NV0073_CTRL_CMD_DP_AUXCH_CTRL => raw::NV0073_CTRL_DP_AUXCH_CTRL_PARAMS;
    DpControl: raw::NV0073_CTRL_CMD_DP_CTRL => raw::NV0073_CTRL_DP_CTRL_PARAMS;
    DpStream: raw::NV0073_CTRL_CMD_DP_CONFIG_STREAM => raw::NV0073_CTRL_CMD_DP_CONFIG_STREAM_PARAMS;
    EngineTable: raw::NV2080_CTRL_CMD_FIFO_GET_DEVICE_INFO_TABLE => raw::NV2080_CTRL_FIFO_GET_DEVICE_INFO_TABLE_PARAMS;
    DisplaySupported: raw::NV0073_CTRL_CMD_SYSTEM_GET_SUPPORTED => raw::NV0073_CTRL_SYSTEM_GET_SUPPORTED_PARAMS;
    DisplayStaticInfo: raw::NV2080_CTRL_CMD_INTERNAL_DISPLAY_GET_STATIC_INFO => raw::NV2080_CTRL_INTERNAL_DISPLAY_GET_STATIC_INFO_PARAMS;
    DisplayInstMem: raw::NV2080_CTRL_CMD_INTERNAL_DISPLAY_WRITE_INST_MEM => raw::NV2080_CTRL_INTERNAL_DISPLAY_WRITE_INST_MEM_PARAMS;
    DisplayPushbuffer: raw::NV2080_CTRL_CMD_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER => raw::NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS;
    Bind: raw::NVA06F_CTRL_CMD_BIND => raw::NVA06F_CTRL_BIND_PARAMS;
    Schedule: raw::NVA06F_CTRL_CMD_GPFIFO_SCHEDULE => raw::NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS;
    MethodBufferSize: raw::NV2080_CTRL_CMD_CE_GET_FAULT_METHOD_BUFFER_SIZE => raw::NV2080_CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE_PARAMS;
    PageDirectory: raw::NV0080_CTRL_CMD_DMA_SET_PAGE_DIRECTORY => raw::NV0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS;
}

/// `exchange` accepts/returns RPC payloads (not complete queue frames). It must
/// check outer RPC status and service asynchronous firmware events. Handles
/// remain selectors in the assigned GPU session, not authority or owned RAM.
pub struct Client<T> {
    handle: u32,
    exchange: T,
}
impl<T: FnMut(u32, &[u8]) -> Result<Vec<u8>>> Client<T> {
    pub fn new(handle: u32, exchange: T) -> Self {
        Self { handle, exchange }
    }

    /// A scoped view of the same serialized transport, not a new GPU authority.
    pub fn for_client(&mut self, handle: u32) -> Client<&mut T> {
        Client::new(handle, &mut self.exchange)
    }

    pub fn alloc<A: Alloc>(
        &mut self,
        parent: u32,
        handle: u32,
        params: &A::Params,
    ) -> Result<A::Params> {
        let params = params.bytes()?;
        let request = encode(self.handle, parent, handle, A::CLASS, &params)?;
        let reply = (self.exchange)(RM_ALLOC, &request)?;
        check(&reply, handle, A::CLASS, params.len())?;
        let header = raw::rpc_gsp_rm_alloc_v03_00::read(
            &reply[..size_of::<raw::rpc_gsp_rm_alloc_v03_00>()],
        )?;
        if header.hClient != self.handle || header.hParent != parent {
            return Err(crate::Error::Malformed);
        }
        A::Params::read(&reply[size_of::<raw::rpc_gsp_rm_alloc_v03_00>()..])
    }

    pub fn control<C: Control>(&mut self, object: u32, params: &C::Params) -> Result<C::Params> {
        let params = params.bytes()?;
        let request = encode_ctrl(self.handle, object, C::COMMAND, &params)?;
        let reply = (self.exchange)(RM_CONTROL, &request)?;
        check_ctrl(&reply, object, C::COMMAND, params.len())?;
        let header = raw::rpc_gsp_rm_control_v03_00::read(
            &reply[..size_of::<raw::rpc_gsp_rm_control_v03_00>()],
        )?;
        if header.hClient != self.handle {
            return Err(crate::Error::Malformed);
        }
        C::Params::read(&reply[size_of::<raw::rpc_gsp_rm_control_v03_00>()..])
    }
}
