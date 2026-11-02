// SPDX-License-Identifier: GPL-2.0-only
//! Single pre-scanout RAM -> VRAM -> RAM transaction. Fresh immutable ring and
//! pushbuffer, virtual semaphore, opaque RM token, real completion then verify.
//! No retry, post-publication page-table edit, ring reuse or uncertain cleanup.
use super::*;
use crate::native::VramRegion;
const BYTES: usize = 65536;
const FENCE: usize = 2 * BYTES;
const PUSH: usize = FENCE + 4096;
const RING_SLOT: u64 = 0x8000;
const PUSH_SLOT: u64 = 0x9000;
const FENCE_SLOT: u64 = 0xa000;

pub(crate) struct Retained {
    _tables: DmaBlock,
    _ring: DmaBlock,
    _userd: DmaBlock,
    _methods: DmaBlock,
    _transfer: DmaBlock,
}
fn pattern(index: usize) -> u32 {
    0x9e37_79b9 ^ index as u32
}

pub(super) fn roundtrip<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    image: VramRegion,
    libos: Span,
    version: u32,
    boot_deadline: u64,
) -> Result<()> {
    if image.bytes < BYTES as u64 {
        return Err(Error::Range);
    }
    ctx.with_platform(|p| {
        if p.ce_started || p.display_wake.publication_started {
            return Err(Error::State);
        }
        p.ce_started = true;
        Ok(())
    })?;
    let deadline = ctx
        .time_us()?
        .checked_add(10_000_000)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    let engines = rm_engines(ctx, queue, rm, libos, version, deadline)?;
    if !engines
        .iter()
        .any(|e| e.rm_type == rm::RM_ENGINE_TYPE_COPY0)
    {
        return Err(Error::Unavailable);
    }
    let (vram, _limit) = rm_vram(ctx, queue, rm, libos, version, deadline)?;
    let size = VRAM_BYTES; // rm_vram returns an inclusive limit, not a length
    let instance = ctx.with_platform(|p| p.map_vram(vram, size, deadline))?;
    let zero = vec![0; size as usize / 4];
    ctx.with_platform(|p| instance.write(p, 0, &zero, deadline))?;
    let mut va = gmmu_setup(ctx, queue, rm, libos, version, deadline)?;
    let mut ring = ctx.with_platform(|p| p.allocate(4096, DeviceAccess::Read))?;
    let mut userd = ctx.with_platform(|p| p.allocate(4096, DeviceAccess::ReadWrite))?;
    let method_size = usize::try_from(rm_mthdbuf_size(ctx, queue, rm, libos, version, deadline)?)
        .map_err(|_| Error::Range)?;
    let mut methods = ctx.with_platform(|p| p.allocate(method_size, DeviceAccess::ReadWrite))?;
    let mut transfer = ctx.with_platform(|p| p.allocate(PUSH + 4096, DeviceAccess::ReadWrite))?;
    unsafe {
        ring.zero()?;
        userd.zero()?;
        methods.zero()?;
        transfer.zero()?;
        for index in 0..BYTES / 4 {
            transfer.write32(index * 4, pattern(index))?;
        }
    }
    for page in 0..size / 4096 {
        va.map_vid(0x1000 + page * 4096, vram + page * 4096)?;
    }
    va.map_sys(RING_SLOT, ring.iova)?;
    va.map_sys(PUSH_SLOT, transfer.iova + PUSH as u64)?;
    va.map_sys(FENCE_SLOT, transfer.iova + FENCE as u64)?;
    let mut words = Vec::new();
    words.try_reserve_exact(82).map_err(|_| Error::Exhausted)?;
    // SET_OBJECT binds subchannel0 to the allocated engine's class.
    words.push(gsp_ce::header(raw::HORN_CE_SET_OBJECT, 1)?);
    words.push(raw::AMPERE_DMA_COPY_B);
    gsp_ce::encode_transfer(
        &gsp_ce::Copy {
            source: transfer.iova,
            destination: image.base,
            bytes: BYTES as u32,
            fence: VA_BASE + FENCE_SLOT,
        },
        gsp_ce::Target::System,
        gsp_ce::Target::Vram,
        false,
        &mut words,
    )?;
    gsp_ce::encode_transfer(
        &gsp_ce::Copy {
            source: image.base,
            destination: transfer.iova + BYTES as u64,
            bytes: BYTES as u32,
            fence: VA_BASE + FENCE_SLOT,
        },
        gsp_ce::Target::Vram,
        gsp_ce::Target::System,
        true,
        &mut words,
    )?;
    if words.len() * 4 > 4096 {
        return Err(Error::Range);
    }
    unsafe {
        for (i, word) in words.iter().enumerate() {
            transfer.write32(PUSH + i * 4, *word)?;
        }
        ring.initialize(
            0,
            &gsp_ce::gpfifo_entry(VA_BASE + PUSH_SLOT, words.len() as u32)?.to_le_bytes(),
        )?;
        va.arena.initialize(0, &va.bytes)?;
    }
    // Everything, including both launches and the fence PTE, is complete before
    // any RM/channel publication. Latch even if that publication returns error.
    va.published = true;
    ctx.with_platform(|p| {
        p.dma_write_barrier();
        Ok(())
    })?;
    let desc = |base, size, address_space, cache| raw::NV_MEMORY_DESC_PARAMS {
        base,
        size,
        addressSpace: address_space,
        cacheAttrib: cache,
    };
    let slot = 0x200 * rm::CHANNEL_CID as usize;
    let args = raw::NV_CHANNEL_ALLOC_PARAMS {
        gpFifoOffset: VA_BASE + RING_SLOT,
        gpFifoEntries: 512,
        flags: rm::CHAN_FLAG_PRIVILEGED
            | rm::CHAN_FLAG_USERD_PAGE_FIXED
            | ((rm::CHANNEL_CID & raw::HORN_CHANNEL_USERD_MASK) << raw::HORN_CHANNEL_USERD_SHIFT),
        hVASpace: rm::HANDLE_VASPACE,
        engineType: rm::ENGINE_COPY0,
        cid: rm::CHANNEL_CID,
        subDeviceId: 1,
        internalFlags: rm::CHAN_INTERNAL_ADMIN,
        instanceMem: desc(vram, 4096, 2, 1),
        userdMem: desc(userd.iova + slot as u64, 0x200, 1, 0),
        ramfcMem: desc(vram, 0x200, 2, 1),
        mthdbufMem: desc(methods.iova, method_size as u64, 1, 0),
        ..Default::default()
    };
    let token;
    {
        let mut api = rm_api(ctx, queue, rm.client, libos, version, deadline);
        let pd = api.control::<api::PageDirectory>(
            rm.device,
            &raw::NV0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS {
                physAddress: va.root,
                numEntries: gmmu::entries(0)? as u32,
                flags: rm::SET_PD_APERTURE_SYSMEM_COH,
                hVASpace: rm::HANDLE_VASPACE,
                ..Default::default()
            },
        )?;
        if pd.physAddress != va.root
            || pd.numEntries != gmmu::entries(0)? as u32
            || pd.flags != rm::SET_PD_APERTURE_SYSMEM_COH
            || pd.hVASpace != rm::HANDLE_VASPACE
        {
            return Err(Error::State);
        }
        let channel = api.alloc::<api::Channel>(rm.device, rm::HANDLE_CHANNEL, &args)?;
        let same = |a: &raw::NV_MEMORY_DESC_PARAMS, b: &raw::NV_MEMORY_DESC_PARAMS| {
            a.base == b.base
                && a.size == b.size
                && a.addressSpace == b.addressSpace
                && a.cacheAttrib == b.cacheAttrib
        };
        if channel.gpFifoOffset != args.gpFifoOffset
            || channel.gpFifoEntries != args.gpFifoEntries
            || channel.hVASpace != args.hVASpace
            || channel.engineType != args.engineType
            || !same(&channel.userdMem, &args.userdMem)
            || !same(&channel.instanceMem, &args.instanceMem)
            || !same(&channel.ramfcMem, &args.ramfcMem)
            || !same(&channel.mthdbufMem, &args.mthdbufMem)
        {
            return Err(Error::State);
        }
        let engine = api.alloc::<api::CopyEngine>(
            rm::HANDLE_CHANNEL,
            rm::HANDLE_CE,
            &raw::NVC0B5_ALLOCATION_PARAMETERS {
                version: raw::NVC0B5_ALLOCATION_PARAMETERS_VERSION_1,
                engineType: rm::ENGINE_COPY0,
            },
        )?;
        if engine.version != raw::NVC0B5_ALLOCATION_PARAMETERS_VERSION_1
            || engine.engineType != rm::ENGINE_COPY0
        {
            return Err(Error::State);
        }
        let bound = api.control::<api::Bind>(
            rm::HANDLE_CHANNEL,
            &raw::NVA06F_CTRL_BIND_PARAMS {
                engineType: rm::ENGINE_COPY0,
            },
        )?;
        if bound.engineType != rm::ENGINE_COPY0 {
            return Err(Error::State);
        }
        token = api
            .control::<api::WorkSubmitToken>(rm::HANDLE_CHANNEL, &Default::default())?
            .workSubmitToken;
        if token == u32::MAX {
            return Err(Error::State);
        }
        let scheduled = api.control::<api::Schedule>(
            rm::HANDLE_CHANNEL,
            &raw::NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS {
                bEnable: 1,
                bSkipSubmit: 0,
            },
        )?;
        if scheduled.bEnable != 1 || scheduled.bSkipSubmit != 0 {
            return Err(Error::State);
        }
    }
    sel4::debug_println!(
        "NVIDIA_CE_ROUNDTRIP_BEGIN bytes={} vram={:#x} push_va={:#x} fence_va={:#x} token={:#x} table=sealed ring_reuse=false",
        BYTES,
        image.base,
        VA_BASE + PUSH_SLOT,
        VA_BASE + FENCE_SLOT,
        token
    );
    ctx.with_platform(|p| {
        p.dma_write_barrier();
        Ok(())
    })?;
    unsafe {
        userd.write32(slot + raw::HORN_CE_GP_PUT as usize, 1)?;
    }
    ctx.with_platform(|p| {
        p.dma_write_barrier();
        p.write32(0xbb0090, token)
    })?;
    let started = ctx.time_us()?;
    let fence_deadline = started
        .checked_add(2_000_000)
        .ok_or(Error::Range)?
        .min(deadline);
    loop {
        let now = ctx.time_us()?;
        if now >= fence_deadline {
            return Err(Error::Timeout);
        }
        display_event(ctx, queue, libos, version, fence_deadline)?;
        let value = unsafe { transfer.read32(FENCE)? };
        if value == gsp_ce::FENCE_PAYLOAD {
            break;
        }
        if value != 0 {
            return Err(Error::State);
        }
        ctx.sleep_until(now.saturating_add(1000).min(fence_deadline))?;
    }
    // GPU-written coherent bytes use volatile loads, never shared Rust slices.
    for index in 0..BYTES / 4 {
        if index % 1024 == 0 && ctx.time_us()? >= deadline {
            return Err(Error::Timeout);
        }
        if unsafe { transfer.read32(BYTES + index * 4)? } != pattern(index) {
            return Err(Error::Checksum);
        }
    }
    // Diagnostic only, once before scanout: also validate the CPU BAR1 view of
    // every word. No such full readback is present in the upload hot path.
    let mut observed = vec![0; BYTES / 4];
    ctx.with_platform(|p| image.read(p, 0, &mut observed, deadline))?;
    if observed
        .iter()
        .enumerate()
        .any(|(i, value)| *value != pattern(i))
    {
        return Err(Error::Checksum);
    }
    if ctx.time_us()? >= deadline {
        return Err(Error::Timeout);
    }
    let get = unsafe { userd.read32(slot + raw::HORN_CE_GP_GET as usize)? };
    if get > 1 {
        return Err(Error::State);
    } // GET isn't completion or retirement.
    sel4::debug_println!(
        "NVIDIA_CE_ROUNDTRIP_DONE bytes={} elapsed_us={} gp_get={} fence=completed ram=verified bar1=verified visible=not-applicable backing=retained",
        BYTES,
        ctx.time_us()?.saturating_sub(started),
        get
    );
    ctx.with_platform(|p| {
        p.ce_backing = Some(Retained {
            _tables: va.arena,
            _ring: ring,
            _userd: userd,
            _methods: methods,
            _transfer: transfer,
        });
        Ok(())
    })
}
