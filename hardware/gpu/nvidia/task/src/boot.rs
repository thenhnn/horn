// SPDX-License-Identifier: GPL-2.0-only
//! Executable AD106 cold boot. No host driver/reset operations.
use crate::{
    Reader,
    native::{DmaBlock, Native},
    p,
    shared::Shared,
    time::Time,
};
use alloc::{vec, vec::Vec};
use horn_devmgr::protocol::*;
use horn_driver::{DeviceAccess, DeviceSession};
use horn_ipc::*;
use horn_nvidia_gsp::{
    Error, FirmwareStatus, Result,
    boot::{self, PAGE, Span},
    falcon::{Ad106, Handoff, PinnedSource, Platform, Stage},
    firmware::Bundle,
    gmmu,
    queue::{Memory, Queue},
    raw,
    rm::{self, client as api},
    vbios::{Command, Fwsec},
    wire,
};
use horn_nvidia_gsp::{ce as gsp_ce, display as gsp_disp};
mod modeset;
mod scanout;
mod images {
    include!(concat!(env!("OUT_DIR"), "/images.rs"));
}
fn span(block: &DmaBlock) -> Result<Span> {
    Span::new(block.iova, block.bytes)
}
/// Run one step with a visible marker, and record pure-parser failures with
/// the real stage instead of the generic quarantine marker.
fn step<T>(
    ctx: &mut Ad106<Native>,
    stage: Stage,
    name: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    sel4::debug_println!("NVIDIA_GSP_STEP name={}", name);
    match f() {
        Ok(value) => Ok(value),
        Err(error) => {
            sel4::debug_println!("NVIDIA_GSP_STEP_FAIL name={} error={:?}", name, error);
            ctx.fail_stage(stage, error);
            Err(error)
        }
    }
}
fn stage(ctx: &mut Ad106<Native>, data: &[u8], access: DeviceAccess) -> Result<DmaBlock> {
    let mut block = ctx.with_platform(|p| p.allocate(data.len(), access))?;
    // The fresh mapping is not published; Read grants are immutable after copy.
    // Zero first: Falcon DMA transfers whole 256-byte blocks, so the last block
    // of a non-aligned segment must read defined padding, never stale heap.
    unsafe {
        block.zero()?;
        block.initialize(0, data)?;
    }
    Ok(block)
}
fn send<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    function: u32,
    payload: &[u8],
    deadline: u64,
) -> Result<()> {
    loop {
        let now = ctx.time_us()?;
        if now >= deadline {
            return Err(Error::Timeout);
        }
        if let Some(_publication) = queue.try_send(function, 0, [payload, &[]])? {
            ctx.notify_queue()?;
            return Ok(());
        }
        ctx.sleep_until((now + 1000).min(deadline))?;
    }
}
/// Handles are our own identifiers; RM echoes them, so a mismatched reply is
/// rejected instead of being trusted.
struct Rm {
    client: u32,
    device: u32,
    subdevice: u32,
}

/// Adapt the native queue/event loop to the payload-level typed RM client.
/// The outer RPC result is distinct from the RM status inside its body.
fn rm_api<'a, M: Memory>(
    ctx: &'a mut Ad106<Native>,
    queue: &'a mut Queue<M>,
    client: u32,
    libos: Span,
    version: u32,
    deadline: u64,
) -> rm::Client<impl FnMut(u32, &[u8]) -> Result<Vec<u8>> + 'a> {
    rm::Client::new(client, move |function, body: &[u8]| {
        send(ctx, queue, function, body, deadline)?;
        let reply = wait(ctx, queue, function, libos, version, deadline)?;
        let packet = wire::Packet::decode(&reply)?;
        packet.success().map_err(|error| {
            sel4::debug_println!(
                "NVIDIA_RM_RPC_FAIL function={:#x} status={:#x} private={:#x} error={:?}",
                function,
                packet.header.status,
                packet.header.private_status,
                error
            );
            error
        })?;
        Ok(packet.payload.to_vec())
    })
}

fn rm_root<T: FnMut(u32, &[u8]) -> Result<Vec<u8>>>(
    api: &mut rm::Client<T>,
    client: u32,
) -> Result<()> {
    api.alloc::<api::Root>(
        client,
        client,
        &raw::NV0000_ALLOC_PARAMETERS {
            hClient: client,
            processID: u32::MAX,
            ..Default::default()
        },
    )?;
    Ok(())
}

fn rm_client<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<Rm> {
    let rm = Rm {
        client: rm::HANDLE_CLIENT,
        device: rm::HANDLE_DEVICE,
        subdevice: rm::HANDLE_SUBDEVICE,
    };
    let mut api = rm_api(ctx, queue, rm.client, libos, version, deadline);
    rm_root(&mut api, rm.client)?;
    api.alloc::<api::Device>(
        rm.client,
        rm.device,
        &raw::NV0080_ALLOC_PARAMETERS {
            hClientShare: rm.client,
            ..Default::default()
        },
    )?;
    api.alloc::<api::Subdevice>(rm.device, rm.subdevice, &Default::default())?;
    Ok(rm)
}

/// Driver-owned VA space: one 4 KiB sysmem page mapped at `VA_BASE`, with the
/// root handed to RM via SET_PAGE_DIRECTORY. This is the mapping model the
/// channel and copy engine will use.
struct Va {
    base: u64,
    root: u64,
    arena: DmaBlock,
    bytes: Vec<u8>,
}

impl Va {
    /// Map a VRAM page `slot` bytes above the VA base. A channel's instance,
    /// USERD and RAMFC must be reachable through this page directory: RM cannot
    /// populate an externally owned vspace itself and reports NO_MEMORY.
    fn map_vid(&mut self, ctx: &mut Ad106<Native>, slot: u64, vram: u64) -> Result<()> {
        self.map(slot, gmmu::pte_vid(vram)?)
    }
    /// Map a device-visible system-memory page. The GPFIFO must be reachable
    /// through this page directory: the channel's `gpFifoOffset` is a VA in the
    /// channel's vspace, not an IOVA.
    fn map_sys(&mut self, slot: u64, iova: u64) -> Result<()> {
        self.map(slot, gmmu::pte_sys(iova)?)
    }
    fn map(&mut self, slot: u64, entry: u64) -> Result<()> {
        let index = gmmu::index(VA_BASE + slot, 4)?;
        {
            let mut tables = gmmu::Tables::new(self.bytes.as_mut_slice(), 5)?;
            tables.write(index, 4, entry)?;
        }
        unsafe { self.arena.initialize(0, &self.bytes)? };
        Ok(())
    }
}

/// The GPFIFO entry carries the pushbuffer address in 30 bits, so everything
/// the channel addresses through its page directory has to live below 4 GiB.
const VA_BASE: u64 = 0x2000_0000;

fn gmmu_setup<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<Va> {
    let mut arena = ctx.with_platform(|p| p.allocate(5 * gmmu::PAGE, DeviceAccess::ReadWrite))?;
    let mut bytes = vec![0u8; 5 * gmmu::PAGE];
    {
        let mut tables = gmmu::Tables::new(&mut bytes, 5)?;
        let root = arena.iova;
        let page = gmmu::PAGE as u64;
        tables.write_root_big(gmmu::index(VA_BASE, 0)?, gmmu::pde(root + page)?)?;
        tables.write(gmmu::index(VA_BASE, 1)?, 1, gmmu::pde(root + 2 * page)?)?;
        tables.write(gmmu::index(VA_BASE, 2)?, 2, gmmu::pde(root + 3 * page)?)?;
        tables.write(gmmu::index(VA_BASE, 3)?, 3, gmmu::pde(root + 4 * page)?)?;
    }
    unsafe {
        arena.zero()?;
        arena.initialize(0, &bytes)?;
    }

    // Only index and flags are meaningful: nouveau leaves the VA size and
    // window to RM, and a caller-chosen size/limit is rejected as an invalid
    // argument.
    let mut api = rm_api(ctx, queue, rm.client, libos, version, deadline);
    api.alloc::<api::Vaspace>(
        rm.device,
        rm::HANDLE_VASPACE,
        &raw::NV_VASPACE_ALLOCATION_PARAMETERS {
            flags: rm::VASPACE_EXTERNALLY_OWNED,
            ..Default::default()
        },
    )
    .map_err(|error| {
        sel4::debug_println!("NVIDIA_GMMU_FAILED step=vspace error={:?}", error);
        error
    })?;
    api.control::<api::PageDirectory>(
        rm.device,
        &raw::NV0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS {
            physAddress: arena.iova,
            numEntries: gmmu::entries(0)? as u32,
            flags: rm::SET_PD_APERTURE_SYSMEM_COH,
            hVASpace: rm::HANDLE_VASPACE,
            ..Default::default()
        },
    )
    .map_err(|error| {
        sel4::debug_println!("NVIDIA_GMMU_FAILED step=page-directory error={:?}", error);
        error
    })?;
    Ok(Va {
        base: VA_BASE,
        root: arena.iova,
        arena,
        bytes,
    })
}

