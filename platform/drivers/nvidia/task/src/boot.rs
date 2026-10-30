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
    rm,
    vbios::{Command, Fwsec},
    wire,
};
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

fn rm_alloc<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    h_client: u32,
    h_parent: u32,
    h_object: u32,
    class: u32,
    params: &[u8],
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<()> {
    let body = rm::encode(h_client, h_parent, h_object, class, params)?;
    send(ctx, queue, rm::RM_ALLOC, &body, deadline)?;
    let reply = wait(ctx, queue, rm::RM_ALLOC, libos, version, deadline)?;
    let packet = wire::Packet::decode(&reply)?;
    rm::check(packet.payload, h_object, class, params.len())
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
    let fail = |step: &str, error: Error| {
        sel4::debug_println!("NVIDIA_RM_FAILED step={} error={:?}", step, error);
        error
    };
    // The root object is its own parent and its own client.
    rm_alloc(
        ctx,
        queue,
        rm.client,
        rm.client,
        rm.client,
        rm::NV01_ROOT,
        &rm::root_params(rm.client),
        libos,
        version,
        deadline,
    )
    .map_err(|error| fail("client", error))?;
    rm_alloc(
        ctx,
        queue,
        rm.client,
        rm.client,
        rm.device,
        rm::NV01_DEVICE_0,
        &rm::device_params(rm.client),
        libos,
        version,
        deadline,
    )
    .map_err(|error| fail("device", error))?;
    rm_alloc(
        ctx,
        queue,
        rm.client,
        rm.device,
        rm.subdevice,
        rm::NV20_SUBDEVICE_0,
        &rm::subdevice_params(),
        libos,
        version,
        deadline,
    )
    .map_err(|error| fail("subdevice", error))?;
    Ok(rm)
}

/// Driver-owned VA space: one 4 KiB sysmem page mapped at `VA_BASE`, with the
/// root handed to RM via SET_PAGE_DIRECTORY. This is the mapping model the
/// channel and copy engine will use.
struct Va {
    base: u64,
    root: u64,
    source: u64,
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

const VA_BASE: u64 = 0x1_0000_0000;

fn gmmu_setup<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<Va> {
    let source = ctx.with_platform(|p| p.allocate(gmmu::PAGE, DeviceAccess::ReadWrite))?;
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
        tables.write(gmmu::index(VA_BASE, 4)?, 4, gmmu::pte_sys(source.iova)?)?;
    }
    unsafe {
        arena.zero()?;
        arena.initialize(0, &bytes)?;
    }

    // Only index and flags are meaningful: nouveau leaves the VA size and
    // window to RM, and a caller-chosen size/limit is rejected as an invalid
    // argument.
    rm_alloc(
        ctx,
        queue,
        rm.client,
        rm.device,
        rm::HANDLE_VASPACE,
        rm::FERMI_VASPACE_A,
        &rm::vspace_params(rm::VASPACE_EXTERNALLY_OWNED, 0, 0, 0),
        libos,
        version,
        deadline,
    )
    .map_err(|error| {
        sel4::debug_println!("NVIDIA_GMMU_FAILED step=vspace error={:?}", error);
        error
    })?;

