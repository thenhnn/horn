use crate::op::FwOp;

pub const HEADER_LEN: usize = 45; // packed: 4 + 32 + 4 + 4 + 1

#[derive(Debug)]
pub struct Blob {
    pub name: String,
    pub version: String,
    pub ops: Vec<FwOp>,
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

// name is firmware name (without .fw)
pub fn parse_fw_blob(name: String, data: &[u8]) -> Result<Blob, String> {
    if data.len() < 4 {
        return Err(format!("{} bytes: shorter than one opcode", data.len()));
    }

    let (version, code) = if u32le(data, 0) == 0 {
        if data.len() < HEADER_LEN {
            return Err(format!("{} bytes: shorter than fw_info", data.len()));
        }
        let sum = data.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        if sum != 0 {
            return Err(format!("checksum: whole file sums to {sum:#04x}"));
        }
        let v = &data[4..36];
        let end = v.iter().position(|&c| c == 0).unwrap_or(32);
        let version = String::from_utf8_lossy(&v[..end]).trim_end().to_string();
        let start = u32le(data, 36) as usize;
        let count = u32le(data, 40) as usize; // ENTRIES, not bytes
        if start > data.len() {
            return Err("fw_start past end".into());
        }
        if count > (data.len() - start) / 4 {
            return Err("fw_len past end".into());
        }
        (Some(version), &data[start..start + count * 4])
    } else {
        // raw format: whole blob is code, version := file name
        if data.len() % 4 != 0 {
            return Err(format!("raw, size {} not a multiple of 4", data.len()));
        }
        (None, data)
    };

    let words: Vec<u32> = code
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect();
    let n = words.len();
    let mut ops = Vec::with_capacity(n);

    for (i, &a) in words.iter().enumerate() {
        let reg = ((a & 0x0fff_0000) >> 16) as u16;
        let val = (a & 0xffff) as u16;

        let op = match a >> 28 {
            0x0 => FwOp::Read(reg),
            0x1 => FwOp::Or(val),
            0x2 => FwOp::And(val),
            0x3 if reg as usize <= i => FwOp::Bjmpn(reg),
            0x3 => return Err(format!("op {i}: backward jump {reg} before start")),
            0x4 if val <= 1 => FwOp::MdioChg { to_mcu: val == 1 },
            0x4 => return Err(format!("op {i}: MDIO_CHG value {val} > 1")),
            0x7 => FwOp::ClearCount,
            0x8 => FwOp::Write { reg, val },
            0x9 if i + 2 < n => FwOp::CountEqSkip(val),
            0x9 => return Err(format!("op {i}: count-skip past end")),
            0xa if (i + 1 + reg as usize) < n => FwOp::EqSkip { n: reg, val },
            0xb if (i + 1 + reg as usize) < n => FwOp::NeqSkip { n: reg, val },
            0xa | 0xb => return Err(format!("op {i}: conditional skip past end")),
            0xc => FwOp::WritePrev(reg),
            0xd if (i + 1 + reg as usize) < n => FwOp::Skip(reg),
            0xd => return Err(format!("op {i}: skip past end")),
            0xe => FwOp::Delay(val),
            k => return Err(format!("op {i}: invalid opcode {k:#x}")),
        };
        ops.push(op);
    }
    Ok(Blob {
        name: name.clone(),
        version: version.unwrap_or(name),
        ops,
    })
}

pub fn ident(stem: &str) -> String {
    let mut s: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();

    if s.is_empty() || s.as_bytes()[0].is_ascii_digit() {
        s.insert_str(0, "fw_");
    }

    s
}
