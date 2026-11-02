//! Field-wise little-endian codec for generated C types. Never reads Rust
//! padding, creates references into unaligned replies, or dereferences NvP64.
#![allow(non_snake_case)]
use crate::{Error, Result, raw::*};
use alloc::vec::Vec;
use core::mem::{offset_of, size_of};

pub trait Wire: Sized {
    const SIZE: usize = size_of::<Self>();
    fn write(&self, out: &mut [u8]) -> Result<()>;
    fn read(bytes: &[u8]) -> Result<Self>;
    fn bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        out.try_reserve_exact(Self::SIZE)
            .map_err(|_| Error::Exhausted)?;
        out.resize(Self::SIZE, 0);
        self.write(&mut out)?;
        Ok(out)
    }
}
fn exact(bytes: &[u8], size: usize) -> Result<()> {
    if bytes.len() == size {
        Ok(())
    } else {
        Err(Error::Malformed)
    }
}
macro_rules! scalar {
    ($($t:ty),* $(,)?) => {$(
        impl Wire for $t {
            fn write(&self, out: &mut [u8]) -> Result<()> {
                exact(out, Self::SIZE)?;
                out.copy_from_slice(&self.to_le_bytes());
                Ok(())
            }
            fn read(bytes: &[u8]) -> Result<Self> {
                Ok(Self::from_le_bytes(bytes.try_into().map_err(|_| Error::Malformed)?))
            }
        }
    )*};
}
scalar!(u8, i8, u16, i16, u32, i32, u64, i64);
impl<T: Wire + Default, const N: usize> Wire for [T; N] {
    const SIZE: usize = N * T::SIZE;
    fn write(&self, out: &mut [u8]) -> Result<()> {
        exact(out, Self::SIZE)?;
        for (n, value) in self.iter().enumerate() {
            value.write(&mut out[n * T::SIZE..(n + 1) * T::SIZE])?;
        }
        Ok(())
    }
    fn read(bytes: &[u8]) -> Result<Self> {
        exact(bytes, Self::SIZE)?;
        let mut out = core::array::from_fn(|_| T::default());
        for (n, value) in out.iter_mut().enumerate() {
            *value = T::read(&bytes[n * T::SIZE..(n + 1) * T::SIZE])?;
        }
        Ok(out)
    }
}
// These are opaque ABI address slots, never pointers that this driver may
// dereference. No references or ownership are constructed from firmware bytes.
impl Wire for NvP64 {
    const SIZE: usize = 8;
    fn write(&self, out: &mut [u8]) -> Result<()> {
        (*self as usize as u64).write(out)
    }
    fn read(bytes: &[u8]) -> Result<Self> {
        Ok(u64::read(bytes)? as usize as Self)
    }
}
impl<T> Wire for __IncompleteArrayField<T> {
    const SIZE: usize = 0;
    fn write(&self, out: &mut [u8]) -> Result<()> {
        exact(out, 0)
    }
    fn read(bytes: &[u8]) -> Result<Self> {
        exact(bytes, 0)?;
        Ok(Self::new())
    }
}
impl Wire for () {
    fn write(&self, out: &mut [u8]) -> Result<()> {
        exact(out, 0)
    }
    fn read(bytes: &[u8]) -> Result<Self> {
        exact(bytes, 0)
    }
}
fn put<T: Wire>(out: &mut [u8], offset: usize, value: &T) -> Result<()> {
    value.write(
        out.get_mut(offset..offset + T::SIZE)
            .ok_or(Error::Malformed)?,
    )
}
fn get<T: Wire>(bytes: &[u8], offset: usize) -> Result<T> {
    T::read(
        bytes
            .get(offset..offset + T::SIZE)
            .ok_or(Error::Malformed)?,
    )
}
// Field names only: sizes, alignment and offsets come from clang's generated
// types. Exhaustive destructuring makes adding/removing a field a compile error.
macro_rules! fields {
    ($t:ident { $($field:ident),* $(,)? }) => {
        impl Wire for $t {
            fn write(&self, out: &mut [u8]) -> Result<()> {
                exact(out, Self::SIZE)?;
                out.fill(0);
                let Self { $($field),* } = self;
                $(put(out, offset_of!(Self, $field), $field)?;)*
                Ok(())
            }
            fn read(bytes: &[u8]) -> Result<Self> {
                exact(bytes, Self::SIZE)?;
                Ok(Self { $($field: get(bytes, offset_of!(Self, $field))?),* })
            }
        }
    };
}
fields!(NV0005_ALLOC_PARAMETERS {
    hParentClient,
    hSrcResource,
    hClass,
    notifyIndex,
    data
});
fields!(NV2080_CTRL_EVENT_SET_NOTIFICATION_PARAMS {
    event,
    action,
    bNotifyState,
    info32,
    info16
});
fields!(rpc_post_event_v17_00 {
    hClient,
    hEvent,
    notifyIndex,
    data,
    info16,
    status,
    eventDataSize,
    bNotifyList,
    eventData
});
fields!(Nv2080HotplugNotification {
    plugDisplayMask,
    unplugDisplayMask
});
fields!(Nv2080DpIrqNotification { displayId });
fields!(NV0073_CTRL_SYSTEM_GET_ACTIVE_PARAMS {
    subDeviceInstance,
    head,
    flags,
    displayId
});
fields!(NV0073_CTRL_SPECIFIC_GET_CONNECTOR_DATA_PARAMS {
    subDeviceInstance,
    displayId,
    flags,
    DDCPartners,
    count,
    data,
    platform
});
fields!(
    NV0073_CTRL_SPECIFIC_GET_CONNECTOR_DATA_PARAMS__bindgen_ty_1 {
        index,
        type_,
        location
    }
);
fields!(NV0073_CTRL_SPECIFIC_OR_GET_INFO_PARAMS {
    subDeviceInstance,
    displayId,
    index,
    type_,
    protocol,
    ditherType,
    ditherAlgo,
    location,
    rootPortId,
    dcbIndex,
    vbiosAddress,
    bIsLitByVbios,
    bIsDispDynamic
});
fields!(NV0073_CTRL_CMD_DSC_CAP_PARAMS {
    bDscSupported,
    encoderColorFormatMask,
    lineBufferSizeKB,
    rateBufferSizeKB,
    bitsPerPixelPrecision,
    maxNumHztSlices,
    lineBufferBitDepth
});
fields!(NV0073_CTRL_DFP_ASSIGN_SOR_INFO {
    displayMask,
    sorType
});
fields!(NV0073_CTRL_DFP_ASSIGN_SOR_PARAMS {
    subDeviceInstance,
    displayId,
    sorExcludeMask,
    slaveDisplayId,
    forceSublinkConfig,
    bIs2Head1Or,
    sorAssignList,
    sorAssignListWithTag,
    reservedSorMask,
    flags
});
fields!(NV0073_CTRL_DP_AUXCH_CTRL_PARAMS {
    subDeviceInstance,
    displayId,
    bAddrOnly,
    cmd,
    addr,
    data,
    size,
    replyType,
    retryTimeMs
});
fields!(NV0073_CTRL_DP_CTRL_PARAMS {
    subDeviceInstance,
    displayId,
    cmd,
    data,
    err,
    retryTimeMs,
    eightLaneDpcdBaseAddr
});
fields!(NV0073_CTRL_CMD_DP_CONFIG_STREAM_PARAMS {
    subDeviceInstance,
    head,
    sorIndex,
    dpLink,
    bEnableOverride,
    bMST,
    singleHeadMultistreamMode,
    hBlankSym,
    vBlankSym,
    colorFormat,
    bEnableTwoHeadOneOr,
    MST,
    SST
});
fields!(NV0073_CTRL_CMD_DP_CONFIG_STREAM_PARAMS__bindgen_ty_1 {
    slotStart,
    slotEnd,
    PBN,
    Timeslice,
    sendACT,
    singleHeadMSTPipeline,
    bEnableAudioOverRightPanel
});
fields!(NV0073_CTRL_CMD_DP_CONFIG_STREAM_PARAMS__bindgen_ty_2 {
    bEnhancedFraming,
    tuSize,
    waterMark,
    bEnableAudioOverRightPanel
});
fields!(NV0073_CTRL_CMD_DP_SET_MANUAL_DISPLAYPORT_PARAMS { subDeviceInstance });
fields!(NV0073_CTRL_CMD_DP_GET_CAPS_PARAMS {
    subDeviceInstance,
    sorIndex,
    maxLinkRate,
    dpVersionsSupported,
    UHBRSupportedByGpu,
    minPClkForCompressed,
    bIsMultistreamSupported,
    bIsSCEnabled,
    bHasIncreasedWatermarkLimits,
    bIsPC2Disabled,
    isSingleHeadMSTSupported,
    bFECSupported,
    bIsTrainPhyRepeater,
    bOverrideLinkBw,
    bUseRgFlushSequence,
    bSupportDPDownSpread,
    DSC
});
fields!(rpc_gsp_rm_alloc_v03_00 {
    hClient,
    hParent,
    hObject,
    hClass,
    status,
    paramsSize,
    flags,
    reserved,
    params
});
fields!(rpc_gsp_rm_control_v03_00 {
    hClient,
    hObject,
    cmd,
    status,
    paramsSize,
    flags,
    params
});
fields!(NV0000_ALLOC_PARAMETERS {
    hClient,
    processID,
    processName,
    pOsPidInfo
});
fields!(NV0080_ALLOC_PARAMETERS {
    deviceId,
    hClientShare,
    hTargetClient,
    hTargetDevice,
    flags,
    vaSpaceSize,
    vaStartInternal,
    vaLimitInternal,
    vaMode
});
fields!(NV2080_ALLOC_PARAMETERS { subDeviceId });
fields!(NV_MEMORY_ALLOCATION_PARAMS {
    owner,
    type_,
    flags,
    width,
    height,
    pitch,
    attr,
    attr2,
    format,
    comprCovg,
    zcullCovg,
    rangeLo,
    rangeHi,
    size,
    alignment,
    offset,
    limit,
    address,
    ctagOffset,
    hVASpace,
    internalflags,
    tag,
    numaNode
});
fields!(NV_VASPACE_ALLOCATION_PARAMETERS {
    index,
    flags,
    vaSize,
    vaStartInternal,
    vaLimitInternal,
    bigPageSize,
    vaBase
});
fields!(NV_MEMORY_DESC_PARAMS {
    base,
    size,
    addressSpace,
    cacheAttrib
});
fields!(NV_CHANNEL_ALLOC_PARAMS {
    hObjectError,
    hObjectBuffer,
    gpFifoOffset,
    gpFifoEntries,
    flags,
    hContextShare,
    hVASpace,
    hUserdMemory,
    userdOffset,
    engineType,
    cid,
    subDeviceId,
    hObjectEccError,
    instanceMem,
    userdMem,
    ramfcMem,
    mthdbufMem,
    hPhysChannelGroup,
    internalFlags,
    errorNotifierMem,
    eccErrorNotifierMem,
    ProcessID,
    SubProcessID,
    encryptIv,
    decryptIv,
    hmacNonce,
    tpcConfigID
});
fields!(NV50VAIO_CHANNELDMA_ALLOCATION_PARAMETERS {
    channelInstance,
    hObjectBuffer,
    hObjectNotify,
    offset,
    pControl,
    flags,
    channelPBSize,
    subDeviceId
});
fields!(NV50VAIO_CHANNELPIO_ALLOCATION_PARAMETERS {
    channelInstance,
    hObjectNotify,
    pControl
});
fields!(NV0073_CTRL_SYSTEM_GET_SUPPORTED_PARAMS {
    subDeviceInstance,
    displayMask,
    displayMaskDDC
});
fields!(NV2080_CTRL_INTERNAL_DISPLAY_GET_STATIC_INFO_PARAMS {
    feHwSysCap,
    windowPresentMask,
    bFbRemapperEnabled,
    numHeads,
    i2cPort,
    internalDispActiveMask,
    embeddedDisplayPortMask,
    bExternalMuxSupported,
    bInternalMuxSupported,
    numDispChannels
});
fields!(NV2080_CTRL_INTERNAL_DISPLAY_WRITE_INST_MEM_PARAMS {
    instMemPhysAddr,
    instMemSize,
    instMemAddrSpace,
    instMemCpuCacheAttr
});
fields!(NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS {
    addressSpace,
    physicalAddr,
    limit,
    cacheSnoop,
    hclass,
    channelInstance,
    valid,
    pbTargetAperture,
    channelPBSize,
    subDeviceId
});
fields!(NV0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS {
    physAddress,
    numEntries,
    flags,
    hVASpace,
    chId,
    subDeviceId,
    pasid
});
fields!(NVA06F_CTRL_BIND_PARAMS { engineType });
fields!(NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS {
    bEnable,
    bSkipSubmit
});
fields!(NV2080_CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE_PARAMS { size });
fields!(NVC0B5_ALLOCATION_PARAMETERS {
    version,
    engineType
});
fields!(NV2080_CTRL_FIFO_DEVICE_ENTRY {
    engineData,
    pbdmaIds,
    pbdmaFaultIds,
    numPbdmas,
    engineName
});
fields!(NV2080_CTRL_FIFO_GET_DEVICE_INFO_TABLE_PARAMS {
    baseIndex,
    numEntries,
    bMore,
    entries
});