    // numEntries is the ROOT table's entry count (2 bits for GA10x), not the
    // 512-entry leaf size: nouveau derives it from the largest-shift level.
    let params = rm::page_directory_params(
        arena.iova,
        gmmu::entries(0)? as u32,
        rm::SET_PD_APERTURE_SYSMEM_COH,
        rm::HANDLE_VASPACE,
    );
    let body = rm::encode_ctrl(
        rm.client,
        rm.device,
        rm::CTRL_DMA_SET_PAGE_DIRECTORY,
        &params,
    )?;
    send(ctx, queue, rm::RM_CONTROL, &body, deadline)?;
    let reply = wait(ctx, queue, rm::RM_CONTROL, libos, version, deadline)?;
    let packet = wire::Packet::decode(&reply)?;
    rm::check_ctrl(
        packet.payload,
        rm.device,
        rm::CTRL_DMA_SET_PAGE_DIRECTORY,
        params.len(),
    )
    .map_err(|error| {
        sel4::debug_println!("NVIDIA_GMMU_FAILED step=page-directory error={:?}", error);
        error
    })?;
    Ok(Va {
        base: VA_BASE,
        root: arena.iova,
        source: source.iova,
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
    let params = rm::engine_table_params();
    let body = rm::encode_ctrl(
        rm.client,
        rm.subdevice,
        rm::CTRL_FIFO_GET_DEVICE_INFO_TABLE,
        &params,
    )?;
    send(ctx, queue, rm::RM_CONTROL, &body, deadline)?;
    let reply = wait(ctx, queue, rm::RM_CONTROL, libos, version, deadline)?;
    let packet = wire::Packet::decode(&reply)?;
    rm::check_ctrl(
        packet.payload,
        rm.subdevice,
        rm::CTRL_FIFO_GET_DEVICE_INFO_TABLE,
        params.len(),
    )?;
    let entries = rm::engine_entries(packet.payload)?;
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
    let body = rm::encode_ctrl(
        rm.client,
        rm.subdevice,
        rm::CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE,
        &rm::mthdbuf_params(),
    )?;
    send(ctx, queue, rm::RM_CONTROL, &body, deadline)?;
    let reply = wait(ctx, queue, rm::RM_CONTROL, libos, version, deadline)?;
    let packet = wire::Packet::decode(&reply)?;
    rm::check_ctrl(
        packet.payload,
        rm.subdevice,
        rm::CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE,
        rm::mthdbuf_params().len(),
    )?;
    let size = rm::mthdbuf_size(packet.payload)? as u64;
    sel4::debug_println!("NVIDIA_MTHDBUF_SIZE size={:#x}", size);
    if size == 0 {
        return Err(Error::Range);
    }
    let page = gmmu::PAGE as u64;
    Ok((size + page - 1) & !(page - 1))
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
    let params = rm::vram_params(rm.client, VRAM_BYTES, 0)?;
    let body = rm::encode(
        rm.client,
        rm.device,
        HANDLE_VRAM,
        rm::NV01_MEMORY_LOCAL_USER,
        &params,
    )?;
    send(ctx, queue, rm::RM_ALLOC, &body, deadline)?;
    let reply = wait(ctx, queue, rm::RM_ALLOC, libos, version, deadline)?;
    let packet = wire::Packet::decode(&reply)?;
    rm::check(
        packet.payload,
        HANDLE_VRAM,
        rm::NV01_MEMORY_LOCAL_USER,
        params.len(),
    )?;
    let payload = packet.payload;
    // The reply is NVOS21_PARAMETERS (32 bytes: hRoot, hObjectParent,
    // hObjectNew, hClass, pAllocParms, paramsSize, status) followed by
    // NV_MEMORY_ALLOCATION_PARAMS. Reading the placement from the header
    // region gave offset 0 and the whole-framebuffer limit, which is why the
    // channel instance pointed at reserved VRAM and the alloc failed.
    let params = &payload[32..];
    if params.len() < 96 {
        return Err(Error::Malformed);
    }
    let size = u64::from_le_bytes(params[64..72].try_into().unwrap());
    let offset = u64::from_le_bytes(params[80..88].try_into().unwrap());
    let limit = u64::from_le_bytes(params[88..96].try_into().unwrap());
    sel4::debug_println!(
        "NVIDIA_RM_VRAM_REPLY size={:#x} offset={:#x} limit={:#x}",
        size,
        offset,
        limit
    );
    Ok((offset, limit))
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
    // gf100_chan_userd.size: a channel's USERD window is one page.
    const USERD: u64 = 0x1000;
    const RAMFC: u64 = 0x200;
    const GPFIFO_ENTRIES: u32 = 512;

    let mut gpfifo =
        ctx.with_platform(|p| p.allocate(GPFIFO_ENTRIES as usize * 8, DeviceAccess::ReadWrite))?;
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
    if let Some(va) = va {
        let page = gmmu::PAGE as u64;
        let mut slot = INSTANCE;
        let mut offset = 0;
        while offset < VRAM_BYTES {
            va.map_vid(ctx, slot, vram + offset)?;
            slot += page;
            offset += page;
        }
    }
    let instance = rm::memory_desc(vram, INSTANCE, 2, 1);
    let userd = rm::memory_desc(vram + INSTANCE, USERD, 2, 1);
    // RAMFC lives inside the instance block: nouveau passes the instance base
    // again instead of an offset past USERD.
    let ramfc = rm::memory_desc(vram, RAMFC, 2, 1);
    let methods = rm::memory_desc(mthdbuf.iova, mthdbuf_bytes, 1, 0);
    let params = rm::channel_params(rm::Channel {
        h_vaspace: rm::HANDLE_VASPACE,
        gp_fifo: gpfifo_va,
        gp_fifo_entries: GPFIFO_ENTRIES,
        cid: rm::CHANNEL_CID,
        instance: &instance,
        userd: &userd,
        ramfc: &ramfc,
        mthdbuf: &methods,
    })?;
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
        vram + INSTANCE,
        USERD,
        vram,
        RAMFC,
        mthdbuf.iova,
        mthdbuf_bytes
    );
    rm_alloc(
        ctx,
        queue,
        rm.client,
        rm.device,
        rm::HANDLE_CHANNEL,
        rm::AMPERE_CHANNEL_GPFIFO_A,
        &params,
        libos,
        version,
        deadline,
    )
    .map_err(|error| {
        sel4::debug_println!("NVIDIA_CHANNEL_FAILED step=channel error={:?}", error);
        error
    })?;

    rm_alloc(
        ctx,
        queue,
        rm.client,
        rm::HANDLE_CHANNEL,
        rm::HANDLE_CE,
        rm::AMPERE_DMA_COPY_B,
        &[],
        libos,
        version,
        deadline,
    )
    .map_err(|error| {
        sel4::debug_println!("NVIDIA_CHANNEL_FAILED step=ce error={:?}", error);
        error
    })?;
    Ok((rm::CHANNEL_CID, gpfifo_va))
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
                return Err(Error::FirmwareStatus(FirmwareStatus(packet.header.function)));
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
                let mut va = match gmmu_setup(
                    &mut ctx,
                    &mut queue,
                    &rm,
                    inputs.libos,
                    inputs.app_version,
                    deadline,
                ) {
                    Ok(va) => {
                        sel4::debug_println!(
                            "NVIDIA_GMMU_READY va={:#x} root={:#x} vspace={:#x} source={:#x}",
                            va.base,
                            va.root,
                            rm::HANDLE_VASPACE,
                            va.source
                        );
                        Some(va)
                    }
                    Err(error) => {
                        sel4::debug_println!("NVIDIA_GMMU_FAILED error={:?}", error);
                        None
                    }
                };
                match rm_vram(
                    &mut ctx,
                    &mut queue,
                    &rm,
                    inputs.libos,
                    inputs.app_version,
                    deadline,
                ) {
                    Ok((offset, limit)) => {
                        sel4::debug_println!(
                            "NVIDIA_RM_VRAM_OK size={} offset={:#x} limit={:#x}",
                            VRAM_BYTES,
                            offset,
                            limit
                        );
                        match rm_channel(
                            &mut ctx,
                            &mut queue,
                            &rm,
                            va.as_mut(),
                            offset,
                            inputs.libos,
                            inputs.app_version,
                            deadline,
                        ) {
                            Ok((cid, gpfifo)) => sel4::debug_println!(
                                "NVIDIA_CHANNEL_OK cid={} gpfifo={:#x}",
                                cid,
                                gpfifo
                            ),
                            Err(error) => {
                                sel4::debug_println!("NVIDIA_CHANNEL_FAILED error={:?}", error)
                            }
                        }
                    }
                    Err(error) => sel4::debug_println!("NVIDIA_RM_VRAM_FAILED error={:?}", error),
                }
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
                        return Err(Error::FirmwareStatus(FirmwareStatus(packet.header.function)));
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
