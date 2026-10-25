{lib}: {
  crossSystem = lib.systems.examples.aarch64-multiplatform;
  settings = {
    KernelPlatform = "qemu-arm-virt";
    KernelMaxNumNodes = 4;
    ARM_CPU = "cortex-a53";
    QEMU_GIC_VERSION = 3;
  };
}
