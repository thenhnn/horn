// SPDX-License-Identifier: GPL-2.0-only
//! Single direct-SST connector: correlate RM IDs with the probed DCB connector,
//! acquire SOR, let RM train the PHY, then publish one interlocked modeset.
use super::*;
use gsp_disp::{dp::Link, head, timing::Timing};
use horn_nvidia_gsp::rm::codec::Wire;

const HOTPLUG_EVENT: u32 = 0x007e0000;
const DP_IRQ_EVENT: u32 = 0x007e0001;

/// r535_gsp_device_event_ctor/get and r535_disp_oneinit: scoped RM objects,
/// no host callback pointer or extra DMA. Retained for the display session.
pub(super) fn subscribe<T: FnMut(u32, &[u8]) -> Result<Vec<u8>>>(
    api: &mut rm::Client<T>,
) -> Result<()> {
    for (handle, event) in [
        (HOTPLUG_EVENT, raw::NV2080_NOTIFIERS_HOTPLUG),
        (DP_IRQ_EVENT, raw::NV2080_NOTIFIERS_DP_IRQ),
    ] {
        api.alloc::<api::DisplayEvent>(
            HANDLE_DISP_SUBDEVICE,
            handle,
            &raw::NV0005_ALLOC_PARAMETERS {
                hParentClient: HANDLE_DISP_CLIENT,
                hClass: raw::NV01_EVENT_KERNEL_CALLBACK_EX,
                notifyIndex: raw::NV01_EVENT_CLIENT_RM | event,
                ..Default::default()
            },
        )?;
        let reply = api.control::<api::EventNotification>(
            HANDLE_DISP_SUBDEVICE,
            &raw::NV2080_CTRL_EVENT_SET_NOTIFICATION_PARAMS {
                event,
                action: raw::NV2080_CTRL_EVENT_SET_NOTIFICATION_ACTION_REPEAT,
                ..Default::default()
            },
        )?;
        if reply.event != event
            || reply.action != raw::NV2080_CTRL_EVENT_SET_NOTIFICATION_ACTION_REPEAT
        {
            return Err(Error::Malformed);
        }
        sel4::debug_println!(
            "NVIDIA_DISPLAY_SUBSCRIBED handle={:#x} event={} action=repeat recovery=none",
            handle,
            event
        );
    }
    Ok(())
}

