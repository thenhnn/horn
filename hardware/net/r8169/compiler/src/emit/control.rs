use std::collections::BTreeMap;

use proc_macro2::{Literal, Span, TokenStream};
use quote::quote;
use syn::Lifetime;

use super::{FnGen, Label, LoopSpan, flow::Flow, lit16};
use crate::op::FwOp;

fn innermost(loops: &[LoopSpan], p: usize) -> Option<usize> {
    (0..loops.len())
        .filter(|&i| loops[i].header <= p && p < loops[i].end)
        .max_by_key(|&i| loops[i].header)
}

impl<'a> FnGen<'a> {
    pub(super) fn build(name: &'a str, ops: &'a [FwOp]) -> Result<Self, String> {
        let n = ops.len();
        let mut fwd: Vec<Option<usize>> = vec![None; n];
        let mut cond: Vec<Option<TokenStream>> = (0..n).map(|_| None).collect();
        let mut back_at: Vec<Option<usize>> = vec![None; n];

        for (pc, op) in ops.iter().enumerate() {
            match *op {
                FwOp::Bjmpn(k) => {
                    let k = k as usize;
                    if k > pc {
                        return Err(format!("{name}: op {pc}: BJMPN {k} runs off the front"));
                    }
                    back_at[pc] = Some(pc - k);
                }
                FwOp::Skip(k) => fwd[pc] = Some(pc + 1 + k as usize),
                FwOp::EqSkip { n: k, val } => {
                    fwd[pc] = Some(pc + 1 + k as usize);
                    let v = lit16(val);
                    cond[pc] = Some(quote! { data == #v });
                }
                FwOp::NeqSkip { n: k, val } => {
                    fwd[pc] = Some(pc + 1 + k as usize);
                    let v = lit16(val);
                    cond[pc] = Some(quote! { data != #v });
                }
                FwOp::CountEqSkip(k) => {
                    fwd[pc] = Some(pc + 2);
                    let v = Literal::u32_unsuffixed(k as u32);
                    cond[pc] = Some(quote! { count == #v });
                }
                _ => {}
            }
        }
        for (pc, &target) in fwd.iter().enumerate() {
            if let Some(t) = target
                && t > n
            {
                return Err(format!("{name}: op {pc} skips to {t}, past end {n}"));
            }
        }

        let flow = Flow::analyze(ops, &fwd, &back_at);

        // loops: [header, max(back_edge_pc) + 1)
        let mut ends: BTreeMap<usize, usize> = BTreeMap::new();
        for (pc, &target) in back_at.iter().enumerate() {
            if let Some(h) = target {
                let e = ends.entry(h).or_insert(pc + 1);
                *e = (*e).max(pc + 1);
            }
        }
        let mut loops: Vec<LoopSpan> = ends
            .into_iter()
            .map(|(header, end)| LoopSpan {
                header,
                end,
                parent: None,
            })
            .collect();

        for i in 0..loops.len() {
            let mut best = None;
            for j in 0..i {
                if loops[j].header < loops[i].header && loops[i].header < loops[j].end {
                    if loops[i].end > loops[j].end {
                        return Err(format!(
                            "{name}: loops at {} and {} overlap without nesting",
                            loops[j].header, loops[i].header
                        ));
                    }
                    best = Some(j);
                }
            }
            loops[i].parent = best;
        }
        for (pc, &target) in back_at.iter().enumerate() {
            if let Some(h) = target
                && innermost(&loops, pc).map(|i| loops[i].header) != Some(h)
            {
                return Err(format!(
                    "{name}: op {pc}: BJMPN to {h} is not the innermost loop header"
                ));
            }
        }
        for lp in &loops {
            let has_exit = fwd[lp.header..lp.end]
                .iter()
                .any(|target| target.is_some_and(|t| t >= lp.end));
            if !has_exit {
                println!(
                    "cargo:warning={name}: the loop at op {} can never exit",
                    lp.header
                );
            }
        }

        // one label per forward target
        let mut by_target: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (pc, &target) in fwd.iter().enumerate() {
            if let Some(t) = target {
                by_target.entry(t).or_default().push(pc);
            }
        }

        let mut labels: Vec<Label> = Vec::new();
        let mut dest: Vec<Option<TokenStream>> = (0..n).map(|_| None).collect();

        for (&t, srcs) in &by_target {
            if t == n {
                for &s in srcs {
                    dest[s] = Some(quote! { break 'done; });
                }
                continue;
            }

            if let Some(li) = innermost(&loops, srcs[0])
                && loops[li].end == t
                && srcs.iter().all(|&s| innermost(&loops, s) == Some(li))
            {
                let lt = Lifetime::new(&format!("'loop{}", loops[li].header), Span::call_site());
                for &s in srcs {
                    dest[s] = Some(quote! { break #lt; });
                }
                continue;
            }

            // open the block outside every loop it must escape
            let mut open = *srcs.iter().min().unwrap();
            loop {
                let mut moved = false;
                for lp in &loops {
                    if lp.header < open && open < lp.end && t >= lp.end {
                        open = lp.header;
                        moved = true;
                    }
                }
                if !moved {
                    break;
                }
            }
            for lp in &loops {
                if open < lp.header && lp.header < t && t < lp.end {
                    return Err(format!(
                        "{name}: skips to {t} land inside the loop at {}",
                        lp.header
                    ));
                }
            }

            let region = (0..loops.len())
                .filter(|&i| loops[i].header <= open && open < loops[i].end && t < loops[i].end)
                .max_by_key(|&i| loops[i].header);

            let lt = Lifetime::new(&format!("'skip{t}"), Span::call_site());
            for &s in srcs {
                dest[s] = Some(quote! { break #lt; });
            }
            labels.push(Label {
                open,
                close: t,
                region,
                lt,
            });
        }

        // labeled blocks nest LIFO: crossing ranges are unrepresentable
        for i in 0..labels.len() {
            for j in 0..i {
                let (a0, a1) = (labels[i].open, labels[i].close);
                let (b0, b1) = (labels[j].open, labels[j].close);
                if a0 < b1 && b0 < a1 && !((a0 <= b0 && b1 <= a1) || (b0 <= a0 && a1 <= b1)) {
                    return Err(format!(
                        "{name}: skip ranges [{a0},{a1}) and [{b0},{b1}) cross"
                    ));
                }
            }
        }

        Ok(FnGen {
            name,
            ops,
            n,
            cond,
            dest,
            flow,
            loops,
            labels,
        })
    }
}
