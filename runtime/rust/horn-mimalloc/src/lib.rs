//! Resident VM-backed mimalloc. IPC bookkeeping uses the caller's control heap.
#![no_std]
#![feature(allocator_api)]

use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::NonNull;
use horn_ipc::*;
use horn_vm::protocol::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    Vm(VmError),
    Batch(VmBatchError),
    Rpc(RpcError),
    Invalid,
    Exhausted,
}
impl From<CallError<VmError>> for Error {
    fn from(value: CallError<VmError>) -> Self {
        match value {
            CallError::Remote(error) => Self::Vm(error),
            CallError::Rpc(error) => Self::Rpc(error),
        }
    }
}
impl From<CallError<VmBatchError>> for Error {
    fn from(value: CallError<VmBatchError>) -> Self {
        match value {
            CallError::Remote(error) => Self::Batch(error),
            CallError::Rpc(error) => Self::Rpc(error),
        }
    }
}
impl From<RpcError> for Error {
    fn from(value: RpcError) -> Self {
        Self::Rpc(value)
    }
}

#[derive(Clone, Copy)]
struct Allocation {
    address: usize,
    size: usize,
    region: Cap<VmRegionObjectTag>,
    memory: Option<Cap<VmMemoryObjectTag>>,
}

pub struct VmHeap {
    transport: Sel4Transport<Sel4CapSpace>,
    factory: Cap<VmFactoryTag>,
    space: Cap<VmAddressSpaceTag>,
    allocations: [Option<Allocation>; 128],
    page_size: usize,
    limit: usize,
}
impl VmHeap {
    pub fn new(
        transport: Sel4Transport<Sel4CapSpace>,
        factory: CapRef,
        space: CapRef,
    ) -> Result<Self, Error> {
        let mut result = Self {
            transport,
            factory: Cap::from_ref(factory),
            space: Cap::from_ref(space),
            allocations: [None; 128],
            page_size: 0,
            limit: 0,
        };
        let info = result.info()?;
        if info.features.0 & VmFeatures::RESIDENT.0 == 0
            || info.page_size != 4096
            || info.batch_limit < 2
        {
            return Err(Error::Invalid);
        }
        result.page_size = usize::try_from(info.page_size).map_err(|_| Error::Invalid)?;
        result.limit = usize::try_from(info.limit_bytes).map_err(|_| Error::Invalid)?;
        Ok(result)
    }
    fn info(&mut self) -> Result<VmInfo, Error> {
        Ok(VmFactoryClient::new(&mut self.transport, self.factory).info()?)
    }
    fn close(&mut self, memory: Cap<VmMemoryObjectTag>) -> Result<(), Error> {
        VmMemoryClient::new(&mut self.transport, Cap::from_ref(memory.as_ref())).close()?;
        self.transport.space.delete(memory.as_ref())?;
        Ok(())
    }
    pub fn allocate(&mut self, size: usize, alignment: usize) -> Result<NonNull<u8>, Error> {
        if size == 0 || !alignment.is_power_of_two() {
            return Err(Error::Invalid);
        }
        let size =
            size.checked_add(self.page_size - 1).ok_or(Error::Invalid)? & !(self.page_size - 1);
        let index = self
            .allocations
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Exhausted)?;
        let protection = VmProtection(VmProtection::READ.0 | VmProtection::WRITE.0);
        let memory = VmFactoryClient::new(&mut self.transport, self.factory).allocate(
            size as u64,
            VmBacking::Resident,
            protection,
        )?;
        let region = match VmAddressSpaceClient::new(&mut self.transport, self.space).reserve(
            VmRegionRequest {
                address: 0,
                length: size as u64,
                alignment: alignment.max(self.page_size) as u64,
                placement: VmPlacement::Anywhere,
                ceiling: protection,
            },
        ) {
            Ok(region) => region,
            Err(error) => {
                self.close(memory)?;
                return Err(error.into());
            }
        };
        // Record authority before any further RPC can fail; failed cleanup stays retained.
        self.allocations[index] = Some(Allocation {
            address: 0,
            size,
            region,
            memory: Some(memory),
        });
        let result = (|| {
            let info =
                VmRegionClient::new(&mut self.transport, Cap::from_ref(region.as_ref())).info()?;
            let address = usize::try_from(info.address).map_err(|_| Error::Invalid)?;
            if address == 0
                || (address as u64)
                    .checked_add(size as u64)
                    .is_none_or(|end| end > (1u64 << 32))
            {
                return Err(Error::Invalid);
            }
            self.allocations[index].as_mut().unwrap().address = address;
            let mut updates = List::default();
            updates.len = 2;
            updates.data[0] = VmUpdate::Prepare(VmRange {
                offset: 0,
                length: size as u64,
            });
            updates.data[1] = VmUpdate::Map(VmMap {
                offset: 0,
                length: size as u64,
                memory_offset: 0,
                protection,
            });
            VmRegionClient::new(&mut self.transport, Cap::from_ref(region.as_ref()))
                .apply(Some(memory), Parcel::inline(updates))?;
            self.close(memory)?;
            self.allocations[index].as_mut().unwrap().memory = None;
            NonNull::new(address as *mut u8).ok_or(Error::Invalid)
        })();
        if result.is_err() {
            self.release(index)?;
        }
        result
    }
    fn release(&mut self, index: usize) -> Result<(), Error> {
        let allocation = self.allocations[index].unwrap();
        VmRegionClient::new(
            &mut self.transport,
            Cap::from_ref(allocation.region.as_ref()),
        )
        .destroy()?;
        self.transport.space.delete(allocation.region.as_ref())?;
        // The region is gone, even if closing an unpublished backing later fails.
        self.allocations[index] = None;
        if let Some(memory) = allocation.memory {
            self.close(memory)?;
        }
        Ok(())
    }
    pub fn free(&mut self, address: usize, size: usize) -> Result<(), Error> {
        let index = self
            .allocations
            .iter()
            .position(|entry| {
                entry.is_some_and(|a| a.address == address && (a.size == size || size == 0))
            })
            .ok_or(Error::Invalid)?;
        self.release(index)
    }
}

