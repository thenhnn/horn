#[repr(C)]
pub struct HornTag {
    pub magic: [u8; 4],
    pub version: u32,
    pub component: [u8; 32],
    pub layout_hash: u64,
}

#[used]
#[unsafe(link_section = ".horn")]
pub static HORN_TAG: HornTag = HornTag {
    magic: *b"HORN",
    version: 1,
    component: *b"demo.counter\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    layout_hash: 0,
};