/// Ask RM which engines exist and on which runlist they sit. The runlist id is
/// RM-assigned, so the channel's engine type and runqueue are read from here
/// instead of assumed.
fn rm_engines<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<Vec<rm::EngineEntry>> {
    let table = rm_api(ctx, queue, rm.client, libos, version, deadline)
        .control::<api::EngineTable>(rm.subdevice, &Default::default())?;
    let entries = rm::engine_table_entries(&table)?;
    for entry in &entries {
        let name = core::str::from_utf8(&entry.name).unwrap_or("?");
        sel4::debug_println!(
            "NVIDIA_ENGINE name={} rm_type={} runlist={} instance={}",
            name.trim_end_matches('\0'),
            entry.rm_type,
            entry.runlist,
            entry.instance
        );
    }
    Ok(entries)
}

/// Ask RM how large the CE fault method buffer must be. The driver does not get
/// to pick this size: RM rejects a short buffer when the channel is created.
fn rm_mthdbuf_size<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<u64> {
    let params = rm_api(ctx, queue, rm.client, libos, version, deadline)
        .control::<api::MethodBufferSize>(rm.subdevice, &Default::default())?;
    let size = u64::from(params.size);
    if size == 0 {
        return Err(Error::Range);
    }
    sel4::debug_println!("NVIDIA_MTHDBUF_SIZE size={:#x}", size);
    let page = gmmu::PAGE as u64;
    Ok(size.checked_add(page - 1).ok_or(Error::Range)? & !(page - 1))
}

/// One RM-owned VRAM buffer, large enough for the channel instance, USERD and
/// RAMFC of the first channel.
const VRAM_BYTES: u64 = 0x4000;
const HANDLE_VRAM: u32 = 0x0040_0000;

fn rm_vram<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<(u64, u64)> {
    rm_vram_alloc(
        ctx,
        queue,
        rm,
        HANDLE_VRAM,
        VRAM_BYTES,
        0,
        libos,
        version,
        deadline,
    )
}

/// Allocate one VRAM surface through RM and return its placement.
fn rm_vram_alloc<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    handle: u32,
    size: u64,
    alignment: u64,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<(u64, u64)> {
    rm_vram_alloc_in(
        ctx, queue, rm.client, rm.device, handle, size, alignment, libos, version, deadline,
    )
}

/// Allocate one VRAM surface at a chosen offset. Display buffers must be
/// addressable with 32 bits, and RM hands out high offsets by default.
fn rm_vram_fixed<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    client: u32,
    device: u32,
    handle: u32,
    size: u64,
    offset: u64,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<u64> {
    let params = rm::vram_request_fixed(client, size, offset)?;
    let reply = rm_api(ctx, queue, client, libos, version, deadline)
        .alloc::<api::Vram>(device, handle, &params)?;
    sel4::debug_println!(
        "NVIDIA_RM_VRAM_FIXED handle={:#x} asked={:#x} placed={:#x}",
        handle,
        offset,
        reply.offset
    );
    if reply.offset != offset {
        return Err(Error::Range);
    }
    Ok(reply.offset)
}

/// Same, in an explicit client and device: display memory belongs to the
/// display client.
#[allow(clippy::too_many_arguments)]
fn rm_vram_alloc_in<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    client: u32,
    device: u32,
    handle: u32,
    size: u64,
    alignment: u64,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<(u64, u64)> {
    let params = rm::vram_request(client, size, alignment)?;
    let reply = rm_api(ctx, queue, client, libos, version, deadline)
        .alloc::<api::Vram>(device, handle, &params)?;
    sel4::debug_println!(
        "NVIDIA_RM_VRAM_REPLY handle={:#x} size={:#x} offset={:#x} limit={:#x}",
        handle,
        reply.size,
        reply.offset,
        reply.limit
    );
    if reply.size < size
        || reply
            .limit
            .checked_add(1)
            .is_none_or(|length| length < size)
    {
        return Err(Error::Range);
    }
    Ok((reply.offset, reply.limit))
}

