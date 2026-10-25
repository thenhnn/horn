{lib}: {
  pc = import ./pc.nix {inherit lib;};
  qemu-aarch64 = import ./qemu-aarch64.nix {inherit lib;};
  qemu-riscv64 = import ./qemu-riscv64.nix {inherit lib;};
}
