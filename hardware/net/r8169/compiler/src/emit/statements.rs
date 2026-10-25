use proc_macro2::TokenStream;
use quote::quote;

use super::{flow::Mode, lit16};
use crate::op::FwOp;

fn access(mode: Mode, phy: TokenStream, mcu: TokenStream) -> TokenStream {
    match mode {
        Mode::Phy => phy,
        Mode::Mcu => mcu,
        Mode::Dynamic => quote! { if mcu { #mcu } else { #phy } },
    }
}

pub(super) fn stmt(op: FwOp, mode: Mode, track_mode: bool) -> TokenStream {
    match op {
        FwOp::Read(reg) => {
            let reg = lit16(reg);
            let read = access(
                mode,
                quote! { io.phy_read(#reg) },
                quote! { io.mcu_read(#reg) },
            );
            quote! {
                data = #read;
                count = count.wrapping_add(1);
            }
        }
        FwOp::Write { reg, val } => {
            let (reg, val) = (lit16(reg), lit16(val));
            let write = access(
                mode,
                quote! { io.phy_write(#reg, #val) },
                quote! { io.mcu_write(#reg, #val) },
            );
            quote! { #write; }
        }
        FwOp::WritePrev(reg) => {
            let reg = lit16(reg);
            let write = access(
                mode,
                quote! { io.phy_write(#reg, data) },
                quote! { io.mcu_write(#reg, data) },
            );
            quote! { #write; }
        }
        FwOp::Or(val) => {
            let val = lit16(val);
            quote! { data |= #val; }
        }
        FwOp::And(val) => {
            let val = lit16(val);
            quote! { data &= #val; }
        }
        FwOp::ClearCount => quote! { count = 0; },
        FwOp::MdioChg { to_mcu } if track_mode => quote! { mcu = #to_mcu; },
        FwOp::MdioChg { .. } => TokenStream::new(),
        FwOp::Delay(ms) => {
            let ms = lit16(ms);
            quote! { t.sleep_ms(#ms).await; }
        }
        _ => unreachable!("branch opcodes are emitted by step"),
    }
}

pub(super) fn writes(ops: &[FwOp], mode: Mode) -> TokenStream {
    let pairs = ops.iter().map(|op| {
        let FwOp::Write { reg, val } = *op else {
            unreachable!()
        };
        let (reg, val) = (lit16(reg), lit16(val));
        quote! { (#reg, #val) }
    });
    let body = access(
        mode,
        quote! { for &(reg, val) in WRITES { io.phy_write(reg, val); } },
        quote! { for &(reg, val) in WRITES { io.mcu_write(reg, val); } },
    );
    quote! {{
        const WRITES: &[(u16, u16)] = &[#(#pairs,)*];
        #body
    }}
}