/// First CE channel on our own VA space. The GPFIFO and method buffer live in
/// sysmem (we must write them), while the instance, USERD and RAMFC come from
/// the RM VRAM allocation, as the ABI requires.
fn rm_channel<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    mut va: Option<&mut Va>,
    vram: u64,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<(u32, u64)> {
    const INSTANCE: u64 = 0x1000;
    // gv100_chan_userd.size: one 0x200 slot per channel inside the USERD page.
    const USERD: u64 = 0x200;
    const RAMFC: u64 = 0x200;
    const GPFIFO_ENTRIES: u32 = 512;

    let mut gpfifo =
        ctx.with_platform(|p| p.allocate(GPFIFO_ENTRIES as usize * 8, DeviceAccess::ReadWrite))?;
    // USERD in system memory: the driver publishes GP_PUT with a plain store,
    // so the kick path needs no video-memory aperture at all.
    let mut userd_block = ctx.with_platform(|p| p.allocate(gmmu::PAGE, DeviceAccess::ReadWrite))?;
    unsafe { userd_block.zero()? };
    // The GPFIFO is addressed by VA in the channel's vspace, so it has to be
    // mapped there before the channel can use it.
    const GPFIFO_SLOT: u64 = 0x8000;
    let gpfifo_va = VA_BASE + GPFIFO_SLOT;
    if let Some(va) = va.as_deref_mut() {
        va.map_sys(GPFIFO_SLOT, gpfifo.iova)?;
    }
    let engines = rm_engines(ctx, queue, rm, libos, version, deadline)?;
    let copy = engines
        .iter()
        .find(|e| e.rm_type == rm::RM_ENGINE_TYPE_COPY0)
        .copied();
    sel4::debug_println!("NVIDIA_ENGINES_COPY present={}", copy.is_some());
    let mthdbuf_bytes = rm_mthdbuf_size(ctx, queue, rm, libos, version, deadline)?;
    let mut mthdbuf =
        ctx.with_platform(|p| p.allocate(mthdbuf_bytes as usize, DeviceAccess::ReadWrite))?;
    unsafe {
        gpfifo.zero()?;
        mthdbuf.zero()?;
    }

    // The instance, USERD and RAMFC live in VRAM and must be reachable through
    // the channel's page directory; RM cannot add them to an externally owned
    // vspace and would otherwise fail the allocation with NO_MEMORY.
    // USERD (0x200) and RAMFC (0x200) are not page aligned inside the block, so
    // map the whole VRAM allocation page by page instead of per object.
    if let Some(va) = va.as_deref_mut() {
        let page = gmmu::PAGE as u64;
        let mut slot = INSTANCE;
        let mut offset = 0;
        while offset < VRAM_BYTES {
            va.map_vid(ctx, slot, vram + offset)?;
            slot += page;
            offset += page;
        }
    }
    let desc = |base, size, address_space, cache| raw::NV_MEMORY_DESC_PARAMS {
        base,
        size,
        addressSpace: address_space,
        cacheAttrib: cache,
    };
    let params = raw::NV_CHANNEL_ALLOC_PARAMS {
        gpFifoOffset: gpfifo_va,
        gpFifoEntries: GPFIFO_ENTRIES,
        flags: rm::CHAN_FLAG_PRIVILEGED
            | rm::CHAN_FLAG_USERD_PAGE_FIXED
            | ((rm::CHANNEL_CID & raw::HORN_CHANNEL_USERD_MASK) << raw::HORN_CHANNEL_USERD_SHIFT),
        hVASpace: rm::HANDLE_VASPACE,
        engineType: rm::ENGINE_COPY0,
        cid: rm::CHANNEL_CID,
        subDeviceId: 1,
        internalFlags: rm::CHAN_INTERNAL_ADMIN,
        instanceMem: desc(vram, INSTANCE, 2, 1),
        userdMem: desc(
            userd_block.iova + USERD * rm::CHANNEL_CID as u64,
            USERD,
            1,
            0,
        ),
        ramfcMem: desc(vram, RAMFC, 2, 1),
        mthdbufMem: desc(mthdbuf.iova, mthdbuf_bytes, 1, 0),
        ..Default::default()
    };
    // The device accepts the channel as its parent (the subdevice handle is
    // rejected with INVALID_OBJECT_PARENT); the earlier NO_MEMORY came from a
    // resource this allocation asks RM to provide.
    sel4::debug_println!(
        "NVIDIA_CHANNEL_PARAMS gpfifo={:#x} entries={} vaspace={:#x} engine={} cid={} instance={:#x}/{} userd={:#x}/{} ramfc={:#x}/{} mthd={:#x}/{}",
        gpfifo_va,
        GPFIFO_ENTRIES,
        rm::HANDLE_VASPACE,
        rm::ENGINE_COPY0,
        rm::CHANNEL_CID,
        vram,
        INSTANCE,
        userd_block.iova + USERD * rm::CHANNEL_CID as u64,
        USERD,
        vram,
        RAMFC,
        mthdbuf.iova,
        mthdbuf_bytes
    );
    {
        let mut api = rm_api(ctx, queue, rm.client, libos, version, deadline);
        api.alloc::<api::Channel>(rm.device, rm::HANDLE_CHANNEL, &params)
            .map_err(|error| {
                sel4::debug_println!("NVIDIA_CHANNEL_FAILED step=channel error={:?}", error);
                error
            })?;
        api.alloc::<api::CopyEngine>(
            rm::HANDLE_CHANNEL,
            rm::HANDLE_CE,
            &raw::NVC0B5_ALLOCATION_PARAMETERS {
                version: raw::NVC0B5_ALLOCATION_PARAMETERS_VERSION_1,
                engineType: rm::ENGINE_COPY0,
            },
        )
        .map_err(|error| {
            sel4::debug_println!("NVIDIA_CHANNEL_FAILED step=ce error={:?}", error);
            error
        })?;
        api.control::<api::Bind>(
            rm::HANDLE_CHANNEL,
            &raw::NVA06F_CTRL_BIND_PARAMS {
                engineType: rm::ENGINE_COPY0,
            },
        )?;
        api.control::<api::Schedule>(
            rm::HANDLE_CHANNEL,
            &raw::NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS {
                bEnable: 1,
                bSkipSubmit: 0,
            },
        )?;
    }
    sel4::debug_println!("NVIDIA_CHANNEL_BOUND engine={}", rm::ENGINE_COPY0);
    let (source, destination, fence) = ce_copy(
        ctx,
        queue,
        va,
        &mut gpfifo,
        &mut userd_block,
        libos,
        version,
        deadline,
    )?;
    sel4::debug_println!(
        "NVIDIA_CE_COPY_OK source={:#x} destination={:#x} fence={:#x} bytes={}",
        source,
        destination,
        fence,
        CE_COPY_BYTES
    );
    Ok((rm::CHANNEL_CID, gpfifo_va))
}

/// Display objects RM exposes: the common control object, the per-chip root, and
/// the instance memory RM must know about before any of it works.
/// Display objects live in their own RM client and device, as nouveau does:
/// the display state is per client, and channels are validated against it.
const HANDLE_DISP_CLIENT: u32 = 0xc2d0_0000;
const HANDLE_DISP_DEVICE: u32 = 0xde2d_0000;
const HANDLE_DISP_SUBDEVICE: u32 = 0x5d2d_0000;
const HANDLE_DISP_INST: u32 = 0x0041_0000;
/// Nouveau's Ada path normally uses a 4-KiB coherent system-memory ring.
const DISP_PB_BYTES: u64 = 0x1000;
const DISP_PB_SIZE: u32 = raw::ChannelPBSize_PB_SIZE_4KB;
const DISP_INST_BYTES: u64 = gsp_disp::instance::INSTANCE_BYTES;
const DISP_NOTIFIER_HANDLE: u32 = 0x1000;

