use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Deserialize;

use crate::parser::Blob;

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    url: String,
    license: String,
    copyright: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Database {
    source: Source,
    chips: Vec<Rule>,
    extended_chips: Vec<Rule>,
    non_gmii: NonGmii,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NonGmii {
    rtl8106eus: Chip,
    rtl8107e: Chip,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chip {
    mac_version: u8,
    name: String,
    firmware: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    mask: String,
    value: String,
    #[serde(default)]
    extended: bool,
    mac_version: Option<u8>,
    name: Option<String>,
    firmware: Option<String>,
}

fn hex(value: &str) -> Result<u32, String> {
    value
        .strip_prefix("0x")
        .and_then(|v| u32::from_str_radix(v, 16).ok())
        .ok_or_else(|| format!("invalid hexadecimal u32: {value}"))
}

fn chip(
    version: u8,
    name: &str,
    firmware: Option<&str>,
    blobs: &BTreeMap<&str, usize>,
    versions: &mut BTreeSet<u8>,
) -> Result<TokenStream, String> {
    if version == 0 || name.trim().is_empty() {
        return Err("chip requires a nonzero MAC version and a nonempty name".into());
    }
    versions.insert(version);
    let version = format_ident!("V{version}");
    let firmware = match firmware {
        Some(name) => {
            let id = blobs
                .get(name)
                .ok_or_else(|| format!("unknown firmware: {name}"))?;
            quote! { Some(&FIRMWARE[#id]) }
        }
        None => quote! { None },
    };
    Ok(quote! { ChipInfo { mac_version: MacVersion::#version, name: #name, firmware: #firmware } })
}

fn rules(
    rows: &[Rule],
    extended_table: bool,
    blobs: &BTreeMap<&str, usize>,
    versions: &mut BTreeSet<u8>,
) -> Result<Vec<TokenStream>, String> {
    let mut previous = Vec::new();
    let mut output = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let (mask, value) = (hex(&row.mask)?, hex(&row.value)?);
        if mask == 0 || value & !mask != 0 {
            return Err(format!("rule {i}: zero mask or value outside mask"));
        }
        if previous
            .iter()
            .any(|&(m, v)| mask & m == m && value & m == v)
        {
            return Err(format!("rule {i}: shadowed by an earlier rule"));
        }
        previous.push((mask, value));
        let descriptor = if row.extended {
            if extended_table
                || row.mac_version.is_some()
                || row.name.is_some()
                || row.firmware.is_some()
            {
                return Err(format!("rule {i}: invalid extended-XID marker"));
            }
            quote! { None }
        } else {
            let info = chip(
                row.mac_version
                    .ok_or_else(|| format!("rule {i}: missing mac_version"))?,
                row.name
                    .as_deref()
                    .ok_or_else(|| format!("rule {i}: missing name"))?,
                row.firmware.as_deref(),
                blobs,
                versions,
            )?;
            quote! { Some(#info) }
        };
        output.push(quote! { ChipRule { mask: #mask, value: #value, chip: #descriptor } });
    }
    Ok(output)
}

pub fn emit_module(json: &str, firmware: &[Blob]) -> Result<TokenStream, String> {
    let data: Database = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let mut blobs = BTreeMap::new();
    for (id, blob) in firmware.iter().enumerate() {
        if blobs.insert(blob.name.as_str(), id).is_some() {
            return Err(format!("duplicate firmware: {}", blob.name));
        }
    }
    let mut versions = BTreeSet::new();
    let main = rules(&data.chips, false, &blobs, &mut versions)?;
    let extended = rules(&data.extended_chips, true, &blobs, &mut versions)?;
    if !extended.is_empty() && !data.chips.iter().any(|row| row.extended) {
        return Err("extended table has no main-table marker".into());
    }
    let fast_ethernet = |info: &Chip, versions: &mut BTreeSet<u8>| {
        chip(
            info.mac_version,
            &info.name,
            info.firmware.as_deref(),
            &blobs,
            versions,
        )
    };
    let rtl8106eus = fast_ethernet(&data.non_gmii.rtl8106eus, &mut versions)?;
    let rtl8107e = fast_ethernet(&data.non_gmii.rtl8107e, &mut versions)?;
    let variants = versions.iter().map(|version| {
        let name = format_ident!("V{version}");
        quote! { #name = #version }
    });
    let provenance = format!(
        "Chip metadata: {}\nSPDX-License-Identifier: {}\n{}",
        data.source.url,
        data.source.license,
        data.source.copyright.join("\n")
    );
    Ok(quote! {
        use super::{ChipInfo, FIRMWARE};
        #[doc = #provenance]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum MacVersion { #(#variants,)* }

        pub(super) struct ChipRule {
            pub mask: u32,
            pub value: u32,
            pub chip: Option<ChipInfo>,
        }
        pub(super) static CHIPS: &[ChipRule] = &[#(#main,)*];
        pub(super) static EXTENDED_CHIPS: &[ChipRule] = &[#(#extended,)*];
        pub(super) static RTL8106EUS: ChipInfo = #rtl8106eus;
        pub(super) static RTL8107E: ChipInfo = #rtl8107e;
    })
}