/// r535_gsp_msg_post_event: validate generated header/length, then decode only
/// our registered notifications. A notification never authorizes a new UPDATE.
pub(super) fn notification(packet: &wire::Packet<'_>, phase: &str, clock: u64) -> Result<()> {
    if packet.header.function != wire::NOTIFY {
        return Ok(());
    }
    packet.success()?;
    let size = core::mem::size_of::<raw::rpc_post_event_v17_00>();
    let header =
        raw::rpc_post_event_v17_00::read(packet.payload.get(..size).ok_or(Error::Malformed)?)?;
    let bytes = header.eventDataSize as usize;
    if size.checked_add(bytes) != Some(packet.payload.len()) || header.bNotifyList > 1 {
        return Err(Error::Malformed);
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_NOTIFICATION phase={} clock_us={} client={:#x} event={:#x} index={:#x} data={:#x} info16={:#x} status={:#x} bytes={} list={}",
        phase,
        clock,
        header.hClient,
        header.hEvent,
        header.notifyIndex,
        header.data,
        header.info16,
        header.status,
        bytes,
        header.bNotifyList
    );
    if !matches!(header.hEvent, HOTPLUG_EVENT | DP_IRQ_EVENT) {
        return Ok(()); // Other clients' event objects carry no display authority.
    }
    if header.hClient != HANDLE_DISP_CLIENT {
        return Err(Error::Malformed);
    }
    let start = core::mem::offset_of!(raw::rpc_post_event_v17_00, eventData);
    let end = start.checked_add(bytes).ok_or(Error::Range)?;
    let data = packet.payload.get(start..end).ok_or(Error::Malformed)?;
    if header.hEvent == HOTPLUG_EVENT {
        let event = raw::Nv2080HotplugNotification::read(data)?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_HPD phase={} clock_us={} plug={:#x} unplug={:#x} recovery=none",
            phase,
            clock,
            event.plugDisplayMask,
            event.unplugDisplayMask
        );
    } else {
        let event = raw::Nv2080DpIrqNotification::read(data)?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_DP_IRQ phase={} clock_us={} display_id={:#x} recovery=none",
            phase,
            clock,
            event.displayId
        );
    }
    Ok(())
}

/// Same GET_ACTIVE query as r535_disp_get_active, additionally distinguish
/// boot-established displays from displays established by an NV client.
/// A topology mismatch is an observation, not permission to reroute/retrain.
/// Transport/RM errors and malformed echoes still stop the diagnostic.
pub(super) fn active<T: FnMut(u32, &[u8]) -> Result<Vec<u8>>>(
    api: &mut rm::Client<T>,
    stage: &str,
    heads: u32,
    expected: Option<u32>,
) -> Result<()> {
    if heads == 0 || heads > 8 {
        return Err(Error::Range);
    }
    for head in 0..heads {
        for (scope, flags) in [
            (
                "any",
                raw::NV0073_CTRL_SYSTEM_GET_ACTIVE_FLAGS_CLIENT_DISABLE,
            ),
            (
                "nv-client",
                raw::NV0073_CTRL_SYSTEM_GET_ACTIVE_FLAGS_CLIENT_ENABLE,
            ),
        ] {
            sel4::debug_println!(
                "NVIDIA_DISPLAY_ACTIVE_BEGIN stage={} head={} scope={}",
                stage,
                head,
                scope
            );
            let reply = api.control::<api::DisplayActive>(
                rm::HANDLE_DISP_COMMON,
                &raw::NV0073_CTRL_SYSTEM_GET_ACTIVE_PARAMS {
                    head,
                    flags,
                    ..Default::default()
                },
            )?;
            if reply.subDeviceInstance != 0 || reply.head != head || reply.flags != flags {
                return Err(Error::Malformed);
            }
            sel4::debug_println!(
                "NVIDIA_DISPLAY_ACTIVE stage={} head={} scope={} display_id={:#x} expected={:?} matches={:?}",
                stage,
                head,
                scope,
                reply.displayId,
                expected,
                expected.map(|id| reply.displayId == id)
            );
        }
    }
    Ok(())
}

/// Captured after allocation but before even the first probe/ownership UPDATE.
pub(super) fn baseline(ctx: &mut Ad106<Native>, deadline: u64, plan: &Link) -> Result<()> {
    sel4::debug_println!(
        "NVIDIA_DISPLAY_BASELINE_BEGIN clock_us={} updates_submitted=false",
        ctx.time_us()?
    );
    stream_snapshot(ctx, deadline, "before-first-update")?;
    snapshot(ctx, deadline, plan)?;
    sel4::debug_println!("NVIDIA_DISPLAY_BASELINE_END clock_us={}", ctx.time_us()?);
    Ok(())
}

pub(super) fn route<T: FnMut(u32, &[u8]) -> Result<Vec<u8>>>(
    api: &mut rm::Client<T>,
    supported: u32,
    report: &crate::display::Report,
    t: &Timing,
) -> Result<Link> {
    // No branch/MST, LTTPR, DSC or DP2/UHBR implementation in this first mode.
    if !matches!(report.dpcd[0], 0x11 | 0x12) || report.dpcd[5] & 1 != 0 {
        return Err(Error::Unsupported);
    }
    api.control::<api::ManualDisplayPort>(rm::HANDLE_DISP_COMMON, &Default::default())?;
    let mut selected = None;
    for bit in 0..32 {
        let display_id = 1u32 << bit;
        if supported & display_id == 0 {
            continue;
        }
        let info = api.control::<api::OutputInfo>(
            rm::HANDLE_DISP_COMMON,
            &raw::NV0073_CTRL_SPECIFIC_OR_GET_INFO_PARAMS {
                displayId: display_id,
                ..Default::default()
            },
        )?;
        if info.displayId != display_id || info.subDeviceInstance != 0 {
            return Err(Error::Malformed);
        }
        if info.type_ != raw::NV0073_CTRL_SPECIFIC_OR_TYPE_SOR {
            continue;
        }
        let link = match info.protocol {
            raw::NV0073_CTRL_SPECIFIC_OR_PROTOCOL_SOR_DP_A => 1,
            raw::NV0073_CTRL_SPECIFIC_OR_PROTOCOL_SOR_DP_B => 2,
            _ => continue,
        };
        let connector = api.control::<api::ConnectorData>(
            rm::HANDLE_DISP_COMMON,
            &raw::NV0073_CTRL_SPECIFIC_GET_CONNECTOR_DATA_PARAMS {
                displayId: display_id,
                ..Default::default()
            },
        )?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_RM_OUTPUT id={:#x} link={} connector={} count={} location={}",
            display_id,
            link,
            connector.data[0].index,
            connector.count,
            info.location
        );
        if connector.displayId != display_id
            || connector.subDeviceInstance != 0
            || connector.count == 0
            || connector.count as usize > connector.data.len()
        {
            return Err(Error::Malformed);
        }
        if connector.count != 1 || connector.data[0].index != u32::from(report.connector) {
            continue;
        }
        if info.location != 0 || selected.replace((display_id, link)).is_some() {
            return Err(Error::Unsupported);
        }
    }
    let (display_id, link) = selected.ok_or(Error::Unavailable)?;
    let assigned = api.control::<api::AssignSor>(
        rm::HANDLE_DISP_COMMON,
        &raw::NV0073_CTRL_DFP_ASSIGN_SOR_PARAMS {
            displayId: display_id,
            ..Default::default()
        },
    )?;
    if assigned.displayId != display_id || assigned.subDeviceInstance != 0 {
        return Err(Error::Malformed);
    }
    let mut sor = None;
    for (i, entry) in assigned.sorAssignListWithTag.iter().enumerate() {
        if entry.displayMask & display_id != 0 {
            if entry.displayMask != display_id || sor.replace(i as u32).is_some() {
                return Err(Error::Unsupported);
            }
        }
    }
    let sor = sor.ok_or(Error::Unavailable)?;
    let caps = api.control::<api::DpCaps>(
        rm::HANDLE_DISP_COMMON,
        &raw::NV0073_CTRL_CMD_DP_GET_CAPS_PARAMS {
            sorIndex: sor,
            ..Default::default()
        },
    )?;
    if caps.subDeviceInstance != 0 || caps.sorIndex != sor {
        return Err(Error::Malformed);
    }
    let (sink_rate, mut lanes) = t.sink_link(&report.dpcd)?;
    let mut rate = sink_rate.min(gsp_disp::dp::source_rate(&caps)?);
    if caps.bOverrideLinkBw != 0 {
        if !matches!(report.dp_rate, 6 | 10 | 20 | 30) || !matches!(report.dp_lanes, 1 | 2 | 4) {
            return Err(Error::Unsupported);
        }
        rate = rate.min(report.dp_rate);
        lanes = lanes.min(report.dp_lanes);
    }
    let plan = Link::new(
        t,
        display_id,
        sor,
        link,
        rate,
        lanes,
        report.dpcd[2] & 0x80 != 0,
        caps.bHasIncreasedWatermarkLimits != 0,
    )?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_DP_PLAN id={:#x} connector={} sor={} link={} rate={:#x} lanes={} enhanced={} watermark={} hblank={} vblank={} source_rate_raw={:#x} dcb_rate={:#x} dcb_lanes={} dcb_override={}",
        display_id,
        report.connector,
        sor,
        link,
        rate,
        lanes,
        plan.enhanced,
        plan.watermark,
        plan.hblank,
        plan.vblank,
        caps.maxLinkRate,
        report.dp_rate,
        report.dp_lanes,
        caps.bOverrideLinkBw
    );
    Ok(plan)
}

/// r535_dp_aux_xfer: SDK request size is N-1, reply size is actual N.
/// After RM claims the output, all subsequent AUX traffic goes through RM.
fn aux<T: FnMut(u32, &[u8]) -> Result<Vec<u8>>>(
    api: &mut rm::Client<T>,
    id: u32,
    address: u32,
    read: bool,
    data: &mut [u8],
) -> Result<()> {
    if data.is_empty() || data.len() > 16 || address >= 1 << 20 {
        return Err(Error::Range);
    }
    let mut params = raw::NV0073_CTRL_DP_AUXCH_CTRL_PARAMS {
        displayId: id,
        addr: address,
        size: data.len() as u32 - 1,
        cmd: if read {
            raw::HORN_DP_AUX_READ
        } else {
            raw::HORN_DP_AUX_WRITE
        },
        ..Default::default()
    };
    if !read {
        params.data[..data.len()].copy_from_slice(data);
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_RM_AUX_BEGIN id={:#x} addr={:#x} read={} bytes={}",
        id,
        address,
        read,
        data.len()
    );
    let reply = api.control::<api::DpAux>(rm::HANDLE_DISP_COMMON, &params).map_err(|error| {
        sel4::debug_println!("NVIDIA_DISPLAY_RM_AUX_FAILED id={:#x} addr={:#x} cmd={:#x} request_size={} error={:?} retry=false",
            id, address, params.cmd, params.size, error);
        error
    })?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_RM_AUX_REPLY addr={:#x} reply={:#x} bytes={} retry_ms={}",
        address,
        reply.replyType,
        reply.size,
        reply.retryTimeMs
    );
    if reply.subDeviceInstance != 0
        || reply.displayId != id
        || reply.addr != address
        || reply.bAddrOnly != 0
        || reply.replyType != raw::HORN_DP_AUX_ACK
        || reply.retryTimeMs != 0
        || reply.size as usize != data.len()
    {
        return Err(Error::State);
    }
    if read {
        data.copy_from_slice(&reply.data[..data.len()]);
    }
    Ok(())
}

pub(super) fn train<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    report: &crate::display::Report,
    plan: &Link,
) -> Result<()> {
    let deadline = ctx
        .time_us()?
        .checked_add(5_000_000)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    // Same sink D0 power-up as nouveau_dp_train, through the RM-owned AUX.
    aux(
        &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
        plan.display_id,
        0x600,
        false,
        &mut [1],
    )?;
    let powered = ctx.time_us()?.saturating_add(1000).min(deadline);
    ctx.sleep_until(powered)?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_DP_TRAIN_BEGIN id={:#x} sor={} rate={:#x} lanes={}",
        plan.display_id,
        plan.sor,
        plan.rate,
        plan.lanes
    );
    {
        let mut api = rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline);
        let reply =
            api.control::<api::DpControl>(rm::HANDLE_DISP_COMMON, &plan.training(&report.dpcd))?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_DP_TRAIN_REPLY id={:#x} cmd={:#x} data={:#x} err={:#x} retry_ms={}",
            reply.displayId,
            reply.cmd,
            reply.data,
            reply.err,
            reply.retryTimeMs
        );
        plan.check_training(&reply)?;
    }
    // Include TRAINING_PATTERN_SET: CR/EQ alone can still describe a link
    // left sending training symbols instead of scrambled video.
    let mut config = [0; 7];
    let mut status = [0; 6];
    {
        let mut api = rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline);
        aux(&mut api, plan.display_id, 0x100, true, &mut config)?;
        aux(&mut api, plan.display_id, 0x202, true, &mut status)?;
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_DP_LINK_STATUS config={:02x?} status={:02x?}",
        config,
        status
    );
    if config[0] != plan.rate
        || config[1] & 31 != plan.lanes
        || (config[1] & 0x80 != 0) != plan.enhanced
    {
        return Err(Error::State);
    }
    plan.check_status(&status)?;
    if config[2] & 0x27 != 0 {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_DP_NOT_VIDEO training_pattern={:#x}",
            config[2]
        );
        return Err(Error::State);
    }
    let params = plan.stream();
    let reply = rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline)
        .control::<api::DpStream>(rm::HANDLE_DISP_COMMON, &params)?;
    if reply.head != 0
        || reply.sorIndex != plan.sor
        || reply.dpLink != params.dpLink
        || reply.bMST != 0
        || reply.bEnableOverride != 1
        || reply.SST.waterMark != plan.watermark
    {
        return Err(Error::State);
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_DP_TRAINED id={:#x} sor={} rate={:#x} lanes={} tu=64 watermark={}",
        plan.display_id,
        plan.sor,
        plan.rate,
        plan.lanes,
        plan.watermark
    );
    Ok(())
}

