use super::*;

struct Pool {
    available: usize,
    live: usize,
    quarantine_release: bool,
}
impl Frames for Pool {
    type Frame = ();
    fn page_size(&self) -> usize {
        4096
    }
    fn allocate_zeroed(&mut self) -> Result<(), Error> {
        if self.available == 0 {
            return Err(Error::Exhausted);
        }
        self.available -= 1;
        self.live += 1;
        Ok(())
    }
    fn release(&mut self, _: ()) -> Result<(), Error> {
        self.live -= 1;
        if self.quarantine_release {
            return Err(Error::Backend);
        }
        self.available += 1;
        Ok(())
    }
}

#[test]
fn rounds_backing_without_changing_logical_size() {
    let mut pool = Pool {
        available: 2,
        live: 0,
        quarantine_release: false,
    };
    let object = MemoryObject::allocate(4097, &mut pool).unwrap();
    assert_eq!(object.size(), 4097);
    assert_eq!(object.resident_size(), 8192);
    assert_eq!(object.page_count(), 2);
    object.release(&mut pool).unwrap();
    assert_eq!(pool.available, 2);
}

#[test]
fn rejects_empty_and_overflowing_sizes_before_allocation() {
    let mut pool = Pool {
        available: 2,
        live: 0,
        quarantine_release: false,
    };
    assert!(matches!(
        MemoryObject::allocate(0, &mut pool),
        Err(Error::InvalidSize)
    ));
    assert!(matches!(
        MemoryObject::allocate(u64::MAX, &mut pool),
        Err(Error::InvalidSize)
    ));
    assert_eq!(pool.live, 0);
}

#[test]
fn partial_allocation_rolls_back_and_can_be_retried() {
    let mut pool = Pool {
        available: 2,
        live: 0,
        quarantine_release: false,
    };
    assert!(matches!(
        MemoryObject::allocate(12288, &mut pool),
        Err(Error::Exhausted)
    ));
    assert_eq!((pool.available, pool.live), (2, 0));
    MemoryObject::allocate(8192, &mut pool)
        .unwrap()
        .release(&mut pool)
        .unwrap();
    assert_eq!(pool.available, 2);
}

#[test]
fn cleanup_failure_is_reported_and_never_reuses_quarantined_backing() {
    let mut pool = Pool {
        available: 2,
        live: 0,
        quarantine_release: true,
    };
    assert!(matches!(
        MemoryObject::allocate(12288, &mut pool),
        Err(Error::Backend)
    ));
    assert_eq!((pool.available, pool.live), (0, 0));
}

#[test]
fn explicit_release_attempts_every_frame_even_after_error() {
    let mut pool = Pool {
        available: 2,
        live: 0,
        quarantine_release: true,
    };
    let object = MemoryObject::allocate(8192, &mut pool).unwrap();
    assert_eq!(object.release(&mut pool), Err(Error::Backend));
    assert_eq!(pool.live, 0);
}