fn rm_disp<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    libos: Span,
    version: u32,
    deadline: u64,
    report: &crate::display::Report,
) -> Result<()> {
    let timing = gsp_disp::timing::Timing::preferred(&report.monitor)?;
    timing.sink_link(&report.dpcd)?;
    if report.heads & 1 == 0 {
        return Err(Error::Unsupported);
    }
    let mut notifier = ctx.with_platform(|p| p.allocate(4096, DeviceAccess::ReadWrite))?;
    unsafe {
        notifier.zero()?;
    } // before any descriptor/publication; initially NOT_BEGUN
    let mut core_pb =
        ctx.with_platform(|p| p.allocate(DISP_PB_BYTES as usize, DeviceAccess::Read))?;
    // Fully initialize before publication. Never rewrite GPU-visible words,
    // including after a timeout; the session retains this DMA allocation.
    unsafe {
        core_pb.zero()?;
    }
    let instance = {
        let mut api = rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline);
        rm_root(&mut api, HANDLE_DISP_CLIENT)?;
        api.alloc::<api::Device>(
            HANDLE_DISP_CLIENT,
            HANDLE_DISP_DEVICE,
            &raw::NV0080_ALLOC_PARAMETERS {
                hClientShare: HANDLE_DISP_CLIENT,
                ..Default::default()
            },
        )?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_RM step=client handle={:#x} device={:#x}",
            HANDLE_DISP_CLIENT,
            HANDLE_DISP_DEVICE
        );

        // Nouveau creates this even though display controls use the internal
        // client's subdevice. Keep the hardware-validated object tree.
        api.alloc::<api::Subdevice>(
            HANDLE_DISP_DEVICE,
            HANDLE_DISP_SUBDEVICE,
            &Default::default(),
        )?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_RM step=subdevice handle={:#x}",
            HANDLE_DISP_SUBDEVICE
        );
        display_memory(
            &mut api,
            HANDLE_DISP_INST,
            &rm::vram_request(HANDLE_DISP_CLIENT, DISP_INST_BYTES, DISP_INST_BYTES)?,
        )?
        .offset
    };
    let (program, plan) = {
        let mut api = rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline);
        api.for_client(rm.client).control::<api::DisplayInstMem>(
            rm.subdevice,
            &raw::NV2080_CTRL_INTERNAL_DISPLAY_WRITE_INST_MEM_PARAMS {
                instMemPhysAddr: instance,
                instMemSize: DISP_INST_BYTES,
                instMemAddrSpace: rm::DISP_INST_ADDR_SPACE,
                instMemCpuCacheAttr: rm::DISP_INST_CACHE_ATTR,
            },
        )?;
        sel4::debug_println!("NVIDIA_DISPLAY_RM step=inst-mem");
        api.alloc::<api::DisplayCommon>(HANDLE_DISP_DEVICE, rm::HANDLE_DISP_COMMON, &())?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_RM step=common handle={:#x}",
            rm::HANDLE_DISP_COMMON
        );
        let supported =
            api.control::<api::DisplaySupported>(rm::HANDLE_DISP_COMMON, &Default::default())?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_RM step=supported mask={:#x}",
            supported.displayMask
        );
        let info = api
            .for_client(rm.client)
            .control::<api::DisplayStaticInfo>(rm.subdevice, &Default::default())?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_RM step=static mask={:#x} windows={:#x} heads={} channels={}",
            supported.displayMask,
            info.windowPresentMask,
            info.numHeads,
            info.numDispChannels
        );
        let topology = gsp_disp::commands::Topology::new(info.windowPresentMask, info.numHeads)?;
        let plan = modeset::route(&mut api, supported.displayMask, report, &timing)?;
        let mut program = gsp_disp::commands::Core::new(topology, DISP_NOTIFIER_HANDLE)?;
        gsp_disp::head::append(&mut program, &timing, &plan)?;
        unsafe {
            for (i, &word) in program.push.words().iter().enumerate() {
                core_pb.write32(i * 4, word)?;
            }
        }
        api.alloc::<api::Display>(HANDLE_DISP_DEVICE, rm::HANDLE_DISP_ROOT, &())?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_RM step=root handle={:#x}",
            rm::HANDLE_DISP_ROOT
        );

        display_pushbuffer(&mut api, rm, core_pb.iova, rm::AD102_DISP_CORE_CHANNEL_DMA)?;
        let core_handle = rm::disp_chan_handle(rm::AD102_DISP_CORE_CHANNEL_DMA, 0);
        sel4::debug_println!(
            "NVIDIA_DISPLAY_CORE_INPUT client={:#x} parent={:#x} handle={:#x} pb={:#x} bytes={} size_class={} get_put=0 subdevices=1 target=coherent-sysmem",
            HANDLE_DISP_CLIENT,
            rm::HANDLE_DISP_ROOT,
            core_handle,
            core_pb.iova,
            DISP_PB_BYTES,
            DISP_PB_SIZE
        );
        let core = api.alloc::<api::DisplayCore>(
            rm::HANDLE_DISP_ROOT,
            core_handle,
            &raw::NV50VAIO_CHANNELDMA_ALLOCATION_PARAMETERS {
                channelPBSize: DISP_PB_SIZE,
                subDeviceId: 1,
                ..Default::default()
            },
        );
        let reply = core.map_err(|error| {
            sel4::debug_println!("NVIDIA_DISPLAY_RM step=core error={:?}", error);
            error
        })?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_CORE_ALLOC_OK handle={:#x} get_put={:#x} size_class={} subdevices={:#x}",
            core_handle,
            reply.offset,
            reply.channelPBSize,
            reply.subDeviceId
        );
        (program, plan)
    }; // End the RPC client's exclusive transport borrow before direct submission.
    let surface = scanout::prepare(ctx, queue, libos, version, deadline, &timing)?;
    let luts = modeset::luts(ctx, queue, libos, version, deadline)?;
    let window = scanout::window(ctx, queue, rm, libos, version, deadline, &timing)?;
    let bindings = [
        gsp_disp::instance::Binding::coherent(
            0,
            DISP_NOTIFIER_HANDLE,
            notifier.iova,
            notifier.bytes,
        )?,
        gsp_disp::instance::Binding::vram(1, scanout::IMAGE_HANDLE, surface.base, surface.bytes)?,
        gsp_disp::instance::Binding::vram(
            0,
            gsp_disp::head::LUT_HANDLE,
            luts.base,
            gsp_disp::head::LUT_BYTES,
        )?,
        gsp_disp::instance::Binding::vram(
            1,
            gsp_disp::head::LUT_HANDLE,
            luts.base + gsp_disp::head::LUT_BYTES as u64,
            gsp_disp::head::LUT_BYTES,
        )?,
        gsp_disp::instance::Binding::coherent(
            1,
            DISP_NOTIFIER_HANDLE,
            notifier.iova,
            notifier.bytes,
        )?,
    ];
    ctx.with_platform(|p| {
        gsp_disp::instance::install(p, instance, HANDLE_DISP_CLIENT, &bindings, deadline)
    })?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_RAMHT_OK instance={:#x} notifier={:#x} bytes={} handle={:#x}",
        instance,
        notifier.iova,
        notifier.bytes,
        DISP_NOTIFIER_HANDLE
    );
    core_submit(ctx, queue, libos, version, deadline, &notifier, &program)?;
    scanout::submit_window(ctx, queue, libos, version, deadline, &window)?;
    modeset::train(ctx, queue, libos, version, deadline, report, &plan)?;
    modeset::commit(
        ctx, queue, libos, version, deadline, &notifier, &program, &window,
    )
}

/// Allocation policy stays here; the ABI and reply decoding live in the client.
fn display_memory<T: FnMut(u32, &[u8]) -> Result<Vec<u8>>>(
    api: &mut rm::Client<T>,
    handle: u32,
    params: &raw::NV_MEMORY_ALLOCATION_PARAMS,
) -> Result<raw::NV_MEMORY_ALLOCATION_PARAMS> {
    let size = params.size;
    let reply = api.alloc::<api::Vram>(HANDLE_DISP_DEVICE, handle, params)?;
    if reply.size < size
        || reply
            .limit
            .checked_add(1)
            .is_none_or(|length| length < size)
    {
        return Err(Error::Range);
    }
    sel4::debug_println!(
        "NVIDIA_RM_VRAM_REPLY handle={:#x} size={:#x} offset={:#x} limit={:#x} attr={:#x} attr2={:#x} kind={:#x} requested_attr={:#x}",
        handle,
        reply.size,
        reply.offset,
        reply.limit,
        reply.attr,
        reply.attr2,
        reply.format,
        params.attr
    );
    Ok(reply)
}

/// Announce one display channel's push buffer to RM. The channel object cannot
/// be created before RM knows where its methods live.
fn display_pushbuffer<T: FnMut(u32, &[u8]) -> Result<Vec<u8>>>(
    api: &mut rm::Client<T>,
    rm: &Rm,
    iova: u64,
    class: u32,
) -> Result<()> {
    api.for_client(rm.client)
        .control::<api::DisplayPushbuffer>(
            rm.subdevice,
            &raw::NV2080_CTRL_INTERNAL_DISPLAY_CHANNEL_PUSHBUFFER_PARAMS {
                addressSpace: rm::DISP_ADDR_SYSMEM,
                physicalAddr: iova,
                limit: DISP_PB_BYTES - 1,
                cacheSnoop: 1,
                hclass: class,
                channelInstance: 0,
                valid: 1,
                // Same PHYS_PCI_COHERENT selector used by nouveau r570.
                pbTargetAperture: raw::NV_DMA_TARGET_NODE_PHYSICAL_PCI_COHERENT,
                channelPBSize: DISP_PB_SIZE,
                subDeviceId: 1,
            },
        )?;
    Ok(())
}

fn display_event<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<()> {
    ctx.with_platform(Native::service_pending_gsp_event)?;
    // Service firmware events while polling, as in the RPC wait path.
    // No synchronous RPC is outstanding during direct submission.
    if let Some(bytes) = queue.try_receive()? {
        let packet = wire::Packet::decode(&bytes)?;
        if packet.header.function == wire::CPU_SEQUENCE {
            unsafe {
                ctx.run_sequence(packet.payload, libos, version, deadline)?;
            }
        } else if wire::fatal_event(packet.header.function) {
            sel4::debug_println!("NVIDIA_DISPLAY_FATAL event={:#x}", packet.header.function);
            return Err(Error::FirmwareStatus(FirmwareStatus(
                packet.header.function,
            )));
        } else if packet.header.function < 4096 {
            return Err(Error::Malformed);
        }
    }
    Ok(())
}