pub(super) fn luts<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<scanout::Surface> {
    let mut params = rm::vram_request(HANDLE_DISP_CLIENT, 0x10000, 0x10000)?;
    params.attr = raw::HORN_SURFACE_ATTR;
    let reply = display_memory(
        &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
        0x0043_0000,
        &params,
    )?;
    gsp_disp::surface::validate_memory(params.size, &reply)?;
    let region = gsp_disp::pramin::Region::new(reply.offset, reply.size)?;
    let mut words = head::identity_luts()?;
    words
        .try_reserve_exact(0x10000 / 4 - words.len())
        .map_err(|_| Error::Exhausted)?;
    words.resize(0x10000 / 4, 0);
    for offset in (0..reply.size).step_by(0x10000) {
        ctx.with_platform(|p| region.write(p, offset, &words, deadline))?;
        if offset == 0 {
            words.fill(0);
        }
        display_event(ctx, queue, libos, version, deadline)?;
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_LUT_READY vram={:#x} bytes={} ilut=fp16 olut=u0.16 entries=1024 readback=verified",
        reply.offset,
        reply.size
    );
    Ok(scanout::Surface {
        base: reply.offset,
        bytes: reply.size as usize,
    })
}

/// Passive head-0 formatter/raster diagnostics. Register selectors and fields:
/// NVIDIA/open-gpu-doc@9fdf5c4062007929d9f4e6cbad9c9771fe61b880,
/// manuals/ampere/ga102/dev_display_withoffset.ref.txt (also used by nouveau's
/// gv100/ga102 display path). AD106 interpretation remains diagnostic, not an
/// assertion that the sink receives these values. No read-clear registers;
/// in particular RG_UNDERFLOW and SF_DP_IRQ are cleared by writes, never here.
/// Do not use GET_RG_UNDERFLOW_PROP: that RM query clears underflow history.
fn stream_snapshot(ctx: &mut Ad106<Native>, deadline: u64, stage: &str) -> Result<()> {
    if ctx.time_us()? >= deadline {
        return Err(Error::Timeout);
    }
    let clock = ctx.time_us()?;
    let registers = [
        ("rg-underflow", 0x61630c),
        ("rg-underflow-pixel", 0x616310),
        ("rg-status", 0x616314),
        ("rg-loadv", 0x616320),
        ("postcomp-loadv", 0x61611c),
        ("sf-test", 0x61650c),
        ("sf-linkctl", 0x616540),
        ("sf-mn-config", 0x61654c),
        ("sf-dp-irq", 0x616664),
        ("supervisor-head", 0x6107ac),
        ("core-status", 0x610630),
        ("window0-status", 0x610664),
    ];
    let mut values = [0u32; 12];
    ctx.with_platform(|p| {
        for (value, &(_, addr)) in values.iter_mut().zip(&registers) {
            *value = p.read32(addr)?;
        }
        Ok(())
    })?;
    for ((name, addr), value) in registers.into_iter().zip(values) {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_STREAM stage={} clock_us={} field={} addr={:#x} value={:#x}",
            stage,
            clock,
            name,
            addr,
            value
        );
    }
    // SF_DP_MN contains N and M adjustment controls, NOT measured Mvid.
    // MSA*_DEBUG_MAUD/NAUD are audio and deliberately not sampled as video M/N.
    sel4::debug_println!(
        "NVIDIA_DISPLAY_STREAM_DECODE stage={} uflow={} loadv_uflow={} uflow_frames={} rg_stalled={} rg_opmode={} sf_opmode={} sf_owner={:#x} sf_enable={} tu={} sync={} enhanced={} lanes_mask={:#x} n_config={} m_delta={} m_mod={} video_msa_observed=false",
        stage,
        (values[0] >> 4) & 1,
        (values[0] >> 5) & 1,
        (values[0] >> 16) & 0xff,
        (values[2] >> 3) & 1,
        (values[2] >> 14) & 3,
        (values[5] >> 8) & 3,
        (values[5] >> 10) & 0xf,
        values[6] & 1,
        (values[6] >> 2) & 0x7f,
        (values[6] >> 10) & 1,
        (values[6] >> 14) & 1,
        (values[6] >> 16) & 0xff,
        values[7] & 0xffffff,
        (values[7] >> 24) & 0xf,
        (values[7] >> 30) & 3
    );
    Ok(())
}

/// Read-only method shadows, following nouveau gv100_disp_{core,wndw}_mthd.
/// These are latched method values, not proof that the fetch/color path works.
/// Never write shadows, clear errors, re-submit UPDATE or reuse a notifier here.
fn snapshot(ctx: &mut Ad106<Native>, deadline: u64, plan: &Link) -> Result<()> {
    let head = [
        ("display-id", raw::HORN_HEAD_DISPLAY_ID),
        ("raster", raw::HORN_HEAD_RASTER_SIZE),
        ("sync-end", raw::HORN_HEAD_RASTER_SYNC_END),
        ("blank-end", raw::HORN_HEAD_RASTER_BLANK_END),
        ("blank-start", raw::HORN_HEAD_RASTER_BLANK_START),
        ("control", raw::HORN_HEAD_CONTROL),
        ("clock", raw::HORN_HEAD_PIXEL_CLOCK_FREQUENCY),
        ("view-point", raw::HORN_HEAD_VIEWPORT_POINT_IN),
        ("view-in", raw::HORN_HEAD_VIEWPORT_SIZE_IN),
        ("view-out", raw::HORN_HEAD_VIEWPORT_SIZE_OUT),
        ("view-adjust", raw::HORN_HEAD_VIEWPORT_POINT_OUT_ADJUST),
        ("scaler", raw::HORN_HEAD_OUTPUT_SCALER),
        ("output", raw::HORN_HEAD_CONTROL_OUTPUT_RESOURCE),
        ("sor0", raw::HORN_SOR_CONTROL),
        ("owner0", raw::HORN_WINDOW_CONTROL),
        ("olut", raw::HORN_HEAD_OLUT_CONTROL),
        ("olut-scale", raw::HORN_HEAD_OLUT_FP_NORM_SCALE),
        ("olut-dma", raw::HORN_HEAD_CONTEXT_DMA_OLUT),
        ("olut-offset", raw::HORN_HEAD_OFFSET_OLUT),
        ("ocsc0", raw::HORN_HEAD_OCSC0CONTROL),
        ("ocsc1", raw::HORN_HEAD_OCSC1CONTROL),
    ];
    let window = [
        ("size", raw::NVC57E_SET_SIZE),
        ("params", raw::NVC57E_SET_PARAMS),
        ("pitch", raw::HORN_WINDOW_PLANAR_STORAGE),
        ("image-dma", raw::HORN_WINDOW_ISO),
        ("image-offset", raw::HORN_WINDOW_OFFSET),
        ("point-in", raw::HORN_WINDOW_POINT_IN),
        ("size-in", raw::NVC57E_SET_SIZE_IN),
        ("size-out", raw::NVC57E_SET_SIZE_OUT),
        ("blend", raw::NVC57E_SET_COMPOSITION_CONTROL),
        ("alpha", raw::NVC57E_SET_COMPOSITION_CONSTANT_ALPHA),
        ("factors", raw::NVC57E_SET_COMPOSITION_FACTOR_SELECT),
        ("csc-c00", raw::NVC57E_SET_FMT_COEFFICIENT_C00),
        ("ilut", raw::NVC57E_SET_ILUT_CONTROL),
        ("ilut-dma", raw::NVC57E_SET_CONTEXT_DMA_ILUT),
        ("ilut-offset", raw::NVC57E_SET_OFFSET_ILUT),
    ];
    for (channel, assembly, armed, methods) in [
        (
            "core",
            gsp_disp::CORE_USER,
            raw::HORN_CORE_ARMED,
            head.as_slice(),
        ),
        (
            "window0",
            raw::HORN_WINDOW_USER,
            raw::HORN_WINDOW_ARMED,
            window.as_slice(),
        ),
    ] {
        for &(name, method) in methods {
            if ctx.time_us()? >= deadline {
                return Err(Error::Timeout);
            }
            let (assy, arm) = ctx
                .with_platform(|p| Ok((p.read32(assembly + method)?, p.read32(armed + method)?)))?;
            sel4::debug_println!(
                "NVIDIA_DISPLAY_STATE channel={} field={} method={:#x} assy={:#x} armed={:#x}",
                channel,
                name,
                method,
                assy,
                arm
            );
        }
    }
    // Read-only source state from nouveau's gv100_sor_dp_watermark/audio_sym,
    // gf119_sor_dp_links/clock and gm107_sor_dp_pattern. Never program the PHY
    // behind RM's back. These raw words do not by themselves prove valid MSA.
    let sor = plan.sor * 0x800;
    let link = sor + if plan.link == 2 { 0x80 } else { 0 };
    for (name, addr) in [
        ("watermark", 0x616550),
        ("hblank", 0x616568),
        ("vblank", 0x61656c),
        ("clock", 0x612300 + sor),
        ("link-control", 0x61c10c + link),
        (
            "pattern",
            0x61c110 + sor + if plan.link == 2 { 0x1c } else { 0 },
        ),
    ] {
        if ctx.time_us()? >= deadline {
            return Err(Error::Timeout);
        }
        let value = ctx.with_platform(|p| p.read32(addr))?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_DP_SOURCE field={} addr={:#x} value={:#x}",
            name,
            addr,
            value
        );
    }
    // Published DPCA contains a frame/line counter. Two samples separated by
    // >2 preferred-mode frames distinguish a running raster from a static shadow.
    let first = ctx.with_platform(|p| p.read32(raw::HORN_HEAD_RASTER_COUNTER))?;
    let wake = ctx.time_us()?.checked_add(25_000).ok_or(Error::Range)?;
    if wake >= deadline {
        return Err(Error::Timeout);
    }
    ctx.sleep_until(wake)?;
    let second = ctx.with_platform(|p| p.read32(raw::HORN_HEAD_RASTER_COUNTER))?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_RASTER head=0 first={:#x} second={:#x} interval_us=25000 visible=operator-unconfirmed",
        first,
        second
    );
    Ok(())
}

pub(super) fn commit<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    notifier: &DmaBlock,
    core: &gsp_disp::commands::Core,
    window: &scanout::Window,
    plan: &Link,
) -> Result<()> {
    let deadline = ctx
        .time_us()?
        .checked_add(2_000_000)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    let targets = [
        (
            raw::HORN_WINDOW_USER,
            window.assembly_put,
            window.program.put(),
        ),
        (gsp_disp::CORE_USER, core.ownership_put, core.push.put()),
    ];
    let submit_us = ctx.time_us()?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_COMMIT_BEGIN clock_us={} deadline_us={}",
        submit_us,
        deadline
    );
    // Check both old prefixes *before* either UPDATE. Never wait for an
    // interlocked window to finish before publishing the matching core UPDATE.
    ctx.with_platform(|p| {
        for &(base, old, _) in &targets {
            if p.read32(base + gsp_disp::PUT)? != gsp_disp::put_word(old)?
                || p.read32(base + gsp_disp::GET)? != gsp_disp::put_word(old)?
            {
                return Err(Error::State);
            }
        }
        Ok(())
    })?;
    stream_snapshot(ctx, deadline, "before-update")?;
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    ctx.with_platform(|p| {
        for &(base, _, target) in &targets {
            p.write32(base + gsp_disp::PUT, gsp_disp::put_word(target)?)?;
        }
        Ok(())
    })?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_MODESET_SUBMIT head=0 window=0 core_put={} window_put={} core_slot={} window_slot={}",
        core.push.put(),
        window.program.put(),
        head::MODE_NOTIFIER,
        head::WINDOW_NOTIFIER
    );
    loop {
        display_event(ctx, queue, libos, version, deadline)?;
        let fetched = ctx.with_platform(|p| {
            let mut complete = true;
            for &(base, old, target) in &targets {
                let put = p.read32(base + gsp_disp::PUT)?;
                let raw_get = p.read32(base + gsp_disp::GET)?;
                let get = gsp_disp::get_dwords(raw_get);
                if put != gsp_disp::put_word(target)?
                    || raw_get == u32::MAX
                    || get < old
                    || get > target
                {
                    return Err(Error::State);
                }
                complete &= get == target;
            }
            Ok(complete)
        })?;
        let core_status = unsafe { notifier.read32(head::MODE_NOTIFIER)? };
        let window_status = unsafe { notifier.read32(head::WINDOW_NOTIFIER)? };
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let core_done = gsp_disp::notifier_finished(core_status)?;
        let _ = gsp_disp::notifier_finished(window_status)?; // reject invalid status
        let window_state =
            (window_status >> raw::HORN_NOTIFIER_STATUS_SHIFT) & raw::HORN_NOTIFIER_STATUS_MASK;
        if fetched && core_done && window_state != raw::NV_DISP_NOTIFIER__0_STATUS_NOT_BEGUN {
            // A displayed window can stay BEGUN until a later flip retires it.
            sel4::debug_println!(
                "NVIDIA_DISPLAY_MODESET_COMPLETE head=0 window=0 core_status={:#x} window_status={:#x} visible=operator-unconfirmed",
                core_status,
                window_status
            );
            sel4::debug_println!(
                "NVIDIA_DISPLAY_COMMIT_DONE clock_us={} elapsed_us={}",
                ctx.time_us()?,
                ctx.time_us()?.saturating_sub(submit_us)
            );
            stream_snapshot(ctx, deadline, "after-commit")?;
            snapshot(ctx, deadline, plan)?;
            // Separate observation deadline; do not extend the 2s commit timeout.
            // No post-commit AUX: its reproduced RM error used to stop event
            // service before the monitor had time to finish acquiring the mode.
            let origin = ctx.time_us()?;
            let observe_deadline = origin
                .checked_add(5_000_000)
                .ok_or(Error::Range)?
                .min(boot_deadline);
            for (stage, delay) in [
                ("settled-0ms", 0),
                ("settled-500ms", 500_000),
                ("settled-3000ms", 3_000_000),
            ] {
                let wake = origin.checked_add(delay).ok_or(Error::Range)?;
                if wake >= observe_deadline {
                    return Err(Error::Timeout);
                }
                loop {
                    display_event(ctx, queue, libos, version, observe_deadline)?;
                    let now = ctx.time_us()?;
                    if now >= wake {
                        break;
                    }
                    ctx.sleep_until(now.saturating_add(10_000).min(wake))?;
                }
                stream_snapshot(ctx, observe_deadline, stage)?;
                let registers = ctx.with_platform(|p| {
                    Ok([
                        p.read32(gsp_disp::CORE_USER + gsp_disp::PUT)?,
                        p.read32(gsp_disp::CORE_USER + gsp_disp::GET)?,
                        p.read32(raw::HORN_WINDOW_USER + gsp_disp::PUT)?,
                        p.read32(raw::HORN_WINDOW_USER + gsp_disp::GET)?,
                        p.read32(raw::HORN_HEAD_RASTER_COUNTER)?,
                    ])
                })?;
                sel4::debug_println!(
                    "NVIDIA_DISPLAY_OBSERVE stage={} clock_us={} elapsed_us={} registers={:08x?} core_status={:#x} window_status={:#x}",
                    stage,
                    ctx.time_us()?,
                    ctx.time_us()?.saturating_sub(origin),
                    registers,
                    unsafe { notifier.read32(head::MODE_NOTIFIER)? },
                    unsafe { notifier.read32(head::WINDOW_NOTIFIER)? }
                );
                active(
                    &mut rm_api(
                        ctx,
                        queue,
                        HANDLE_DISP_CLIENT,
                        libos,
                        version,
                        observe_deadline,
                    ),
                    stage,
                    1,
                    Some(plan.display_id),
                )?;
            }
            return Ok(());
        }
        let now = ctx.time_us()?;
        if now >= deadline {
            sel4::debug_println!(
                "NVIDIA_DISPLAY_MODESET_TIMEOUT fetched={} core_status={:#x} window_status={:#x}",
                fetched,
                core_status,
                window_status
            );
            return Err(Error::Timeout);
        }
        ctx.sleep_until(now.saturating_add(1000).min(deadline))?;
    }
}
