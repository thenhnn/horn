//! Low-level Horn startup ABI. No allocator, VM client, or seL4 dependency.
//! This is a bootstrap substrate, not yet the public managed-handle API.
#![no_std]

pub mod bindings;
pub mod startup;
