// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// Adapted from NOVA 3497a403, firmware/{gsp,riscv,booter}.rs.
use crate::{Error, Result, tlv::Tlv};
pub const VERSION: &str = "570.144";
pub const GSP_BYTES: usize = 63_541_248;
const BUILD_ID: [u8; 20] = [
    0x1e, 0x56, 0x54, 0xb5, 0x16, 0x60, 0xfd, 0x9c, 0x46, 0x21, 0x33, 0xe4, 0x63, 0x81, 0xaf, 0x21,
    0x83, 0xdd, 0x43, 0x71,
];

fn extent(blob: &[u8], start: u32, length: u32) -> Result<()> {
    if length == 0
        || start as usize % 256 != 0
        || length as usize % 256 != 0
        || (start as usize)
            .checked_add(length as usize)
            .is_none_or(|end| end > blob.len())
    {
        return Err(Error::Range);
    }
    Ok(())
}
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub source: u32,
    pub destination: u32,
    pub length: u32,
}
pub struct Bootloader<'a> {
    pub blob: &'a [u8],
    pub code: u32,
    pub data: u32,
    pub manifest: u32,
    pub app_version: u32,
}
impl<'a> Bootloader<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let tlv = Tlv::new(data)?;
        let blob = tlv.bytes(b"BLOB")?;
        let code = tlv.u32(b"CDOF")?;
        let data = tlv.u32(b"DAOF")?;
        let manifest = tlv.u32(b"MFOF")?;
        // Hash pins authenticate distributed bytes; only the GPU verifies their
        // NVIDIA signatures. These are structural checks, not authentication.
        if blob.len() != 36864
            || [code, data, manifest]
                .iter()
                .any(|o| *o as usize >= blob.len() || o & 255 != 0)
        {
            return Err(Error::Range);
        }
        Ok(Self {
            blob,
            code,
            data,
            manifest,
            app_version: tlv.u32(b"APPV")?,
        })
    }
}
pub struct Booter<'a> {
    tlv: Tlv<'a>,
    pub blob: &'a [u8],
    pub imem: Segment,
    pub dmem: Segment,
    pub signature_patch: u32,
    pub pkc_data_offset: u32,
    pub engine_mask: u16,
    pub ucode_id: u8,
}
impl<'a> Booter<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let tlv = Tlv::new(data)?;
        let blob = tlv.bytes(b"BLOB")?;
        let imem = Segment {
            source: tlv.u32(b"A0CO")?,
            destination: 0,
            length: tlv.u32(b"A0CS")?,
        };
        let dmem = Segment {
            source: tlv.u32(b"DAOF")?,
            destination: 0,
            length: tlv.u32(b"DASZ")?,
        };
        extent(blob, imem.source, imem.length)?;
        extent(blob, dmem.source, dmem.length)?;
        extent(blob, tlv.u32(b"CDOF")?, tlv.u32(b"CDSZ")?)?;
        if imem
            .source
            .checked_add(imem.length)
            .is_none_or(|end| end > dmem.source)
        {
            return Err(Error::Range);
        }
        let signature_patch = tlv.u32(b"PLOC")?;
        let pkc_data_offset = signature_patch
            .checked_sub(dmem.source)
            .ok_or(Error::Range)?;
        let engine_mask = u16::try_from(tlv.u32(b"ENID")?).map_err(|_| Error::Range)?;
        let ucode_id = u8::try_from(tlv.u32(b"UCID")?).map_err(|_| Error::Range)?;
        if engine_mask != 1
            || ucode_id != 3
            || tlv.u32(b"NSIG")? != 2
            || tlv.bytes(b"SIGN")?.len() != 768
        {
            return Err(Error::Unsupported);
        }
        // Check the entire patch, not just its first byte.
        let dmem_end = dmem.source.checked_add(dmem.length).ok_or(Error::Range)?;
        if signature_patch
            .checked_add(384)
            .is_none_or(|end| end > dmem_end)
        {
            return Err(Error::Range);
        }
        Ok(Self {
            tlv,
            blob,
            imem,
            dmem,
            signature_patch,
            pkc_data_offset,
            engine_mask,
            ucode_id,
        })
    }
    /// CPU-exclusive staging only. Install an original production signature at
    /// its designated slot; do not modify redistributed files. No dev fallback.
    pub fn prepare<'s>(
        &self,
        hardware_fuse: u32,
        staging: &'s mut [u8],
    ) -> Result<crate::falcon::Prepared<'s>> {
        let signature = self.tlv.signature(hardware_fuse)?;
        if staging.len() != self.blob.len() {
            return Err(Error::Step("booter-staging"));
        }
        staging.copy_from_slice(self.blob);
        let at = self.signature_patch as usize;
        let end = at
            .checked_add(signature.len())
            .ok_or(Error::Step("booter-patch"))?;
        staging
            .get_mut(at..end)
            .ok_or(Error::Step("booter-patch-range"))?
            .copy_from_slice(signature);
        crate::falcon::Prepared::new(
            staging,
            crate::falcon::Engine::Sec2,
            self.imem,
            self.dmem,
            crate::falcon::Brom {
                pkc: self.pkc_data_offset,
                engines: self.engine_mask,
                ucode: self.ucode_id,
            },
            self.imem.source,
            None,
        )
    }
}
pub struct Bundle<'a> {
    pub image: &'a [u8],
    pub signatures: &'a [u8],
    pub bootloader: Bootloader<'a>,
    pub loader: Booter<'a>,
    pub unloader: Booter<'a>,
}
impl<'a> Bundle<'a> {
    /// Inputs must come from the fixed-hash distribution. Structural validation
    /// does not substitute for the hash pin or hardware signature checks.
    pub fn parse(
        meta: &'a [u8],
        image: &'a [u8],
        bootloader: &'a [u8],
        loader: &'a [u8],
        unloader: &'a [u8],
    ) -> Result<Self> {
        let tlv = Tlv::new(meta)?;
        if tlv.bytes(b"FILE")? != b"gsp.bin"
            || tlv.bytes(b"BLID")? != BUILD_ID
            || tlv.u32(b"SIZE")? as usize != GSP_BYTES
            || image.len() != GSP_BYTES
        {
            return Err(Error::Version);
        }
        let signatures = tlv.bytes(b"SIGN")?;
        if signatures.len() != 4096 {
            return Err(Error::Malformed);
        }
        Ok(Self {
            image,
            signatures,
            bootloader: Bootloader::parse(bootloader)?,
            loader: Booter::parse(loader)?,
            unloader: Booter::parse(unloader)?,
        })
    }
}