mod native {
    use super::*;
    use core::{
        cell::UnsafeCell,
        ffi::{CStr, c_char, c_int, c_void},
        mem::MaybeUninit,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct State(UnsafeCell<MaybeUninit<VmHeap>>);
    unsafe impl Sync for State {}
    static STATE: State = State(UnsafeCell::new(MaybeUninit::uninit()));
    static READY: AtomicBool = AtomicBool::new(false);
    static BUSY: AtomicBool = AtomicBool::new(false);
    static LIMIT: AtomicUsize = AtomicUsize::new(0);
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            BUSY.store(false, Ordering::Release);
        }
    }
    fn with_heap<T>(f: impl FnOnce(&mut VmHeap) -> Result<T, Error>) -> Result<T, Error> {
        if !READY.load(Ordering::Acquire)
            || BUSY
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            return Err(Error::Exhausted);
        }
        let _guard = Guard;
        f(unsafe { (&mut *STATE.0.get()).assume_init_mut() })
    }
    unsafe extern "C" {
        fn horn_mimalloc_start();
        fn mi_malloc_aligned(size: usize, alignment: usize) -> *mut c_void;
        fn mi_free(address: *mut c_void);
        fn mi_collect(force: bool);
    }
    /// Install once, before allocator use. The runtime must initialize native TLS.
    /// IPC metadata must use an independent allocator, not this mimalloc instance.
    pub unsafe fn install(heap: VmHeap) -> Result<(), Error> {
        if BUSY
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(Error::Invalid);
        }
        let _guard = Guard;
        if READY.load(Ordering::Acquire) {
            return Err(Error::Invalid);
        }
        LIMIT.store(heap.limit, Ordering::Release);
        unsafe {
            (*STATE.0.get()).write(heap);
        }
        READY.store(true, Ordering::Release);
        drop(_guard);
        unsafe {
            horn_mimalloc_start();
        }
        Ok(())
    }
    pub fn info() -> Result<VmInfo, Error> {
        with_heap(VmHeap::info)
    }
    pub fn collect() {
        if READY.load(Ordering::Acquire) {
            unsafe {
                mi_collect(true);
            }
        }
    }
    pub(super) fn allocate(layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if !READY.load(Ordering::Acquire) {
            return Err(AllocError);
        }
        if layout.size() == 0 {
            return Ok(NonNull::slice_from_raw_parts(
                NonNull::new(layout.align() as *mut u8).unwrap(),
                0,
            ));
        }
        let address =
            NonNull::new(unsafe { mi_malloc_aligned(layout.size(), layout.align()) }.cast::<u8>())
                .ok_or(AllocError)?;
        Ok(NonNull::slice_from_raw_parts(address, layout.size()))
    }
    pub(super) unsafe fn deallocate(address: NonNull<u8>, layout: Layout) {
        if layout.size() != 0 {
            unsafe {
                mi_free(address.as_ptr().cast());
            }
        }
    }
    #[unsafe(no_mangle)]
    extern "C" fn horn_mimalloc_alloc(size: usize, alignment: usize) -> *mut c_void {
        match with_heap(|heap| heap.allocate(size, alignment)) {
            Ok(pointer) => pointer.as_ptr().cast(),
            Err(_) => core::ptr::null_mut(),
        }
    }
    #[unsafe(no_mangle)]
    extern "C" fn horn_mimalloc_free(address: *mut c_void, size: usize) -> c_int {
        if with_heap(|heap| heap.free(address as usize, size)).is_ok() {
            0
        } else {
            22
        }
    }
    #[unsafe(no_mangle)]
    extern "C" fn horn_mimalloc_limit() -> usize {
        LIMIT.load(Ordering::Acquire)
    }
    #[unsafe(no_mangle)]
    extern "C" fn horn_mimalloc_message(message: *const c_char) {
        let message = unsafe { CStr::from_ptr(message) };
        for &byte in message.to_bytes() {
            sel4::debug_put_char(byte);
        }
    }
    #[unsafe(no_mangle)]
    extern "C" fn horn_mimalloc_yield() {
        sel4::r#yield();
    }
    #[unsafe(no_mangle)]
    extern "C" fn horn_mimalloc_abort() {
        panic!("mimalloc aborted")
    }
}

pub use native::{collect, info, install};

#[derive(Clone, Copy, Default)]
pub struct Mimalloc;
unsafe impl Allocator for Mimalloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        native::allocate(layout)
    }
    unsafe fn deallocate(&self, address: NonNull<u8>, layout: Layout) {
        unsafe {
            native::deallocate(address, layout);
        }
    }
}
