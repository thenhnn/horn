use horn_vm::sel4::{FramePool, PAGE_SIZE};
use horn_vm::{Error, MemoryObject};

pub fn check_backing(pool: &mut FramePool, scratch: usize) {
    for dirty in [true, false] {
        let object = MemoryObject::allocate((pool.capacity() * PAGE_SIZE) as u64, pool).unwrap();
        for frame in object.frames() {
            let cap = pool.cap(frame).unwrap();
            cap.frame_map(
                ::sel4::init_thread::slot::VSPACE.cap(),
                scratch,
                ::sel4::CapRights::read_write(),
                ::sel4::VmAttributes::default(),
            )
            .unwrap();
            unsafe {
                let bytes = core::slice::from_raw_parts_mut(scratch as *mut u8, PAGE_SIZE);
                assert!(bytes.iter().all(|byte| *byte == 0));
                if dirty {
                    bytes.fill(0xa5);
                }
            }
            cap.frame_unmap().unwrap();
        }
        object.release(pool).unwrap();
    }
    assert!(matches!(
        MemoryObject::allocate(((pool.capacity() + 1) * PAGE_SIZE) as u64, pool),
        Err(Error::QuotaExceeded)
    ));
    assert_eq!(pool.available(), pool.capacity());
    ::sel4::debug_println!("MEMORY_BACKING_PASS zeroed=true rollback=true reclaimed=true");
}
