use core::ops::Range;
use horn_memory::sel4::KernelAllocator;

/// Demo convenience wrapper; the reusable allocator itself is fallible.
pub(crate) struct ObjectAllocator(pub(crate) KernelAllocator);

impl ObjectAllocator {
    pub(crate) fn new(bootinfo: &sel4::BootInfo, empty_slots: Range<usize>) -> Self {
        Self(KernelAllocator::new(
            sel4::init_thread::slot::CNODE.cap(),
            find_largest_kernel_untyped(bootinfo),
            empty_slots,
        ))
    }

    pub(crate) fn allocate(&mut self, blueprint: sel4::ObjectBlueprint) -> sel4::cap::Unspecified {
        self.0.allocate(blueprint).unwrap()
    }

    pub(crate) fn allocate_fixed_sized<T: sel4::CapTypeForObjectOfFixedSize>(&mut self) -> sel4::Cap<T> {
        self.0.allocate_fixed_sized().unwrap()
    }

    pub(crate) fn allocate_variable_sized<T: sel4::CapTypeForObjectOfVariableSize>(
        &mut self, size_bits: usize,
    ) -> sel4::Cap<T> {
        self.allocate(T::object_blueprint(size_bits)).cast()
    }
}

pub(crate) fn find_largest_kernel_untyped(bootinfo: &sel4::BootInfo) -> sel4::cap::Untyped {
    let (index, _) = bootinfo.untyped_list().iter().enumerate()
        .filter(|(_, desc)| !desc.is_device())
        .max_by_key(|(_, desc)| desc.size_bits()).unwrap();
    bootinfo.untyped().index(index).cap()
}
