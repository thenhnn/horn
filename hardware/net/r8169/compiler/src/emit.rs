use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Lifetime, LitStr};

use crate::{op::FwOp, parser::Blob};

mod control;
mod flow;
mod statements;

use flow::Flow;

const MIN_TABLE_WRITES: usize = 8;

fn lit16(v: u16) -> Literal {
    Literal::u16_unsuffixed(v)
}

fn fn_ident(name: &str) -> Ident {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format_ident!("fw_{clean}")
}

pub fn emit_module(blobs: &[Blob]) -> Result<TokenStream, String> {
    let count = blobs.len();
    let fns = blobs
        .iter()
        .map(|b| FnGen::build(&b.name, &b.ops).map(|g| g.emit()))
        .collect::<Result<Vec<_>, _>>()?;

    let registry = blobs.iter().enumerate().map(|(i, b)| {
        let name = LitStr::new(&b.name, Span::call_site());
        let version = LitStr::new(&b.version, Span::call_site());
        let id = Literal::u16_unsuffixed(i as u16);
        quote! { Firmware { name: #name, version: #version, id: #id } }
    });

    let arms = blobs.iter().enumerate().map(|(i, b)| {
        let id = Literal::u16_unsuffixed(i as u16);
        let f = fn_ident(&b.name);
        quote! { #id => #f(io, t).await, }
    });

    Ok(quote! {
        use super::{Firmware, PhyIo, Timer};

        static FIRMWARE_DATA: [Firmware; #count] = [#(#registry,)*];
        pub static FIRMWARE: &[Firmware] = &FIRMWARE_DATA;

        pub async fn run<I: PhyIo + ?Sized, T: Timer + ?Sized>(id: u16, io: &mut I, t: &mut T) {
            match id {
                #(#arms)*
                _ => unreachable!("firmware id out of range: {}", id),
            }
        }

        #(#fns)*
    })
}

struct LoopSpan {
    header: usize,
    end: usize,
    parent: Option<usize>,
}
struct Label {
    open: usize,
    close: usize,
    region: Option<usize>,
    lt: Lifetime,
}

struct FnGen<'a> {
    name: &'a str,
    ops: &'a [FwOp],
    n: usize,
    cond: Vec<Option<TokenStream>>,
    dest: Vec<Option<TokenStream>>,
    flow: Flow,
    loops: Vec<LoopSpan>,
    labels: Vec<Label>,
}

impl<'a> FnGen<'a> {
    fn emit(&self) -> TokenStream {
        let body = self.range(None, 0, self.n, None);
        let f = fn_ident(self.name);
        let mode = self
            .flow
            .track_mode
            .then(|| quote! { let mut mcu = false; });
        quote! {
            #[allow(unused_assignments, unused_mut, unused_variables,
                    unreachable_code, unused_labels)]
            pub async fn #f<I: PhyIo + ?Sized, T: Timer + ?Sized>(io: &mut I, t: &mut T) {
                let mut data: u16 = 0;
                let mut count: u32 = 0;
                #mode
                'done: {
                    #body
                }
            }
        }
    }

    fn step(&self, pc: usize) -> TokenStream {
        if let FwOp::Bjmpn(k) = self.ops[pc] {
            let lt = Lifetime::new(&format!("'loop{}", pc - k as usize), Span::call_site());
            return quote! { continue #lt; };
        }
        if let Some(d) = &self.dest[pc] {
            return match &self.cond[pc] {
                None => d.clone(),
                Some(c) => quote! { if #c { #d } },
            };
        }
        statements::stmt(self.ops[pc], self.flow.mode(pc), self.flow.track_mode)
    }

    fn range(
        &self,
        region: Option<usize>,
        start: usize,
        end: usize,
        skip: Option<usize>,
    ) -> TokenStream {
        let mut out = TokenStream::new();
        let mut p = start;
        while p < end {
            let lbl = self
                .labels
                .iter()
                .enumerate()
                .filter(|(i, l)| {
                    Some(*i) != skip && l.region == region && l.open == p && l.close <= end
                })
                .max_by_key(|(_, l)| l.close);

            if let Some((i, l)) = lbl {
                let inner = self.range(region, p, l.close, Some(i));
                let lt = &l.lt;
                out.extend(quote! { #lt: { #inner } });
                p = l.close;
                continue;
            }

            if let Some(li) = (0..self.loops.len()).find(|&i| {
                self.loops[i].header == p
                    && self.loops[i].parent == region
                    && self.loops[i].end <= end
            }) {
                let lp = &self.loops[li];
                let body = self.range(Some(li), lp.header, lp.end, None);
                let lt = Lifetime::new(&format!("'loop{}", lp.header), Span::call_site());
                out.extend(quote! { #lt: loop { #body } });
                p = lp.end;
                continue;
            }

            if matches!(self.ops[p], FwOp::Write { .. }) {
                let mut next = p + 1;
                while next < end
                    && !self.flow.entries[next]
                    && matches!(self.ops[next], FwOp::Write { .. })
                    && self.flow.mode(next) == self.flow.mode(p)
                {
                    next += 1;
                }
                if next - p >= MIN_TABLE_WRITES {
                    out.extend(statements::writes(&self.ops[p..next], self.flow.mode(p)));
                    p = next;
                    continue;
                }
            }
            out.extend(self.step(p));
            p += 1;
        }
        out
    }
}
