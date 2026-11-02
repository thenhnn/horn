// SPDX-License-Identifier: GPL-2.0-only
//! Periodic unsynchronized writes to the currently scanned image, in service.
//! No new UPDATE, flip, CRC capture, ring reuse, GPU writer or allocation release.
use super::super::modeset;
use super::*;

const WIDTH: usize = 512;
const HEIGHT: usize = 256;
const BORDER: usize = 4;
const PERIOD_US: u64 = 1_000_000;
const COLORS: [(u32, &str); 4] = [
    (0xffff_00ff, "magenta"),
    (0xff00_ffff, "cyan"),
    (0xffff_ff00, "yellow"),
    (0xff00_ff00, "green"),
];

pub(in crate::boot) struct Live {
    region: VramRegion,
    timing: Timing,
    x: usize,
    y: usize,
    line: Vec<u32>,
    next: u64,
    changes: u64,
}

/// Called only after commit's successful one-shot CRC detach/DONE.
pub(in crate::boot) fn start(
    ctx: &mut Ad106<Native>,
    timing: Timing,
    surface: &Surface,
) -> Result<Live> {
    if timing.bytes > surface.bytes {
        return Err(Error::State);
    }
    let x = usize::from(timing.width)
        .checked_sub(WIDTH)
        .ok_or(Error::Range)?
        / 2;
    let y = usize::from(timing.height)
        .checked_sub(HEIGHT)
        .ok_or(Error::Range)?
        / 2;
    let end = (y + HEIGHT - 1)
        .checked_mul(timing.pitch as usize)
        .and_then(|v| v.checked_add((x + WIDTH) * 4))
        .ok_or(Error::Range)?;
    if end > timing.bytes {
        return Err(Error::Range);
    }
    let region = VramRegion {
        base: surface.base,
        bytes: surface.bytes as u64,
    };
    let mut line = Vec::new();
    line.try_reserve_exact(WIDTH)
        .map_err(|_| Error::Exhausted)?;
    line.resize(WIDTH, 0u32);
    modeset::activation_ready(ctx)?;
    ctx.with_platform(|p| {
        if p.display_wake.live_patch_started || !p.display_wake.publication_started {
            return Err(Error::State);
        }
        p.display_wake.live_patch_started = true;
        Ok(())
    })?;
    let now = ctx.time_us()?;
    let next = now.checked_add(2_000_000).ok_or(Error::Range)?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_LIVE_ANIMATION_READY clock_us={} first_us={} period_us={} frame_limit=none crc=already-detached new_update=false",
        now,
        next,
        PERIOD_US
    );
    Ok(Live {
        region,
        timing,
        x,
        y,
        line,
        next,
        changes: 0,
    })
}

impl Live {
    pub(in crate::boot) fn next(&self) -> u64 {
        self.next
    }

    /// A finite write transaction per service tick, not an infinite boot loop.
    /// Failures stop the session; a partial frame is never retried. Ordinary
    /// successful color changes are intentionally repeated without a count cap.
    pub(in crate::boot) fn tick<M: Memory>(
        &mut self,
        ctx: &mut Ad106<Native>,
        queue: &mut Queue<M>,
        libos: Span,
        version: u32,
    ) -> Result<()> {
        let start = ctx.time_us()?;
        if start < self.next {
            return Ok(());
        }
        let deadline = start.checked_add(2_000_000).ok_or(Error::Range)?;
        let (color, name) = COLORS[(self.changes % COLORS.len() as u64) as usize];
        sel4::debug_println!(
            "NVIDIA_DISPLAY_LIVE_PATCH_BEGIN clock_us={} change={} x={} y={} width={} height={} fill={} border=white path=bar1-uc new_update=false vsync=false tearing_allowed=true",
            start,
            self.changes,
            self.x,
            self.y,
            WIDTH,
            HEIGHT,
            name
        );
        for row in 0..HEIGHT {
            display_event(ctx, queue, libos, version, deadline)?;
            modeset::activation_ready(ctx)?;
            self.line.fill(if row < BORDER || row >= HEIGHT - BORDER {
                0xffff_ffff
            } else {
                color
            });
            self.line[..BORDER].fill(0xffff_ffff);
            self.line[WIDTH - BORDER..].fill(0xffff_ffff);
            let offset = (self.y + row)
                .checked_mul(self.timing.pitch as usize)
                .and_then(|v| v.checked_add(self.x * 4))
                .ok_or(Error::Range)?;
            // Scanout only reads this retained image; no CE/render writer is
            // submitted. Ordered BAR1 stores drain once per batch, not per pixel.
            // This is not a display-cache visibility or visible-pixel proof.
            ctx.with_platform(|p| self.region.write(p, offset as u64, &self.line, deadline))?;
        }
        display_event(ctx, queue, libos, version, deadline)?;
        modeset::activation_ready(ctx)?;
        let now = ctx.time_us()?;
        if now >= deadline {
            return Err(Error::Timeout);
        }
        sel4::debug_println!(
            "NVIDIA_DISPLAY_LIVE_PATCH_DONE clock_us={} change={} fill={} elapsed_us={} bytes={} readback=batch-tail-only new_update=false visible=operator-unconfirmed backing=retained",
            now,
            self.changes,
            name,
            now.saturating_sub(start),
            WIDTH * HEIGHT * 4
        );
        self.changes = self.changes.checked_add(1).ok_or(Error::Range)?;
        // No catch-up burst after a delayed tick: schedule from actual start,
        // with a small minimum wait if this write took more than one period.
        self.next = start
            .checked_add(PERIOD_US)
            .ok_or(Error::Range)?
            .max(now.checked_add(10_000).ok_or(Error::Range)?);
        Ok(())
    }
}
