#[derive(Debug, Clone, Copy)]
pub enum FwOp {
    Read(u16),
    Or(u16),
    And(u16),
    Bjmpn(u16),
    MdioChg { to_mcu: bool },
    ClearCount,
    Write { reg: u16, val: u16 },
    CountEqSkip(u16),
    EqSkip { n: u16, val: u16 },
    NeqSkip { n: u16, val: u16 },
    WritePrev(u16),
    Skip(u16),
    Delay(u16),
}
