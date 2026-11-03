//! Freestanding C archive of the shared, allocation-free startup validator.
#![no_std]

pub use libhorn::startup::{horn_startup_lookup, horn_startup_validate};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // Validation returns errors; a panic is a programming error, never fallback.
    loop {
        core::hint::spin_loop();
    }
}
