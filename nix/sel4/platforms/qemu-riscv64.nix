{lib}: {
  crossSystem = lib.systems.examples.riscv64;
  settings = {
    KernelPlatform = "qemu-riscv-virt";
    KernelMaxNumNodes = 4;
  };
}