/// Four immutable prefixes: fetch probes, then notifier UPDATE and ownership.
/// Both completion stages use distinct, initialized notifier slots.
fn core_submit<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    notifier: &DmaBlock,
    program: &gsp_disp::commands::Core,
) -> Result<()> {
    let deadline = ctx
        .time_us()?
        .checked_add(2_000_000)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    let read = |p: &mut Native| -> Result<(u32, u32)> {
        Ok((
            p.read32(gsp_disp::CORE_USER + gsp_disp::PUT)?,
            p.read32(gsp_disp::CORE_USER + gsp_disp::GET)?,
        ))
    };
    let (put, get) = ctx.with_platform(read)?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_CORE_REGS put_raw={:#x} get_raw={:#x}",
        put,
        get
    );
    if put != 0 || get != 0 {
        return Err(Error::State); // no ring reset or guessed resume position
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_OWNERSHIP_INPUT windows={:#x} heads={} put={} notifier_offset={}",
        program.topology.windows,
        program.topology.heads,
        program.ownership_put,
        gsp_disp::commands::OWNER_NOTIFIER
    );
    let mut previous = 0;
    for (kind, target, slot) in [
        ("nop", gsp_disp::NOP_PUT, None),
        ("update", gsp_disp::UPDATE_PUT, None),
        ("notifier", gsp_disp::NOTIFIER_PUT, Some(0)),
        (
            "ownership",
            program.ownership_put,
            Some(gsp_disp::commands::OWNER_NOTIFIER),
        ),
    ] {
        let mut fetched = false;
        if ctx.time_us()? >= deadline {
            return Err(Error::Timeout);
        }
        // x86 coherent DMA writes precede the MMIO PUT doorbell. Do not use
        // BAR1 or a GPU VA for this sysmem ring; RM was given its scoped IOVA.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        let value = gsp_disp::put_word(target)?;
        ctx.with_platform(|p| p.write32(gsp_disp::CORE_USER + gsp_disp::PUT, value))?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_CORE_SUBMIT kind={} put={} put_raw={:#x}",
            kind,
            target,
            value
        );
        loop {
            display_event(ctx, queue, libos, version, deadline)?;
            let (put, get_raw) = ctx.with_platform(read)?;
            let get = gsp_disp::get_dwords(get_raw);
            let now = ctx.time_us()?;
            if put != value || get_raw == u32::MAX || get < previous || get > target {
                sel4::debug_println!(
                    "NVIDIA_DISPLAY_CORE_BAD_REGS kind={} put_raw={:#x} get_raw={:#x}",
                    kind,
                    put,
                    get_raw
                );
                return Err(Error::State);
            }
            if now >= deadline {
                sel4::debug_println!(
                    "NVIDIA_DISPLAY_CORE_TIMEOUT kind={} put={} get={} put_raw={:#x} get_raw={:#x}",
                    kind,
                    target,
                    get,
                    put,
                    get_raw
                );
                return Err(Error::Timeout);
            }
            if get == target {
                if !fetched {
                    sel4::debug_println!(
                        "NVIDIA_DISPLAY_CORE_FETCHED kind={} put={} get={} notifier={}",
                        kind,
                        target,
                        get,
                        if slot.is_some() {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    );
                    fetched = true;
                }
                if let Some(offset) = slot {
                    // Volatile, no Rust references into GPU-writable notifier RAM.
                    let status = unsafe { notifier.read32(offset)? };
                    core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
                    if !gsp_disp::notifier_finished(status)? {
                        ctx.sleep_until(now.saturating_add(1000).min(deadline))?;
                        continue;
                    }
                    sel4::debug_println!(
                        "NVIDIA_DISPLAY_CORE_COMPLETE kind={} status={:#x} put={} get={} notifier_offset={}",
                        if kind == "notifier" { "update" } else { kind },
                        status,
                        target,
                        get,
                        offset
                    );
                }
                previous = get;
                break;
            }
            ctx.sleep_until(now.saturating_add(1000).min(deadline))?;
        }
    }
    Ok(())
}

/// One fenced 4-KiB copy through the copy engine: build the pushbuffer, publish
/// a GPFIFO entry, ring the channel and poll the fence the engine writes.
const CE_COPY_BYTES: usize = 4096;
const PUSH_SLOT: u64 = 0x9000;
/// `NV_RAMUSERD_GP_PUT`, in dwords, inside the channel's USERD slot.
const USERD_GP_PUT: usize = 0x8c;
const USERD_GP_GET: usize = 0x88;

fn ce_copy<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    mut va: Option<&mut Va>,
    gpfifo: &mut DmaBlock,
    userd_block: &mut DmaBlock,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<(u64, u64, u64)> {
    // One allocation for source, destination, fence and pushbuffer: every DMA
    // object costs two capability slots in devmgr, and the service CSpace is
    // the scarce resource here, not memory.
    const SRC: usize = 0;
    const DST: usize = CE_COPY_BYTES;
    const FENCE: usize = 2 * CE_COPY_BYTES;
    const PUSH: usize = 3 * CE_COPY_BYTES;
    let mut block =
        ctx.with_platform(|p| p.allocate(4 * CE_COPY_BYTES, DeviceAccess::ReadWrite))?;
    let pattern: Vec<u8> = (0..CE_COPY_BYTES).map(|i| (i as u8) ^ 0x5a).collect();
    unsafe {
        block.zero()?;
        block.initialize(SRC, &pattern)?;
    }
    // The pushbuffer is reached through the channel's page directory.
    if let Some(va) = va.as_deref_mut() {
        va.map_sys(PUSH_SLOT, block.iova + PUSH as u64)?;
    }
    let push_va = VA_BASE + PUSH_SLOT;
    let mut words = Vec::new();
    gsp_ce::encode_copy(
        &gsp_ce::Copy {
            source: block.iova + SRC as u64,
            destination: block.iova + DST as u64,
            bytes: CE_COPY_BYTES as u32,
            fence: block.iova + FENCE as u64,
        },
        &mut words,
    )?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(words.len() * 4)
        .map_err(|_| Error::Exhausted)?;
    for word in &words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    unsafe { block.initialize(PUSH, &bytes)? };
    let entry = gsp_ce::gpfifo_entry(push_va, words.len() as u32)?;
    unsafe { gpfifo.initialize(0, &entry.to_le_bytes())? };
    sel4::debug_println!(
        "NVIDIA_CE_SUBMIT pushbuffer={:#x} dwords={} entry={:#x} source={:#x} destination={:#x} fence={:#x}",
        push_va,
        words.len(),
        entry,
        block.iova + SRC as u64,
        block.iova + DST as u64,
        block.iova + FENCE as u64
    );
    // Publish the entry: GP_PUT is one past the last submitted entry, and the
    // USERD lives in system memory so a plain store is enough.
    const USERD_SLOT: usize = 0x200;
    let slot = USERD_SLOT * rm::CHANNEL_CID as usize;
    unsafe {
        userd_block
            .pointer()
            .as_ptr()
            .add(slot + USERD_GP_PUT)
            .cast::<u32>()
            .write_volatile(1);
    }
    let (put, get) = unsafe {
        let base = userd_block.pointer().as_ptr().add(slot);
        (
            base.add(USERD_GP_PUT).cast::<u32>().read_volatile(),
            base.add(USERD_GP_GET).cast::<u32>().read_volatile(),
        )
    };
    sel4::debug_println!("NVIDIA_CE_USERD put={} get={}", put, get);
    // Ring the channel's doorbell: the FIFO only samples GP_PUT when its
    // doorbell is written with (runlist << 16) | chid.
    let doorbell: u32 = 0x00bb_0000 + 0x90;
    ctx.with_platform(|p| {
        p.write32(doorbell, rm::CHANNEL_CID)?;
        Ok(())
    })?;
    sel4::debug_println!(
        "NVIDIA_CE_DOORBELL offset={:#x} value={}",
        doorbell,
        rm::CHANNEL_CID
    );
    // The engine writes the payload when the copy retires; a timeout here is a
    // real failure, never a "probably fine".
    loop {
        let now = ctx.time_us()?;
        if now >= deadline {
            sel4::debug_println!("NVIDIA_CE_COPY_TIMEOUT");
            return Err(Error::Timeout);
        }
        let word = unsafe { (block.pointer().as_ptr().add(FENCE) as *const u32).read_volatile() };
        if word == gsp_ce::FENCE_PAYLOAD {
            break;
        }
    }
    let copied =
        unsafe { core::slice::from_raw_parts(block.pointer().as_ptr().add(DST), CE_COPY_BYTES) };
    if copied != pattern.as_slice() {
        sel4::debug_println!("NVIDIA_CE_COPY_MISMATCH");
        return Err(Error::Checksum);
    }
    Ok((
        block.iova + SRC as u64,
        block.iova + DST as u64,
        block.iova + FENCE as u64,
    ))
}

fn wait<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    function: u32,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<Vec<u8>> {
    // Function correlation only: r570 does not echo request sequence on every
    // reply. There is one outstanding request and a fresh queue per boot epoch.
    // The absolute deadline is the only bound; an iteration cap would fire
    // before it and mask a slow firmware.
    loop {
        let now = ctx.time_us()?;
        if now >= deadline {
            return Err(Error::Timeout);
        }
        if let Some(bytes) = queue.try_receive()? {
            let packet = wire::Packet::decode(&bytes)?;
            if packet.header.function == function {
                return Ok(bytes);
            }
            if packet.header.function == wire::CPU_SEQUENCE {
                let field = |n: usize| {
                    packet
                        .payload
                        .get(n..n + 4)
                        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                };
                sel4::debug_println!(
                    "NVIDIA_GSP_SEQUENCE bytes={} words={} commands={} rpc_sequence={}",
                    packet.payload.len(),
                    field(0),
                    field(4),
                    packet.header.rpc_sequence
                );
                unsafe {
                    ctx.run_sequence(packet.payload, libos, version, deadline)?;
                }
            } else if wire::fatal_event(packet.header.function) {
                return Err(Error::FirmwareStatus(FirmwareStatus(
                    packet.header.function,
                )));
            } else if wire::post_event(packet.header.function) {
                sel4::debug_println!(
                    "NVIDIA_GSP_POST bytes={} rpc_sequence={}",
                    packet.payload.len(),
                    packet.header.rpc_sequence
                );
            } else if packet.header.function < 4096 {
                packet.success()?;
            }
        } else {
            ctx.sleep_until((now + 1000).min(deadline))?;
        }
    }
}
fn bar(config: &[u32; 64], index: usize) -> Result<u64> {
    let at = 4 + index;
    let low = config[at];
    if low & 1 != 0 {
        return Err(Error::Unsupported);
    }
    let high = if low & 6 == 4 {
        *config.get(at + 1).ok_or(Error::Range)? as u64
    } else {
        0
    };
    Ok((high << 32) | u64::from(low & !15))
}
pub fn run(session: DeviceSession, time: Time, resources: PciResources) -> ! {
    let mut ctx = Ad106::new(Native::new(session, time));
    let result = (|| -> Result<()> {
        // Whole-boot budget inside the profile's 180 s READY window: firmware
        // staging alone is 60.6 MiB of chunked VT-d mappings.
        let deadline = ctx
            .time_us()?
            .checked_add(150_000_000)
            .ok_or(Error::Range)?;
        sel4::debug_println!("NVIDIA_GSP_BOOT_BEGIN abi=570.144 dma_limit=67108864");
        ctx.probe(deadline)?;
        sel4::debug_println!("NVIDIA_GSP_STEP name=prom");
        let prom = ctx.with_platform(Native::prom)?;
        sel4::debug_println!(
            "NVIDIA_PROM bytes={} sig={:02x}{:02x}",
            prom.len(),
            prom.first().copied().unwrap_or(0),
            prom.get(1).copied().unwrap_or(0)
        );
        match horn_nvidia_gsp::vbios::layout(&prom) {
            Ok(l) => sel4::debug_println!(
                "NVIDIA_PROM_LAYOUT rom={:#x} images={} pci={:?} fwsec={:?} end={:?}",
                l.rom,
                l.images,
                l.pci,
                l.fwsec,
                l.end
            ),
            Err(error) => sel4::debug_println!("NVIDIA_PROM_LAYOUT error={:?}", error),
        }
        for (i, chunk) in prom[..prom.len().min(0x40)].chunks(16).enumerate() {
            sel4::debug_println!("NVIDIA_PROM_HEAD {:04x} {:02x?}", i * 16, chunk);
        }
        if let Some(pointer) = prom.get(0x18..0x1a) {
            let pcir = usize::from(u16::from_le_bytes([pointer[0], pointer[1]]));
            sel4::debug_println!("NVIDIA_PROM_PCIR at={:#x}", pcir);
            if let Some(area) = prom.get(pcir..pcir.saturating_add(0x20)) {
                for (i, chunk) in area.chunks(16).enumerate() {
                    sel4::debug_println!("NVIDIA_PROM_PCIR_BYTES {:04x} {:02x?}", i * 16, chunk);
                }
            }
        }
        for tag in [b"PCIR", b"NPDS", b"NPDE"] {
            let scan = &prom[..prom.len().min(0x1000)];
            if let Some(at) = scan.windows(4).position(|window| window == tag) {
                sel4::debug_println!(
                    "NVIDIA_PROM_TAG {} at={:#x}",
                    core::str::from_utf8(tag).unwrap_or("?"),
                    at
                );
            }
        }
        match Fwsec::descriptor_bytes(&prom) {
            Ok(desc) => {
                let word = |n: usize| {
                    desc.get(n..n + 4)
                        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                };
                let half = |n: usize| {
                    desc.get(n..n + 2)
                        .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
                };
                sel4::debug_println!(
                    "NVIDIA_FWSEC_DESC len={} hdr={:#x} version={} size={} stored={} imem={} dmem={} count={} versions={:#x} engines={:#x} ucode_id={} imem_dest={:#x} dmem_dest={:#x} pkc={:#x} iface={:#x}",
                    desc.len(),
                    word(0),
                    (word(0) >> 8) & 255,
                    word(0) >> 16,
                    word(4),
                    word(20),
                    word(32),
                    desc.get(39).copied().unwrap_or(0),
                    half(40),
                    half(36),
                    desc.get(38).copied().unwrap_or(0),
                    word(16),
                    word(28),
                    word(8),
                    word(12)
                );
            }
            Err(error) => sel4::debug_println!("NVIDIA_FWSEC_DESC error={:?}", error),
        }
        let fwsec = step(&mut ctx, Stage::Fuse, "fwsec", || Fwsec::from_prom(&prom))?;
        let bundle = step(&mut ctx, Stage::Fuse, "bundle", || {
            Bundle::parse(
                images::META,
                images::IMAGE,
                images::BOOTLOADER,
                images::LOAD,
                images::UNLOAD,
            )
        })?;
        sel4::debug_println!("NVIDIA_GSP_STEP name=framebuffer");
        let fb = ctx.framebuffer()?;
        sel4::debug_println!(
            "NVIDIA_GSP_LAYOUT fb={} frts={:#x} wpr={:#x}",
            fb.bytes(),
            fb.frts().start,
            fb.wpr().start
        );
        let fuse = ctx.signature_fuse(fwsec.engine_mask, fwsec.ucode_id)?;
        let mut frts_cpu = vec![0; fwsec.ucode.len()];
        let mut sb_cpu = vec![0; fwsec.ucode.len()];
        let range = fb.frts();
        let frts = fwsec.prepare(
            fuse,
            Command::Frts {
                address: range.start,
                bytes: range.end - range.start,
            },
            &mut frts_cpu,
        )?;
        let sb = fwsec.prepare(fuse, Command::Sb, &mut sb_cpu)?;
        let load_fuse = ctx.signature_fuse(bundle.loader.engine_mask, bundle.loader.ucode_id)?;
        let unload_fuse =
            ctx.signature_fuse(bundle.unloader.engine_mask, bundle.unloader.ucode_id)?;
        let mut load_cpu = vec![0; bundle.loader.blob.len()];
        let mut unload_cpu = vec![0; bundle.unloader.blob.len()];
        let loader = bundle.loader.prepare(load_fuse, &mut load_cpu)?;
        let unloader = bundle.unloader.prepare(unload_fuse, &mut unload_cpu)?;
        // All four signed sources, including cleanup, exist before first reset.
        let frts_dma = stage(&mut ctx, frts.bytes(), DeviceAccess::Read)?;
        let sb_dma = stage(&mut ctx, sb.bytes(), DeviceAccess::Read)?;
        let load_dma = stage(&mut ctx, loader.bytes(), DeviceAccess::Read)?;
        let unload_dma = stage(&mut ctx, unloader.bytes(), DeviceAccess::Read)?;
        let frts_pin = unsafe { PinnedSource::from_grant(&frts, frts_dma.iova)? };
        let sb_pin = unsafe { PinnedSource::from_grant(&sb, sb_dma.iova)? };
        let load_pin = unsafe { PinnedSource::from_grant(&loader, load_dma.iova)? };
        let unload_pin = unsafe { PinnedSource::from_grant(&unloader, unload_dma.iova)? };
        sel4::debug_println!(
            "NVIDIA_GSP_STAGE image_bytes={} cleanup_prepared=true",
            bundle.image.len()
        );
        let started = ctx.time_us()?;
        let image = stage(&mut ctx, bundle.image, DeviceAccess::Read)?;
        sel4::debug_println!(
            "NVIDIA_GSP_IMAGE_STAGED bytes={} elapsed_us={}",
            bundle.image.len(),
            ctx.time_us()?.saturating_sub(started)
        );
        let mut radix_cpu = vec![0; boot::radix_bytes(bundle.image.len())?];
        let mut radix = ctx.with_platform(|p| p.allocate(radix_cpu.len(), DeviceAccess::Read))?;
        boot::radix3(span(&image)?, span(&radix)?, &mut radix_cpu)?;
        unsafe {
            radix.initialize(0, &radix_cpu)?;
        }
        let bootloader = stage(&mut ctx, bundle.bootloader.blob, DeviceAccess::Read)?;
        let signatures = stage(&mut ctx, bundle.signatures, DeviceAccess::Read)?;
        let mut page = [0; PAGE];
        fb.wpr_metadata(
            &bundle,
            span(&radix)?,
            span(&bootloader)?,
            span(&signatures)?,
            &mut page,
        )?;
        let wpr_meta = stage(&mut ctx, &page, DeviceAccess::Read)?;
        let mut queue_cpu = vec![0; boot::QUEUE_BYTES];
        let mut queue_dma =
            ctx.with_platform(|p| p.allocate(boot::QUEUE_BYTES, DeviceAccess::ReadWrite))?;
        boot::queues(span(&queue_dma)?, &mut queue_cpu)?;
        unsafe {
            queue_dma.initialize(0, &queue_cpu)?;
        }
        boot::rmargs(span(&queue_dma)?, &mut page)?;
        let rmargs = stage(&mut ctx, &page, DeviceAccess::Read)?;
        let mut logs = Vec::new();
        for _ in 0..3 {
            let mut block =
                ctx.with_platform(|p| p.allocate(boot::LOG_BYTES, DeviceAccess::ReadWrite))?;
            let mut bytes = vec![0; boot::LOG_BYTES];
            boot::log_buffer(span(&block)?, &mut bytes)?;
            unsafe {
                block.initialize(0, &bytes)?;
            }
            logs.push(block);
        }
        boot::libos(
            [span(&logs[0])?, span(&logs[1])?, span(&logs[2])?],
            span(&rmargs)?,
            &mut page,
        )?;
        let libos = stage(&mut ctx, &page, DeviceAccess::Read)?;
        let flush = ctx.with_platform(|p| p.allocate(PAGE, DeviceAccess::ReadWrite))?;
        let inputs = Handoff {
            framebuffer: &fb,
            flush: span(&flush)?,
            libos: span(&libos)?,
            wpr_meta: span(&wpr_meta)?,
            frts: &frts_pin,
            sb: &sb_pin,
            loader: &load_pin,
            unloader: &unload_pin,
            app_version: bundle.bootloader.app_version,
        };
        let mut queue = unsafe { Queue::attach(Shared::new(&mut queue_dma))? };
        ctx.with_platform(Native::bind_irq)?;
        let cfg = &resources.configuration.data;
        // pci_dev_id(): vendor in the high half, device in the low half.
        let pci_dev_id = ((cfg[0] & 0xffff) << 16) | (cfg[0] >> 16);
        let system = boot::system_info(
            [bar(cfg, 0)?, bar(cfg, 1)?, bar(cfg, 3)?],
            pci_dev_id,
            [cfg[0], cfg[11], cfg[2] & 255],
        )?;
        send(
            &mut ctx,
            &mut queue,
            wire::SET_SYSTEM_INFO,
            &system,
            deadline,
        )?;
        send(
            &mut ctx,
            &mut queue,
            wire::SET_REGISTRY,
            &boot::registry()?,
            deadline,
        )?;
        // Remapped IRQ and all mappings are committed; only now authorize DMA.
        ctx.with_platform(|p| unsafe { p.activate() })?;
        sel4::debug_println!("NVIDIA_GSP_HANDOFF_BEGIN bus_master=true");
        unsafe {
            ctx.handoff(&inputs, deadline)?;
        }
        sel4::debug_println!("NVIDIA_GSP_RISCV_ACTIVE init_done=false");
        let init = wait(
            &mut ctx,
            &mut queue,
            wire::INIT_DONE,
            inputs.libos,
            inputs.app_version,
            deadline,
        )?;
        wire::Packet::decode(&init)?.init_done()?;
        sel4::debug_println!("NVIDIA_GSP_INIT_DONE");
        // The firmware image and its boot-only scaffolding are no longer read by
        // the GSP: release them (and their capability slots) before the next
        // allocation, keeping only what the unload path needs.
        for (name, block) in [
            ("image", image),
            ("radix", radix),
            ("bootloader", bootloader),
            ("signatures", signatures),
        ] {
            match ctx.with_platform(|p| p.release(block)) {
                Ok(()) => sel4::debug_println!("NVIDIA_STAGING_RELEASED name={}", name),
                Err(error) => sel4::debug_println!(
                    "NVIDIA_STAGING_RELEASE_FAILED name={} error={:?}",
                    name,
                    error
                ),
            }
        }
        send(
            &mut ctx,
            &mut queue,
            wire::STATIC_INFO,
            &vec![0; wire::STATIC_INFO_BYTES],
            deadline,
        )?;
        let reply = wait(
            &mut ctx,
            &mut queue,
            wire::STATIC_INFO,
            inputs.libos,
            inputs.app_version,
            deadline,
        )?;
        let packet = wire::Packet::decode(&reply)?;
        sel4::debug_println!(
            "NVIDIA_GSP_STATIC_INFO bytes={} status={:#x} observed_fb={}",
            packet.payload.len(),
            packet.header.status,
            fb.bytes()
        );
        let info = wire::StaticInfo::parse(&packet, fb.bytes())?;
        ctx.with_platform(Native::enable_gsp_events)?;
        // Clear-before-enable can leave old messages without a fresh interrupt.
        while let Some(bytes) = queue.try_receive()? {
            let p = wire::Packet::decode(&bytes)?;
            if wire::fatal_event(p.header.function) {
                return Err(Error::FirmwareStatus(FirmwareStatus(p.header.function)));
            }
        }
        let snapshot = p::ProbeSnapshot {
            boot0: ctx.with_platform(|p| p.read32(0))?,
            boot42: ctx.with_platform(|p| p.read32(0xa00))?,
            gfw_state: p::GfwState::Complete,
            gfw_progress: 255,
            bus_master: true,
            gsp_booted: true,
            vbios_state: p::VbiosState::FwsecParsed,
            fwsec_bytes: fwsec.ucode.len() as u32,
            fwsec_engines: fwsec.engine_mask as u32,
            fwsec_ucode: fwsec.ucode_id as u32,
            fwsec_signature_versions: fwsec.signature_versions as u32,
        };
        sel4::debug_println!(
            "NVIDIA_GSP_READY name={} fb={} regions={} transport_sequence={} rpc_sequence={}",
            info.name,
            info.framebuffer_bytes,
            info.regions,
            packet.header.transport_sequence,
            packet.header.rpc_sequence
        );
        // Display first: it depends on nothing RM provides, and it is what the
        // operator can see, so it must not sit behind a channel timeout.
        let display = match ctx.with_platform(|p| crate::display::probe(p, &prom)) {
            Ok(report) => {
                sel4::debug_println!(
                    "NVIDIA_DISPLAY connector={} kind=dp aux={} pad={} dpcd={:02x?}",
                    report.connector,
                    report.aux_channel,
                    report.pad,
                    report.dpcd
                );
                sel4::debug_println!(
                    "NVIDIA_MONITOR name={} manufacturer={} size={:?}x{:?}cm modes={}",
                    report.monitor.name.as_deref().unwrap_or("unknown"),
                    report.monitor.manufacturer.as_deref().unwrap_or("unknown"),
                    report.monitor.width_cm,
                    report.monitor.height_cm,
                    report.monitor.modes.len()
                );
                let timing = gsp_disp::timing::Timing::preferred(&report.monitor)?;
                let (rate, lanes) = timing.sink_link(&report.dpcd)?;
                sel4::debug_println!(
                    "NVIDIA_DISPLAY_PREFERRED dcb={} heads={:#x} width={} height={} clock_khz={} total={}x{} pitch={} bytes={} sink_rate={:#x} lanes={} state=preflight-only",
                    report.dcb_index,
                    report.heads,
                    timing.width,
                    timing.height,
                    timing.pixel_clock_khz,
                    timing.h_total,
                    timing.v_total,
                    timing.pitch,
                    timing.bytes,
                    rate,
                    lanes
                );
                for (index, mode) in report.monitor.modes.iter().enumerate() {
                    sel4::debug_println!(
                        "NVIDIA_MODE index={} {}x{}{} full={} preferred={} refresh_mhz={:?} clock_khz={:?} h={}/{:?}/{:?} v={}/{:?}/{:?}",
                        index,
                        mode.width,
                        mode.height,
                        if mode.interlaced { "i" } else { "p" },
                        mode.pixel_clock_khz.is_some(),
                        mode.preferred,
                        mode.refresh_mhz,
                        mode.pixel_clock_khz,
                        mode.h_front_porch,
                        mode.h_sync,
                        mode.h_back_porch,
                        mode.v_front_porch,
                        mode.v_sync,
                        mode.v_back_porch
                    );
                }
                Some(report)
            }
            Err(error) => {
                sel4::debug_println!("NVIDIA_DISPLAY_FAILED error={:?}", error);
                None
            }
        };
        horn_runtime::notify_ready(DEVICE_DRIVER_CAPS_READY_SLOT);
        // First RM client above the firmware: root -> device -> subdevice.
        // A failure here is reported but must not tear down a healthy GSP.
        match rm_client(
            &mut ctx,
            &mut queue,
            inputs.libos,
            inputs.app_version,
            deadline,
        ) {
            Ok(rm) => {
                sel4::debug_println!(
                    "NVIDIA_RM_READY client={:#x} device={:#x} subdevice={:#x}",
                    rm.client,
                    rm.device,
                    rm.subdevice
                );
                // No monitor: leave the display untouched rather than synthesize a mode.
                let report = display.as_ref().ok_or(Error::Unavailable)?;
                rm_disp(
                    &mut ctx,
                    &mut queue,
                    &rm,
                    inputs.libos,
                    inputs.app_version,
                    deadline,
                    report,
                )
                .map_err(|error| {
                    sel4::debug_println!("NVIDIA_DISPLAY_RM_FAILED error={:?}", error);
                    error
                })?;
                // Compute/GMMU and window/cursor probes remain parked while
                // validating the display core; no timeout-heavy CE side path.
            }
            Err(error) => sel4::debug_println!("NVIDIA_RM_FAILED error={:?}", error),
        }
        let endpoint = sel4::cap::Endpoint::from_bits(DEVICE_DRIVER_CAPS_SERVICE_SLOT as _);
        let reply_cap = sel4::cap::Reply::from_bits(DEVICE_DRIVER_CAPS_REPLY_SLOT as _);
        let receive = sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _)
            .absolute_cptr(sel4::CPtr::from_bits(crate::SLOTS.public_receive() as _));
        loop {
            sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&receive));
            let (message, badge) = endpoint.recv(reply_cap);
            if badge & horn_supervisor::STOP != 0 {
                let stop_deadline = ctx.time_us()?.checked_add(800_000).ok_or(Error::Range)?;
                ctx.with_platform(Native::disable_gsp_events)?;
                let mut shutdown = [0u8; 8]; // PowerStateLevel0, bInPMTransition=false.
                shutdown[4..].copy_from_slice(&0u32.to_le_bytes());
                send(
                    &mut ctx,
                    &mut queue,
                    wire::UNLOADING,
                    &shutdown,
                    stop_deadline,
                )?;
                let bytes = wait(
                    &mut ctx,
                    &mut queue,
                    wire::UNLOADING,
                    inputs.libos,
                    inputs.app_version,
                    stop_deadline,
                )?;
                wire::Packet::decode(&bytes)?.success()?;
                ctx.wait_suspended(stop_deadline)?;
                unsafe {
                    ctx.unload(&inputs, stop_deadline)?;
                }
                drop(queue);
                ctx.with_platform(Native::close)?;
                sel4::cap::Notification::from_bits(DEVICE_DRIVER_CAPS_STOPPED_SLOT as _).signal();
                loop {
                    sel4::cap::Notification::from_bits(DEVICE_DRIVER_CAPS_WAKE_SLOT as _).wait();
                }
            }
            if badge & horn_supervisor::IRQ != 0 {
                ctx.with_platform(Native::service_gsp_event)?;
                while let Some(bytes) = queue.try_receive()? {
                    let packet = wire::Packet::decode(&bytes)?;
                    if wire::fatal_event(packet.header.function) {
                        return Err(Error::FirmwareStatus(FirmwareStatus(
                            packet.header.function,
                        )));
                    }
                }
            } else if badge == crate::CLIENT {
                ctx.with_platform(|native| {
                    let request = {
                        let root = sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _);
                        let mut server =
                            Sel4Server::new(root, endpoint, receive, &mut native.transport().space);
                        server.decode_received_dynamic(
                            message,
                            badge,
                            reply_cap,
                            p::nvidia_probe_object_request_layout,
                            p::NvidiaProbeObjectRequest::decode,
                        )
                    };
                    if let Ok((_, request, reply)) = request {
                        let response = p::nvidia_probe_object_dispatch(
                            request,
                            &mut Reader {
                                snapshot: &snapshot,
                                stopped: false,
                            },
                        );
                        p::nvidia_probe_object_reply(
                            reply,
                            &mut native.transport().space,
                            &response,
                        )
                        .map_err(|_| Error::Backend)?;
                    }
                    Ok(())
                })?;
            }
            receive.delete().map_err(|_| Error::Backend)?;
        }
    })();
    let error = result.err().unwrap_or(Error::State);
    let _ = ctx.quarantine(error);
    sel4::debug_println!(
        "NVIDIA_GSP_FAILED error={:?} diagnostic={:?} dma_retained=true",
        error,
        ctx.diagnostic()
    );
    // Panic aborts this TCB; supervisor removes publication and devmgr must
    // acknowledge SID isolation/drain before process memory can be reclaimed.
    panic!("AD106 GSP boot/runtime failed; no reset/retry");
}
