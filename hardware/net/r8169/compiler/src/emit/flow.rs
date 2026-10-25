use std::collections::VecDeque;

use crate::op::FwOp;

const PHY: u8 = 1;
const MCU: u8 = 2;
const BOTH: u8 = PHY | MCU;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Phy,
    Mcu,
    Dynamic,
}

pub(super) struct Flow {
    modes: Vec<u8>,
    pub entries: Vec<bool>,
    pub track_mode: bool,
}

impl Flow {
    pub fn analyze(ops: &[FwOp], forward: &[Option<usize>], backward: &[Option<usize>]) -> Self {
        let mut modes = vec![0; ops.len() + 1];
        let mut entries = vec![false; ops.len() + 1];
        modes[0] = PHY;
        entries[0] = true;
        for target in forward.iter().chain(backward).flatten() {
            entries[*target] = true;
        }

        let mut pending = VecDeque::from([0]);
        while let Some(pc) = pending.pop_front() {
            if pc == ops.len() {
                continue;
            }
            let mode = match ops[pc] {
                FwOp::MdioChg { to_mcu: false } => PHY,
                FwOp::MdioChg { to_mcu: true } => MCU,
                _ => modes[pc],
            };
            let jump = backward[pc].or(forward[pc]);
            let fallthrough = !matches!(ops[pc], FwOp::Bjmpn(_) | FwOp::Skip(_));
            for next in [jump, fallthrough.then_some(pc + 1)].into_iter().flatten() {
                let merged = modes[next] | mode;
                if modes[next] != merged {
                    modes[next] = merged;
                    pending.push_back(next);
                }
            }
        }

        let track_mode = ops.iter().enumerate().any(|(pc, op)| {
            modes[pc] == BOTH
                && matches!(op, FwOp::Read(_) | FwOp::Write { .. } | FwOp::WritePrev(_))
        });
        Self {
            modes,
            entries,
            track_mode,
        }
    }

    pub fn mode(&self, pc: usize) -> Mode {
        match self.modes[pc] {
            MCU => Mode::Mcu,
            BOTH => Mode::Dynamic,
            _ => Mode::Phy,
        }
    }
}
