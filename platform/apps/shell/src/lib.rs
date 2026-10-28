#![no_std]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
